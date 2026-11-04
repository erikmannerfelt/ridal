//! Moving every interpretation of one radargram onto another (#500).
//!
//! Radargram ids are immutable: the id is hashed into the revision id, and
//! it keys the interpretations, the raw source, the profile assignment and
//! the catalog overrides. "Renaming" a radargram is therefore three steps,
//! each of which exists on its own: a new radargram under the new id, this
//! transfer of the picks onto it, and the removal of the old one, which
//! archives what is left behind.
//!
//! # Whether the picks still line up
//!
//! Checked, not assumed. When the two current revisions describe the same
//! axes, a document drawn on the old one's current revision is pointed at
//! the new one's, and is current there at once. Otherwise each document
//! keeps the revision it was drawn on, and that revision's kept axes and
//! ledger record are copied under the new id, so the carry machinery
//! (#148, #370) places the picks as it does after a reprocess and each
//! owner adopts their own. Carry looks a revision up by radargram and
//! revision id and never asks which radargram a revision id was declared
//! for, so the copies are all it needs.
//!
//! # What is never done
//!
//! Documents are not merged: one is written only where its owner has
//! nothing on the new radargram yet ([`Expectation::Absent`]), as a restore
//! from the archive is (#435). The originals are left where they are; the
//! removal that follows archives them, which is what keeps them from
//! reattaching to whatever later takes the old id.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "picks are transferred through the server's HTTP routes; a \
                  CLI-only build still compiles the project API"
    )
)]

use std::collections::BTreeSet;

use gprinterp::Document;

use super::interpretations::{self, InterpretationError};
use super::revisions::{self, ledger, SnapshotError};
use super::store::{DocumentStore, Expectation, StoreError, Version};
use crate::identity::{RadargramId, UserId};

/// The key under a transferred document's `meta` that says where it came
/// from.
pub const PROVENANCE_KEY: &str = "ridal_transferred_from";

/// What a transfer moves from and to.
#[derive(Debug, Clone)]
pub struct Plan<'a> {
    pub from: &'a RadargramId,
    /// The revision `from` serves now.
    pub from_revision: &'a str,
    pub to: &'a RadargramId,
    /// The revision `to` serves now.
    pub to_revision: &'a str,
    /// Whether the two current revisions describe the same axes
    /// ([`same_grid`]). Only then is a document drawn on `from_revision`
    /// pointed at `to_revision`.
    pub same_grid: bool,
    /// When, RFC 3339, for the provenance and for the copied ledger
    /// records of revisions that were still current under `from`.
    pub at: &'a str,
    /// Who asked, for the provenance.
    pub by: &'a str,
}

/// One document that was moved.
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub user: UserId,
    pub lines: usize,
    /// The revision the document now names, if it names one.
    pub revision_id: Option<String>,
    /// Whether that is `to`'s current revision. When it is not, the picks
    /// are carried onto it and their owner adopts them, as after a
    /// reprocess.
    pub current: bool,
    pub version: Version,
}

/// What [`transfer`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub moved: Vec<Moved>,
    /// Users who already have an interpretation of `to`. Their document on
    /// `from` was not moved, and is archived with `from` if it is removed.
    pub occupied: Vec<UserId>,
    /// Files under `from` whose names are not a valid user id (#213), left
    /// where they are.
    pub unreadable: Vec<String>,
    /// Revisions whose kept axes and ledger record were copied under `to`
    /// so the documents naming them can be carried.
    pub revisions_copied: Vec<String>,
    /// Revisions documents name that have no kept axes under `from`. Those
    /// documents carry only through axes of their own, as they did before.
    pub revisions_missing: Vec<String>,
}

#[derive(Debug)]
pub enum TransferError {
    Interpretation(InterpretationError),
    Snapshot(SnapshotError),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Interpretation(e) => write!(f, "{e}"),
            TransferError::Snapshot(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TransferError {}

impl From<InterpretationError> for TransferError {
    fn from(e: InterpretationError) -> Self {
        TransferError::Interpretation(e)
    }
}

impl From<SnapshotError> for TransferError {
    fn from(e: SnapshotError) -> Self {
        TransferError::Snapshot(e)
    }
}

impl From<StoreError> for TransferError {
    fn from(e: StoreError) -> Self {
        TransferError::Interpretation(InterpretationError::Store(e))
    }
}

/// Whether picks drawn on `from`'s current revision fit `to`'s as they
/// are: both kept axes exist and describe the same mapping.
///
/// The radargram ids inside the snapshots differ by construction and are
/// not compared. A revision without kept axes is never the same grid,
/// which sends its documents through carry: a refusal there is visible,
/// where a wrong "same" would be silent.
pub fn same_grid(
    store: &DocumentStore,
    from: &RadargramId,
    from_revision: &str,
    to: &RadargramId,
    to_revision: &str,
) -> Result<bool, SnapshotError> {
    let (Some(a), Some(b)) = (
        revisions::get(store, from, from_revision)?,
        revisions::get(store, to, to_revision)?,
    ) else {
        return Ok(false);
    };
    Ok(revisions::describes_same_mapping(&a, &b))
}

/// Users with an interpretation of `radargram`, and the file stems that are
/// not valid user ids. What a caller checks permission against before
/// calling [`transfer`].
pub fn owners(
    store: &DocumentStore,
    radargram: &RadargramId,
) -> Result<(Vec<UserId>, Vec<String>), InterpretationError> {
    interpretations::list_users_checked(store, radargram)
}

/// Copy every interpretation of `plan.from` onto `plan.to`.
///
/// The caller holds the lifecycle lock, so no save, removal or restore
/// lands between reading a document and writing its copy.
pub fn transfer(store: &DocumentStore, plan: &Plan<'_>) -> Result<Report, TransferError> {
    let (users, unreadable) = owners(store, plan.from)?;
    let mut report = Report {
        unreadable,
        ..Report::default()
    };

    // Every document is rewritten before anything is written, so the
    // revisions the copies will name are known and copied first: a
    // document is never on `to` naming axes that are not there yet.
    let mut pending = Vec::new();
    let mut needed = BTreeSet::new();
    for user in users {
        let Some(stored) = interpretations::read(store, plan.from, &user)? else {
            continue;
        };
        let document = rewritten(stored.document, plan);
        if let Some(revision) = revision_of(&document) {
            if revision != plan.to_revision {
                needed.insert(revision.to_string());
            }
        }
        pending.push((user, document));
    }

    for revision in &needed {
        if copy_revision(store, plan, revision)? {
            report.revisions_copied.push(revision.clone());
        } else {
            report.revisions_missing.push(revision.clone());
        }
    }

    for (user, document) in pending {
        let version =
            match interpretations::write(store, plan.to, &user, &document, &Expectation::Absent) {
                Ok(version) => version,
                Err(InterpretationError::Store(StoreError::Conflict { .. })) => {
                    report.occupied.push(user);
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
        let revision_id = revision_of(&document).map(str::to_string);
        report.moved.push(Moved {
            user,
            lines: document.features.len(),
            // A document that does not say is drawn as if it were current,
            // as a restored one is.
            current: revision_id.as_deref().is_none_or(|r| r == plan.to_revision),
            revision_id,
            version,
        });
    }
    Ok(report)
}

fn revision_of(document: &Document) -> Option<&str> {
    document.source.as_ref()?.revision_id.as_deref()
}

/// `document` as it belongs on `plan.to`: its key and source id, its
/// revision where the grids match, and a note of where it came from.
fn rewritten(mut document: Document, plan: &Plan<'_>) -> Document {
    document.key = plan.to.to_string();
    let from_revision = revision_of(&document).map(str::to_string);
    if let Some(source) = document.source.as_mut() {
        if source.id.is_some() {
            source.id = Some(plan.to.to_string());
        }
        if plan.same_grid && source.revision_id.as_deref() == Some(plan.from_revision) {
            source.revision_id = Some(plan.to_revision.to_string());
        }
    }
    let provenance = serde_json::json!({
        "radargram_id": plan.from.as_str(),
        "revision_id": from_revision,
        "at": plan.at,
        "by": plan.by,
    });
    document
        .meta
        .get_or_insert_with(Default::default)
        .insert(PROVENANCE_KEY.to_string(), provenance);
    document
}

/// Copy `revision`'s kept axes and ledger record from `plan.from` to
/// `plan.to`. Returns whether there were axes to copy.
///
/// The record is copied as ended, because under `plan.to` the revision was
/// never served: the new id's own current revision is what replaced it,
/// which is what carry and the 30-day sweep (#434) both read.
fn copy_revision(
    store: &DocumentStore,
    plan: &Plan<'_>,
    revision: &str,
) -> Result<bool, TransferError> {
    let Some(mut snapshot) = revisions::get(store, plan.from, revision)? else {
        return Ok(false);
    };
    snapshot.radargram_id = plan.to.to_string();
    revisions::put(store, plan.to, &snapshot)?;
    ledger::update(store, |l| {
        let record = l
            .radargrams
            .get(plan.from.as_str())
            .and_then(|records| records.iter().find(|r| r.revision_id == revision))
            .cloned()
            .unwrap_or_else(|| ledger::RevisionRecord {
                revision_id: revision.to_string(),
                axis_checksum: Some(snapshot.checksum()),
                ..Default::default()
            });
        let records = l.radargrams.entry(plan.to.to_string()).or_default();
        if records.iter().any(|r| r.revision_id == revision) {
            return;
        }
        let ended = ledger::RevisionRecord {
            superseded_at: record.superseded_at.clone().or(Some(plan.at.to_string())),
            superseded_by: Some(plan.to_revision.to_string()),
            ..record
        };
        // Oldest first: it ended before the new id's current revision.
        records.insert(0, ended);
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        (dir, project)
    }

    fn snapshot(radargram: &str, revision: &str, dt: f64) -> revisions::AxisSnapshot {
        revisions::AxisSnapshot {
            radargram_id: radargram.to_string(),
            revision_id: revision.to_string(),
            y_anchor: Some("twtt".to_string()),
            y_values: (0..8).map(|i| i as f64 * dt).collect(),
            x_values: (0..40).map(|i| i as f64).collect(),
            y_alternate: None,
        }
    }

    fn document(key: &str, revision: Option<&str>) -> Document {
        let mut value = serde_json::json!({
            "key": key,
            "features": [{
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": [[1.0, 2.0], [9.0, 3.0]]},
                "properties": {"id": "f-0001", "label": "bed"}
            }]
        });
        if let Some(revision) = revision {
            value["source"] = serde_json::json!({"id": key, "revision_id": revision});
        }
        serde_json::from_value(value).unwrap()
    }

    fn ids() -> (RadargramId, RadargramId) {
        (
            RadargramId::new("dat_0130_a1").unwrap(),
            RadargramId::new("dronbreen-2022-0130").unwrap(),
        )
    }

    fn plan<'a>(from: &'a RadargramId, to: &'a RadargramId, same_grid: bool) -> Plan<'a> {
        Plan {
            from,
            from_revision: "old-current",
            to,
            to_revision: "new-current",
            same_grid,
            at: "2026-11-04T20:00:00Z",
            by: "erik",
        }
    }

    #[test]
    fn the_same_grid_points_picks_at_the_new_revision() {
        let (_dir, project) = project();
        let store = project.documents();
        let (from, to) = ids();
        revisions::put(store, &from, &snapshot(from.as_str(), "old-current", 2.0)).unwrap();
        revisions::put(store, &to, &snapshot(to.as_str(), "new-current", 2.0)).unwrap();
        assert!(same_grid(store, &from, "old-current", &to, "new-current").unwrap());

        let erik = UserId::new("erik").unwrap();
        interpretations::write(
            store,
            &from,
            &erik,
            &document(from.as_str(), Some("old-current")),
            &Expectation::Absent,
        )
        .unwrap();

        let report = transfer(store, &plan(&from, &to, true)).unwrap();
        assert_eq!(report.moved.len(), 1);
        assert!(report.moved[0].current);
        assert!(report.revisions_copied.is_empty());

        let moved = interpretations::read(store, &to, &erik).unwrap().unwrap();
        assert_eq!(moved.document.key, to.as_str());
        let source = moved.document.source.unwrap();
        assert_eq!(source.id.as_deref(), Some(to.as_str()));
        assert_eq!(source.revision_id.as_deref(), Some("new-current"));
        let provenance = &moved.document.meta.unwrap()[PROVENANCE_KEY];
        assert_eq!(provenance["radargram_id"], from.as_str());
        assert_eq!(provenance["revision_id"], "old-current");
        // The original is left for the removal to archive.
        assert!(interpretations::read(store, &from, &erik)
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_different_grid_keeps_the_revision_and_copies_what_carry_needs() {
        let (_dir, project) = project();
        let store = project.documents();
        let (from, to) = ids();
        revisions::put(store, &from, &snapshot(from.as_str(), "old-current", 2.0)).unwrap();
        revisions::put(store, &to, &snapshot(to.as_str(), "new-current", 4.0)).unwrap();
        ledger::update(store, |l| {
            ledger::note_current(l, from.as_str(), "old-current", Some("sum".into()));
            ledger::note_facts(
                l,
                from.as_str(),
                "old-current",
                ledger::RevisionFacts {
                    time_zero_ns: Some(12.0),
                    ..Default::default()
                },
            );
            ledger::note_current(l, to.as_str(), "new-current", None);
        })
        .unwrap();
        assert!(!same_grid(store, &from, "old-current", &to, "new-current").unwrap());

        let (erik, anna, olga) = (
            UserId::new("erik").unwrap(),
            UserId::new("anna").unwrap(),
            UserId::new("olga").unwrap(),
        );
        for (user, revision) in [
            (&erik, Some("old-current")),
            (&anna, Some("never-kept")),
            (&olga, None),
        ] {
            interpretations::write(
                store,
                &from,
                user,
                &document(from.as_str(), revision),
                &Expectation::Absent,
            )
            .unwrap();
        }

        let report = transfer(store, &plan(&from, &to, false)).unwrap();
        assert_eq!(report.revisions_copied, vec!["old-current".to_string()]);
        assert_eq!(report.revisions_missing, vec!["never-kept".to_string()]);
        let erik_moved = report.moved.iter().find(|m| m.user == erik).unwrap();
        assert!(!erik_moved.current);
        assert_eq!(erik_moved.revision_id.as_deref(), Some("old-current"));
        // A document naming no revision is drawn as if current.
        assert!(
            report
                .moved
                .iter()
                .find(|m| m.user == olga)
                .unwrap()
                .current
        );

        let copied = revisions::get(store, &to, "old-current").unwrap().unwrap();
        assert_eq!(copied.radargram_id, to.as_str());
        let (l, _) = ledger::read(store).unwrap();
        // The new id's own revision stays current, the copy has ended, and
        // what processing did comes along for the carry warnings.
        assert_eq!(l.current(to.as_str()).unwrap().revision_id, "new-current");
        let facts = ledger::facts(&l, to.as_str(), "old-current");
        assert_eq!(facts.time_zero_ns, Some(12.0));
        let record = &l.radargrams[to.as_str()][0];
        assert_eq!(record.revision_id, "old-current");
        assert_eq!(record.superseded_by.as_deref(), Some("new-current"));
        assert_eq!(record.axis_checksum.as_deref(), Some("sum"));
    }

    #[test]
    fn picks_already_on_the_new_radargram_are_never_replaced() {
        let (_dir, project) = project();
        let store = project.documents();
        let (from, to) = ids();
        let erik = UserId::new("erik").unwrap();
        interpretations::write(
            store,
            &from,
            &erik,
            &document(from.as_str(), None),
            &Expectation::Absent,
        )
        .unwrap();
        let mut theirs = document(to.as_str(), None);
        theirs.features.clear();
        interpretations::write(store, &to, &erik, &theirs, &Expectation::Absent).unwrap();

        let report = transfer(store, &plan(&from, &to, true)).unwrap();
        assert!(report.moved.is_empty());
        assert_eq!(report.occupied, vec![erik.clone()]);
        let kept = interpretations::read(store, &to, &erik).unwrap().unwrap();
        assert!(kept.document.features.is_empty());
    }
}
