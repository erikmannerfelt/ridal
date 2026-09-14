//! Who changed the catalog, and when (#147).
//!
//! ```text
//! audit.json
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
//! Not a revision ledger either. A ledger records what a radargram *is*
//! across revisions and is what re-anchoring reads (#148); this records what
//! people *did*, and nothing reads it back.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the catalog is edited through the browser; a CLI-only \
                  build still needs the types to read a project"
    )
)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::store::{DocumentStore, Expectation, StoreError};

/// The document's path, relative to the project root.
pub const FILE: &str = "audit.json";

/// The most entries kept.
///
/// Trimmed from the front when it would be exceeded. A log that grows
/// without bound is a log that eventually stops being written because
/// rewriting it costs too much — this document is rewritten whole on every
/// append, which is the trade for keeping it a plain readable file rather
/// than inventing an append-only format.
pub const MAX_ENTRIES: usize = 10_000;

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
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Log {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

#[derive(Debug)]
pub enum AuditError {
    Store(StoreError),
    Malformed { path: PathBuf, message: String },
}

impl std::fmt::Display for AuditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditError::Store(e) => write!(f, "{e}"),
            AuditError::Malformed { path, message } => {
                write!(f, "{} is not a valid audit log: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for AuditError {}

impl From<StoreError> for AuditError {
    fn from(e: StoreError) -> Self {
        AuditError::Store(e)
    }
}

fn path_of() -> PathBuf {
    PathBuf::from(FILE)
}

/// Read the log, with the version to write back against.
pub fn read(store: &DocumentStore) -> Result<(Log, Expectation), AuditError> {
    let relative = path_of();
    let Some(stored) = store.read(&relative)? else {
        return Ok((Log::default(), Expectation::Absent));
    };
    let parsed: Log = serde_json::from_str(&stored.text).map_err(|e| AuditError::Malformed {
        path: store.root().join(&relative),
        message: e.to_string(),
    })?;
    Ok((parsed, Expectation::Version(stored.version)))
}

/// Append one entry.
///
/// Read-modify-write against the version read, retried on conflict, like
/// every other document here. Two operators removing two radargrams at once
/// is the ordinary case rather than the exotic one.
pub fn append(store: &DocumentStore, entry: Entry) -> Result<(), AuditError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let (mut log, expectation) = read(store)?;
        log.entries.push(entry.clone());
        if log.entries.len() > MAX_ENTRIES {
            let excess = log.entries.len() - MAX_ENTRIES;
            log.entries.drain(..excess);
        }
        let mut text = serde_json::to_string_pretty(&log).map_err(|e| AuditError::Malformed {
            path: store.root().join(path_of()),
            message: e.to_string(),
        })?;
        text.push('\n');
        match store.write(&path_of(), &text, &expectation) {
            Ok(_) => return Ok(()),
            Err(StoreError::Conflict { .. }) if attempts < 3 => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Append without failing the operation it describes.
///
/// The operation has already happened by the time this is called: the file
/// is installed or gone. Failing the request now would report a failure
/// that did not occur and invite the caller to retry something that
/// succeeded, which is worse than an unlogged action. The problem is
/// printed rather than swallowed.
pub fn record(store: &DocumentStore, entry: Entry) {
    if let Err(e) = append(store, entry) {
        eprintln!("Warning: could not write to the audit log: {e}");
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

    fn entry(id: &str, action: Action) -> Entry {
        Entry {
            at: "2026-09-13T12:00:00Z".to_string(),
            user: "erik".to_string(),
            action,
            radargram_id: id.to_string(),
            revision_id: None,
            note: None,
        }
    }

    #[test]
    fn a_project_with_no_history_reads_as_empty() {
        let (_dir, store) = store();
        let (log, expectation) = read(&store).unwrap();
        assert!(log.entries.is_empty());
        assert!(matches!(expectation, Expectation::Absent));
    }

    #[test]
    fn entries_accumulate_in_order() {
        let (_dir, store) = store();
        append(&store, entry("line-01", Action::Added)).unwrap();
        append(&store, entry("line-02", Action::Added)).unwrap();
        append(&store, entry("line-01", Action::Removed)).unwrap();

        let (log, _) = read(&store).unwrap();
        assert_eq!(log.entries.len(), 3);
        assert_eq!(log.entries[0].radargram_id, "line-01");
        assert_eq!(log.entries[2].action, Action::Removed);
    }

    #[test]
    fn the_log_is_trimmed_from_the_front_rather_than_growing_forever() {
        // Oldest first, because the recent past is what anyone is looking
        // for when a radargram goes missing.
        let (_dir, store) = store();
        let mut log = Log::default();
        for i in 0..MAX_ENTRIES {
            log.entries
                .push(entry(&format!("line-{i:05}"), Action::Added));
        }
        let text = format!("{}\n", serde_json::to_string(&log).unwrap());
        store.write(&path_of(), &text, &Expectation::Any).unwrap();

        append(&store, entry("newest", Action::Removed)).unwrap();

        let (log, _) = read(&store).unwrap();
        assert_eq!(log.entries.len(), MAX_ENTRIES);
        assert_eq!(log.entries.last().unwrap().radargram_id, "newest");
        assert_eq!(
            log.entries.first().unwrap().radargram_id,
            "line-00001",
            "the oldest one gave way"
        );
    }

    #[test]
    fn a_failure_to_log_does_not_fail_the_thing_it_logs() {
        // By the time this is called the file is installed or gone. Failing
        // now would report a failure that did not happen and invite a retry
        // of something that succeeded.
        let (dir, store) = store();
        store
            .write(&path_of(), "{ not json", &Expectation::Any)
            .unwrap();

        assert!(append(&store, entry("line-01", Action::Added)).is_err());
        record(&store, entry("line-01", Action::Added));
        // Still broken, still not a panic, and the caller was not told.
        assert!(std::fs::read_to_string(dir.path().join(FILE))
            .unwrap()
            .starts_with("{ not json"));
    }
}
