//! Raw files and radargram sources (#382, phase 4).
//!
//! ```text
//! POST   /api/v1/raw?filename=…          upload one file; answers its blake3
//! POST   /api/v1/raw?upload=…            store one sent in chunks (#396)
//! GET    /api/v1/raw/{blake3}            download a stored file
//! GET    /api/v1/sources                 which radargram ids have raw data
//! GET    /api/v1/sources/{radargram_id}  what a radargram is made from
//! GET    /api/v1/sources/{radargram_id}/raw.zip  its raw files, as one zip (#460)
//! PUT    /api/v1/sources/{radargram_id}  set or switch it, and read each recording
//! DELETE /api/v1/sources/{radargram_id}  forget it
//! ```
//!
//! DEMs, which a source links to by id, are [`super::dem_routes`].
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
use super::needs::{Needs, Who};
use super::openapi::NetcdfFile;
use super::routes::{ApiError, ErrorBody, OrInternal};
use crate::project::raw::{self, NamedFile, Source, SourceNames};
use crate::project::roles::{DownloadScope, Role};
use crate::project::store::{StoreError, Version};

pub(super) fn project_for<'a>(
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

pub(super) fn store_error(error: &StoreError) -> ApiError {
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

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UploadQuery {
    /// The file's name, which says what it is to its recording
    /// (`DAT_0001_A1.rd3`). Required, unless `upload` is given, whose name
    /// it is otherwise.
    pub filename: Option<String>,
    /// A finished upload sent in chunks (`POST /api/v1/uploads`), to store in
    /// place of a request body, which is then left empty.
    pub upload: Option<String>,
}

/// `POST /api/v1/raw`: the file was stored, or was already.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct StoredFile {
    pub name: String,
    pub blake3: String,
    pub bytes: u64,
}

/// Upload one raw file as the request body, or store one sent in chunks
/// (`upload`). `filename` is required for a body, since a file's name says what
/// it is to its recording (`DAT_0001_A1.rd3`). Uploading a file the project
/// already has stores nothing new.
#[utoipa::path(
    post,
    path = "/api/v1/raw",
    tag = "Raw data",
    params(UploadQuery),
    request_body(content = NetcdfFile, content_type = "application/octet-stream", description = "The file."),
    responses(
        (status = 201, description = "Stored, or already there.", body = StoredFile),
        (status = 400, description = "`filename_required`.", body = ErrorBody),
        (status = 404, description = "`no_such_upload`.", body = ErrorBody),
        (status = 409, description = "`upload_incomplete` or `upload_busy`.", body = ErrorBody),
        (status = 413, description = "`project_full`: the project's size limit would be exceeded.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
pub async fn upload_raw_file(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "upload raw data")?;
    let claimed = query
        .upload
        .as_deref()
        .map(|id| super::upload_routes::claim(&state, project, &caller, id))
        .transpose()?;
    let name = query
        .filename
        .or_else(|| claimed.as_ref().map(|c| c.upload.name.clone()))
        .ok_or_else(|| {
            ApiError::bad_request(
                "filename_required",
                "Say what the file is called with `?filename=`: its name is what tells \
             Ridal what it is to its recording.",
            )
        })?;
    // The same measure as a radargram upload, under the same lock, so that
    // the two kinds of upload cannot each see room for one file. The lock is
    // not held while the body streams (#396): a multi-GB file over a slow
    // link would otherwise hold up every other upload, removal and replace
    // in the project for as long as it takes. The room is checked again
    // under the lock before the file is installed instead.
    let cap = project.max_bytes();
    let room = {
        let _lifecycle = state.lifecycle_lock().await;
        let room = cap.saturating_sub(project.size_bytes());
        let declared = super::upload_routes::declared_length(&headers, claimed.as_ref());
        if declared.is_some_and(|declared| declared > room) {
            return Err(super::lifecycle_routes::too_large(
                declared.unwrap_or_default(),
                room,
                cap,
            ));
        }
        room
    };
    let incoming = raw::incoming_path(project.data_dir()).or_internal("upload_failed")?;
    let cleanup = super::lifecycle_routes::TempFile(incoming.clone());
    // A body stops at `room`, so a wrong Content-Length cannot overfill.
    let bytes = super::upload_routes::receive(claimed, project, body, &incoming, room, cap).await?;
    // Outside the lock: a file of several GB takes a while to hash.
    let hashed = {
        let incoming = incoming.clone();
        tokio::task::spawn_blocking(move || crate::recipe::hash_file(&incoming))
            .await
            .or_internal("upload_failed")?
            .or_internal("upload_failed")?
    };

    let lifecycle = state.lifecycle_lock().await;
    // The project's size includes this upload's own `.part` file, which is
    // what is being asked about, so it is taken out rather than counted
    // twice. Other uploads still streaming are counted: of two that each
    // fit alone, the first to finish is installed.
    let room = cap.saturating_sub(project.size_bytes().saturating_sub(bytes));
    if bytes > room {
        // Removed before the lock is let go, so the next upload to finish
        // does not count this one's file too.
        drop(cleanup);
        return Err(super::lifecycle_routes::too_large(bytes, room, cap));
    }
    let data_dir = project.data_dir().to_path_buf();
    let stored = tokio::task::spawn_blocking(move || {
        raw::install_hashed(&data_dir, &incoming, &name, &hashed.blake3)
    })
    .await
    .or_internal("upload_failed")?
    .or_internal("upload_failed")?;
    cleanup.installed();
    drop(lifecycle);
    Ok((
        StatusCode::CREATED,
        Json(StoredFile {
            name: stored.name,
            blake3: stored.blake3,
            bytes,
        }),
    ))
}

/// A stored raw file.
#[utoipa::path(
    get,
    path = "/api/v1/raw/{blake3}",
    tag = "Raw data",
    responses(
        (status = 200, description = "The file, as an attachment.", content_type = "application/octet-stream"),
        (status = 404, description = "`no_such_file`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::download(DownloadScope::All)))),
)]
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
        .or_internal("raw_open_failed")?;
    let length = file
        .metadata()
        .await
        .map(|m| m.len())
        .or_internal("raw_stat_failed")?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, length.to_string()),
        ],
        axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}

/// A file name to put in a `Content-Disposition` header: a stored name is
/// whatever an uploader called the file, and a quote or a control character
/// in it would end the header's value early.
pub(super) fn header_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '"' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Where each of a source's files goes in its zip, and where it is stored.
///
/// Under a directory named after the radargram, so that unpacking two zips
/// side by side keeps them apart. A merged source has one numbered
/// directory per recording, in merge order, as [`raw::materialize`] lays
/// them out to be read: two recordings may share file names.
fn source_zip_entries(
    data_dir: &std::path::Path,
    id: &crate::identity::RadargramId,
    source: &Source,
) -> Result<Vec<(String, std::path::PathBuf)>, ApiError> {
    let merged = source.bundles.len() > 1;
    let mut entries = Vec::new();
    for (index, bundle) in source.bundles.iter().enumerate() {
        for file in &bundle.files {
            let name = raw::plain_name(&file.file.name).or_internal("source_read_failed")?;
            let stored = raw::file_path(data_dir, &file.file.blake3)
                .filter(|path| path.is_file())
                .ok_or_else(|| {
                    ApiError::internal(
                        "raw_missing",
                        format!(
                            "{name} ({}) of '{id}' is not stored in this project.",
                            file.file.blake3
                        ),
                    )
                })?;
            let in_zip = if merged {
                format!("{id}/{}/{name}", index + 1)
            } else {
                format!("{id}/{name}")
            };
            entries.push((in_zip, stored));
        }
    }
    Ok(entries)
}

/// What a zip being written sends on: the response body, in chunks.
struct ChannelWriter {
    sender: tokio::sync::mpsc::Sender<std::io::Result<axum::body::Bytes>>,
    buffer: Vec<u8>,
}

impl ChannelWriter {
    /// Large enough that a raw file of several GB is not millions of
    /// messages, small enough that a slow reader holds back little.
    const CHUNK: usize = 256 * 1024;

    fn send(&mut self) -> std::io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(Self::CHUNK));
        // Fails only once the response is gone, as when the download is
        // cancelled, which is what stops the writing.
        self.sender
            .blocking_send(Ok(chunk.into()))
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() >= Self::CHUNK {
            self.send()?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send()
    }
}

/// Write `entries` as a zip into `writer`, each file stored as it is.
///
/// Stored, not deflated: raw radar data is mostly noise to a compressor,
/// and a file of several GB is not worth the CPU. Zip64 throughout, since
/// a stream cannot go back to widen a header once a file turns out large.
fn write_zip(
    entries: &[(String, std::path::PathBuf)],
    writer: impl std::io::Write,
) -> std::io::Result<()> {
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    let mut zip = zip::ZipWriter::new_stream(writer);
    for (name, path) in entries {
        let mut file = std::fs::File::open(path)?;
        // When the file was stored, which is when it was uploaded: what a
        // stored file has to say about its age. Left at the format's
        // 1980 where that cannot be read or put in a zip's date.
        let stored_at = file
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).naive_utc())
            .and_then(|t| {
                use chrono::{Datelike, Timelike};
                zip::DateTime::from_date_and_time(
                    u16::try_from(t.year()).ok()?,
                    t.month() as u8,
                    t.day() as u8,
                    t.hour() as u8,
                    t.minute() as u8,
                    t.second() as u8,
                )
                .ok()
            });
        let options = match stored_at {
            Some(time) => options.last_modified_time(time),
            None => options,
        };
        zip.start_file(name.as_str(), options)
            .map_err(std::io::Error::other)?;
        std::io::copy(&mut file, &mut zip)?;
    }
    let mut inner = zip.finish().map_err(std::io::Error::other)?.into_inner();
    inner.flush()
}

/// A radargram's raw files as one zip, laid out as they were uploaded: under
/// a directory named after the radargram, and with one numbered directory
/// per recording, in merge order, when there are several. The DEM it links
/// to is not included; that is `GET /api/v1/dems/{dem}/file`.
#[utoipa::path(
    get,
    path = "/api/v1/sources/{radargram_id}/raw.zip",
    tag = "Raw data",
    responses(
        (status = 200, description = "The zip, as an attachment.", content_type = "application/zip"),
        (status = 404, description = "`no_source`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::download(DownloadScope::All)))),
)]
// Streamed as it is written (#460): a merged source can be several GB, so
// neither memory nor a temporary copy on disk is the place to build it.
pub async fn download_source(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
) -> Result<Response, ApiError> {
    caller.require_download(DownloadScope::All, "raw data")?;
    let project = readable_project(&state)?;
    let id = parse_radargram(&radargram_id)?;
    let (source, _) = raw::read_source(project.documents(), &id)
        .or_internal("source_read_failed")?
        .ok_or_else(|| {
            ApiError::not_found(
                "no_source",
                format!("'{id}' has no raw data in this project."),
            )
        })?;
    // Every file is found before the response starts, so a missing one is
    // an error status rather than a zip cut short.
    let entries = source_zip_entries(project.data_dir(), &id, &source)?;

    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    tokio::task::spawn_blocking(move || {
        let mut writer = ChannelWriter {
            sender: sender.clone(),
            buffer: Vec::with_capacity(ChannelWriter::CHUNK),
        };
        if let Err(e) = write_zip(&entries, &mut writer) {
            // An error item ends the body unfinished, which the client
            // sees as a failed download rather than a short, valid zip.
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                let _ = sender.blocking_send(Err(e));
            }
        }
    });
    let body = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            super::routes::attachment(&format!("{id}-raw.zip")),
        ],
        axum::body::Body::from_stream(body),
    )
        .into_response())
}

/// `GET /api/v1/sources`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SourceList {
    /// Sorted. Served or not: raw data uploaded for a radargram that has
    /// not been processed yet is listed too.
    pub radargram_ids: Vec<String>,
    /// The names each source gives its radargram until it is served, for
    /// those that give any (#382 phase 7).
    pub names: std::collections::BTreeMap<String, SourceNames>,
}

/// The ids of every radargram the project has raw data for, whether or not it
/// has been processed yet, and the names each source gives its radargram until
/// it is served.
#[utoipa::path(
    get,
    path = "/api/v1/sources",
    tag = "Raw data",
    responses(
        (status = 200, description = "The sources.", body = SourceList),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
pub async fn list_sources(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<SourceList>, ApiError> {
    caller.require(Role::Viewer, "see which radargrams have raw data")?;
    let project = readable_project(&state)?;
    let mut radargram_ids =
        raw::source_ids(project.documents()).or_internal("source_read_failed")?;
    radargram_ids.sort();
    let mut names = std::collections::BTreeMap::new();
    for id in &radargram_ids {
        let Ok(id) = crate::identity::RadargramId::new(id) else {
            continue;
        };
        if let Some((Source { names: Some(n), .. }, _)) =
            raw::read_source(project.documents(), &id).or_internal("source_read_failed")?
        {
            names.insert(id.to_string(), n);
        }
    }
    Ok(Json(SourceList {
        radargram_ids,
        names,
    }))
}

/// `GET`/`PUT /api/v1/sources/{radargram_id}`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SourceView {
    pub radargram_id: String,
    pub source: Source,
    /// What each recording's header and positions say, in merge order.
    /// Only `PUT` reads the recordings; `GET` leaves this empty.
    pub recordings: Vec<crate::inspect::InfoRecord>,
}

/// What the radargram is made from, with an `ETag`.
#[utoipa::path(
    get,
    path = "/api/v1/sources/{radargram_id}",
    tag = "Raw data",
    responses(
        (status = 200, description = "Its source.", body = SourceView),
        (status = 404, description = "`no_source`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
pub async fn get_source(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
) -> Result<Response, ApiError> {
    caller.require(Role::Viewer, "see what a radargram is made from")?;
    let project = readable_project(&state)?;
    let id = parse_radargram(&radargram_id)?;
    let (source, version) = raw::read_source(project.documents(), &id)
        .or_internal("source_read_failed")?
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
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SourceBody {
    /// The recordings, in merge order, each the files it is made of by name
    /// and hash, as `POST /api/v1/raw` answered. Which file is the header,
    /// the data and the positions is read from the names.
    pub recordings: Vec<Vec<NamedFile>>,
    /// An EPSG code, such as `EPSG:32633`. Without it, the positions go in
    /// the UTM zone that suits them best.
    #[serde(default)]
    pub crs: Option<String>,
    /// One of the project's DEMs, by id (`GET /api/v1/dems`).
    #[serde(default)]
    pub dem: Option<String>,
    /// The antenna's frequency in MHz, overriding what the files say.
    #[serde(default)]
    pub antenna_mhz: Option<f32>,
    /// The antenna separation in m, overriding what the files say.
    #[serde(default)]
    pub antenna_separation: Option<f32>,
    /// What to call the radargram until it is served (#382 phase 7).
    /// Absent keeps what the source had; `null` removes it.
    #[serde(default, deserialize_with = "super::preference_routes::present")]
    pub names: Option<Option<SourceNames>>,
}

/// `names` as a source keeps them: a display name, and a group whose id is
/// derived from its name unless given. Every part empty is none.
pub(super) fn source_names(names: SourceNames) -> Result<Option<SourceNames>, ApiError> {
    let display_name = names
        .display_name
        .as_deref()
        .and_then(crate::identity::DisplayName::from_input);
    if names.group_id.is_some()
        && names
            .group_name
            .as_deref()
            .and_then(crate::identity::GroupName::from_input)
            .is_none()
    {
        return Err(ApiError::bad_request(
            "group_id_without_name",
            "A group id is the URL-safe form of a group's name, so give the name too.",
        ));
    }
    let group = crate::identity::resolve_group(
        names.group_name.as_deref(),
        names.group_id.as_deref(),
        None,
        None,
    )
    .map_err(|e| ApiError::bad_request("invalid_group_id", e))?;
    if display_name.is_none() && group.is_none() {
        return Ok(None);
    }
    Ok(Some(SourceNames {
        display_name: display_name.map(|n| n.to_string()),
        group_name: group.as_ref().map(|(name, _)| name.to_string()),
        group_id: group.map(|(_, id)| id.to_string()),
    }))
}

/// An antenna override: a positive number, or the files are read as
/// though it were, and every draft from them fails or comes out wrong.
pub(super) fn antenna_override(
    value: Option<f32>,
    code: &'static str,
    what: &str,
) -> Result<Option<f32>, ApiError> {
    match value {
        Some(v) if !(v.is_finite() && v > 0.) => Err(ApiError::bad_request(
            code,
            format!("The {what} must be a positive number, not {v}."),
        )),
        _ => Ok(value),
    }
}

/// A source's CRS, as an EPSG code: `EPSG:32633` ([`raw::epsg_code`]).
fn source_crs(text: &str) -> Result<String, ApiError> {
    raw::epsg_code(text).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_crs",
            format!("'{text}' is not an EPSG code; write it as `EPSG:32633`."),
        )
    })
}

/// Set or switch what the radargram is made from. Every recording is read
/// before anything is saved, and what its header and positions say is in the
/// answer. Recordings that cannot be merged into one radargram (different CRS,
/// antenna frequency, time window or antenna separation) are refused. Every
/// draft and batch made from the source uses its CRS, DEM and antenna
/// overrides, and changing them makes what was processed without them `stale`.
/// Takes `If-Match`.
#[utoipa::path(
    put,
    path = "/api/v1/sources/{radargram_id}",
    tag = "Raw data",
    request_body = SourceBody,
    responses(
        (status = 200, description = "The source as saved, with its `ETag`.", body = SourceView),
        (status = 400, description = "`recordings_not_mergeable`, `source_unreadable` (such as a DEM that does not cover the recordings, or an antenna frequency nothing gives), `invalid_antenna_mhz` or `invalid_antenna_separation`.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
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
    let antenna_mhz = antenna_override(
        body.antenna_mhz,
        "invalid_antenna_mhz",
        "antenna frequency (MHz)",
    )?;
    let antenna_separation = antenna_override(
        body.antenna_separation,
        "invalid_antenna_separation",
        "antenna separation (m)",
    )?;
    let names = match body.names {
        Some(names) => names.map(source_names).transpose()?.flatten(),
        // Kept, so that switching the recordings of a radargram not
        // processed yet does not forget what it is to be called. Compared
        // against `expected` with the rest when it is written.
        None => raw::read_source(project.documents(), &id)
            .or_internal("source_read_failed")?
            .and_then(|(source, _)| source.names),
    };
    let dem = body
        .dem
        .as_deref()
        .map(|id| super::dem_routes::link_to(project, &super::dem_routes::parse_dem(id)?))
        .transpose()?;

    // Reading recordings parses headers and projects positions, which runs
    // PROJ: off the async executor.
    let result = tokio::task::spawn_blocking(move || {
        let work = raw::incoming_path(&data_dir)?.with_extension("source");
        let outcome = (|| -> Result<_, String> {
            let bundles = raw::bundles_of(&data_dir, &body.recordings, &work)?;
            let records = read_recordings(
                &data_dir,
                &body.recordings,
                &work.join("read"),
                crs.clone(),
                dem.as_ref(),
                antenna_mhz,
                antenna_separation,
            )?;
            Ok((
                Source {
                    bundles,
                    crs: crs.clone(),
                    dem,
                    antenna_mhz,
                    antenna_separation,
                    names,
                },
                records,
            ))
        })();
        let _ = std::fs::remove_dir_all(&work);
        outcome
    })
    .await
    .or_internal("source_failed")?;
    let (source, recordings) = result.map_err(|e| ApiError::bad_request("source_unreadable", e))?;
    // Refused now rather than when the first draft fails to merge them.
    crate::inspect::check_mergeable(&recordings)
        .map_err(|e| ApiError::bad_request("recordings_not_mergeable", e))?;

    let version = write_with_dem(&state, project, &id, &source, &expected)?;
    // A switched source may leave files nothing names any more.
    sweep(&state, project);
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

/// What each of `recordings` says, read as a draft would read them: with
/// `crs`, `dem` and the antenna overrides, so a DEM that does not cover
/// them is refused here, and a frequency the files cannot give is accepted
/// when the source gives it. Lays them out under `work`, which the caller
/// removes.
pub(super) fn read_recordings(
    data_dir: &std::path::Path,
    recordings: &[Vec<NamedFile>],
    work: &std::path::Path,
    crs: Option<String>,
    dem: Option<&raw::SourceDem>,
    antenna_mhz: Option<f32>,
    antenna_separation: Option<f32>,
) -> Result<Vec<crate::inspect::InfoRecord>, String> {
    let headers: Vec<std::path::PathBuf> = raw::materialize(data_dir, recordings, work)?
        .into_iter()
        .map(|resolved| resolved.header)
        .collect();
    let records = crate::inspect::inspect(crate::inspect::InfoParams {
        filepaths: headers,
        dem_path: dem.and_then(|d| raw::file_path(data_dir, &d.file.blake3)),
        cor_path: None,
        medium_velocity: 0.168,
        crs,
        override_antenna_mhz: antenna_mhz,
        override_antenna_separation: antenna_separation,
    })
    .map_err(|e| match dem {
        Some(dem) => format!(
            "the recordings cannot be read with the DEM '{}': {e}",
            dem.id
        ),
        None => format!("the recordings cannot be read: {e}"),
    })?;
    Ok(records
        .into_iter()
        .filter_map(|record| match record {
            crate::inspect::Inspected::Raw(record) => Some(record),
            crate::inspect::Inspected::Processed(_) => None,
        })
        .collect())
}

/// Write `source`, checking first that the DEM it links to, if any, still
/// has the file it was read with. The DEM's file cannot change while a
/// source links to it, but this one may not be linked yet.
pub(super) fn write_with_dem(
    state: &AppState,
    project: &crate::project::Project,
    id: &crate::identity::RadargramId,
    source: &Source,
    expected: &crate::project::store::Expectation,
) -> Result<Version, ApiError> {
    let _links = state.dem_links();
    if let Some(dem) = &source.dem {
        if super::dem_routes::link_to(project, &dem.id)? != *dem {
            return Err(ApiError::conflict(
                "dem_changed",
                format!(
                    "The DEM '{}' was given another file while the recordings were \
                     read. Try again.",
                    dem.id
                ),
            ));
        }
    }
    raw::write_source(project.documents(), id, source, expected).map_err(|e| store_error(&e))
}

/// Forget what the radargram is made from, and take it out of its processing
/// profile. Files no other source names are deleted after an hour.
#[utoipa::path(
    delete,
    path = "/api/v1/sources/{radargram_id}",
    tag = "Raw data",
    responses(
        (status = 204, description = "Forgotten."),
        (status = 404, description = "`no_source`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
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
    // With no raw data there is nothing to process it from.
    super::lifecycle_routes::take_out_of_profile(project, &id);
    sweep(&state, project);
    Ok(StatusCode::NO_CONTENT)
}

/// Delete the raw files nothing needs any more: no source names them, and
/// no draft or batch item still being processed reads them. Not fatal: what it fails to
/// delete, the next sweep will.
pub(super) fn sweep(state: &AppState, project: &crate::project::Project) {
    let mut keep: HashSet<String> = state.drafts.in_use(state.processing.as_deref());
    keep.extend(state.batches.in_use());
    if let Err(e) = raw::sweep(
        project.documents(),
        project.data_dir(),
        &keep,
        raw::SWEEP_MIN_AGE,
    ) {
        tracing::warn!(error = e.as_str(), "raw data left unswept");
    }
}
