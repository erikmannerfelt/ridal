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
    /// A member with this role or above. Written as the role's own name.
    #[serde(untagged)]
    Role(Role),
}

/// What an endpoint needs: the `x-ridal-needs` of one operation.
///
/// An object rather than a bare string, so that what the remaining routes
/// need ("their own") can be added as fields without changing what the ones
/// already annotated say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Needs {
    pub who: Who,
    /// The download scope it needs as well, for what it lets someone take
    /// away rather than look at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<DownloadScope>,
}

impl Needs {
    pub const fn new(who: Who) -> Self {
        Self {
            who,
            download: None,
        }
    }

    /// Anyone who may read the project, with this download scope.
    pub const fn download(scope: DownloadScope) -> Self {
        Self {
            who: Who::Anyone,
            download: Some(scope),
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
    }
}
