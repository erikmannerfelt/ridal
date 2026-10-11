//! Raw files and radargram sources (#382, phase 4).
//!
//! ```text
//! POST   /api/v1/raw?filename=…          upload one file; answers its blake3
//! GET    /api/v1/raw/{blake3}            download a stored file
//! GET    /api/v1/sources/{radargram_id}  what a radargram is made from
//! PUT    /api/v1/sources/{radargram_id}  set or switch it, and read each recording
//! DELETE /api/v1/sources/{radargram_id}  forget it
//! ```
//!
//! A source is keyed by radargram id, not by a served radargram, so one can
//! be set for an id that has no processed file yet: that is where server
//! processing starts. The files themselves live in [`crate::project::raw`].

use std::collections::HashSet;
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
use crate::project::raw::{self, NamedFile, Source};
use crate::project::roles::{DownloadScope, Role};
use crate::project::store::{StoreError, Version};

fn project_for<'a>(
    state: &'a AppState,
    caller: &Caller,
    action: &str,
) -> Result<&'a crate::project::Project, ApiError> {
    caller.require(Role::Operator, action)?;
    state.project.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "not_a_project",
            "This catalog is not a Ridal project, so there is nowhere to keep raw \
             data. Run `ridal project init` in the directory you are serving, then \
             restart.",
        )
    })
}

fn store_error(error: &StoreError) -> ApiError {
    match error {
        StoreError::Conflict { .. } => {
            ApiError::precondition_failed("version_conflict", error.to_string())
        }
        _ => ApiError::internal("store_failed", error.to_string()),
    }
}

fn with_etag(version: &Version) -> [(header::HeaderName, String); 1] {
    [(header::ETAG, version.to_string())]
}

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    /// The file's name, which says what it is to its recording
    /// (`DAT_0001_A1.rd3`). Required.
    pub filename: Option<String>,
}

/// `POST /api/v1/raw`: the file was stored, or was already.
#[derive(Debug, Serialize)]
pub struct StoredFile {
    pub name: String,
    pub blake3: String,
    pub bytes: u64,
}

pub async fn upload_raw_file(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "upload raw data")?;
    let name = query.filename.ok_or_else(|| {
        ApiError::bad_request(
            "filename_required",
            "Say what the file is called with `?filename=`: its name is what tells \
             Ridal what it is to its recording.",
        )
    })?;
    // The same measure as a radargram upload, under the same lock, so that
    // the two kinds of upload cannot each see room for one file.
    let _lifecycle = state.lifecycle_lock().await;
    let cap = project.max_bytes();
    let room = cap.saturating_sub(project.size_bytes());
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|declared| declared > room) {
        return Err(super::lifecycle_routes::too_large(
            declared.unwrap_or_default(),
            room,
            cap,
        ));
    }
    let incoming = raw::incoming_path(project.data_dir())
        .map_err(|e| ApiError::internal("upload_failed", e))?;
    let cleanup = super::lifecycle_routes::TempFile(incoming.clone());
    let bytes = super::lifecycle_routes::stream_to_file(body, &incoming, room, cap).await?;
    let data_dir = project.data_dir().to_path_buf();
    let stored = tokio::task::spawn_blocking(move || raw::install(&data_dir, &incoming, &name))
        .await
        .map_err(|e| ApiError::internal("upload_failed", e.to_string()))?
        .map_err(|e| ApiError::internal("upload_failed", e))?;
    cleanup.installed();
    Ok((
        StatusCode::CREATED,
        Json(StoredFile {
            name: stored.name,
            blake3: stored.blake3,
            bytes,
        }),
    ))
}

pub async fn download_raw_file(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(hash): Path<String>,
) -> Result<Response, ApiError> {
    // Raw data is taken away under the same scope as the processed file.
    caller.require_download(DownloadScope::All, "raw data")?;
    let project = readable_project(&state)?;
    let path = raw::file_path(project.data_dir(), &hash)
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            ApiError::not_found("no_such_file", "This project has no file with that hash.")
        })?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| ApiError::internal("raw_open_failed", e.to_string()))?;
    let length = file
        .metadata()
        .await
        .map(|m| m.len())
        .map_err(|e| ApiError::internal("raw_stat_failed", e.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, length.to_string()),
        ],
        axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}

/// `GET`/`PUT /api/v1/sources/{radargram_id}`.
#[derive(Debug, Serialize)]
pub struct SourceView {
    pub radargram_id: String,
    pub source: Source,
    /// What each recording's header and positions say, in merge order.
    /// Only `PUT` reads the recordings; `GET` leaves this empty.
    pub recordings: Vec<crate::gpr::InfoRecord>,
}

pub async fn get_source(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
) -> Result<Response, ApiError> {
    caller.require(Role::Viewer, "see what a radargram is made from")?;
    let project = readable_project(&state)?;
    let id = parse_radargram(&radargram_id)?;
    let (source, version) = raw::read_source(project.documents(), &id)
        .map_err(|e| ApiError::internal("source_read_failed", e))?
        .ok_or_else(|| {
            ApiError::not_found(
                "no_source",
                format!("'{id}' has no raw data in this project."),
            )
        })?;
    Ok((
        with_etag(&version),
        Json(SourceView {
            radargram_id: id.to_string(),
            source,
            recordings: Vec::new(),
        }),
    )
        .into_response())
}

/// The body of `PUT /api/v1/sources/{radargram_id}`.
#[derive(Debug, Deserialize)]
pub struct SourceBody {
    /// The recordings, in merge order, each the files it is made of by name
    /// and hash, as `POST /api/v1/raw` answered. Which file is the header,
    /// the data and the positions is read from the names.
    pub recordings: Vec<Vec<NamedFile>>,
    #[serde(default)]
    pub crs: Option<String>,
    #[serde(default)]
    pub dem: Option<NamedFile>,
}

/// A source's CRS, as an EPSG code: `EPSG:32633`.
///
/// Rebuilt from the number rather than passed on, because it ends up on
/// the command line of the PROJ tools. The other forms `ridal process
/// --crs` takes are for the command line, where the person typing them is
/// the one running the tools.
fn source_crs(text: &str) -> Result<String, ApiError> {
    let trimmed = text.trim();
    let code = trimmed
        .get(..5)
        .filter(|prefix| prefix.eq_ignore_ascii_case("epsg:"))
        .and_then(|_| trimmed[5..].parse::<u32>().ok())
        .filter(|code| *code > 0);
    code.map(|code| format!("EPSG:{code}")).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_crs",
            format!("'{text}' is not an EPSG code; write it as `EPSG:32633`."),
        )
    })
}

pub async fn put_source(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SourceBody>,
) -> Result<Response, ApiError> {
    let project = project_for(&state, &caller, "set what a radargram is made from")?;
    let id = parse_radargram(&radargram_id)?;
    if body.recordings.is_empty() {
        return Err(ApiError::bad_request(
            "no_recordings",
            "A source needs at least one recording.",
        ));
    }
    let expected = expectation_from(&headers);
    let data_dir = project.data_dir().to_path_buf();
    let crs = body.crs.as_deref().map(source_crs).transpose()?;

    // Reading recordings parses headers and projects positions, which runs
    // PROJ: off the async executor.
    let result = tokio::task::spawn_blocking(move || {
        let work = raw::incoming_path(&data_dir)?.with_extension("source");
        let outcome = (|| {
            let bundles = raw::bundles_of(&data_dir, &body.recordings, &work)?;
            let dem = match &body.dem {
                None => None,
                Some(dem) => {
                    if !raw::contains(&data_dir, &dem.blake3) {
                        return Err(format!(
                            "{} ({}) has not been uploaded to this project",
                            dem.name, dem.blake3
                        ));
                    }
                    Some(crate::recipe::HashedFile {
                        name: dem.name.clone(),
                        blake3: dem.blake3.clone(),
                    })
                }
            };
            let headers: Vec<std::path::PathBuf> =
                raw::materialize(&data_dir, &body.recordings, &work.join("read"))?
                    .into_iter()
                    .map(|resolved| resolved.header)
                    .collect();
            let records = crate::gpr::inspect(crate::gpr::InfoParams {
                filepaths: headers,
                dem_path: dem
                    .as_ref()
                    .and_then(|d| raw::file_path(&data_dir, &d.blake3)),
                cor_path: None,
                medium_velocity: 0.168,
                crs: crs.clone(),
                override_antenna_mhz: None,
                override_antenna_separation: None,
            })
            .map_err(|e| format!("the recordings cannot be read: {e}"))?
            .into_iter()
            .filter_map(|record| match record {
                crate::gpr::Inspected::Raw(record) => Some(record),
                crate::gpr::Inspected::Processed(_) => None,
            })
            .collect::<Vec<_>>();
            Ok((
                Source {
                    bundles,
                    crs: crs.clone(),
                    dem,
                },
                records,
            ))
        })();
        let _ = std::fs::remove_dir_all(&work);
        outcome
    })
    .await
    .map_err(|e| ApiError::internal("source_failed", e.to_string()))?;
    let (source, recordings) = result.map_err(|e| ApiError::bad_request("source_unreadable", e))?;

    let version = raw::write_source(project.documents(), &id, &source, &expected)
        .map_err(|e| store_error(&e))?;
    // A switched source may leave files nothing names any more.
    sweep(project);
    Ok((
        with_etag(&version),
        Json(SourceView {
            radargram_id: id.to_string(),
            source,
            recordings,
        }),
    )
        .into_response())
}

pub async fn delete_source(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "remove a radargram's raw data")?;
    let id = parse_radargram(&radargram_id)?;
    let removed = raw::remove_source(project.documents(), &id, &expectation_from(&headers))
        .map_err(|e| store_error(&e))?;
    if !removed {
        return Err(ApiError::not_found(
            "no_source",
            format!("'{id}' has no raw data in this project."),
        ));
    }
    sweep(project);
    Ok(StatusCode::NO_CONTENT)
}

/// Delete the raw files nothing needs any more. Not fatal: what it fails to
/// delete, the next sweep will.
pub(super) fn sweep(project: &crate::project::Project) {
    if let Err(e) = raw::sweep(
        project.documents(),
        project.data_dir(),
        &HashSet::new(),
        raw::SWEEP_MIN_AGE,
    ) {
        tracing::warn!(error = e.as_str(), "raw data left unswept");
    }
}
