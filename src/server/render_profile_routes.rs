//! Render profiles: the built-ins and the project's own (#183).
//!
//! ```text
//! GET    /api/v1/render-profiles          every profile the project can render with
//! GET    /api/v1/render-profiles/{name}   one, as JSON or (`?format=toml`) a profile file
//! PUT    /api/v1/render-profiles/{name}   create or replace a project profile
//! DELETE /api/v1/render-profiles/{name}   delete a project profile
//! ```
//!
//! Reading needs `viewer`; changing needs `operator`, as for the project's
//! other shared settings. The built-ins cannot be changed or deleted; an
//! operator hides one through the settings instead
//! (`render.hidden_profiles`).
//!
//! [`ProfileSet`] is the one answer to "which profiles are there" for every
//! route and page, so that a project's profiles render, list and validate
//! the same way everywhere a built-in does.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::{expectation_from, readable_project};
use super::needs::{Needs, Who};
use super::routes::{ApiError, ErrorBody};
use crate::project::render_profiles::{self, RenderProfileError};
use crate::project::roles::Role;
use crate::project::store::StoreError;
use crate::render::profile::RenderProfile;

/// The profile a request that names none renders with, under every other
/// layer of the cascade.
pub const FALLBACK_PROFILE: &str = "default";

/// Every render profile a project knows: the built-ins, then its own.
///
/// Built-ins come first in lookups as well as in lists, so a project
/// profile can never change what a built-in name means. The project's store
/// already refuses such names; this also covers a profile made before a
/// later release added a built-in of the same name.
pub struct ProfileSet {
    built_in: Vec<RenderProfile>,
    project: Arc<Vec<RenderProfile>>,
    hidden: Vec<String>,
}

impl ProfileSet {
    fn new(project: Arc<Vec<RenderProfile>>, hidden: Vec<String>) -> Self {
        Self {
            built_in: RenderProfile::built_in_profiles(),
            project,
            hidden,
        }
    }

    /// The built-ins and `profiles`, with what `project` hides.
    pub fn of_project(
        project: Option<&crate::project::Project>,
        profiles: Arc<Vec<RenderProfile>>,
    ) -> Self {
        Self::new(
            profiles,
            project.map(|p| p.hidden_profiles()).unwrap_or_default(),
        )
    }

    /// The profile called `name`, hidden or not.
    pub fn lookup(&self, name: &str) -> Option<&RenderProfile> {
        self.built_in
            .iter()
            .chain(self.project.iter())
            .find(|p| p.name == name)
    }

    pub fn is_built_in(&self, name: &str) -> bool {
        self.built_in.iter().any(|p| p.name == name)
    }

    pub fn is_hidden(&self, name: &str) -> bool {
        self.is_built_in(name) && self.hidden.iter().any(|h| h == name)
    }

    /// Every profile, hidden ones included: built-ins first.
    pub fn all(&self) -> impl Iterator<Item = &RenderProfile> {
        let project = self.project.iter().filter(|p| !self.is_built_in(&p.name));
        self.built_in.iter().chain(project)
    }

    /// The profiles to choose from, plus `active` if it is hidden: a page
    /// open on a hidden profile (from a link, or a stored preference) still
    /// shows it selected rather than a list matching nothing.
    pub fn offered(&self, active: &str) -> Vec<&RenderProfile> {
        self.all()
            .filter(|p| !self.is_hidden(&p.name) || p.name == active)
            .collect()
    }

    /// [`Self::offered`], by name.
    pub fn offered_names(&self, active: &str) -> Vec<String> {
        self.offered(active)
            .into_iter()
            .map(|p| p.name.clone())
            .collect()
    }

    /// The overview-file tags of every profile anyone can ask for, for
    /// clearing out the overviews of the rest
    /// ([`super::overview_cache::OverviewDiskCache::harvest`]).
    pub fn overview_tags(&self) -> HashSet<String> {
        self.all().map(super::overview_cache::profile_tag).collect()
    }
}

/// A project's own profiles from its store, with a warning for each file
/// that could not be one. Empty without a project.
pub fn load_project_profiles(project: Option<&crate::project::Project>) -> Vec<RenderProfile> {
    let Some(project) = project else {
        return Vec::new();
    };
    match render_profiles::list(project.documents()) {
        Ok((profiles, problems)) => {
            for problem in problems {
                eprintln!("Warning: skipping a render profile: {problem}");
            }
            profiles
        }
        Err(e) => {
            eprintln!("Warning: could not read the project's render profiles: {e}");
            Vec::new()
        }
    }
}

pub(super) fn render_profile_error(error: RenderProfileError) -> ApiError {
    use RenderProfileError as E;
    match &error {
        E::Store(StoreError::Conflict { .. }) => {
            ApiError::precondition_failed("version_conflict", error.to_string())
        }
        E::Store(_) | E::Malformed { .. } => {
            ApiError::internal("render_profile_store_failed", error.to_string())
        }
        E::InvalidName(_) => {
            ApiError::bad_request("invalid_render_profile_name", error.to_string())
        }
        E::BuiltIn(_) => ApiError::conflict("built_in_render_profile", error.to_string()),
        E::Reserved(_) => ApiError::bad_request("reserved_render_profile_name", error.to_string()),
        E::NameMismatch { .. } => {
            ApiError::bad_request("render_profile_name_mismatch", error.to_string())
        }
        E::Invalid(_) => ApiError::bad_request("invalid_render_profile", error.to_string()),
    }
}

fn no_such_profile(name: &str) -> ApiError {
    ApiError::not_found(
        "no_such_render_profile",
        format!("There is no render profile called '{name}'."),
    )
}

fn writable_project<'a>(
    state: &'a AppState,
    caller: &Caller,
    action: &str,
) -> Result<&'a crate::project::Project, ApiError> {
    caller.require(Role::Operator, action)?;
    state.project.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "not_a_project",
            "This catalog is not a Ridal project, so there is nowhere to keep \
             render profiles. Run `ridal project init` in the directory you are \
             serving, then restart.",
        )
    })
}

/// Where a profile comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileScope {
    /// Part of Ridal; cannot be changed or deleted.
    BuiltIn,
    /// The project's own.
    Project,
}

/// A render profile, as the list and the single view give it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RenderProfileView {
    #[serde(flatten)]
    pub profile: RenderProfile,
    pub scope: ProfileScope,
    /// Left out of the lists people choose from, but still rendered when
    /// asked for by name. Only a built-in can be hidden.
    pub hidden: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RenderProfileList {
    /// Built-ins first, then the project's own, hidden ones included.
    pub profiles: Vec<RenderProfileView>,
    /// The profile a request that names none renders with: the project's
    /// default, or `default`.
    pub default: String,
}

fn view_of(set: &ProfileSet, profile: &RenderProfile) -> RenderProfileView {
    RenderProfileView {
        scope: if set.is_built_in(&profile.name) {
            ProfileScope::BuiltIn
        } else {
            ProfileScope::Project
        },
        hidden: set.is_hidden(&profile.name),
        profile: profile.clone(),
    }
}

/// Every render profile this project can render with, each in full.
#[utoipa::path(
    get,
    path = "/api/v1/render-profiles",
    operation_id = "list_render_profiles",
    tag = "Render profiles",
    responses(
        (status = 200, description = "The profiles.", body = RenderProfileList),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
pub async fn list_render_profiles(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<RenderProfileList>, ApiError> {
    caller.require(Role::Viewer, "see the render profiles")?;
    let set = state.render_profiles();
    Ok(Json(RenderProfileList {
        profiles: set.all().map(|p| view_of(&set, p)).collect(),
        default: project_default(&state),
    }))
}

/// The profile the project renders with when nothing names one.
fn project_default(state: &AppState) -> String {
    state
        .project
        .as_ref()
        .and_then(|p| p.default_profile())
        .unwrap_or_else(|| FALLBACK_PROFILE.to_string())
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProfileFormat {
    /// `toml` for a profile file, as `ridal render --render-profile FILE`
    /// and the Python package read; JSON otherwise.
    format: Option<String>,
}

/// One render profile. A project profile comes with an `ETag`.
#[utoipa::path(
    get,
    path = "/api/v1/render-profiles/{name}",
    operation_id = "get_render_profile",
    tag = "Render profiles",
    params(ProfileFormat),
    responses(
        (status = 200, description = "The profile; with `?format=toml`, the profile as a file.", content((RenderProfileView = "application/json"), ("application/toml"))),
        (status = 400, description = "`unknown_format`.", body = ErrorBody),
        (status = 404, description = "`no_such_render_profile`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
pub async fn get_render_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(name): Path<String>,
    Query(query): Query<ProfileFormat>,
) -> Result<Response, ApiError> {
    caller.require(Role::Viewer, "see a render profile")?;
    let set = state.render_profiles();
    let profile = set.lookup(&name).ok_or_else(|| no_such_profile(&name))?;
    match query.format.as_deref() {
        None | Some("json") => {}
        Some("toml") => {
            let text = toml::to_string(profile)
                .map_err(|e| ApiError::internal("render_profile_encode_failed", e.to_string()))?;
            return Ok((
                [
                    (header::CONTENT_TYPE, "application/toml".to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{}.toml\"", profile.name),
                    ),
                ],
                text,
            )
                .into_response());
        }
        Some(other) => {
            return Err(ApiError::bad_request(
                "unknown_format",
                format!("Unknown format '{other}'. Supported: json, toml."),
            ))
        }
    }
    let view = view_of(&set, profile);
    // A built-in has no stored version to be conditional on.
    let version = match view.scope {
        ProfileScope::BuiltIn => None,
        ProfileScope::Project => {
            let project = readable_project(&state)?;
            let name = render_profiles::check_name(&name).map_err(render_profile_error)?;
            render_profiles::read(project.documents(), &name)
                .map_err(render_profile_error)?
                .map(|(_, version)| version)
        }
    };
    let mut response = Json(view).into_response();
    if let Some(version) = version {
        response.headers_mut().insert(
            header::ETAG,
            version
                .to_string()
                .parse()
                .expect("a version is a valid header value"),
        );
    }
    Ok(response)
}

/// Create or replace one of the project's render profiles. The body's
/// `name` must be the one in the path. Takes `If-Match`, and
/// `If-None-Match: *` to only create.
#[utoipa::path(
    put,
    path = "/api/v1/render-profiles/{name}",
    operation_id = "put_render_profile",
    tag = "Render profiles",
    request_body = RenderProfile,
    responses(
        (status = 200, description = "The profile as saved, with its `ETag`.", body = RenderProfileView),
        (status = 400, description = "`invalid_render_profile`, `invalid_render_profile_name`, `reserved_render_profile_name` or `render_profile_name_mismatch`.", body = ErrorBody),
        (status = 409, description = "`built_in_render_profile`: the name is a built-in's.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
pub async fn put_render_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(profile): Json<RenderProfile>,
) -> Result<Response, ApiError> {
    let project = writable_project(&state, &caller, "change a render profile")?;
    let (saved, version) = state
        .edit_render_profiles(|| {
            render_profiles::save(
                project.documents(),
                &name,
                profile,
                &expectation_from(&headers),
            )
        })
        .map_err(render_profile_error)?;
    let set = state.render_profiles();
    Ok((
        [(header::ETAG, version.to_string())],
        Json(view_of(&set, &saved)),
    )
        .into_response())
}

/// Delete one of the project's render profiles. Refused for a built-in, and
/// for the project's default. People and links that use it fall back to
/// the next profile they would have used.
#[utoipa::path(
    delete,
    path = "/api/v1/render-profiles/{name}",
    operation_id = "delete_render_profile",
    tag = "Render profiles",
    responses(
        (status = 204, description = "Deleted."),
        (status = 404, description = "`no_such_render_profile`.", body = ErrorBody),
        (status = 409, description = "`built_in_render_profile`, or `render_profile_is_default`.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
pub async fn delete_render_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let project = writable_project(&state, &caller, "delete a render profile")?;
    if project_default(&state) == name {
        return Err(ApiError::conflict(
            "render_profile_is_default",
            format!(
                "'{name}' is the project's default render profile. \
                 Choose another default first."
            ),
        ));
    }
    let checked = render_profiles::check_name(&name).map_err(render_profile_error)?;
    let removed = state
        .edit_render_profiles(|| {
            render_profiles::delete(project.documents(), &checked, &expectation_from(&headers))
        })
        .map_err(render_profile_error)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(no_such_profile(&name))
    }
}
