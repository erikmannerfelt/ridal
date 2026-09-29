//! Project memberships (#214).
//!
//! Inside a site, a project's `users.json` (the file name is kept) holds
//! **memberships** rather than accounts: for each account that may use the
//! project, its [`Role`] and [`DownloadScope`]. Passwords and invites live
//! server-wide in [`crate::site::accounts`]; a project never holds either.
//!
//! ```json
//! {
//!   "require_auth_to_read": false,
//!   "anonymous_download": "all",
//!   "members": [
//!     { "name": "anna", "role": "admin", "download": "all" }
//!   ]
//! }
//! ```
//!
//! A project whose file still holds the pre-site `users` array is refused by
//! [`crate::site::Site::project`], which is where a site reads it; this
//! module only parses the membership shape.
//!
//! # Absent is not empty
//!
//! As before, [`read`] answers `None` for a project with no file at all,
//! meaning "this project has not opted into authentication", distinct from a
//! file holding no members (everyone was removed).

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "reached through the server's HTTP routes; a CLI-only build \
                  still needs the types for `ridal site` and `ridal project`"
    )
)]

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::roles::{DownloadScope, Role};
use super::store::{DocumentStore, Expectation, StoreError, Version};
use crate::identity::UserId;

/// The membership document, relative to the project's data directory.
pub const MEMBERS_FILE: &str = "users.json";

/// Unknown fields are refused rather than ignored, here and on
/// [`MemberSet`]: this file is read, changed and written back whole, so a
/// field this Ridal does not know would otherwise be silently dropped by the
/// next change -- which is how a whole membership list was once lost to a
/// file written in another shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub name: UserId,
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub download: DownloadScope,
}

impl Member {
    pub fn new(name: UserId, role: Role, download: DownloadScope) -> Self {
        Self {
            name,
            role,
            download,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemberSet {
    /// Whether a login is required to read the catalog at all.
    #[serde(default)]
    pub require_auth_to_read: bool,
    /// What someone who is not a member may download while the project is
    /// publicly readable.
    #[serde(default)]
    pub anonymous_download: DownloadScope,
    #[serde(default)]
    pub members: Vec<Member>,
}

impl MemberSet {
    pub fn get(&self, name: &UserId) -> Option<&Member> {
        self.members.iter().find(|member| &member.name == name)
    }

    pub fn get_mut(&mut self, name: &UserId) -> Option<&mut Member> {
        self.members.iter_mut().find(|member| &member.name == name)
    }

    /// Whether `name` is the only member with the `admin` role.
    pub fn is_last_admin(&self, name: &UserId) -> bool {
        self.get(name)
            .is_some_and(|member| member.role == Role::Admin)
            && !self
                .members
                .iter()
                .any(|member| member.role == Role::Admin && &member.name != name)
    }

    pub fn has_admin(&self) -> bool {
        self.members.iter().any(|member| member.role == Role::Admin)
    }

    /// Give `name` this role and download scope, adding the membership if
    /// there is none. Returns whether it was added.
    pub fn upsert(&mut self, name: &UserId, role: Role, download: DownloadScope) -> bool {
        match self.get_mut(name) {
            Some(member) => {
                member.role = role;
                member.download = download;
                false
            }
            None => {
                self.members.push(Member::new(name.clone(), role, download));
                true
            }
        }
    }

    /// The policy that denies everything: a login required, which nobody can
    /// satisfy, and no anonymous downloads.
    ///
    /// What a membership file that will not parse reads as. Its
    /// [`Default`] is the permissive public policy, which is right for a
    /// project with no file and exactly backwards for one whose policy has
    /// just become unreadable.
    pub fn closed() -> Self {
        Self {
            require_auth_to_read: true,
            anonymous_download: DownloadScope::None,
            members: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum MemberError {
    Store(StoreError),
    Malformed {
        path: PathBuf,
        message: String,
    },
    /// No membership with this name in the project.
    NotFound(String),
    /// A change refused because it would lock everyone out of managing the
    /// project.
    Rejected(String),
}

impl fmt::Display for MemberError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemberError::Store(e) => write!(f, "{e}"),
            MemberError::Malformed { path, message } => {
                write!(f, "could not read {}: {message}", path.display())
            }
            MemberError::NotFound(name) => write!(f, "No member named '{name}'."),
            MemberError::Rejected(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for MemberError {}

impl From<StoreError> for MemberError {
    fn from(error: StoreError) -> Self {
        MemberError::Store(error)
    }
}

fn relative_path() -> PathBuf {
    PathBuf::from(MEMBERS_FILE)
}

/// Read the membership file. `None` means the project has no file.
pub fn read(store: &DocumentStore) -> Result<Option<(MemberSet, Version)>, MemberError> {
    let path = relative_path();
    let Some(document) = store.read(&path)? else {
        return Ok(None);
    };
    let set: MemberSet =
        serde_json::from_str(&document.text).map_err(|e| MemberError::Malformed {
            path: store.root().join(&path),
            message: e.to_string(),
        })?;
    Ok(Some((set, document.version)))
}

/// Read the membership file for an access decision: absent is the public
/// default, and a file that will not parse fails closed
/// ([`MemberSet::closed`]).
pub fn read_for_access(store: &DocumentStore) -> MemberSet {
    match read(store) {
        Ok(Some((set, _))) => set,
        Ok(None) => MemberSet::default(),
        Err(_) => MemberSet::closed(),
    }
}

/// Whether this project has a membership file at all.
pub fn is_configured(store: &DocumentStore) -> Result<bool, MemberError> {
    Ok(store.read(&relative_path())?.is_some())
}

pub fn write(
    store: &DocumentStore,
    set: &MemberSet,
    expected: &Expectation,
) -> Result<Version, MemberError> {
    let text = serde_json::to_string_pretty(set)
        .map_err(|e| MemberError::Malformed {
            path: store.root().join(relative_path()),
            message: e.to_string(),
        })
        .map(|mut text| {
            text.push('\n');
            text
        })?;
    Ok(store.write(&relative_path(), &text, expected)?)
}

/// Read, modify, write -- conditional on the version read, retried once on a
/// conflict, so no mutation is a blind overwrite.
pub fn update<T>(
    store: &DocumentStore,
    change: impl Fn(&mut MemberSet) -> Result<T, MemberError>,
) -> Result<T, MemberError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let (mut set, expectation) = match read(store)? {
            Some((set, version)) => (set, Expectation::Version(version)),
            None => (MemberSet::default(), Expectation::Absent),
        };
        let outcome = change(&mut set)?;
        match write(store, &set, &expectation) {
            Ok(_) => return Ok(outcome),
            Err(MemberError::Store(StoreError::Conflict { .. })) if attempts < 3 => continue,
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

    fn name(value: &str) -> UserId {
        UserId::new(value).unwrap()
    }

    #[test]
    fn absent_reads_as_none() {
        let (_dir, store) = store();
        assert!(read(&store).unwrap().is_none());
        assert!(!is_configured(&store).unwrap());
    }

    #[test]
    fn a_membership_round_trips() {
        let (_dir, store) = store();
        update(&store, |set| {
            set.members
                .push(Member::new(name("anna"), Role::Admin, DownloadScope::All));
            Ok(())
        })
        .unwrap();
        let (set, _version) = read(&store).unwrap().unwrap();
        assert_eq!(set.members.len(), 1);
        assert_eq!(set.get(&name("anna")).unwrap().role, Role::Admin);
        assert!(set.has_admin());
        assert!(set.is_last_admin(&name("anna")));
    }

    #[test]
    fn the_last_admin_is_recognized_only_while_they_are_the_only_one() {
        let (_dir, store) = store();
        update(&store, |set| {
            set.members
                .push(Member::new(name("anna"), Role::Admin, DownloadScope::All));
            set.members
                .push(Member::new(name("bo"), Role::Picker, DownloadScope::All));
            Ok(())
        })
        .unwrap();
        let (set, _version) = read(&store).unwrap().unwrap();
        assert!(set.is_last_admin(&name("anna")));
        assert!(!set.is_last_admin(&name("bo")));
    }

    #[test]
    fn a_file_in_another_shape_is_refused_rather_than_rewritten_without_its_fields() {
        let (_dir, store) = store();
        // The pre-site shape, and a member carrying a field this Ridal does
        // not know. Either would lose data if read leniently and saved back.
        for text in [
            r#"{"users": [{"name": "anna", "role": "admin"}]}"#,
            r#"{"members": [{"name": "anna", "role": "admin", "password_hash": "x"}]}"#,
        ] {
            store
                .write(&relative_path(), text, &Expectation::Any)
                .unwrap();
            assert!(matches!(read(&store), Err(MemberError::Malformed { .. })));
            assert!(update(&store, |_| Ok(())).is_err());
            assert_eq!(store.read(&relative_path()).unwrap().unwrap().text, text);
            // And an access decision over it fails closed.
            assert!(read_for_access(&store).require_auth_to_read);
        }
    }
}
