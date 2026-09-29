//! Roles and download scopes (#131).
//!
//! What someone may do in a project, and what they may take away from it.
//! A project's memberships ([`super::members`]) give each site account one
//! of each.
//!
//! # Two ladders, not one
//!
//! [`Role`] is what someone may *do*; [`DownloadScope`] is what they may
//! *take away*. Both are ladders, but independent ones: a picker who may not
//! export the underlying data and a viewer who may export everything are both
//! reasonable, and folding the second into the first would multiply the
//! roles.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "reached through the server's HTTP routes; a CLI-only build \
                  still needs the types for `ridal site` and `ridal project`"
    )
)]

use std::fmt;

use serde::{Deserialize, Serialize};

/// What someone may do. Each level includes the ones below it.
///
/// The ordering is the ladder, so a permission check is `role >= required`.
/// `Ord` is derived from declaration order, which is why the variants are
/// written weakest-first and must stay that way.
///
/// `editor` is deliberately absent for the [`Operator`](Role::Operator)
/// level: pickers also edit -- their own picks -- so the word would point at
/// the wrong thing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Read the catalog and open radargrams; read the layer vocabulary; set
    /// their own preferences.
    #[default]
    Viewer,
    /// Write their own interpretation.
    Picker,
    /// Run processing; modify radargram metadata; modify the layer
    /// vocabulary; set the project-wide defaults and policy.
    Operator,
    /// Users, roles, download scopes, and the access settings.
    Admin,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Viewer, Role::Picker, Role::Operator, Role::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Picker => "picker",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }

    /// Parse a role from a command-line argument or an API body.
    pub fn parse(value: &str) -> Result<Role, String> {
        Role::ALL
            .into_iter()
            .find(|r| r.as_str() == value)
            .ok_or_else(|| {
                format!(
                    "Unknown role '{value}'. Choose one of: {}.",
                    Role::ALL.map(Role::as_str).join(", ")
                )
            })
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What someone may take off the server. Also a ladder.
///
/// The order earns itself: level 1 picks are in image space -- trace and
/// sample indices, no coordinates -- so they reveal less than level 2, which
/// carries positions and depths. Anyone allowed the point product has no
/// reason to be denied the picks it was derived from.
///
/// **Not a security boundary against a determined reader.** The rendered
/// image and the track are already on the page for anyone who can open it:
/// the viewer draws a radargram from 256x256 chunks over HTTP, and the
/// catalog's maps draw the track. This gates the bulk *download* endpoints,
/// which stops casual export and states an intent; it does not stop someone
/// with read access from reassembling what they can already see. That
/// sentence belongs next to the control in the UI, not only here.
///
/// Defaults to [`All`](DownloadScope::All), which is what every Ridal server
/// did before this existed. The control is there to restrict deliberately,
/// so an upgrade must not quietly take downloads away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadScope {
    /// Nothing leaves the server.
    None,
    /// Derived results only: consensus lines and attributes (#205).
    ///
    /// Deliberately **below** [`Picks`](DownloadScope::Picks), which is the one
    /// place this ladder inverts the usual reading of "more". An aggregate over
    /// many contributors is *less* disclosing than any one contributor's raw
    /// picks, so "you may have the consensus but not the individual
    /// interpretations" is a real and useful setting -- and the only rung that
    /// expresses it. Inserted here rather than appended so the existing rungs
    /// keep their order and their stored spellings.
    Results,
    /// Raw picks (level 1 gprinterp).
    Picks,
    /// Level 2 points; the rendered radargram image. Usually enough to
    /// publish with, which is why this is the line a project is really
    /// deciding about.
    Derived,
    /// The radargram NetCDF; the track.
    #[default]
    All,
}

impl DownloadScope {
    pub const ALL: [DownloadScope; 5] = [
        DownloadScope::None,
        DownloadScope::Results,
        DownloadScope::Picks,
        DownloadScope::Derived,
        DownloadScope::All,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DownloadScope::None => "none",
            DownloadScope::Results => "results",
            DownloadScope::Picks => "picks",
            DownloadScope::Derived => "derived",
            DownloadScope::All => "all",
        }
    }

    pub fn parse(value: &str) -> Result<DownloadScope, String> {
        DownloadScope::ALL
            .into_iter()
            .find(|s| s.as_str() == value)
            .ok_or_else(|| {
                format!(
                    "Unknown download scope '{value}'. Choose one of: {}.",
                    DownloadScope::ALL.map(DownloadScope::as_str).join(", ")
                )
            })
    }
}

impl fmt::Display for DownloadScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_form_a_ladder_weakest_first() {
        // Permission checks are written as `role >= required`, so the
        // derived ordering is load-bearing rather than incidental.
        assert!(Role::Admin > Role::Operator);
        assert!(Role::Operator > Role::Picker);
        assert!(Role::Picker > Role::Viewer);
        assert!(Role::Admin >= Role::Admin);
    }

    #[test]
    fn download_scopes_form_a_ladder_and_default_to_all() {
        assert!(DownloadScope::All > DownloadScope::Derived);
        assert!(DownloadScope::Derived > DownloadScope::Picks);
        assert!(DownloadScope::Picks > DownloadScope::None);
        // Today's behaviour: an upgrade must not take downloads away.
        assert_eq!(DownloadScope::default(), DownloadScope::All);
    }

    #[test]
    fn roles_and_scopes_round_trip_through_their_wire_names() {
        for role in Role::ALL {
            assert_eq!(Role::parse(role.as_str()).unwrap(), role);
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json, format!("\"{}\"", role.as_str()));
            assert_eq!(serde_json::from_str::<Role>(&json).unwrap(), role);
        }
        for scope in DownloadScope::ALL {
            assert_eq!(DownloadScope::parse(scope.as_str()).unwrap(), scope);
            let json = serde_json::to_string(&scope).unwrap();
            assert_eq!(json, format!("\"{}\"", scope.as_str()));
            assert_eq!(serde_json::from_str::<DownloadScope>(&json).unwrap(), scope);
        }
        assert!(Role::parse("editor").is_err());
        assert!(DownloadScope::parse("everything").is_err());
    }
}
