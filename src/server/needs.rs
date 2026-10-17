//! What each endpoint needs of its caller (#330).
//!
//! Permissions are checked inside handlers (`Caller::require`,
//! `Caller::require_download`), where no type can see them. So each
//! `#[utoipa::path]` also declares what it needs, as the `x-ridal-needs`
//! extension:
//!
//! ```text
//! extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator)))))
//! ```
//!
//! The *Needs* column of the HTTP reference is rendered from it, the
//! refusal responses in the spec are derived from it, and `needs_tests`
//! runs every operation as a caller just below the declaration (expecting a
//! refusal) and as one at it (expecting none), so the declaration cannot
//! drift from the handler.

use serde::{Deserialize, Serialize};

use crate::project::roles::{DownloadScope, Role};

/// The name of the extension, on every operation in the spec.
pub const EXTENSION: &str = "x-ridal-needs";

/// Who may call an endpoint at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Who {
    /// No one in particular: it answers without a login even in a project
    /// that requires one to read. Health, signing in, and the like.
    Nobody,
    /// Anyone who may read the project's catalog. In a project that
    /// requires a login to read, that means its members.
    Anyone,
    /// Any signed-in account that may read the project, whatever its role.
    SignedIn,
    /// A server administrator: the `server_admin` flag on an account, which
    /// no project role gives.
    ServerAdmin,
    /// A member with this role or above. Written as the role's own name.
    #[serde(untagged)]
    Role(Role),
}

/// What an endpoint needs: the `x-ridal-needs` of one operation.
///
/// An object rather than a bare string, so that each part can be read on
/// its own and a new one added without changing what the others say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Needs {
    pub who: Who,
    /// The download scope it needs as well, for what it lets someone take
    /// away rather than look at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadScope>,
    /// Whether it acts only on something the caller made, such as their own
    /// draft. Anyone else is refused, or told it does not exist.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own: bool,
    /// Whether the download scope is waived for the caller's own: their own
    /// interpretation needs none, anyone else's needs `download`. Unlike
    /// `own`, which refuses anyone else outright.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub or_own: bool,
}

impl Needs {
    pub const fn new(who: Who) -> Self {
        Self {
            who,
            download: None,
            own: false,
            or_own: false,
        }
    }

    /// Anyone who may read the project, with this download scope.
    pub const fn download(scope: DownloadScope) -> Self {
        Self {
            who: Who::Anyone,
            download: Some(scope),
            own: false,
            or_own: false,
        }
    }

    /// The caller's own, or anyone's with this download scope.
    pub const fn own_or_download(scope: DownloadScope) -> Self {
        Self {
            or_own: true,
            ..Self::download(scope)
        }
    }

    /// Only on what the caller made.
    pub const fn own(self) -> Self {
        Self { own: true, ..self }
    }

    /// With this download scope as well.
    pub const fn and_download(self, scope: DownloadScope) -> Self {
        Self {
            download: Some(scope),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extension's shape is part of the committed spec, and the docs
    /// extension reads it: a role is its plain name, not a nested object.
    #[test]
    fn needs_serialize_flat() {
        let cases = [
            (Who::Nobody, r#"{"who":"nobody"}"#),
            (Who::SignedIn, r#"{"who":"signed_in"}"#),
            (Who::ServerAdmin, r#"{"who":"server_admin"}"#),
            (Who::Role(Role::Operator), r#"{"who":"operator"}"#),
        ];
        for (who, json) in cases {
            assert_eq!(serde_json::to_string(&Needs::new(who)).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<Needs>(json).unwrap(),
                Needs::new(who)
            );
        }
        let all = Needs::download(DownloadScope::All);
        let json = r#"{"who":"anyone","download":"all"}"#;
        assert_eq!(serde_json::to_string(&all).unwrap(), json);
        assert_eq!(serde_json::from_str::<Needs>(json).unwrap(), all);
        let mine = Needs::new(Who::Role(Role::Operator))
            .own()
            .and_download(DownloadScope::All);
        let json = r#"{"who":"operator","download":"all","own":true}"#;
        assert_eq!(serde_json::to_string(&mine).unwrap(), json);
        assert_eq!(serde_json::from_str::<Needs>(json).unwrap(), mine);
        let either = Needs::own_or_download(DownloadScope::Picks);
        let json = r#"{"who":"anyone","download":"picks","or_own":true}"#;
        assert_eq!(serde_json::to_string(&either).unwrap(), json);
        assert_eq!(serde_json::from_str::<Needs>(json).unwrap(), either);
    }
}
