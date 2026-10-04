//! Who changed the site's accounts and projects, and when (#214).
//!
//! ```text
//! audit.jsonl      the current log, one JSON entry per line
//! audit.1.jsonl    the previous one, once the current one has filled up
//! ```
//!
//! Accounts and memberships are what goes wrong quietly: a name stops being
//! a member, an administrator appears, a project disappears, and the first
//! question is who did it. Ridal's per-project audit answers "who changed
//! the catalog"; this one answers "who changed who", at the site root.
//!
//! # Format
//!
//! JSON Lines, appended to and never rewritten, and rotated when it fills
//! up: see [`crate::project::jsonl`], which the project's catalog history
//! shares.
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

use serde::{Deserialize, Serialize};

use crate::identity::ProjectKey;
use crate::project::jsonl;
use crate::project::roles::{DownloadScope, Role};
use crate::project::store::DocumentStore;

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
    /// An API token was created (#194).
    TokenCreated,
    /// An API token was revoked (#194).
    TokenRevoked,
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

/// The history, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Log {
    pub entries: Vec<Entry>,
}

/// Read the whole history, oldest first.
pub fn read(store: &DocumentStore) -> std::io::Result<Log> {
    Ok(Log {
        entries: jsonl::read(store)?,
    })
}

/// Append without failing the operation it describes. The change has already
/// happened by the time this is called, so a failure here must not report a
/// failure that did not occur.
pub fn record(store: &DocumentStore, entry: Entry) {
    if let Err(e) = jsonl::append(store, &entry) {
        eprintln!("Warning: could not write to the site audit log: {e}");
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_keep_their_details() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let key = ProjectKey::new("glac").unwrap();
        record(
            &store,
            Entry::new("anna", Action::MembershipAdded, "bo")
                .project(&key)
                .membership(Role::Picker, DownloadScope::Picks),
        );
        record(&store, Entry::new("cli", Action::AccountRemoved, "bo"));

        let log = read(&store).unwrap();
        assert_eq!(log.entries.len(), 2);
        assert_eq!(log.entries[0].actor, "anna");
        assert_eq!(log.entries[0].project.as_ref().unwrap().as_str(), "glac");
        assert_eq!(log.entries[0].role, Some(Role::Picker));
        assert_eq!(log.entries[1].action, Action::AccountRemoved);
        assert!(dir.path().join(jsonl::FILE).is_file());
    }
}
