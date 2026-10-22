//! Render profiles: the built-ins and the project's own (#183).
//!
//! ```text
//! GET    /api/v1/render-profiles          every profile the project can render with
//! GET    /api/v1/render-profiles/{name}   one, as JSON or (`?format=toml`) a profile file
//! PUT    /api/v1/render-profiles/{name}   create or replace a project profile
//! DELETE /api/v1/render-profiles/{name}   delete a project profile
//!
//! GET    /api/v1/preferences/render-profiles          the caller's personal profiles
//! GET    /api/v1/preferences/render-profiles/{name}   one of them, with an ETag
//! PUT    /api/v1/preferences/render-profiles/{name}   create or replace one
//! DELETE /api/v1/preferences/render-profiles/{name}   delete one
//! ```
//!
//! Reading needs `viewer`; changing needs `operator`, as for the project's
//! other shared settings. A personal profile is the caller's own, like their
//! preferences: any signed-in account keeps its own, while the project's
//! `render.personal_profiles` allows it. The built-ins cannot be changed or deleted; an
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
use crate::project::render_profiles::{self, PersonalProfile, RenderProfileError};
use crate::project::roles::Role;
use crate::project::store::StoreError;
use crate::render::profile::RenderProfile;

/// The profile a request that names none renders with, under every other
/// layer of the cascade.
pub const FALLBACK_PROFILE: &str = "default";

/// Every render profile a project knows: the built-ins, then its own, then
/// (when built for a caller) that person's personal ones.
///
/// Built-ins come first in lookups as well as in lists, so a project
/// profile can never change what a built-in name means. The project's store
/// already refuses such names; this also covers a profile made before a
/// later release added a built-in of the same name. Personal names cannot
/// collide with either: only theirs start with `personal-`.
pub struct ProfileSet {
    built_in: Vec<RenderProfile>,
    project: Arc<Vec<RenderProfile>>,
    hidden: Vec<String>,
    personal: Vec<PersonalProfile>,
}

impl ProfileSet {
    fn new(project: Arc<Vec<RenderProfile>>, hidden: Vec<String>) -> Self {
        Self {
            built_in: RenderProfile::built_in_profiles(),
            project,
            hidden,
            personal: Vec::new(),
        }
    }

    /// This set with `personal`, one person's own profiles, after the rest.
    pub fn with_personal(self, personal: Vec<PersonalProfile>) -> Self {
        Self { personal, ..self }
    }

    /// The personal profile called `name`, if this set has one.
    pub fn personal(&self, name: &str) -> Option<&PersonalProfile> {
        self.personal.iter().find(|p| p.profile.name == name)
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
            .chain(self.personal.iter().map(|p| &p.profile))
            .find(|p| p.name == name)
    }

    pub fn is_built_in(&self, name: &str) -> bool {
        self.built_in.iter().any(|p| p.name == name)
    }

    pub fn is_hidden(&self, name: &str) -> bool {
        self.is_built_in(name) && self.hidden.iter().any(|h| h == name)
    }

    /// Every shared profile (built-in or the project's), hidden ones
    /// included: built-ins first.
    pub fn shared(&self) -> impl Iterator<Item = &RenderProfile> {
        let project = self.project.iter().filter(|p| !self.is_built_in(&p.name));
        self.built_in.iter().chain(project)
    }

    /// [`Self::shared`], then the personal profiles.
    pub fn all(&self) -> impl Iterator<Item = &RenderProfile> {
        self.shared()
            .chain(self.personal.iter().map(|p| &p.profile))
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

    /// [`Self::offered_names`] for the catalog: the shared profiles only,
    /// since its thumbnails are never drawn with a personal one. `active`
    /// is still listed when it is personal, so the select shows what the
    /// page's links carry.
    pub fn offered_shared_names(&self, active: &str) -> Vec<String> {
        self.offered(active)
            .into_iter()
            .filter(|p| !p.is_personal() || p.name == active)
            .map(|p| p.name.clone())
            .collect()
    }

    /// The overview-file tags of every shared profile, for clearing out
    /// the overviews of the rest
    /// ([`super::overview_cache::OverviewDiskCache::harvest`]). Never a
    /// personal profile's: those draw no overview of their own.
    pub fn overview_tags(&self) -> HashSet<String> {
        self.shared()
            .map(super::overview_cache::profile_tag)
            .collect()
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

/// `user`'s personal profiles in `project`, with a warning for each file
/// that could not be one.
pub fn load_personal_profiles(
    project: &crate::project::Project,
    user: &crate::identity::UserId,
) -> Vec<PersonalProfile> {
    match render_profiles::list_personal(project.documents(), user) {
        Ok((profiles, problems)) => {
            for problem in problems {
                eprintln!("Warning: skipping a personal render profile: {problem}");
            }
            profiles
        }
        Err(e) => {
            eprintln!("Warning: could not read {user}'s personal render profiles: {e}");
            Vec::new()
        }
    }
}

/// `caller`'s personal profile called `name`, read on its own: what a tile
/// that names one needs, without listing all of theirs. `None` when there is
/// no such profile, the name is not a personal one, nobody is signed in, or
/// the project does not allow personal profiles.
pub fn lookup_personal(state: &AppState, caller: &Caller, name: &str) -> Option<PersonalProfile> {
    let project = state.personal_project()?;
    let user = caller.user.as_ref()?;
    let name = render_profiles::check_personal_name(name).ok()?;
    match render_profiles::read_personal(project.documents(), user, &name) {
        Ok(found) => found.map(|(profile, _)| profile),
        Err(e) => {
            eprintln!("Warning: skipping a personal render profile: {e}");
            None
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
        E::NotPersonal(_) => {
            ApiError::bad_request("not_a_personal_profile_name", error.to_string())
        }
        E::TooMany => ApiError::conflict("too_many_personal_profiles", error.to_string()),
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
    /// The caller's own, in this project only (`personal-…`).
    Personal,
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
    /// For a personal profile, the shared profile the catalog's thumbnails
    /// are drawn with while it is chosen; unset is the project's default.
    /// `null` for every other profile.
    #[schema(required = true)]
    pub thumbnail_profile: Option<String>,
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
    if let Some(personal) = set.personal(&profile.name) {
        return personal_view(personal.clone());
    }
    RenderProfileView {
        scope: if set.is_built_in(&profile.name) {
            ProfileScope::BuiltIn
        } else {
            ProfileScope::Project
        },
        hidden: set.is_hidden(&profile.name),
        profile: profile.clone(),
        thumbnail_profile: None,
    }
}

fn personal_view(personal: PersonalProfile) -> RenderProfileView {
    RenderProfileView {
        profile: personal.profile,
        scope: ProfileScope::Personal,
        hidden: false,
        thumbnail_profile: personal.thumbnail_profile,
    }
}

/// Every render profile the caller can render with in this project, each in
/// full: the built-ins, the project's own, and the caller's personal ones.
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
    let set = state.render_profiles_for(&caller);
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
    let set = state.render_profiles_for(&caller);
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
    // A built-in has no stored version to be conditional on, and a personal
    // profile's is given by `/preferences/render-profiles/{name}`, where it
    // is changed.
    let version = match view.scope {
        ProfileScope::BuiltIn | ProfileScope::Personal => None,
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

/// The caller's personal render profiles, and whether they may use them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PersonalProfileList {
    pub profiles: Vec<RenderProfileView>,
    /// Whether the project lets people keep their own. When it does not,
    /// the ones they have are kept but neither offered nor rendered, and no
    /// new one can be saved.
    pub allowed: bool,
    /// How many one person may keep in this project.
    pub max: usize,
}

/// The caller's own personal render profiles in this project.
#[utoipa::path(
    get,
    path = "/api/v1/preferences/render-profiles",
    operation_id = "list_personal_render_profiles",
    tag = "Render profiles",
    responses(
        (status = 200, description = "Their profiles.", body = PersonalProfileList),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn)))),
)]
pub async fn list_personal_render_profiles(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<PersonalProfileList>, ApiError> {
    let (project, user) = super::preference_routes::my_preferences_target(&state, &caller)?;
    let (profiles, _) =
        render_profiles::list_personal(project.documents(), user).map_err(render_profile_error)?;
    Ok(Json(PersonalProfileList {
        profiles: profiles.into_iter().map(personal_view).collect(),
        allowed: project.personal_profiles(),
        max: render_profiles::MAX_PERSONAL_PROFILES,
    }))
}

/// One of the caller's personal render profiles, with an `ETag`.
#[utoipa::path(
    get,
    path = "/api/v1/preferences/render-profiles/{name}",
    operation_id = "get_personal_render_profile",
    tag = "Render profiles",
    responses(
        (status = 200, description = "The profile.", body = RenderProfileView),
        (status = 404, description = "`no_such_render_profile`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn).own()))),
)]
pub async fn get_personal_render_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let (project, user) = super::preference_routes::my_preferences_target(&state, &caller)?;
    let checked = render_profiles::check_personal_name(&name).map_err(render_profile_error)?;
    let (personal, version) = render_profiles::read_personal(project.documents(), user, &checked)
        .map_err(render_profile_error)?
        .ok_or_else(|| no_such_profile(&name))?;
    Ok((
        [(header::ETAG, version.to_string())],
        Json(personal_view(personal)),
    )
        .into_response())
}

/// Create or replace one of the caller's personal render profiles. Its name
/// starts with `personal-`, and the body's `name` must be the one in the
/// path. `thumbnail_profile`, if set, names a built-in or project profile.
/// Takes `If-Match`, and `If-None-Match: *` to only create.
#[utoipa::path(
    put,
    path = "/api/v1/preferences/render-profiles/{name}",
    operation_id = "put_personal_render_profile",
    tag = "Render profiles",
    request_body = PersonalProfile,
    responses(
        (status = 200, description = "The profile as saved, with its `ETag`.", body = RenderProfileView),
        (status = 400, description = "`invalid_render_profile`, `invalid_render_profile_name`, `not_a_personal_profile_name`, `render_profile_name_mismatch` or `unknown_thumbnail_profile`.", body = ErrorBody),
        (status = 409, description = "`personal_profiles_off`: the project does not allow them; or `too_many_personal_profiles`.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn)))),
)]
pub async fn put_personal_render_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(personal): Json<PersonalProfile>,
) -> Result<Response, ApiError> {
    let (project, user) = super::preference_routes::my_preferences_target(&state, &caller)?;
    if !project.personal_profiles() {
        return Err(ApiError::conflict(
            "personal_profiles_off",
            "This project does not let people keep render profiles of their own. \
             The ones you have are kept, and come back if an operator allows them again.",
        ));
    }
    // A thumbnail is kept on disk, so it may only be drawn with a profile
    // the project shares.
    if let Some(thumbnail) = &personal.thumbnail_profile {
        if state.render_profiles().lookup(thumbnail).is_none() {
            return Err(ApiError::bad_request(
                "unknown_thumbnail_profile",
                format!(
                    "There is no built-in or project render profile called '{thumbnail}' \
                     to draw thumbnails with."
                ),
            ));
        }
    }
    let (saved, version) = render_profiles::save_personal(
        project.documents(),
        user,
        &name,
        personal,
        &expectation_from(&headers),
    )
    .map_err(render_profile_error)?;
    Ok((
        [(header::ETAG, version.to_string())],
        Json(personal_view(saved)),
    )
        .into_response())
}

/// Delete one of the caller's personal render profiles. Links and settings
/// that name it fall back to the next profile they would have used.
#[utoipa::path(
    delete,
    path = "/api/v1/preferences/render-profiles/{name}",
    operation_id = "delete_personal_render_profile",
    tag = "Render profiles",
    responses(
        (status = 204, description = "Deleted."),
        (status = 404, description = "`no_such_render_profile`.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn).own()))),
)]
// Allowed while the project does not allow personal profiles: what someone
// keeps is theirs to clear out.
pub async fn delete_personal_render_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let (project, user) = super::preference_routes::my_preferences_target(&state, &caller)?;
    let checked = render_profiles::check_personal_name(&name).map_err(render_profile_error)?;
    if render_profiles::delete_personal(
        project.documents(),
        user,
        &checked,
        &expectation_from(&headers),
    )
    .map_err(render_profile_error)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(no_such_profile(&name))
    }
}
