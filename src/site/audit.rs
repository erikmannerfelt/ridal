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
//! JSON Lines, appended to and never rewritten. An append costs the same
//! however long the history is, and an interrupted one can damage only the
//! line it was writing, which [`read`] skips. When the current file passes
//! [`MAX_BYTES`] it is renamed to `audit.1.jsonl`, replacing the one before,
//! so the history is bounded at about twice that. A rename loses nothing: an
//! append that opened the file just before it lands in the renamed file,
//! which is still read.
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

use std::io::{Read, Seek, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::identity::ProjectKey;
use crate::project::store::DocumentStore;
use crate::project::users::{DownloadScope, Role};

/// The current log, relative to the site root.
pub const FILE: &str = "audit.jsonl";

/// The previous log, kept when the current one is rotated.
pub const PREVIOUS_FILE: &str = "audit.1.jsonl";

/// The size at which the current log is rotated. An entry is a couple of
/// hundred bytes, so this keeps something like the last 10 000-20 000.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

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

/// The history, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Log {
    pub entries: Vec<Entry>,
}

/// Read the whole history, oldest first.
///
/// A line that does not parse is skipped rather than failing the read: the
/// one way it arises in normal operation is an append interrupted part-way,
/// and one lost entry should not hide every other.
pub fn read(store: &DocumentStore) -> std::io::Result<Log> {
    let mut entries = Vec::new();
    for name in [PREVIOUS_FILE, FILE] {
        let text = match std::fs::read_to_string(store.root().join(name)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        entries.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Entry>(line).ok()),
        );
    }
    Ok(Log { entries })
}

/// Append one entry, rotating the log first if it has filled up.
pub fn append(store: &DocumentStore, entry: &Entry) -> std::io::Result<()> {
    let path = store.root().join(FILE);
    rotate_if_full(&path, &store.root().join(PREVIOUS_FILE))?;
    let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)?;
    // A previous append interrupted mid-line would otherwise swallow this
    // entry into the same unparsable line.
    if file.metadata()?.len() > 0 {
        let mut last = [0u8; 1];
        file.seek(std::io::SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.insert(0, '\n');
        }
    }
    // One `write_all` of one line to a file opened for appending, so
    // concurrent writers (the server and the CLI) interleave whole lines.
    file.write_all(line.as_bytes())
}

fn rotate_if_full(path: &Path, previous: &Path) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() >= MAX_BYTES => std::fs::rename(path, previous),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Append without failing the operation it describes. The change has already
/// happened by the time this is called, so a failure here must not report a
/// failure that did not occur.
pub fn record(store: &DocumentStore, entry: Entry) {
    if let Err(e) = append(store, &entry) {
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
        assert!(read(&store).unwrap().entries.is_empty());
    }

    #[test]
    fn entries_accumulate_with_their_details() {
        let (dir, store) = store();
        let key = ProjectKey::new("glac").unwrap();
        append(
            &store,
            &Entry::new("anna", Action::MembershipAdded, "bo")
                .project(&key)
                .membership(Role::Picker, DownloadScope::Picks),
        )
        .unwrap();
        append(&store, &Entry::new("cli", Action::AccountRemoved, "bo")).unwrap();

        let log = read(&store).unwrap();
        assert_eq!(log.entries.len(), 2);
        assert_eq!(log.entries[0].actor, "anna");
        assert_eq!(log.entries[0].project.as_ref().unwrap().as_str(), "glac");
        assert_eq!(log.entries[0].role, Some(Role::Picker));
        assert_eq!(log.entries[1].action, Action::AccountRemoved);
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn a_torn_line_costs_only_itself() {
        let (dir, store) = store();
        append(&store, &Entry::new("cli", Action::ProjectCreated, "a")).unwrap();
        // An append interrupted part-way through its line.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE))
            .unwrap()
            .write_all(br#"{"at":"2026-"#)
            .unwrap();
        record(&store, Entry::new("cli", Action::ProjectCreated, "b"));

        let subjects: Vec<String> = read(&store)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.subject)
            .collect();
        assert_eq!(subjects, ["a", "b"]);
    }

    #[test]
    fn a_full_log_is_rotated_and_still_read() {
        let (dir, store) = store();
        append(&store, &Entry::new("cli", Action::ProjectCreated, "old")).unwrap();
        let padding = format!("{}\n", "x".repeat(MAX_BYTES as usize));
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE))
            .unwrap()
            .write_all(padding.as_bytes())
            .unwrap();

        append(&store, &Entry::new("cli", Action::ProjectCreated, "new")).unwrap();
        assert!(dir.path().join(PREVIOUS_FILE).is_file());
        let current = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(current.lines().count(), 1);

        let subjects: Vec<String> = read(&store)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.subject)
            .collect();
        assert_eq!(subjects, ["old", "new"]);
    }
}
