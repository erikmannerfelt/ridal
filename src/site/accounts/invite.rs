//! Invite links for site accounts (#214).
//!
//! A project administrator can invite someone into their project without a
//! server administrator: the invite creates the site account if it does not
//! exist yet and carries the membership to add when it is redeemed. A server
//! administrator's own invite carries no project.
//!
//! The token is shown once and stored only as its `blake3` hash, exactly as
//! a project's invite was before; a link found in an old log is worthless
//! once used or expired.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "reached through the server's HTTP routes; a CLI-only build \
                  still needs the types for `ridal site` and `ridal project`"
    )
)]

use serde::{Deserialize, Serialize};

use super::to_hex;
use crate::identity::ProjectKey;
use crate::project::roles::{DownloadScope, Role};

/// How long an invite link is good for.
pub const INVITE_TTL_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Invite {
    /// `blake3` of the token, hex. The token itself is never stored.
    pub token_hash: String,
    /// Unix seconds after which the invite is refused.
    pub expires: i64,
    /// The project this invite adds the account to, or `None` for a
    /// server-administrator invite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectKey>,
    /// The membership role to grant on redemption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// The membership download scope to grant on redemption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadScope>,
}

impl Invite {
    pub fn is_valid_at(&self, now: i64) -> bool {
        now < self.expires
    }
}

/// Mint an invite. Returns the token to show once, and what to store.
pub fn mint(
    now: i64,
    project: Option<ProjectKey>,
    role: Option<Role>,
    download: Option<DownloadScope>,
) -> Result<(String, Invite), String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("could not read system randomness: {e}"))?;
    let token = to_hex(&bytes);
    let invite = Invite {
        token_hash: blake3::hash(token.as_bytes()).to_hex().to_string(),
        expires: now + INVITE_TTL_DAYS * 24 * 60 * 60,
        project,
        role,
        download,
    };
    Ok((token, invite))
}

/// Mint an invite that will add `project` membership on redemption.
pub fn mint_for_project(
    now: i64,
    project: ProjectKey,
    role: Role,
    download: DownloadScope,
) -> Result<(String, Invite), String> {
    mint(now, Some(project), Some(role), Some(download))
}

/// Whether a presented token matches `invite`, in constant time.
pub fn token_matches(invite: &Invite, token: &str) -> bool {
    let presented = blake3::hash(token.as_bytes());
    blake3::Hash::from_hex(&invite.token_hash).is_ok_and(|stored| stored == presented)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::UserId;

    #[test]
    fn a_token_is_shown_once_and_stored_only_as_a_hash() {
        let (token, invite) = mint(1_000, None, None, None).unwrap();
        assert_eq!(token.len(), 64);
        assert_ne!(invite.token_hash, token);
        assert!(invite.is_valid_at(1_000));
        assert!(!invite.is_valid_at(invite.expires));
        assert_eq!(invite.expires, 1_000 + INVITE_TTL_DAYS * 86_400);
    }

    #[test]
    fn only_the_matching_token_is_accepted() {
        let (token, invite) = mint(0, None, None, None).unwrap();
        assert!(token_matches(&invite, &token));
        assert!(!token_matches(&invite, "not the token"));
    }

    #[test]
    fn a_project_invite_carries_the_membership() {
        let key = ProjectKey::new("glac-2026").unwrap();
        let (_token, invite) =
            mint_for_project(0, key.clone(), Role::Picker, DownloadScope::Picks).unwrap();
        assert_eq!(invite.project.as_ref(), Some(&key));
        assert_eq!(invite.role, Some(Role::Picker));
        assert_eq!(invite.download, Some(DownloadScope::Picks));
        // And an account invite carries none of it.
        let (_token, plain) = mint(0, None, None, None).unwrap();
        assert!(plain.project.is_none() && plain.role.is_none() && plain.download.is_none());
    }

    #[test]
    fn an_account_name_is_a_slug() {
        // Compile-time sanity that the types this module leans on stay
        // coherent: a project key and an account name share the slug rules.
        assert!(UserId::new("share-anna").is_ok());
        assert!(ProjectKey::new("Share Anna").is_err());
    }
}
