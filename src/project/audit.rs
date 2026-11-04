//! Who changed the catalog, and when (#147).
//!
//! ```text
//! audit.jsonl      the current log, one JSON entry per line
//! audit.1.jsonl    the previous one, once the current one has filled up
//! ```
//!
//! Adding and removing radargrams is destructive and multi-user, and the
//! first question anyone asks when a radargram goes missing is who took it.
//! A log is cheap and there is no substitute for it after the fact.
//!
//! # What this is not
//!
//! Not a security control. Ridal has no tamper-evidence to offer: anyone
//! who can edit the project can edit this file, and saying otherwise would
//! be worse than saying nothing. It answers "what happened here" for people
//! who are trying to work out what happened, which is the case it is
//! actually for.
//!
//! JSON Lines, appended to and never rewritten, and rotated when it fills
//! up: see [`super::jsonl`], which the site's ledger shares. It used to be
//! one JSON document rewritten whole on every entry, which cost the length
//! of the history per append and could lose all of it to one interrupted
//! write.
//!
//! Not a revision ledger either. A ledger records what a radargram *is*
//! across revisions and is what re-anchoring reads (#148); this records what
//! people *did*. The one thing that reads it back is [`moved_to`], which
//! answers a save on a radargram whose picks were transferred away (#500).

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the catalog is edited through the browser; a CLI-only \
                  build still needs the types to read a project"
    )
)]

use serde::{Deserialize, Serialize};

use super::jsonl;
use super::store::DocumentStore;

/// What was done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// A radargram was uploaded into the project.
    Added,
    /// A radargram's file was removed from the project.
    Removed,
    /// A radargram in a read-only root was left out of the catalog. Its
    /// file is untouched — Ridal never writes outside the project.
    Ignored,
    /// An ignore was lifted.
    Unignored,
    /// A different revision was put behind an existing radargram id.
    Replaced,
    /// Picks carried from an earlier revision were adopted as drawn on the
    /// current one.
    Adopted,
    /// Someone's interpretation was moved into the archive by an
    /// administrator (#178). Nothing is destroyed.
    Archived,
    /// Archived interpretations were destroyed (#162): the one action here
    /// that cannot be undone.
    Purged,
    /// An archived interpretation was moved back onto its radargram by an
    /// administrator (#435).
    Restored,
    /// A radargram's interpretations were copied onto another radargram,
    /// named in `to_radargram_id` (#500). The one record of an id change.
    Transferred,
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// RFC 3339, UTC.
    pub at: String,
    /// Who. The `default` user on a project with no accounts, which is
    /// honest — it says as much as the project knows.
    pub user: String,
    pub action: Action,
    pub radargram_id: String,
    /// The revision involved, where there was one. Absent for an ignore
    /// lifted on a radargram that is no longer there to have a revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
    /// Free text for what the fields above cannot carry: how many
    /// interpretations were archived with a removal, the display name a
    /// file arrived under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The other radargram, for a transfer: where the picks went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_radargram_id: Option<String>,
}

/// Every entry, oldest first. Only tests read the log back.
#[cfg(test)]
pub fn read(store: &DocumentStore) -> std::io::Result<Vec<Entry>> {
    jsonl::read(store)
}

/// Where the picks of `radargram` were last transferred to (#500), unless
/// the id has been added again since.
///
/// For the one message that must name the new id: a save from a picker
/// still open on a radargram renamed under it. The log is read whole, which
/// is fine for something that happens only when such a save is refused.
pub fn moved_to(store: &DocumentStore, radargram: &str) -> Option<String> {
    let entries: Vec<Entry> = jsonl::read(store).ok()?;
    let mut to = None;
    for entry in entries.iter().filter(|e| e.radargram_id == radargram) {
        match entry.action {
            Action::Transferred => to = entry.to_radargram_id.clone(),
            // A file under the id again is a different radargram.
            Action::Added | Action::Unignored => to = None,
            _ => {}
        }
    }
    to
}

/// Append without failing the operation it describes.
///
/// The operation has already happened by the time this is called: the file
/// is installed or gone. Failing the request now would report a failure
/// that did not occur and invite the caller to retry something that
/// succeeded, which is worse than an unlogged action. The problem is
/// printed rather than swallowed.
pub fn record(store: &DocumentStore, entry: Entry) {
    if let Err(e) = jsonl::append(store, &entry) {
        eprintln!("Warning: could not write to the audit log: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, action: Action) -> Entry {
        Entry {
            to_radargram_id: None,
            at: "2026-09-13T12:00:00Z".to_string(),
            user: "erik".to_string(),
            action,
            radargram_id: id.to_string(),
            revision_id: None,
            note: None,
        }
    }

    #[test]
    fn entries_accumulate_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        record(&store, entry("line-01", Action::Added));
        record(&store, entry("line-02", Action::Added));
        record(&store, entry("line-01", Action::Removed));

        let entries = read(&store).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].radargram_id, "line-01");
        assert_eq!(entries[2].action, Action::Removed);
    }

    #[test]
    fn a_transfer_is_where_the_picks_went_until_the_id_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        assert_eq!(moved_to(&store, "line-01"), None);
        record(
            &store,
            Entry {
                to_radargram_id: Some("line-02".to_string()),
                ..entry("line-01", Action::Transferred)
            },
        );
        record(&store, entry("line-01", Action::Removed));
        assert_eq!(moved_to(&store, "line-01").as_deref(), Some("line-02"));
        assert_eq!(moved_to(&store, "line-02"), None);
        record(&store, entry("line-01", Action::Added));
        assert_eq!(moved_to(&store, "line-01"), None);
    }

    #[test]
    fn a_failure_to_log_does_not_fail_the_thing_it_logs() {
        // By the time this is called the file is installed or gone. Failing
        // now would report a failure that did not happen and invite a retry
        // of something that succeeded.
        let dir = tempfile::tempdir().unwrap();
        // A directory where the log should be: nothing can append to it.
        std::fs::create_dir(dir.path().join(jsonl::FILE)).unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        record(&store, entry("line-01", Action::Added));
        assert!(dir.path().join(jsonl::FILE).is_dir());
    }
}
