//! Processing profiles and assignments (#382, phase 6).
//!
//! ```text
//! GET    /api/v1/processing/profiles                     every profile
//! GET    /api/v1/processing/profiles/{profile}           one, every version, and who uses it
//! PUT    /api/v1/processing/profiles/{profile}           create or change it
//! DELETE /api/v1/processing/profiles/{profile}           delete one nobody uses
//! GET    /api/v1/processing/assignments                  which radargram uses which
//! PUT    /api/v1/processing/assignments/{radargram_id}   put a radargram in a profile
//! DELETE /api/v1/processing/assignments/{radargram_id}   take it out
//! ```
//!
//! Reading needs `viewer`; changing needs `operator`, as agreed in #382.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::{expectation_from, parse_radargram, readable_project};
use super::routes::ApiError;
use crate::identity::{ProfileId, RadargramId};
use crate::project::processing::{self, Assignment, Assignments, Profile, ProfileEdit};
use crate::project::roles::Role;
use crate::project::store::StoreError;

pub(super) fn processing_error(error: processing::ProcessingError) -> ApiError {
    use processing::ProcessingError as E;
    match &error {
        E::Store(StoreError::Conflict { .. }) => {
            ApiError::precondition_failed("version_conflict", error.to_string())
        }
        E::Store(_) | E::Malformed { .. } => {
            ApiError::internal("processing_store_failed", error.to_string())
        }
        E::Steps(_) => ApiError::bad_request("invalid_steps", error.to_string()),
        E::Velocity(_) => ApiError::bad_request("invalid_velocity", error.to_string()),
        E::InUse { .. } => ApiError::conflict("profile_in_use", error.to_string()),
        E::NoSuchProfile(_) => ApiError::not_found("no_such_profile", error.to_string()),
    }
}

fn parse_profile(raw: &str) -> Result<ProfileId, ApiError> {
    ProfileId::new(raw).map_err(|e| ApiError::bad_request("invalid_profile_id", e))
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
             processing profiles. Run `ridal project init` in the directory you are \
             serving, then restart.",
        )
    })
}

/// A profile, as the list and the single view give it.
#[derive(Debug, Serialize)]
pub struct ProfileView {
    #[serde(flatten)]
    pub profile: Profile,
    /// The version radargrams are processed with now.
    pub current_version: u32,
    /// The radargrams that use it.
    pub members: Vec<RadargramId>,
}

#[derive(Debug, Serialize)]
pub struct ProfileList {
    pub profiles: Vec<ProfileView>,
}

pub async fn list_profiles(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<ProfileList>, ApiError> {
    caller.require(Role::Viewer, "see the processing profiles")?;
    let project = readable_project(&state)?;
    let (assignments, _) =
        processing::read_assignments(project.documents()).map_err(processing_error)?;
    let profiles = processing::list_profiles(project.documents())
        .map_err(processing_error)?
        .into_iter()
        .map(|profile| ProfileView {
            current_version: profile.current().version,
            members: assignments
                .members(&profile.id)
                .into_iter()
                .cloned()
                .collect(),
            profile,
        })
        .collect();
    Ok(Json(ProfileList { profiles }))
}

pub async fn get_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(profile): Path<String>,
) -> Result<Response, ApiError> {
    caller.require(Role::Viewer, "see a processing profile")?;
    let project = readable_project(&state)?;
    let id = parse_profile(&profile)?;
    let (profile, version) = processing::read_profile(project.documents(), &id)
        .map_err(processing_error)?
        .ok_or_else(|| processing_error(processing::ProcessingError::NoSuchProfile(id.clone())))?;
    let (assignments, _) =
        processing::read_assignments(project.documents()).map_err(processing_error)?;
    let members = assignments.members(&id).into_iter().cloned().collect();
    Ok((
        [(header::ETAG, version.to_string())],
        Json(ProfileView {
            current_version: profile.current().version,
            profile,
            members,
        }),
    )
        .into_response())
}

pub async fn put_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(profile): Path<String>,
    headers: HeaderMap,
    Json(edit): Json<ProfileEdit>,
) -> Result<Response, ApiError> {
    let project = writable_project(&state, &caller, "change a processing profile")?;
    let id = parse_profile(&profile)?;
    if edit.name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "name_required",
            "A processing profile needs a name.",
        ));
    }
    let (profile, version) = processing::save_profile(
        project.documents(),
        &id,
        edit,
        &caller.audit_name(),
        &chrono::Utc::now().to_rfc3339(),
        &expectation_from(&headers),
    )
    .map_err(processing_error)?;
    let (assignments, _) =
        processing::read_assignments(project.documents()).map_err(processing_error)?;
    let members = assignments.members(&id).into_iter().cloned().collect();
    Ok((
        [(header::ETAG, version.to_string())],
        Json(ProfileView {
            current_version: profile.current().version,
            profile,
            members,
        }),
    )
        .into_response())
}

pub async fn delete_profile(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(profile): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let project = writable_project(&state, &caller, "delete a processing profile")?;
    let id = parse_profile(&profile)?;
    if processing::delete_profile(project.documents(), &id, &expectation_from(&headers))
        .map_err(processing_error)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(processing_error(
            processing::ProcessingError::NoSuchProfile(id),
        ))
    }
}

pub async fn list_assignments(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<Assignments>, ApiError> {
    caller.require(
        Role::Viewer,
        "see which radargram uses which processing profile",
    )?;
    let project = readable_project(&state)?;
    let (assignments, _) =
        processing::read_assignments(project.documents()).map_err(processing_error)?;
    Ok(Json(assignments))
}

#[derive(Debug, Deserialize)]
pub struct AssignmentBody {
    pub profile: String,
    /// Steps of the radargram's own, run before the profile's: `subset`
    /// and `remove_traces` only.
    #[serde(default)]
    pub pre_steps: Vec<String>,
}

pub async fn put_assignment(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
    Json(body): Json<AssignmentBody>,
) -> Result<Json<Assignment>, ApiError> {
    let project = writable_project(&state, &caller, "put a radargram in a processing profile")?;
    let radargram = parse_radargram(&radargram_id)?;
    let profile = parse_profile(&body.profile)?;
    // A profile is processing to run, and without raw data there is
    // nothing to run it on. A source removed later leaves the radargram in
    // its profile, shown as having no raw data.
    let has_source = crate::project::raw::read_source(project.documents(), &radargram)
        .map_err(|e| ApiError::internal("source_read_failed", e))?
        .is_some();
    if !has_source {
        return Err(ApiError::conflict(
            "no_source",
            format!(
                "'{radargram}' has no raw data in this project, so a processing profile \
                 has nothing to process. Attach its raw data first."
            ),
        ));
    }
    let assignment = processing::assign(
        project.documents(),
        &radargram,
        Some((profile, body.pre_steps)),
    )
    .map_err(processing_error)?
    .expect("an assignment was asked for");
    Ok(Json(assignment))
}

pub async fn delete_assignment(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let project = writable_project(&state, &caller, "take a radargram out of its profile")?;
    let radargram = parse_radargram(&radargram_id)?;
    processing::assign(project.documents(), &radargram, None).map_err(processing_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// One radargram in a profile, and where it stands.
#[derive(Debug, Serialize)]
pub struct RadargramStatus {
    pub radargram_id: RadargramId,
    pub profile: ProfileId,
    #[serde(flatten)]
    pub state: processing::ProcessingState,
}

#[derive(Debug, Serialize)]
pub struct StatusList {
    pub radargrams: Vec<RadargramStatus>,
}

/// `GET /api/v1/processing/status`: every radargram in a profile, and
/// whether it is processed as its profile and source say now (#382).
///
/// Reads each served file's recipe, so it opens every file once: off the
/// async executor.
#[derive(Debug, Default, Deserialize)]
pub struct StatusQuery {
    /// Only this radargram: what the process dialog asks, rather than
    /// reading every file in the project to show one.
    pub radargram_id: Option<String>,
}

pub async fn processing_status(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Query(query): Query<StatusQuery>,
) -> Result<Json<StatusList>, ApiError> {
    caller.require(Role::Viewer, "see which radargrams are up to date")?;
    let project = readable_project(&state)?;
    let only = query
        .radargram_id
        .as_deref()
        .map(parse_radargram)
        .transpose()?;
    let radargrams = statuses(&state, project, only.as_ref()).await?;
    Ok(Json(StatusList { radargrams }))
}

/// Where every radargram in a profile stands. Shared by the status route
/// and by a batch deciding what to reprocess.
pub(super) async fn statuses(
    state: &AppState,
    project: &crate::project::Project,
    only: Option<&RadargramId>,
) -> Result<Vec<RadargramStatus>, ApiError> {
    let (assignments, _) =
        processing::read_assignments(project.documents()).map_err(processing_error)?;
    let profiles: std::collections::HashMap<ProfileId, Profile> =
        processing::list_profiles(project.documents())
            .map_err(processing_error)?
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();

    let catalog = state.catalog();
    let mut work = Vec::new();
    for (radargram, assignment) in assignments.radargrams {
        if only.is_some_and(|only| only != &radargram) {
            continue;
        }
        let Some(profile) = profiles.get(&assignment.profile).cloned() else {
            continue;
        };
        let source = crate::project::raw::read_source(project.documents(), &radargram)
            .map_err(|e| ApiError::internal("source_read_failed", e))?
            .map(|(source, _)| source);
        let served = catalog
            .find_entry(radargram.as_str())
            .and_then(|entry| state.absolute_path(entry).ok());
        work.push((radargram, assignment, profile, source, served));
    }
    drop(catalog);

    tokio::task::spawn_blocking(move || {
        work.into_iter()
            .map(|(radargram, assignment, profile, source, served)| {
                let recipe = served
                    .as_ref()
                    .and_then(|path| crate::io::read_recipe(path).ok().flatten());
                RadargramStatus {
                    state: processing::state_of(
                        &assignment,
                        &profile,
                        source.as_ref(),
                        served.is_some(),
                        recipe.as_ref(),
                    ),
                    profile: assignment.profile,
                    radargram_id: radargram,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| ApiError::internal("status_failed", e.to_string()))
}
