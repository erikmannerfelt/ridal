//! Catalog metadata a project can change without reprocessing (#145).
//!
//! ```text
//! overrides.json
//! ```
//!
//! A processed NetCDF is immutable and should stay that way. But the display
//! name and the grouping inside it were decided at processing time, and
//! fine-tuning them otherwise means reprocessing a radargram for a label.
//!
//! # One document, not one per radargram
//!
//! The catalog page renders every entry, so per-radargram documents would be
//! N reads per page load. The whole thing is a few hundred bytes per
//! radargram, and concurrent edits go through [`update`] the same way
//! [`super::users`] handles them.
//!
//! # Two kinds of override
//!
//! The obvious shape is for each radargram to carry both `group_id` and
//! `group_name`. That makes renaming a group an edit to every member, and
//! lets twenty radargrams disagree about what one group is called with the
//! catalog having to pick a winner.
//!
//! So they are split: the per-radargram override sets *membership* only, and
//! the group override is its own thing whose one job today is an
//! authoritative `group_id -> group_name` mapping. Renaming a group is then
//! one edit in one place and the name cannot fork.
//!
//! This is also how the code already thinks -- [`crate::identity`] treats the
//! group name as free-form Unicode and the id as the URL-safe slug derived
//! from it.
//!
//! # What is deliberately not overridable
//!
//! The radargram id. It is the join key for `interpretations/<radargram>/`,
//! for the level 2 `radargram_id` column, and for every URL, so renaming it
//! would orphan picks. A file processed with a bad auto-derived id can only
//! be fixed by reprocessing; if that turns out to hurt, the answer is an
//! alias map rather than a mutable id.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "overrides are edited through the browser; a CLI-only build \
                  still needs the types to read a project's catalog"
    )
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::store::{DocumentStore, Expectation, StoreError, Version};
use crate::identity::{DisplayName, GroupId, GroupName, RadargramId};

/// The document's path, relative to the project root.
pub const FILE: &str = "overrides.json";

/// What a project says about a radargram, over what the file says.
///
/// Every field is `None`/`false` by default and absent means *inherit*,
/// resolved per field: setting only a display name leaves grouping alone.
/// The alternative -- storing the file's current value in every field --
/// would freeze a radargram's metadata at the moment somebody first edited
/// one label of it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RadargramOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<DisplayName>,
    /// Membership only. The group's *name* lives in [`GroupOverride`], so
    /// that two members cannot disagree about it.
    ///
    /// `None` means inherit, which is not the same as [`Ungrouped`]: "the
    /// file decides" and "this one is deliberately in no group" are
    /// different answers, and a project needs both -- the second is how a
    /// radargram gets *out* of a group it was processed into.
    ///
    /// [`Ungrouped`]: GroupMembership::Ungrouped
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupMembership>,
    /// Curation, not access control. An unlisted radargram is absent from
    /// listings and still reachable by anyone who knows its id -- the same
    /// sense as an unlisted video or phone number. Making it enforced would
    /// be per-radargram permissions, which #131 put out of scope, and the
    /// word is chosen so nothing half-promises that.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unlisted: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl RadargramOverride {
    /// Whether this entry says nothing at all.
    ///
    /// [`CatalogOverrides::prune`] drops these before writing, so reverting
    /// every field of a radargram leaves no trace of it in the document
    /// rather than an empty object that reads as "somebody configured this".
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Where a project puts a radargram, over what its file says.
///
/// Tagged rather than a bare `Option<GroupId>` because there are three
/// states and an option has two: inherit, no group, and a named group. The
/// JSON reads as `"ungrouped"` or `{"group": "dronbreen-2022"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupMembership {
    /// In no group, whatever the file says.
    Ungrouped,
    /// In this group. Its *name* comes from [`GroupOverride`] or from the
    /// other members, never from here.
    Group(GroupId),
}

impl GroupMembership {
    pub fn id(&self) -> Option<&GroupId> {
        match self {
            Self::Ungrouped => None,
            Self::Group(id) => Some(id),
        }
    }
}

/// What a project says about a group.
///
/// An object rather than a bare string deliberately: it is the obvious home
/// for the next group-scoped thing (an ordering key, a description, a
/// default render profile), and widening a string to an object later is a
/// migration nobody needs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<GroupName>,
}

impl GroupOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Everything a project says over its files.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogOverrides {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub radargrams: BTreeMap<RadargramId, RadargramOverride>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub groups: BTreeMap<GroupId, GroupOverride>,
}

impl CatalogOverrides {
    /// The override for one radargram, or the empty one.
    pub fn radargram(&self, id: &RadargramId) -> RadargramOverride {
        self.radargrams.get(id).cloned().unwrap_or_default()
    }

    /// Drop entries that say nothing, so a fully reverted radargram leaves
    /// the document as it was before anyone touched it.
    pub fn prune(&mut self) {
        self.radargrams.retain(|_, o| !o.is_empty());
        self.groups.retain(|_, o| !o.is_empty());
    }
}

#[derive(Debug)]
pub enum OverridesError {
    Store(StoreError),
    Malformed { path: PathBuf, message: String },
}

impl std::fmt::Display for OverridesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverridesError::Store(e) => write!(f, "{e}"),
            OverridesError::Malformed { path, message } => write!(
                f,
                "{} is not a valid overrides document: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for OverridesError {}

impl From<StoreError> for OverridesError {
    fn from(e: StoreError) -> Self {
        OverridesError::Store(e)
    }
}

fn path_of() -> PathBuf {
    PathBuf::from(FILE)
}

/// Read the overrides, together with the version to write back against.
///
/// A project that has never had one reads as empty rather than as an absence
/// the caller must handle, paired with [`Expectation::Absent`] so the first
/// write still cannot clobber a document created in between.
pub fn read(store: &DocumentStore) -> Result<(CatalogOverrides, Expectation), OverridesError> {
    let relative = path_of();
    let Some(stored) = store.read(&relative)? else {
        return Ok((CatalogOverrides::default(), Expectation::Absent));
    };
    let parsed: CatalogOverrides =
        serde_json::from_str(&stored.text).map_err(|e| OverridesError::Malformed {
            path: store.root().join(&relative),
            message: e.to_string(),
        })?;
    Ok((parsed, Expectation::Version(stored.version)))
}

/// Read without failing on a malformed document.
///
/// For the request path: a hand-broken overrides file should cost the
/// project its labels, not every page. The editing route uses [`read`]
/// instead, so the fault is visible where it can be fixed -- and so that
/// saving an edit on top of a file nobody can parse is refused rather than
/// silently discarding whatever it held.
pub fn read_lenient(store: &DocumentStore) -> CatalogOverrides {
    read(store).map(|(o, _)| o).unwrap_or_default()
}

pub fn write(
    store: &DocumentStore,
    overrides: &CatalogOverrides,
    expected: &Expectation,
) -> Result<Version, OverridesError> {
    let mut text =
        serde_json::to_string_pretty(overrides).map_err(|e| OverridesError::Malformed {
            path: store.root().join(path_of()),
            message: e.to_string(),
        })?;
    text.push('\n');
    Ok(store.write(&path_of(), &text, expected)?)
}

/// Read, modify, write -- conditional on the version that was read.
///
/// One document for the whole catalog means two operators renaming two
/// different radargrams are editing the same file, which is exactly the
/// case a blind overwrite loses. Retried on conflict for the same reason
/// [`super::users::update`] is.
pub fn update<T>(
    store: &DocumentStore,
    change: impl Fn(&mut CatalogOverrides) -> Result<T, OverridesError>,
) -> Result<T, OverridesError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let (mut overrides, expectation) = read(store)?;
        let outcome = change(&mut overrides)?;
        overrides.prune();
        match write(store, &overrides, &expectation) {
            Ok(_) => return Ok(outcome),
            Err(OverridesError::Store(StoreError::Conflict { .. })) if attempts < 3 => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DocumentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn radargram(name: &str) -> RadargramId {
        RadargramId::new(name).unwrap()
    }

    fn group(name: &str) -> GroupId {
        GroupId::new(name).unwrap()
    }

    #[test]
    fn a_project_that_has_never_been_edited_reads_as_empty() {
        let (_dir, store) = store();
        let (overrides, expectation) = read(&store).unwrap();
        assert_eq!(overrides, CatalogOverrides::default());
        // Paired with Absent, so the first write still cannot clobber a
        // document created between the read and it.
        assert!(matches!(expectation, Expectation::Absent));
    }

    #[test]
    fn overrides_round_trip() {
        let (_dir, store) = store();
        update(&store, |o| {
            o.radargrams.insert(
                radargram("dronbreen-0237"),
                RadargramOverride {
                    display_name: DisplayName::from_input("Drønbreen centre line"),
                    group: Some(GroupMembership::Group(group("dronbreen-2022"))),
                    unlisted: false,
                },
            );
            o.groups.insert(
                group("dronbreen-2022"),
                GroupOverride {
                    name: GroupName::from_input("Drønbreen 2022"),
                },
            );
            Ok(())
        })
        .unwrap();

        let (read_back, _) = read(&store).unwrap();
        let entry = read_back.radargram(&radargram("dronbreen-0237"));
        assert_eq!(
            entry.display_name.as_ref().map(|n| n.as_str()),
            Some("Drønbreen centre line")
        );
        assert_eq!(
            entry.group,
            Some(GroupMembership::Group(group("dronbreen-2022")))
        );
        assert_eq!(
            read_back.groups[&group("dronbreen-2022")]
                .name
                .as_ref()
                .map(|n| n.as_str()),
            Some("Drønbreen 2022")
        );
    }

    #[test]
    fn an_unset_field_is_left_out_rather_than_stored_as_the_files_value() {
        // Absence means inherit. Writing the file's current value into every
        // field would freeze a radargram's metadata at the moment somebody
        // first edited one label of it, and a later reprocessing that fixed
        // the group would never show.
        let (dir, store) = store();
        update(&store, |o| {
            o.radargrams.insert(
                radargram("line-01"),
                RadargramOverride {
                    display_name: DisplayName::from_input("A better name"),
                    ..Default::default()
                },
            );
            Ok(())
        })
        .unwrap();

        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(text.contains("display_name"), "{text}");
        assert!(!text.contains("\"group\":"), "{text}");
        assert!(!text.contains("unlisted"), "{text}");
    }

    #[test]
    fn reverting_every_field_removes_the_entry_rather_than_leaving_an_empty_one() {
        let (dir, store) = store();
        update(&store, |o| {
            o.radargrams.insert(
                radargram("line-01"),
                RadargramOverride {
                    display_name: DisplayName::from_input("A better name"),
                    ..Default::default()
                },
            );
            Ok(())
        })
        .unwrap();

        update(&store, |o| {
            o.radargrams
                .get_mut(&radargram("line-01"))
                .unwrap()
                .display_name = None;
            Ok(())
        })
        .unwrap();

        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(
            !text.contains("line-01"),
            "a fully reverted radargram should leave no trace: {text}"
        );
        assert!(read(&store).unwrap().0.radargrams.is_empty());
    }

    #[test]
    fn two_edits_to_different_radargrams_do_not_lose_each_other() {
        // The reason this module has an `update` at all: one document for
        // the whole catalog means two operators renaming two unrelated
        // radargrams are writing the same file.
        let (_dir, store) = store();
        update(&store, |o| {
            o.radargrams.insert(
                radargram("line-01"),
                RadargramOverride {
                    display_name: DisplayName::from_input("First"),
                    ..Default::default()
                },
            );
            Ok(())
        })
        .unwrap();
        update(&store, |o| {
            o.radargrams.insert(
                radargram("line-02"),
                RadargramOverride {
                    display_name: DisplayName::from_input("Second"),
                    ..Default::default()
                },
            );
            Ok(())
        })
        .unwrap();

        let (overrides, _) = read(&store).unwrap();
        assert_eq!(overrides.radargrams.len(), 2);
    }

    #[test]
    fn an_empty_label_is_refused_rather_than_stored_as_a_blank_name() {
        // `DisplayName::from_input` defines whitespace as absent, so a
        // document holding `""` would carry a value its own constructor
        // rejects -- and would render as an invisible label.
        let (_dir, store) = store();
        store
            .write(
                &path_of(),
                r#"{"radargrams":{"line-01":{"display_name":"   "}}}"#,
                &Expectation::Any,
            )
            .unwrap();

        assert!(matches!(
            read(&store),
            Err(OverridesError::Malformed { .. })
        ));
    }

    #[test]
    fn a_malformed_document_is_an_error_to_edit_and_empty_to_serve() {
        let (_dir, store) = store();
        store
            .write(&path_of(), "{ not json", &Expectation::Any)
            .unwrap();

        // The request path must not turn one broken file into a broken page.
        assert_eq!(read_lenient(&store), CatalogOverrides::default());
        // But editing it must not silently discard whatever it held.
        assert!(matches!(
            read(&store),
            Err(OverridesError::Malformed { .. })
        ));
        assert!(update(&store, |_| Ok(())).is_err());
    }

    #[test]
    fn inherit_and_ungrouped_are_different_answers() {
        // An `Option<GroupId>` would collapse these, and the second is how
        // a radargram gets *out* of a group it was processed into.
        let (dir, store) = store();
        update(&store, |o| {
            o.radargrams.insert(
                radargram("stays"),
                RadargramOverride {
                    display_name: DisplayName::from_input("Inherits its grouping"),
                    ..Default::default()
                },
            );
            o.radargrams.insert(
                radargram("leaves"),
                RadargramOverride {
                    group: Some(GroupMembership::Ungrouped),
                    ..Default::default()
                },
            );
            Ok(())
        })
        .unwrap();

        let (read_back, _) = read(&store).unwrap();
        assert_eq!(read_back.radargram(&radargram("stays")).group, None);
        assert_eq!(
            read_back.radargram(&radargram("leaves")).group,
            Some(GroupMembership::Ungrouped)
        );

        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(text.contains("\"ungrouped\""), "{text}");
    }

    #[test]
    fn unlisted_is_reported_per_radargram() {
        let (_dir, store) = store();
        update(&store, |o| {
            o.radargrams.insert(
                radargram("quiet-one"),
                RadargramOverride {
                    unlisted: true,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .unwrap();

        let (overrides, _) = read(&store).unwrap();
        assert!(overrides.radargram(&radargram("quiet-one")).unlisted);
        // And a radargram nobody has touched is listed, without an entry.
        assert!(!overrides.radargram(&radargram("loud-one")).unlisted);
    }
}
