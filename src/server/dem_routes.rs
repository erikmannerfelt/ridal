//! DEMs as project resources, linked per radargram (#382).
//!
//! ```text
//! GET    /api/v1/dems                       every DEM, and who links to it
//! GET    /api/v1/dems/{dem}                 one
//! PUT    /api/v1/dems/{dem}                 add one from an uploaded file, rename it, or replace its file
//! DELETE /api/v1/dems/{dem}                 delete one nobody links to
//! PUT    /api/v1/sources/{radargram_id}/dem link a radargram to a DEM, or unlink it
//! ```
//!
//! A DEM is uploaded once (its file through `POST /api/v1/raw`, like any
//! raw file) and given a name and an id; a radargram's source links to it
//! by id. It is never part of a processing profile: which DEM covers a
//! radargram is about where it was recorded, not how it is processed.
//!
//! A DEM's file is replaced only while nothing links to it, so nobody's
//! elevations change underneath them: to change a linked DEM, add a new one
//! and relink. Linking or unlinking makes the radargram stale, which
//! [`crate::project::processing`] already says, since the source's DEM hash
//! is no longer the recipe's.
//!
//! Reading needs `viewer`; changing needs `operator`. The file itself is
//! downloaded as raw data, under download scope `all`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::{expectation_from, parse_radargram, readable_project};
use super::needs::{Needs, Who};
use super::raw_routes::{project_for, read_recordings, store_error, write_with_dem, SourceView};
use super::routes::{ApiError, ErrorBody};
use crate::identity::{DemId, RadargramId};
use crate::project::raw::{self, Dem, NamedFile, SourceDem};
use crate::project::roles::Role;
use crate::project::store::{Expectation, Version};
use crate::recipe::HashedFile;

pub(super) fn parse_dem(raw: &str) -> Result<DemId, ApiError> {
    DemId::new(raw).map_err(|e| ApiError::bad_request("invalid_dem_id", e))
}

fn no_such_dem(id: &DemId) -> ApiError {
    ApiError::not_found("no_such_dem", format!("This project has no DEM '{id}'."))
}

fn read_error(e: String) -> ApiError {
    ApiError::internal("dem_read_failed", e)
}

fn in_use(id: &DemId, radargrams: &[RadargramId], doing: &str) -> ApiError {
    let named: Vec<&str> = radargrams.iter().take(5).map(|r| r.as_str()).collect();
    let more = radargrams.len().saturating_sub(named.len());
    ApiError::conflict(
        "dem_in_use",
        format!(
            "'{id}' cannot be {doing} while {} radargram{} link{} to it ({}{}). {}",
            radargrams.len(),
            if radargrams.len() == 1 { "" } else { "s" },
            if radargrams.len() == 1 { "s" } else { "" },
            named.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            },
            "Add the new file as another DEM and link them to that instead.",
        ),
    )
}

/// The source's link to DEM `id`, as it is now.
pub(super) fn link_to(
    project: &crate::project::Project,
    id: &DemId,
) -> Result<SourceDem, ApiError> {
    let (dem, _) = raw::read_dem(project.documents(), id)
        .map_err(read_error)?
        .ok_or_else(|| no_such_dem(id))?;
    Ok(SourceDem {
        id: dem.id,
        file: dem.file,
    })
}

/// A DEM, as the list and the single view give it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DemView {
    #[serde(flatten)]
    pub dem: Dem,
    /// The file's size.
    pub bytes: u64,
    /// The radargrams linked to it, sorted.
    pub radargram_ids: Vec<RadargramId>,
}

fn view_of(project: &crate::project::Project, dem: Dem, users: Vec<RadargramId>) -> DemView {
    let bytes = raw::file_path(project.data_dir(), &dem.file.blake3)
        .and_then(|path| path.metadata().ok())
        .map_or(0, |meta| meta.len());
    DemView {
        dem,
        bytes,
        radargram_ids: users,
    }
}

fn users_of(project: &crate::project::Project, id: &DemId) -> Result<Vec<RadargramId>, ApiError> {
    Ok(raw::dem_users(project.documents())
        .map_err(read_error)?
        .remove(id)
        .unwrap_or_default())
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DemList {
    pub dems: Vec<DemView>,
}

/// Every DEM, with the radargrams linked to it.
#[utoipa::path(
    get,
    path = "/api/v1/dems",
    tag = "DEMs",
    responses(
        (status = 200, description = "The DEMs.", body = DemList),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
pub async fn list_dems(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<DemList>, ApiError> {
    caller.require(Role::Viewer, "see the project's DEMs")?;
    let project = readable_project(&state)?;
    let mut users = raw::dem_users(project.documents()).map_err(read_error)?;
    let dems = raw::list_dems(project.documents())
        .map_err(read_error)?
        .into_iter()
        .map(|dem| {
            let linked = users.remove(&dem.id).unwrap_or_default();
            view_of(project, dem, linked)
        })
        .collect();
    Ok(Json(DemList { dems }))
}

fn with_etag(version: &Version, view: DemView) -> Response {
    ([(header::ETAG, version.to_string())], Json(view)).into_response()
}

/// One DEM, as the list gives it, with an `ETag`.
#[utoipa::path(
    get,
    path = "/api/v1/dems/{dem}",
    tag = "DEMs",
    responses(
        (status = 200, description = "The DEM.", body = DemView),
        (status = 404, description = "No such DEM.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
pub async fn get_dem(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(dem): Path<String>,
) -> Result<Response, ApiError> {
    caller.require(Role::Viewer, "see a DEM")?;
    let project = readable_project(&state)?;
    let id = parse_dem(&dem)?;
    let (dem, version) = raw::read_dem(project.documents(), &id)
        .map_err(read_error)?
        .ok_or_else(|| no_such_dem(&id))?;
    let users = users_of(project, &id)?;
    Ok(with_etag(&version, view_of(project, dem, users)))
}

/// The body of `PUT /api/v1/dems/{dem}`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct DemBody {
    /// What the DEM is called.
    pub name: String,
    /// The file, as `POST /api/v1/raw` answered. Required for a new DEM;
    /// for an existing one, a different file replaces its own, which is
    /// refused while any radargram links to it.
    #[serde(default)]
    pub file: Option<NamedFile>,
}

/// Add a DEM from a file uploaded as raw data, which GDAL must be able to read
/// as a raster. For an existing DEM, `name` renames it and a different `file`
/// replaces its file, refused while any radargram links to it. Takes `If-
/// Match`, and `If-None-Match: *` to only create.
#[utoipa::path(
    put,
    path = "/api/v1/dems/{dem}",
    tag = "DEMs",
    request_body = DemBody,
    responses(
        (status = 200, description = "Changed.", body = DemView),
        (status = 201, description = "Added.", body = DemView),
        (status = 400, description = "`dem_unreadable`, `name_required` or `file_required`.", body = ErrorBody),
        (status = 409, description = "`dem_in_use`: its file cannot change while radargrams link to it.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
pub async fn put_dem(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(dem): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DemBody>,
) -> Result<Response, ApiError> {
    let project = project_for(&state, &caller, "change the project's DEMs")?;
    let id = parse_dem(&dem)?;
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "name_required",
            "A DEM needs a name.",
        ));
    }
    if let Some(file) = &body.file {
        // It is laid out under this name for each job that reads it.
        raw::plain_name(&file.name).map_err(|e| ApiError::bad_request("invalid_file_name", e))?;
        let path = raw::file_path(project.data_dir(), &file.blake3)
            .filter(|path| path.is_file())
            .ok_or_else(|| {
                ApiError::bad_request(
                    "not_uploaded",
                    format!(
                        "{} ({}) has not been uploaded to this project.",
                        file.name, file.blake3
                    ),
                )
            })?;
        // Checked here, once, rather than by every draft that would fail
        // on it later. GDAL runs as a child process: off the executor.
        tokio::task::spawn_blocking(move || crate::dem::check_readable(&path))
            .await
            .map_err(|e| ApiError::internal("dem_check_failed", e.to_string()))?
            .map_err(|e| {
                ApiError::bad_request(
                    "dem_unreadable",
                    format!("{} cannot be used as a DEM: {e}.", file.name),
                )
            })?;
    }

    let (view, version, created, replaced) = {
        let _links = state.dem_links();
        let existing = raw::read_dem(project.documents(), &id).map_err(read_error)?;
        let users = users_of(project, &id)?;
        let file = body.file.map(|file| HashedFile {
            name: file.name,
            blake3: file.blake3,
        });
        let dem = match (existing.as_ref(), file) {
            (None, None) => {
                return Err(ApiError::bad_request(
                    "file_required",
                    format!("'{id}' is a new DEM, so it needs a file."),
                ))
            }
            (None, Some(file)) => Dem {
                id: id.clone(),
                name,
                file,
                by: caller.audit_name(),
                at: chrono::Utc::now().to_rfc3339(),
            },
            (Some((old, _)), Some(file)) if file.blake3 != old.file.blake3 => {
                if !users.is_empty() {
                    return Err(in_use(&id, &users, "given another file"));
                }
                Dem {
                    id: id.clone(),
                    name,
                    file,
                    by: caller.audit_name(),
                    at: chrono::Utc::now().to_rfc3339(),
                }
            }
            (Some((old, _)), _) => Dem {
                name,
                ..old.clone()
            },
        };
        let replaced = existing
            .as_ref()
            .is_some_and(|(old, _)| old.file.blake3 != dem.file.blake3);
        let version = raw::write_dem(project.documents(), &dem, &expectation_from(&headers))
            .map_err(|e| store_error(&e))?;
        (
            view_of(project, dem, users),
            version,
            existing.is_none(),
            replaced,
        )
    };
    if replaced {
        // The old file may be named by nothing now.
        super::raw_routes::sweep(&state, project);
    }
    let mut response = with_etag(&version, view);
    if created {
        *response.status_mut() = StatusCode::CREATED;
    }
    Ok(response)
}

/// Delete a DEM. Refused, naming the radargrams, while any links to it. Takes
/// `If-Match`.
#[utoipa::path(
    delete,
    path = "/api/v1/dems/{dem}",
    tag = "DEMs",
    responses(
        (status = 204, description = "Deleted."),
        (status = 409, description = "`dem_in_use`.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
pub async fn delete_dem(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(dem): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let project = project_for(&state, &caller, "delete a DEM")?;
    let id = parse_dem(&dem)?;
    {
        let _links = state.dem_links();
        let users = users_of(project, &id)?;
        if !users.is_empty() {
            return Err(in_use(&id, &users, "deleted"));
        }
        let removed = raw::remove_dem(project.documents(), &id, &expectation_from(&headers))
            .map_err(|e| store_error(&e))?;
        if !removed {
            return Err(no_such_dem(&id));
        }
    }
    super::raw_routes::sweep(&state, project);
    Ok(StatusCode::NO_CONTENT)
}

/// The body of `PUT /api/v1/sources/{radargram_id}/dem`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct LinkBody {
    /// A DEM id, or `null` for none.
    pub dem: Option<String>,
}

/// Link the radargram to one of the project's DEMs, or unlink it with `null`,
/// leaving its recordings as they are. The recordings are read with the DEM, as
/// setting the source reads them. A radargram processed before the change is
/// stale afterwards. Takes `If-Match`.
#[utoipa::path(
    put,
    path = "/api/v1/sources/{radargram_id}/dem",
    tag = "Raw data",
    request_body = LinkBody,
    responses(
        (status = 200, description = "The source as saved, with its `ETag`.", body = SourceView),
        (status = 400, description = "`source_unreadable`: the DEM does not cover the recordings.", body = ErrorBody),
        (status = 404, description = "`no_source`.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
// Link a radargram's source to a DEM, or unlink it, without adding its
// raw data again. Takes `If-Match` with the source's `ETag`; without one,
// the source as it is read here is the one changed. Linking reads the
// recordings with the DEM, and answers what they say as `PUT
// /sources/{id}` does.
pub async fn link_dem(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<LinkBody>,
) -> Result<Response, ApiError> {
    let project = project_for(&state, &caller, "change a radargram's DEM")?;
    let radargram = parse_radargram(&radargram_id)?;
    let dem = body.dem.as_deref().map(parse_dem).transpose()?;

    let (mut source, version) = raw::read_source(project.documents(), &radargram)
        .map_err(|e| ApiError::internal("source_read_failed", e))?
        .ok_or_else(|| {
            ApiError::not_found(
                "no_source",
                format!("'{radargram}' has no raw data in this project."),
            )
        })?;
    let expected = match expectation_from(&headers) {
        Expectation::Any => Expectation::Version(version),
        given => given,
    };
    source.dem = dem.as_ref().map(|id| link_to(project, id)).transpose()?;

    // Read as a draft would, so a DEM that does not cover the recordings is
    // refused now rather than by the next draft.
    let recordings = match &source.dem {
        None => Vec::new(),
        Some(link) => {
            let data_dir = project.data_dir().to_path_buf();
            let named = source.recordings();
            let crs = source.crs.clone();
            let (mhz, separation) = (source.antenna_mhz, source.antenna_separation);
            let link = link.clone();
            tokio::task::spawn_blocking(move || {
                let work = raw::incoming_path(&data_dir)?.with_extension("source");
                let read =
                    read_recordings(&data_dir, &named, &work, crs, Some(&link), mhz, separation);
                let _ = std::fs::remove_dir_all(&work);
                read
            })
            .await
            .map_err(|e| ApiError::internal("source_failed", e.to_string()))?
            .map_err(|e| ApiError::bad_request("source_unreadable", e))?
        }
    };
    let version = write_with_dem(&state, project, &radargram, &source, &expected)?;
    Ok((
        [(header::ETAG, version.to_string())],
        Json(SourceView {
            radargram_id: radargram.to_string(),
            source,
            recordings,
        }),
    )
        .into_response())
}
