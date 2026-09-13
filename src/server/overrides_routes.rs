//! Editing the catalog metadata a project keeps over its files (#145).
//!
//! Four routes, all `operator` and above:
//!
//! ```text
//! GET  /api/v1/datasets/{id}/properties
//! PUT  /api/v1/datasets/{id}/properties
//! GET  /api/v1/groups/{id}/properties
//! PUT  /api/v1/groups/{id}/properties
//! ```
//!
//! A group is editable in its own right, not only through whichever member
//! happens to be at hand. Renaming one by editing a radargram works, but it
//! asks the operator to find a member first and then reads as though the
//! name belonged to that radargram — which is the thing this document's
//! shape is built to deny.
//!
//! The `GET` answers a question the catalog alone cannot: **what would this
//! field be without its override?** Without that, an override silently
//! shadows a later reprocessing that set the attribute properly, and
//! nothing on screen explains why the new name did not take.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::routes::{lookup_dataset, ApiError};
use crate::identity::{DisplayName, GroupId, GroupName, RadargramId};
use crate::project::overrides::{
    self, CatalogOverrides, GroupMembership, GroupOverride, RadargramOverride,
};
use crate::project::users::Role;

/// What the dialog needs to render itself.
#[derive(Debug, Serialize)]
pub struct Properties {
    radargram_id: String,
    /// What the catalog currently shows.
    effective: EffectiveProperties,
    /// What it would show with the override removed, field by field, so
    /// "revert" can be labelled with the value it would revert *to* rather
    /// than being a leap of faith.
    from_file: FileProperties,
    /// Whether each field is currently overridden, so the dialog can say
    /// which values are the project's opinion and which are the file's.
    overridden: OverriddenFields,
    /// Every group the catalog knows about, for choosing among them
    /// without having to retype a name and hope the slug matches.
    groups: Vec<GroupChoice>,
}

#[derive(Debug, Serialize)]
struct EffectiveProperties {
    display_name: Option<String>,
    group_id: Option<String>,
    group_name: Option<String>,
    unlisted: bool,
}

#[derive(Debug, Serialize)]
struct FileProperties {
    display_name: Option<String>,
    group_id: Option<String>,
    group_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct OverriddenFields {
    display_name: bool,
    group: bool,
}

#[derive(Debug, Serialize)]
struct GroupChoice {
    id: String,
    name: String,
}

/// A whole override, replaced rather than patched.
///
/// The dialog shows every field at once, so a save says what all of them
/// should be. A patch would leave "the user cleared this" and "the user did
/// not mention this" looking identical in JSON, which is exactly the
/// distinction the whole document is built on.
#[derive(Debug, Deserialize)]
pub struct PropertiesBody {
    /// `null` or empty reverts to the file's display name.
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    grouping: Grouping,
    /// The group to join, when `grouping` is `group`.
    #[serde(default)]
    group_id: Option<String>,
    /// What that group is called. Stored against the group, not the
    /// radargram, so renaming it is one edit and its members cannot come to
    /// disagree. Omitted leaves any existing name alone.
    #[serde(default)]
    group_name: Option<String>,
    #[serde(default)]
    unlisted: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Grouping {
    /// Whatever the file says.
    #[default]
    Inherit,
    /// In no group, whatever the file says.
    Ungrouped,
    /// In `group_id`.
    Group,
}

fn project_for<'a>(
    state: &'a AppState,
    caller: &Caller,
    action: &str,
) -> Result<&'a crate::project::Project, ApiError> {
    caller.require(Role::Operator, action)?;
    state.project.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "not_a_project",
            "This catalog is not a Ridal project, so there is nowhere to save \
             its properties. Run `ridal project init` in the directory you are \
             serving, then restart.",
        )
    })
}

pub async fn get_properties(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "read a radargram's properties")?;
    let catalog = state.catalog();
    let entry = lookup_dataset(&catalog, &radargram_id)?;

    // Strict rather than lenient: an operator opening the dialog over an
    // unparsable document must be told, not shown an empty form whose save
    // would discard whatever it held.
    let (stored, _) = overrides::read(project.documents())
        .map_err(|e| ApiError::internal("overrides_read_failed", e.to_string()))?;
    let over = stored.radargram(&entry.radargram_id);

    let mut groups: Vec<GroupChoice> = catalog
        .group_names
        .iter()
        .map(|(id, name)| GroupChoice {
            id: id.as_str().to_string(),
            name: name.as_str().to_string(),
        })
        .collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(Properties {
        radargram_id: entry.radargram_id.to_string(),
        effective: EffectiveProperties {
            display_name: entry.display_name.as_ref().map(|n| n.to_string()),
            group_id: entry.group_id.as_ref().map(|g| g.to_string()),
            group_name: entry.group_name.as_ref().map(|g| g.to_string()),
            unlisted: entry.unlisted,
        },
        from_file: FileProperties {
            display_name: entry.from_file.display_name.as_ref().map(|n| n.to_string()),
            group_id: entry.from_file.group_id.as_ref().map(|g| g.to_string()),
            group_name: entry.from_file.group_name.as_ref().map(|g| g.to_string()),
        },
        overridden: OverriddenFields {
            display_name: over.display_name.is_some(),
            group: over.group.is_some(),
        },
        groups,
    }))
}

pub async fn put_properties(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
    Json(body): Json<PropertiesBody>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "change a radargram's properties")?;
    // Resolved against the catalog first, so editing a radargram that is
    // not there is a 404 rather than a write nobody will ever see.
    let id = {
        let catalog = state.catalog();
        lookup_dataset(&catalog, &radargram_id)?
            .radargram_id
            .clone()
    };

    let display_name = match body.display_name.as_deref() {
        None | Some("") => None,
        Some(name) => Some(DisplayName::from_input(name).ok_or_else(|| {
            ApiError::bad_request("invalid_display_name", "A name cannot be only spaces.")
        })?),
    };

    let (group, group_name) = resolve_grouping(&body)?;

    let over = RadargramOverride {
        display_name,
        group,
        unlisted: body.unlisted,
    };

    overrides::update(project.documents(), |stored| {
        apply(stored, &id, &over, group_name.as_ref());
        Ok(())
    })
    .map_err(|e| ApiError::internal("overrides_write_failed", e.to_string()))?;

    refresh_catalog(&state)?;

    Ok(StatusCode::NO_CONTENT)
}

/// Read the body's three grouping fields as the one decision they describe.
fn resolve_grouping(
    body: &PropertiesBody,
) -> Result<(Option<GroupMembership>, Option<GroupName>), ApiError> {
    match body.grouping {
        Grouping::Inherit => Ok((None, None)),
        Grouping::Ungrouped => Ok((Some(GroupMembership::Ungrouped), None)),
        Grouping::Group => {
            let id = body.group_id.as_deref().unwrap_or("").trim();
            let name = body.group_name.as_deref().unwrap_or("").trim();
            // A new group can be named without being spelled: the slug is
            // derived exactly as processing derives it, so typing an
            // existing group's name lands in that group rather than beside
            // it.
            let group_id = if id.is_empty() {
                GroupId::from_fallback(name).map_err(|e| {
                    ApiError::bad_request(
                        "invalid_group",
                        format!("'{name}' does not give a usable group id: {e}"),
                    )
                })?
            } else {
                GroupId::new(id)
                    .map_err(|e| ApiError::bad_request("invalid_group", e.to_string()))?
            };
            Ok((
                Some(GroupMembership::Group(group_id)),
                GroupName::from_input(name),
            ))
        }
    }
}

/// Write one radargram's override into the document, and the group's name
/// beside it.
fn apply(
    stored: &mut CatalogOverrides,
    id: &RadargramId,
    over: &RadargramOverride,
    group_name: Option<&GroupName>,
) {
    if over.is_empty() {
        // Removed rather than stored empty, so reverting every field
        // returns the document to what it was before anyone touched this
        // radargram.
        stored.radargrams.remove(id);
    } else {
        stored.radargrams.insert(id.clone(), over.clone());
    }

    if let (Some(GroupMembership::Group(group_id)), Some(name)) = (&over.group, group_name) {
        stored.groups.insert(
            group_id.clone(),
            GroupOverride {
                name: Some(name.clone()),
            },
        );
    }
}

/// Re-resolve the catalog so the change is visible without a restart.
///
/// In place rather than by rediscovery. Rediscovery would re-walk the tree
/// and re-read every file's attributes to learn nothing new -- a hundred
/// NetCDF headers to rename one card -- and it would hang a filesystem walk
/// off the end of an HTTP request, which is a worse shape than the cost
/// alone suggests. An override changes what the catalog *says*, never what
/// is in it.
///
/// Both paths share one resolution function, so the re-resolved catalog is
/// what discovery would have produced from the same files.
///
/// The render services are carried over untouched, for the same reason: an
/// override never changes a file's contents, so reopening them would throw
/// away every warm cache for nothing.
fn refresh_catalog(state: &AppState) -> Result<(), ApiError> {
    let project = state
        .project
        .as_ref()
        .ok_or_else(|| ApiError::internal("not_a_project", "no project to refresh"))?;
    // Lenient here, unlike the read above: the document was just written by
    // this process, so an unparsable one is not something the operator can
    // act on, and failing the request after the write succeeded would
    // report a failure that did not happen.
    let stored = overrides::read_lenient(project.documents());
    let snapshot = state.catalog();
    state.replace_catalog(snapshot.reresolved(&stored), snapshot.open_radargrams());
    Ok(())
}

/// What the group dialog needs to render itself.
#[derive(Debug, Serialize)]
pub struct GroupProperties {
    group_id: String,
    /// The name the catalog currently shows.
    name: Option<String>,
    /// What it would show without the override: the name resolved from the
    /// members' own files, or nothing when the group exists only because
    /// somebody put radargrams in it.
    from_file: Option<String>,
    overridden: bool,
    /// How many radargrams are in it, so the dialog can say what a rename
    /// affects rather than leaving it to be guessed.
    member_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct GroupPropertiesBody {
    /// `null` or empty reverts to whatever the member files say.
    #[serde(default)]
    display_name: Option<String>,
}

/// Resolve and validate a group id from the path.
///
/// The ungrouped pseudo-group is refused rather than 404ing: `_none` is not
/// a group that lost its name, it is the absence of one, and saying so is
/// more use than "not found".
fn group_for(raw: &str) -> Result<GroupId, ApiError> {
    if raw == super::app::NO_GROUP_ID {
        return Err(ApiError::bad_request(
            "not_a_group",
            "\"Ungrouped\" is not a group, it is the radargrams that are in none. \
             Put them in a group to give them one.",
        ));
    }
    GroupId::new(raw).map_err(|e| ApiError::bad_request("invalid_group", e.to_string()))
}

pub async fn get_group_properties(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(group_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "read a group's properties")?;
    let id = group_for(&group_id)?;
    let catalog = state.catalog();

    let member_count = catalog
        .entries
        .iter()
        .filter(|e| e.group_id.as_ref() == Some(&id))
        .count();
    if member_count == 0 {
        return Err(ApiError::not_found(
            "group_not_found",
            format!("No group with id '{group_id}'"),
        ));
    }

    // What the files alone would call it. Asked by re-resolving against no
    // overrides at all, rather than by reimplementing the rule: a group
    // whose members are only in it because of an override correctly has no
    // name of its own here.
    let from_file = catalog
        .reresolved(&CatalogOverrides::default())
        .group_names
        .get(&id)
        .map(|name| name.as_str().to_string());

    let (stored, _) = overrides::read(project.documents())
        .map_err(|e| ApiError::internal("overrides_read_failed", e.to_string()))?;

    Ok(Json(GroupProperties {
        group_id: id.as_str().to_string(),
        name: catalog
            .group_names
            .get(&id)
            .map(|name| name.as_str().to_string()),
        from_file,
        overridden: stored
            .groups
            .get(&id)
            .and_then(|group| group.name.as_ref())
            .is_some(),
        member_count,
    }))
}

pub async fn put_group_properties(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(group_id): Path<String>,
    Json(body): Json<GroupPropertiesBody>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "change a group's properties")?;
    let id = group_for(&group_id)?;

    let name = match body.display_name.as_deref() {
        None | Some("") => None,
        Some(value) => Some(GroupName::from_input(value).ok_or_else(|| {
            ApiError::bad_request("invalid_group_name", "A name cannot be only spaces.")
        })?),
    };

    overrides::update(project.documents(), |stored| {
        match &name {
            Some(name) => {
                stored
                    .groups
                    .entry(id.clone())
                    .or_default()
                    .name
                    .replace(name.clone());
            }
            // Reverting clears just the name, leaving any other group
            // setting alone; `prune` drops the entry if nothing is left.
            None => {
                if let Some(group) = stored.groups.get_mut(&id) {
                    group.name = None;
                }
            }
        }
        Ok(())
    })
    .map_err(|e| ApiError::internal("overrides_write_failed", e.to_string()))?;

    refresh_catalog(&state)?;

    Ok(StatusCode::NO_CONTENT)
}
