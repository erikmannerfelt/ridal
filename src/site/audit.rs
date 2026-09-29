//! Who changed the site's accounts and projects, and when (#214).
//!
//! ```text
//! audit.json
//! ```
//!
//! Accounts and memberships are what goes wrong quietly: a name stops being
//! a member, an administrator appears, a project disappears, and the first
//! question is who did it. Ridal's per-project audit answers "who changed
//! the catalog"; this one answers "who changed who", at the site root.
//!
//! # What this is not
//!
//! Not a security control, for the same reason the project audit is not:
//! anyone who can edit the site directory can edit this file, and Ridal has
//! no tamper-evidence to offer. It answers "what happened here" for someone
//! trying to work out what happened, which is the case it is for.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the site is changed through the browser or the CLI; a \
                  CLI-only build still needs the types"
    )
)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::ProjectKey;
use crate::project::store::{DocumentStore, Expectation, StoreError};
use crate::project::users::{DownloadScope, Role};

/// The document's path, relative to the site root.
pub const FILE: &str = "audit.json";

/// The most entries kept, trimmed from the front. The same bound and the
/// same reasoning as the project audit: this file is rewritten whole on
/// every append, so an unbounded one eventually stops being written.
pub const MAX_ENTRIES: usize = 10_000;

/// What was done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// A site account was created.
    AccountCreated,
    /// A site account was removed.
    AccountRemoved,
    /// Server administration was granted.
    ServerAdminGranted,
    /// Server administration was revoked.
    ServerAdminRevoked,
    /// A fresh invite link was issued, for a reset or a lost link.
    InviteIssued,
    /// An account set its own password from an invite.
    AccountActivated,
    /// An account joined a project.
    MembershipAdded,
    /// An account's role or download scope in a project changed.
    MembershipChanged,
    /// An account was removed from a project. The account itself remains.
    MembershipRemoved,
    /// A project was created.
    ProjectCreated,
    /// A project's display name changed.
    ProjectRenamed,
    /// A project was archived.
    ProjectArchived,
    /// A project was unarchived.
    ProjectUnarchived,
    /// A project and everything it owned was deleted.
    ProjectDeleted,
    /// A project's read/anonymous-download policy changed.
    AccessChanged,
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// RFC 3339, UTC.
    pub at: String,
    /// Who. An account name, or `cli` for a change made through the command
    /// line, which has no session to name.
    pub actor: String,
    pub action: Action,
    /// The account or project key the action is about.
    pub subject: String,
    /// The project a membership change involves, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectKey>,
    /// The role involved, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// The download scope involved, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadScope>,
    /// Free text: how a batch was labelled, what a removal left behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Entry {
    pub fn new(actor: impl Into<String>, action: Action, subject: impl Into<String>) -> Self {
        Self {
            at: now_rfc3339(),
            actor: actor.into(),
            action,
            subject: subject.into(),
            project: None,
            role: None,
            download: None,
            note: None,
        }
    }

    pub fn project(mut self, key: &ProjectKey) -> Self {
        self.project = Some(key.clone());
        self
    }

    pub fn membership(mut self, role: Role, download: DownloadScope) -> Self {
        self.role = Some(role);
        self.download = Some(download);
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
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
            AuditError::Malformed { path, message } => write!(
                f,
                "{} is not a valid site audit log: {message}",
                path.display()
            ),
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

/// Append one entry, read-modify-write against the version read and retried
/// on conflict, like every other document here.
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

/// Append without failing the operation it describes. The change has already
/// happened by the time this is called, so a failure here must not report a
/// failure that did not occur.
pub fn record(store: &DocumentStore, entry: Entry) {
    if let Err(e) = append(store, entry) {
        eprintln!("Warning: could not write to the site audit log: {e}");
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DocumentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn a_site_with_no_history_reads_as_empty() {
        let (_dir, store) = store();
        let (log, expectation) = read(&store).unwrap();
        assert!(log.entries.is_empty());
        assert!(matches!(expectation, Expectation::Absent));
    }

    #[test]
    fn entries_accumulate_with_their_details() {
        let (_dir, store) = store();
        let key = ProjectKey::new("glac").unwrap();
        append(
            &store,
            Entry::new("anna", Action::MembershipAdded, "bo")
                .project(&key)
                .membership(Role::Picker, DownloadScope::Picks),
        )
        .unwrap();
        append(&store, Entry::new("cli", Action::AccountRemoved, "bo")).unwrap();

        let (log, _) = read(&store).unwrap();
        assert_eq!(log.entries.len(), 2);
        assert_eq!(log.entries[0].actor, "anna");
        assert_eq!(log.entries[0].project.as_ref().unwrap().as_str(), "glac");
        assert_eq!(log.entries[0].role, Some(Role::Picker));
        assert_eq!(log.entries[1].action, Action::AccountRemoved);
    }

    #[test]
    fn the_log_is_trimmed_from_the_front() {
        let (_dir, store) = store();
        let mut log = Log::default();
        for i in 0..MAX_ENTRIES {
            log.entries.push(Entry::new(
                "cli",
                Action::AccountCreated,
                format!("u{i:05}"),
            ));
        }
        let text = format!("{}\n", serde_json::to_string(&log).unwrap());
        store.write(&path_of(), &text, &Expectation::Any).unwrap();

        append(&store, Entry::new("cli", Action::AccountRemoved, "newest")).unwrap();

        let (log, _) = read(&store).unwrap();
        assert_eq!(log.entries.len(), MAX_ENTRIES);
        assert_eq!(log.entries.last().unwrap().subject, "newest");
    }

    #[test]
    fn a_failure_to_log_does_not_fail_the_thing_it_logs() {
        let (dir, store) = store();
        store
            .write(&path_of(), "{ not json", &Expectation::Any)
            .unwrap();
        assert!(append(&store, Entry::new("cli", Action::ProjectDeleted, "glac")).is_err());
        record(&store, Entry::new("cli", Action::ProjectDeleted, "glac"));
        assert!(std::fs::read_to_string(dir.path().join(FILE))
            .unwrap()
            .starts_with("{ not json"));
    }
}
