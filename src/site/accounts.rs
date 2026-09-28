//! Server-wide accounts for a site (#214).
//!
//! One `accounts.json` at the site root, written `0600` through the
//! [`DocumentStore`](crate::project::store::DocumentStore): it holds every
//! account's password hash and invite, so it is a secret document in the
//! same way a project's `users.json` used to be.
//!
//! An account is *identity only*. What someone may do in a project lives in
//! that project's membership (`crate::project::users`), and a server
//! administrator is a flag here -- they create projects and accounts and act
//! as an administrator in every project.
//!
//! # Absent is not empty
//!
//! As with a project's user file, [`read`] answers `None` for a site with no
//! `accounts.json` at all, which is a site that has not opted into accounts
//! (and is how `ridal gui` runs). That is deliberately distinct from a file
//! holding no accounts, which means every account was removed.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::identity::UserId;
use crate::project::store::{DocumentStore, Expectation, StoreError, Version};

pub mod invite;

pub use invite::Invite;

/// The account document, relative to the site root.
pub const ACCOUNTS_FILE: &str = "accounts.json";

/// The shortest password the server will accept.
///
/// A length floor rather than a composition rule, for the same reason it is
/// in a project's user file: a longer passphrase beats `Password1!`.
pub const MIN_PASSWORD_LEN: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    /// The account's name, validated as a slug like every other identifier.
    pub name: UserId,
    /// Whether this account administers the whole site: projects, accounts
    /// and every project's memberships.
    #[serde(default)]
    pub server_admin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash: Option<String>,
    /// Bumped on password, role and deletion changes so a live session is
    /// revoked on the next request rather than when the cookie ages out.
    #[serde(default)]
    pub credential_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<Invite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

impl Account {
    pub fn new(name: UserId, server_admin: bool) -> Self {
        Self {
            name,
            server_admin,
            password_hash: None,
            credential_version: 1,
            invite: None,
            created: Some(now_rfc3339()),
        }
    }

    pub fn is_activated(&self) -> bool {
        self.password_hash.is_some()
    }

    /// The only shape allowed out of the process: it drops the hash and the
    /// invite, keeping just what a person needs to see.
    pub fn redacted(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name.as_str(),
            "server_admin": self.server_admin,
            "activated": self.is_activated(),
            "invite_pending": self.invite.is_some(),
            "invite_expires": self.invite.as_ref().map(|invite| invite.expires),
            "created": self.created,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountSet {
    #[serde(default)]
    pub users: Vec<Account>,
}

impl AccountSet {
    pub fn get(&self, name: &UserId) -> Option<&Account> {
        self.users.iter().find(|account| &account.name == name)
    }

    pub fn get_mut(&mut self, name: &UserId) -> Option<&mut Account> {
        self.users.iter_mut().find(|account| &account.name == name)
    }

    /// Whether at least one *other* account administers the site.
    pub fn has_another_admin(&self, name: &UserId) -> bool {
        self.users
            .iter()
            .any(|account| account.server_admin && &account.name != name)
    }

    pub fn has_server_admin(&self) -> bool {
        self.users.iter().any(|account| account.server_admin)
    }
}

#[derive(Debug)]
pub enum AccountError {
    Store(StoreError),
    Malformed { path: PathBuf, message: String },
    Duplicate(String),
    NotFound(String),
    Rejected(String),
    Hash(String),
}

impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountError::Store(e) => write!(f, "{e}"),
            AccountError::Malformed { path, message } => {
                write!(f, "could not read {}: {message}", path.display())
            }
            AccountError::Duplicate(name) => write!(f, "An account named '{name}' already exists."),
            AccountError::NotFound(name) => write!(f, "No account named '{name}'."),
            AccountError::Rejected(message) => f.write_str(message),
            AccountError::Hash(message) => write!(f, "Could not hash the password: {message}"),
        }
    }
}

impl std::error::Error for AccountError {}

impl From<StoreError> for AccountError {
    fn from(error: StoreError) -> Self {
        AccountError::Store(error)
    }
}

fn relative_path() -> PathBuf {
    PathBuf::from(ACCOUNTS_FILE)
}

/// Read the account file. `None` means the site has not opted into accounts.
pub fn read(store: &DocumentStore) -> Result<Option<(AccountSet, Version)>, AccountError> {
    let path = relative_path();
    let Some(document) = store.read(&path)? else {
        return Ok(None);
    };
    let set: AccountSet =
        serde_json::from_str(&document.text).map_err(|e| AccountError::Malformed {
            path: store.root().join(&path),
            message: e.to_string(),
        })?;
    Ok(Some((set, document.version)))
}

pub fn is_configured(store: &DocumentStore) -> Result<bool, AccountError> {
    Ok(store.read(&relative_path())?.is_some())
}

/// Write the account file, restricted to its owner.
pub fn write(
    store: &DocumentStore,
    set: &AccountSet,
    expected: &Expectation,
) -> Result<Version, AccountError> {
    let text = serde_json::to_string_pretty(set)
        .map_err(|e| AccountError::Hash(e.to_string()))
        .map(|mut text| {
            text.push('\n');
            text
        })?;
    Ok(store.write_private(&relative_path(), &text, expected)?)
}

/// Read, modify, write -- conditional on the version that was read, retried
/// once on a conflict. Every mutation goes through here so none can be a
/// blind overwrite.
pub fn update<T>(
    store: &DocumentStore,
    change: impl Fn(&mut AccountSet) -> Result<T, AccountError>,
) -> Result<T, AccountError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let (mut set, expectation) = match read(store)? {
            Some((set, version)) => (set, Expectation::Version(version)),
            None => (AccountSet::default(), Expectation::Absent),
        };
        let outcome = change(&mut set)?;
        match write(store, &set, &expectation) {
            Ok(_) => return Ok(outcome),
            Err(AccountError::Store(StoreError::Conflict { .. })) if attempts < 3 => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Refuse a password that is too short to be worth hashing.
pub fn check_password(password: &str) -> Result<(), AccountError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(AccountError::Rejected(format!(
            "A password must be at least {MIN_PASSWORD_LEN} characters. A short \
             sentence is easier to remember than a short password and harder to \
             guess."
        )));
    }
    Ok(())
}

/// Hash a password with Argon2id at the crate's default parameters.
#[cfg(feature = "server")]
pub fn hash_password(password: &str) -> Result<String, AccountError> {
    use argon2::password_hash::phc::PasswordHash;
    use argon2::password_hash::PasswordHasher;

    check_password(password)?;
    let hash: PasswordHash = argon2::Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| AccountError::Hash(e.to_string()))?;
    Ok(hash.to_string())
}

/// Whether `password` matches the account's stored hash.
///
/// An account with no hash -- an unredeemed invite -- answers `false` rather
/// than erroring, so login cannot distinguish it from a wrong password.
#[cfg(feature = "server")]
pub fn verify_password(account: &Account, password: &str) -> bool {
    use argon2::password_hash::phc::PasswordHash;
    use argon2::password_hash::PasswordVerifier;

    let Some(stored) = account.password_hash.as_deref() else {
        return false;
    };
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    argon2::Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

fn now_rfc3339() -> String {
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

    fn name(value: &str) -> UserId {
        UserId::new(value).unwrap()
    }

    #[test]
    fn absent_reads_as_none_not_an_empty_set() {
        let (_dir, store) = store();
        assert!(read(&store).unwrap().is_none());
        assert!(!is_configured(&store).unwrap());
    }

    #[test]
    fn an_account_round_trips_through_the_private_file() {
        let (_dir, store) = store();
        update(&store, |set| {
            set.users.push(Account::new(name("anna"), true));
            Ok(())
        })
        .unwrap();
        let (set, _version) = read(&store).unwrap().unwrap();
        assert_eq!(set.users.len(), 1);
        assert!(set.get(&name("anna")).unwrap().server_admin);
        assert!(set.has_server_admin());
        assert!(!set.has_another_admin(&name("anna")));
    }

    #[test]
    fn redaction_never_carries_the_hash_or_the_invite() {
        let mut account = Account::new(name("anna"), false);
        account.password_hash = Some("$argon2id$secret".to_string());
        let redacted = account.redacted().to_string();
        assert!(!redacted.contains("argon2"), "{redacted}");
        assert!(!redacted.contains("password_hash"), "{redacted}");
        assert!(!redacted.contains("token_hash"), "{redacted}");
        assert!(redacted.contains("\"activated\":true"), "{redacted}");
    }

    #[test]
    fn an_unknown_account_is_distinguishable_from_a_duplicate() {
        let (_dir, store) = store();
        let error = update(&store, |set| {
            set.get_mut(&name("ghost"))
                .map(|_| ())
                .ok_or_else(|| AccountError::NotFound("ghost".to_string()))
        })
        .unwrap_err();
        assert!(matches!(error, AccountError::NotFound(_)), "{error}");
    }

    #[cfg(feature = "server")]
    #[test]
    fn a_password_hash_verifies_and_a_wrong_one_does_not() {
        let mut account = Account::new(name("anna"), false);
        account.password_hash = Some(hash_password("correct horse battery").unwrap());
        assert!(verify_password(&account, "correct horse battery"));
        assert!(!verify_password(&account, "wrong horse battery"));
        // An unredeemed account fails closed rather than erroring.
        let pending = Account::new(name("bob"), false);
        assert!(!verify_password(&pending, "anything at all"));
    }

    #[cfg(feature = "server")]
    #[test]
    fn a_short_password_is_refused_before_hashing() {
        let error = hash_password("short").unwrap_err();
        assert!(matches!(error, AccountError::Rejected(_)), "{error}");
        assert!(check_password("short").is_err());
        assert!(check_password("long enough passphrase").is_ok());
    }
}
