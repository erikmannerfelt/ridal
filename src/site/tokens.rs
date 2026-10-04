//! API tokens for a site (#194).
//!
//! A token is **an account acting in a fixed set of projects**, each with a
//! ceiling on what it may do there. It is how a script authenticates where a
//! browser would use a session cookie, and it is never more than the account
//! behind it: in each granted project the effective role is the lower of the
//! account's membership and the grant's ceiling, read per request, so a
//! demotion or a removed membership takes effect at once.
//!
//! One `tokens.json` at the site root, written `0600` like `accounts.json`.
//! Only `blake3(secret)` is stored. A token is shown once, as
//! `ridal_<id>_<secret>`: the id finds the record, names it in lists and the
//! audit log, and revokes it; the `ridal_` prefix makes a leaked token
//! recognisable to secret scanners. Argon2 is not needed for 256 random bits,
//! and would cost every image-chunk request a password hash.
//!
//! What a token cannot do is decided by the server, not here: reach a project
//! it has no grant for, use the site's own routes (accounts, memberships,
//! creating or revoking tokens), or change a password. A leaked token cannot
//! turn itself into lasting access.
//!
//! `account` is optional in the record so that share tokens (#214), which
//! have no account, fit the same format later. Until they exist, a record
//! without an account is refused.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "reached through the server's HTTP routes; a CLI-only build \
                  still needs the types for `ridal site token`"
    )
)]

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::accounts::{to_hex, Account};
use super::{Site, SiteError};
use crate::identity::{ProjectKey, UserId};
use crate::project::members;
use crate::project::roles::{DownloadScope, Role};
use crate::project::store::{DocumentStore, Expectation, StoreError, Version};

/// The token document, relative to the site root.
pub const TOKENS_FILE: &str = "tokens.json";

/// What every token starts with.
pub const PREFIX: &str = "ridal_";

/// How long a token lives when nobody says otherwise.
pub const DEFAULT_LIFETIME: &str = "90d";

/// The longest a token's name may be. It is a label in a list, not prose.
const MAX_NAME_LEN: usize = 64;

/// What a token may do in one project, at most.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct Grant {
    #[cfg_attr(feature = "server", schema(value_type = String))]
    pub project: ProjectKey,
    /// The highest role the token acts with here. The account's membership
    /// still applies, so the token never has more than the account.
    pub role: Role,
    /// The widest download scope the token has here, under the same rule.
    pub download: DownloadScope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Token {
    /// 16 hex characters, public: it finds the record and names the token.
    pub id: String,
    /// `blake3` of the secret, hex. The secret itself is never stored.
    pub token_hash: String,
    /// The account the token acts as. `None` is reserved for share tokens
    /// (#214) and refused until they exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<UserId>,
    /// A label chosen by whoever made it, such as `laptop` or `ci`.
    pub name: String,
    pub grants: Vec<Grant>,
    /// Unix seconds.
    pub created: i64,
    /// Unix seconds after which the token is refused, or `None` for a token
    /// that never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
}

impl Token {
    pub fn is_valid_at(&self, now: i64) -> bool {
        self.expires.is_none_or(|expires| now < expires)
    }

    /// `name (id): glac as operator/all, ice as viewer/results`, for the
    /// audit log and the command line. Never the secret.
    pub fn describe(&self) -> String {
        let grants: Vec<String> = self
            .grants
            .iter()
            .map(|grant| format!("{} as {}/{}", grant.project, grant.role, grant.download))
            .collect();
        format!("{} ({}): {}", self.name, self.id, grants.join(", "))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenSet {
    #[serde(default)]
    pub tokens: Vec<Token>,
}

impl TokenSet {
    pub fn get(&self, id: &str) -> Option<&Token> {
        self.tokens.iter().find(|token| token.id == id)
    }
}

#[derive(Debug)]
pub enum TokenError {
    Store(StoreError),
    Malformed { path: PathBuf, message: String },
    NotFound(String),
    Rejected(String),
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenError::Store(e) => write!(f, "{e}"),
            TokenError::Malformed { path, message } => {
                write!(f, "could not read {}: {message}", path.display())
            }
            TokenError::NotFound(id) => write!(f, "No token with the id '{id}'."),
            TokenError::Rejected(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for TokenError {}

impl From<StoreError> for TokenError {
    fn from(error: StoreError) -> Self {
        TokenError::Store(error)
    }
}

fn relative_path() -> PathBuf {
    PathBuf::from(TOKENS_FILE)
}

/// Read the token file. Absent and empty mean the same here: no tokens.
pub fn read(store: &DocumentStore) -> Result<(TokenSet, Option<Version>), TokenError> {
    let path = relative_path();
    let Some(document) = store.read(&path)? else {
        return Ok((TokenSet::default(), None));
    };
    let set: TokenSet =
        serde_json::from_str(&document.text).map_err(|e| TokenError::Malformed {
            path: store.root().join(&path),
            message: e.to_string(),
        })?;
    Ok((set, Some(document.version)))
}

/// Read, modify, write -- conditional on the version that was read, retried
/// on a conflict, as [`super::accounts::update`] does.
pub fn update<T>(
    store: &DocumentStore,
    change: impl Fn(&mut TokenSet) -> Result<T, TokenError>,
) -> Result<T, TokenError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let (mut set, version) = read(store)?;
        let expectation = match version {
            Some(version) => Expectation::Version(version),
            None => Expectation::Absent,
        };
        let outcome = change(&mut set)?;
        let mut text = serde_json::to_string_pretty(&set)
            .map_err(|e| TokenError::Rejected(format!("could not write the tokens: {e}")))?;
        text.push('\n');
        match store.write_private(&relative_path(), &text, &expectation) {
            Ok(_) => return Ok(outcome),
            Err(StoreError::Conflict { .. }) if attempts < 3 => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Mint a token. Returns the text to show once, and the record to store.
pub fn mint(
    now: i64,
    account: UserId,
    name: &str,
    grants: Vec<Grant>,
    lifetime_seconds: Option<i64>,
) -> Result<(String, Token), TokenError> {
    let name = check_name(name)?;
    let mut id = [0u8; 8];
    let mut secret = [0u8; 32];
    getrandom::fill(&mut id)
        .and_then(|()| getrandom::fill(&mut secret))
        .map_err(|e| TokenError::Rejected(format!("could not read system randomness: {e}")))?;
    let (id, secret) = (to_hex(&id), to_hex(&secret));
    let token = Token {
        id,
        token_hash: blake3::hash(secret.as_bytes()).to_hex().to_string(),
        account: Some(account),
        name,
        grants,
        created: now,
        expires: lifetime_seconds.map(|seconds| now.saturating_add(seconds)),
    };
    Ok((format!("{PREFIX}{}_{secret}", token.id), token))
}

/// Why a presented token was refused. Deliberately coarse: the holder is
/// either a script with a stale token or someone guessing, and neither is
/// helped by learning which part was wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not a token, an unknown id, or the wrong secret.
    Invalid,
    Expired,
    /// A record without an account: a share token, which this version does
    /// not accept yet.
    Unsupported,
}

/// The record a presented token names, if its secret matches and it is
/// still valid.
pub fn authenticate<'a>(
    set: &'a TokenSet,
    presented: &str,
    now: i64,
) -> Result<&'a Token, Refusal> {
    let (id, secret) = split(presented).ok_or(Refusal::Invalid)?;
    let token = set.get(id).ok_or(Refusal::Invalid)?;
    // `blake3::Hash` compares in constant time.
    let stored = blake3::Hash::from_hex(&token.token_hash).map_err(|_| Refusal::Invalid)?;
    if stored != blake3::hash(secret.as_bytes()) {
        return Err(Refusal::Invalid);
    }
    if !token.is_valid_at(now) {
        return Err(Refusal::Expired);
    }
    if token.account.is_none() {
        return Err(Refusal::Unsupported);
    }
    Ok(token)
}

/// `ridal_<16 hex>_<64 hex>` into its id and secret.
fn split(presented: &str) -> Option<(&str, &str)> {
    let rest = presented.trim().strip_prefix(PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    let hex = |text: &str, len: usize| {
        text.len() == len
            && text
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    };
    (hex(id, 16) && hex(secret, 64)).then_some((id, secret))
}

/// A token lifetime: `30d`, `12w`, `2y` or `never`, as seconds or `None`.
pub fn parse_lifetime(text: &str) -> Result<Option<i64>, TokenError> {
    let text = text.trim();
    if text == "never" {
        return Ok(None);
    }
    let refuse = || {
        TokenError::Rejected(format!(
            "'{text}' is not a lifetime. Use a number of days, weeks or years, \
             such as 30d, 12w or 2y, or 'never'."
        ))
    };
    let (number, unit) = text.split_at(text.len().checked_sub(1).ok_or_else(refuse)?);
    let count: i64 = number.parse().map_err(|_| refuse())?;
    let day = 24 * 60 * 60;
    let unit_seconds = match unit {
        "d" => day,
        "w" => 7 * day,
        "y" => 365 * day,
        _ => return Err(refuse()),
    };
    if count <= 0 {
        return Err(refuse());
    }
    count.checked_mul(unit_seconds).map(Some).ok_or_else(refuse)
}

fn check_name(name: &str) -> Result<String, TokenError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control)
    {
        return Err(TokenError::Rejected(format!(
            "A token needs a name of 1 to {MAX_NAME_LEN} characters, such as 'laptop' \
             or 'ci', so it can be told apart in a list."
        )));
    }
    Ok(name.to_string())
}

/// Refuse grants the account could not exercise: none at all, a project
/// named twice, a project the account is not a member of, or a ceiling
/// above the membership.
///
/// A server administrator may grant any project in the site, up to
/// `admin`, since that is what they are everywhere. The token still never
/// carries server administration itself.
///
/// A project that does not exist and one the account is not a member of are
/// refused with the same words, so this cannot be used to discover projects.
pub fn check_grants(site: &Site, account: &Account, grants: &[Grant]) -> Result<(), TokenError> {
    if grants.is_empty() {
        return Err(TokenError::Rejected(
            "A token needs at least one project to act in.".to_string(),
        ));
    }
    for (index, grant) in grants.iter().enumerate() {
        let key = &grant.project;
        if grants[..index]
            .iter()
            .any(|earlier| &earlier.project == key)
        {
            return Err(TokenError::Rejected(format!(
                "'{key}' is granted twice. Give each project one role and one download scope."
            )));
        }
        let not_yours = || {
            TokenError::Rejected(format!(
                "'{}' is not a member of any project called '{key}'.",
                account.name
            ))
        };
        let project = match site.project(key) {
            Ok(project) => project,
            Err(SiteError::NotFound(_)) => return Err(not_yours()),
            Err(e) => return Err(TokenError::Rejected(e.to_string())),
        };
        if account.server_admin {
            continue;
        }
        let members = members::read_for_access(project.documents());
        let member = members.get(&account.name).ok_or_else(not_yours)?;
        if grant.role > member.role {
            return Err(TokenError::Rejected(format!(
                "'{}' is '{}' in '{key}', so a token cannot act there as '{}'.",
                account.name, member.role, grant.role
            )));
        }
        if grant.download > member.download {
            return Err(TokenError::Rejected(format!(
                "'{}' has the '{}' download scope in '{key}', so a token cannot have \
                 '{}' there.",
                account.name, member.download, grant.download
            )));
        }
    }
    Ok(())
}

/// The widest download scope `account` could give a token in `project`:
/// its membership's, or everything for a server administrator. What a grant
/// that names no scope means. `None` when it has no standing there, which
/// [`check_grants`] then refuses in its own words.
pub fn widest_download(
    site: &Site,
    account: &Account,
    project: &ProjectKey,
) -> Option<DownloadScope> {
    if account.server_admin {
        return Some(DownloadScope::All);
    }
    let project = site.project(project).ok()?;
    members::read_for_access(project.documents())
        .get(&account.name)
        .map(|member| member.download)
}

/// Remove every token of an account, as part of removing the account, so a
/// later account of the same name does not inherit them.
pub fn remove_account(store: &DocumentStore, name: &UserId) -> Result<(), TokenError> {
    update(store, |set| {
        set.tokens
            .retain(|token| token.account.as_ref() != Some(name));
        Ok(())
    })
}

/// Remove a deleted project from every token, and any token left with no
/// project at all.
pub fn remove_project(store: &DocumentStore, key: &ProjectKey) -> Result<(), TokenError> {
    update(store, |set| {
        for token in &mut set.tokens {
            token.grants.retain(|grant| &grant.project != key);
        }
        set.tokens.retain(|token| !token.grants.is_empty());
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(project: &str, role: Role, download: DownloadScope) -> Grant {
        Grant {
            project: ProjectKey::new(project).unwrap(),
            role,
            download,
        }
    }

    fn anna() -> UserId {
        UserId::new("anna").unwrap()
    }

    #[test]
    fn a_token_is_shown_once_and_stored_only_as_a_hash() {
        let (text, token) = mint(
            1_000,
            anna(),
            "laptop",
            vec![grant("glac", Role::Viewer, DownloadScope::All)],
            Some(60),
        )
        .unwrap();
        // No message: it would print the token, which the code scanner
        // rightly treats as a secret written to a log.
        assert!(text.starts_with("ridal_"));
        assert_eq!(text.len(), PREFIX.len() + 16 + 1 + 64);
        assert!(!text.contains(&token.token_hash));
        let stored = serde_json::to_string(&token).unwrap();
        assert!(
            !stored.contains(text.rsplit('_').next().unwrap()),
            "{stored}"
        );
        assert_eq!(token.expires, Some(1_060));
    }

    #[test]
    fn only_the_whole_token_authenticates_and_only_until_it_expires() {
        let (text, token) = mint(
            0,
            anna(),
            "ci",
            vec![grant("glac", Role::Operator, DownloadScope::All)],
            Some(100),
        )
        .unwrap();
        let set = TokenSet {
            tokens: vec![token.clone()],
        };
        assert_eq!(authenticate(&set, &text, 50).unwrap().id, token.id);
        assert_eq!(authenticate(&set, &text, 100), Err(Refusal::Expired));

        // Same id, another secret; and a malformed one.
        let forged = format!("{PREFIX}{}_{}", token.id, "0".repeat(64));
        assert_eq!(authenticate(&set, &forged, 50), Err(Refusal::Invalid));
        assert_eq!(authenticate(&set, "ridal_nope", 50), Err(Refusal::Invalid));
        assert_eq!(
            authenticate(&set, &text.to_uppercase(), 50),
            Err(Refusal::Invalid)
        );

        // A record without an account is a share token, not accepted yet.
        let mut share = token;
        share.account = None;
        let set = TokenSet {
            tokens: vec![share],
        };
        assert_eq!(authenticate(&set, &text, 50), Err(Refusal::Unsupported));
    }

    #[test]
    fn a_token_without_an_expiry_never_expires() {
        let (text, token) = mint(
            0,
            anna(),
            "forever",
            vec![grant("glac", Role::Viewer, DownloadScope::None)],
            None,
        )
        .unwrap();
        assert_eq!(token.expires, None);
        let set = TokenSet {
            tokens: vec![token],
        };
        assert!(authenticate(&set, &text, i64::MAX).is_ok());
    }

    #[test]
    fn lifetimes_are_days_weeks_years_or_never() {
        assert_eq!(parse_lifetime("30d").unwrap(), Some(30 * 86_400));
        assert_eq!(parse_lifetime("2w").unwrap(), Some(14 * 86_400));
        assert_eq!(parse_lifetime("1y").unwrap(), Some(365 * 86_400));
        assert_eq!(parse_lifetime("never").unwrap(), None);
        assert_eq!(parse_lifetime(DEFAULT_LIFETIME).unwrap(), Some(90 * 86_400));
        for bad in [
            "",
            "d",
            "0d",
            "-1d",
            "30",
            "30h",
            "1.5y",
            "99999999999999999y",
        ] {
            assert!(parse_lifetime(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_token_needs_a_short_name() {
        let grants = || vec![grant("glac", Role::Viewer, DownloadScope::All)];
        assert!(mint(0, anna(), "  ", grants(), None).is_err());
        assert!(mint(0, anna(), &"x".repeat(MAX_NAME_LEN + 1), grants(), None).is_err());
        assert!(mint(0, anna(), "a\nb", grants(), None).is_err());
        assert_eq!(
            mint(0, anna(), " ci ", grants(), None).unwrap().1.name,
            "ci"
        );
    }

    #[test]
    fn removing_an_account_or_a_project_takes_its_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let bob = UserId::new("bob").unwrap();
        let both = vec![
            grant("glac", Role::Viewer, DownloadScope::All),
            grant("ice", Role::Operator, DownloadScope::All),
        ];
        let (_, annas) = mint(0, anna(), "a", both.clone(), None).unwrap();
        let (_, only_glac) = mint(
            0,
            bob.clone(),
            "b",
            vec![grant("glac", Role::Viewer, DownloadScope::All)],
            None,
        )
        .unwrap();
        let (_, bobs_both) = mint(0, bob.clone(), "c", both, None).unwrap();
        update(&store, |set| {
            set.tokens = vec![annas.clone(), only_glac.clone(), bobs_both.clone()];
            Ok(())
        })
        .unwrap();

        // A project goes from every token, and a token left with nothing goes.
        remove_project(&store, &ProjectKey::new("glac").unwrap()).unwrap();
        let (set, _) = read(&store).unwrap();
        assert_eq!(set.tokens.len(), 2);
        assert!(set.get(&only_glac.id).is_none());
        assert_eq!(set.get(&annas.id).unwrap().grants.len(), 1);

        remove_account(&store, &bob).unwrap();
        let (set, _) = read(&store).unwrap();
        assert_eq!(set.tokens.len(), 1);
        assert_eq!(set.tokens[0].account, Some(anna()));
    }
}
