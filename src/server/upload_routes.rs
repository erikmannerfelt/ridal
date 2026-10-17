//! Uploads sent in chunks, which survive a dropped connection (#396).
//!
//! ```text
//! POST   /api/v1/uploads            declare {name, size}; answers an id
//! GET    /api/v1/uploads/{upload}   how much has arrived
//! PUT    /api/v1/uploads/{upload}   one chunk, where `Content-Range` says
//! DELETE /api/v1/uploads/{upload}   give up, deleting what arrived
//! ```
//!
//! A finished upload is used by naming it where a file would otherwise be
//! the request body: `POST /api/v1/raw?upload={id}`, `POST
//! /api/v1/datasets?upload={id}` and `POST
//! /api/v1/datasets/{radargram_id}/replace?upload={id}`. Each takes the
//! file exactly as it takes a body, so a file sent in chunks is checked,
//! stored and hashed the same as one sent whole, and the upload is gone
//! whatever the answer.
//!
//! A chunk must start where the last one ended. One that does not is
//! refused with the offset the server has, in the body's message and in
//! an `Upload-Offset` header, and a chunk that breaks off halfway keeps
//! what arrived: the offset is the length of the file on disk
//! ([`crate::project::uploads`]), so a client resumes by asking for it
//! and sending from there. No chunk is hashed on its own; TCP covers a
//! chunk in transit, and the file's hash, taken when it is used, is the
//! same as for a file sent whole.
//!
//! # Locks
//!
//! The lifecycle lock is held only to measure the project before a chunk
//! is written, never while it arrives: a chunk over a slow link would
//! otherwise hold up every other upload, removal and replace in the
//! project, as a whole-file upload once did (#414). What keeps two
//! requests from writing to one upload at once is [`Sending`], one entry
//! per upload, which a second request finds taken and is refused with
//! `upload_busy`.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::needs::{Needs, Who};
use super::routes::{ApiError, ErrorBody};
use crate::project::roles::Role;
use crate::project::uploads::{self, Upload};

/// The largest chunk accepted. Large enough that a few-GB file is a
/// hundred requests or so, small enough to stay under the request limits
/// of the proxies a server sits behind (Cloudflare's is 100 MB).
pub const MAX_CHUNK_BYTES: u64 = 64 * 1024 * 1024;

/// The header that carries an upload's offset, on every answer about one.
pub const UPLOAD_OFFSET: HeaderName = HeaderName::from_static("upload-offset");

/// The uploads a request is sending to or taking right now.
#[derive(Debug, Default)]
pub struct Sending(std::sync::Mutex<HashSet<String>>);

/// One upload in [`Sending`], until it is dropped.
pub(super) struct SendingGuard<'a> {
    sending: &'a Sending,
    id: String,
}

impl Sending {
    fn claim(&self, id: &str) -> Result<SendingGuard<'_>, ApiError> {
        let mut busy = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !busy.insert(id.to_string()) {
            return Err(ApiError::conflict(
                "upload_busy",
                "Another request is sending to this upload. Wait for it to finish, \
                 then ask how far the upload has got.",
            ));
        }
        Ok(SendingGuard {
            sending: self,
            id: id.to_string(),
        })
    }
}

impl Drop for SendingGuard<'_> {
    fn drop(&mut self) {
        self.sending
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
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
            "This catalog is not a Ridal project, so there is nowhere to upload \
             to. Run `ridal project init` in the directory you are serving, then \
             restart.",
        )
    })
}

fn no_such_upload() -> ApiError {
    ApiError::not_found(
        "no_such_upload",
        "There is no such upload: it was used, cancelled, or left for a day without \
         a chunk and deleted. Start the upload again.",
    )
}

/// The upload, when it exists and is the caller's. Someone else's is
/// answered as missing, so an id says nothing about whose it is.
fn caller_upload(
    project: &crate::project::Project,
    caller: &Caller,
    id: &str,
) -> Result<Upload, ApiError> {
    let upload = uploads::read(project.documents(), project.data_dir(), id)
        .map_err(|e| ApiError::internal("upload_read_failed", e))?
        .ok_or_else(no_such_upload)?;
    if upload.owner != owner(caller) {
        return Err(no_such_upload());
    }
    Ok(upload)
}

fn owner(caller: &Caller) -> Option<String> {
    caller.user.as_ref().map(|user| user.as_str().to_string())
}

fn offset_of(project: &crate::project::Project, id: &str) -> Result<u64, ApiError> {
    uploads::offset(project.data_dir(), id).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            no_such_upload()
        } else {
            ApiError::internal("upload_read_failed", e.to_string())
        }
    })
}

/// The body of `POST /api/v1/uploads`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct NewUpload {
    /// The file's name, which says what it is to its recording
    /// (`DAT_0001_A1.rd3`), and which a radargram upload uses in what it
    /// says about the file.
    pub name: String,
    /// The whole file's size in bytes.
    pub size: u64,
}

/// An upload, and how far it has got.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UploadStatus {
    pub id: String,
    pub name: String,
    /// The whole file's size in bytes, as declared.
    pub size: u64,
    /// How many bytes have arrived: where the next chunk starts. Equal to
    /// `size` once the upload can be used.
    pub offset: u64,
    /// When the upload is deleted if nothing more is sent to it, RFC 3339.
    pub expires_at: String,
}

fn status(
    project: &crate::project::Project,
    id: &str,
    upload: &Upload,
) -> Result<impl IntoResponse, ApiError> {
    let offset = offset_of(project, id)?;
    let expires_at = uploads::expires_at(project.data_dir(), id)
        .map_err(|e| ApiError::internal("upload_read_failed", e.to_string()))?;
    Ok((
        [(UPLOAD_OFFSET, offset.to_string())],
        Json(UploadStatus {
            id: id.to_string(),
            name: upload.name.clone(),
            size: upload.size,
            offset,
            expires_at: chrono::DateTime::<chrono::Utc>::from(expires_at).to_rfc3339(),
        }),
    ))
}

/// Start an upload of a file to be sent in chunks. The project's size limit is
/// checked against the declared size now, and against each chunk as it comes.
#[utoipa::path(
    post,
    path = "/api/v1/uploads",
    tag = "Uploads",
    request_body = NewUpload,
    responses(
        (status = 201, description = "Started, with nothing sent yet.", body = UploadStatus),
        (status = 400, description = "`invalid_name`.", body = ErrorBody),
        (status = 413, description = "`project_full`: the project's size limit would be exceeded.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
pub async fn create_upload(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Json(body): Json<NewUpload>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "upload a file")?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 255 || name.contains(['/', '\\']) {
        return Err(ApiError::bad_request(
            "invalid_name",
            "Give the file's own name, without a directory: it is what says what \
             the file is.",
        ));
    }
    let cap = project.max_bytes();
    let _lifecycle = state.lifecycle_lock().await;
    // Uploads left for a day are swept here, before the room is measured,
    // so the space they held counts as free. Not fatal: what is not swept
    // now is swept next time.
    if let Err(e) = uploads::sweep(project.data_dir(), uploads::MAX_IDLE) {
        tracing::warn!(error = e.as_str(), "idle uploads left unswept");
    }
    let room = cap.saturating_sub(project.size_bytes());
    if body.size > room {
        return Err(super::lifecycle_routes::too_large(body.size, room, cap));
    }
    let upload = Upload {
        name: name.to_string(),
        size: body.size,
        owner: owner(&caller),
        created: chrono::Utc::now().to_rfc3339(),
    };
    let id = uploads::create(project.documents(), project.data_dir(), &upload)
        .map_err(|e| ApiError::internal("upload_failed", e))?;
    Ok((StatusCode::CREATED, status(project, &id, &upload)?))
}

/// How far an upload has got: where the next chunk starts.
#[utoipa::path(
    get,
    path = "/api/v1/uploads/{upload}",
    tag = "Uploads",
    responses(
        (status = 200, description = "The upload, with its offset also in `Upload-Offset`.", body = UploadStatus),
        (status = 404, description = "`no_such_upload`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator)).own()))),
)]
pub async fn get_upload(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "upload a file")?;
    let upload = caller_upload(project, &caller, &id)?;
    status(project, &id, &upload)
}

/// `bytes A-B/T` as `(A, B, T)`, with `B` inclusive.
fn content_range(headers: &HeaderMap) -> Option<(u64, u64, u64)> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let (range, total) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (
        start.trim().parse().ok()?,
        end.trim().parse().ok()?,
        total.trim().parse().ok()?,
    );
    (start <= end).then_some((start, end, total))
}

/// Why a chunk did not all arrive. What did is kept either way.
enum Short {
    /// The body ended, or went on, other than where its range said.
    Mismatch(u64),
    /// The connection broke.
    Interrupted(String),
}

/// Append exactly `length` bytes of `body` to `path`, and answer how many
/// were written: `length`, or what arrived before the body was found to be
/// shorter or longer than its range, or broke off.
async fn append(
    body: axum::body::Body,
    path: &std::path::Path,
    length: u64,
) -> Result<Result<u64, (u64, Short)>, ApiError> {
    use std::io::Write;

    let failed = |e: std::io::Error| {
        ApiError::internal("upload_failed", format!("Could not write the chunk: {e}"))
    };
    let mut file = std::fs::File::options()
        .append(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                no_such_upload()
            } else {
                failed(e)
            }
        })?;
    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    let outcome = loop {
        match futures_util::StreamExt::next(&mut stream).await {
            None if written == length => break Ok(written),
            None => break Err((written, Short::Mismatch(written))),
            Some(Err(e)) => break Err((written, Short::Interrupted(e.to_string()))),
            Some(Ok(chunk)) => {
                let room = (length - written) as usize;
                let take = chunk.len().min(room);
                file.write_all(&chunk[..take]).map_err(failed)?;
                written += take as u64;
                if chunk.len() > room {
                    break Err((
                        written,
                        Short::Mismatch(written + (chunk.len() - room) as u64),
                    ));
                }
            }
        }
    };
    file.flush().map_err(failed)?;
    Ok(outcome)
}

/// Send one chunk of an upload as the request body, at the byte range
/// `Content-Range` gives (`bytes 0-33554431/5000000000`). It must start where
/// the upload has got to; one that breaks off keeps what arrived.
#[utoipa::path(
    put,
    path = "/api/v1/uploads/{upload}",
    tag = "Uploads",
    params(
        ("Content-Range" = String, Header, description = "`bytes START-END/SIZE`: where this chunk goes, `END` inclusive, and the whole file's size."),
    ),
    request_body(content = super::openapi::Chunk, content_type = "application/octet-stream", description = "The chunk."),
    responses(
        (status = 200, description = "The chunk is in, with the new offset also in `Upload-Offset`.", body = UploadStatus),
        (status = 400, description = "`invalid_content_range`, `size_mismatch`, or `range_mismatch` or `upload_interrupted`: the body was not the range it said, and what arrived of it is kept. The offset is in `Upload-Offset`.", body = ErrorBody),
        (status = 404, description = "`no_such_upload`.", body = ErrorBody),
        (status = 409, description = "`offset_mismatch`: the chunk does not start where the upload has got to, which is in `Upload-Offset`; or `upload_busy`.", body = ErrorBody),
        (status = 413, description = "`chunk_too_large`, or `project_full`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator)).own()))),
)]
pub async fn put_chunk(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "upload a file")?;
    let upload = caller_upload(project, &caller, &id)?;
    let (start, end, total) = content_range(&headers).ok_or_else(|| {
        ApiError::bad_request(
            "invalid_content_range",
            "Say where the chunk goes with `Content-Range: bytes START-END/SIZE`, \
             END inclusive.",
        )
    })?;
    if total != upload.size || end >= upload.size {
        return Err(ApiError::bad_request(
            "size_mismatch",
            format!(
                "This upload was declared as {} bytes, and that range does not fit it.",
                upload.size
            ),
        ));
    }
    let length = end - start + 1;
    if length > MAX_CHUNK_BYTES {
        return Err(ApiError::payload_too_large(
            "chunk_too_large",
            format!("Send chunks of at most {MAX_CHUNK_BYTES} bytes."),
        ));
    }
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|declared| declared != length) {
        return Err(ApiError::bad_request(
            "range_mismatch",
            "`Content-Length` and `Content-Range` disagree about the chunk's length.",
        ));
    }

    let _sending = state.uploads.claim(&id)?;
    let offset = offset_of(project, &id)?;
    if start != offset {
        return Err(ApiError::conflict(
            "offset_mismatch",
            format!("The upload has {offset} bytes, so the next chunk starts at {offset}."),
        )
        .with_header(UPLOAD_OFFSET, offset.to_string()));
    }
    {
        let _lifecycle = state.lifecycle_lock().await;
        let cap = project.max_bytes();
        let room = cap.saturating_sub(project.size_bytes());
        if length > room {
            return Err(super::lifecycle_routes::too_large(length, room, cap));
        }
    }

    let part = uploads::part_path(project.data_dir(), &id).ok_or_else(no_such_upload)?;
    let outcome = append(body, &part, length).await?;
    // Cancelled while the chunk arrived: what was written went into a file
    // that is no longer there.
    let upload = caller_upload(project, &caller, &id)?;
    let short = match outcome {
        Ok(_) => return status(project, &id, &upload),
        Err((_, short)) => short,
    };
    let offset = offset_of(project, &id)?;
    let error = match short {
        Short::Mismatch(received) => ApiError::bad_request(
            "range_mismatch",
            format!(
                "The chunk was {received} bytes, not the {length} its range said. What \
                 fitted the range is kept: the upload has {offset} bytes."
            ),
        ),
        Short::Interrupted(e) => ApiError::bad_request(
            "upload_interrupted",
            format!(
                "The chunk stopped ({e}). What arrived is kept: the upload has {offset} bytes."
            ),
        ),
    };
    Err(error.with_header(UPLOAD_OFFSET, offset.to_string()))
}

/// Give up on an upload, deleting what has arrived.
#[utoipa::path(
    delete,
    path = "/api/v1/uploads/{upload}",
    tag = "Uploads",
    responses(
        (status = 204, description = "Gone."),
        (status = 404, description = "`no_such_upload`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator)).own()))),
)]
// Not claimed through `Sending`: a cancel comes straight after the client
// abandons a chunk the server may still be reading, and should not be
// refused for it. A chunk still arriving writes into a file that is gone,
// and is answered as `no_such_upload`.
pub async fn delete_upload(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "upload a file")?;
    caller_upload(project, &caller, &id)?;
    uploads::remove(project.documents(), project.data_dir(), &id)
        .map_err(|e| ApiError::internal("upload_failed", e))?;
    Ok(StatusCode::NO_CONTENT)
}

/// A finished upload that a request is about to use in place of its body:
/// the caller's, complete, and held in [`Sending`] so that nothing else
/// sends to it or takes it meanwhile.
pub(super) struct Claimed<'a> {
    id: String,
    pub upload: Upload,
    _sending: SendingGuard<'a>,
}

/// Claim the upload a request names with `?upload=`.
///
/// Called before the request does anything else, so that an upload that
/// cannot be used is refused before the work that would follow it.
pub(super) fn claim<'a>(
    state: &'a AppState,
    project: &crate::project::Project,
    caller: &Caller,
    id: &str,
) -> Result<Claimed<'a>, ApiError> {
    let upload = caller_upload(project, caller, id)?;
    let sending = state.uploads.claim(id)?;
    let offset = offset_of(project, id)?;
    if offset != upload.size {
        return Err(ApiError::conflict(
            "upload_incomplete",
            format!(
                "The upload has {offset} of its {} bytes. Send the rest, then use it.",
                upload.size
            ),
        )
        .with_header(UPLOAD_OFFSET, offset.to_string()));
    }
    Ok(Claimed {
        id: id.to_string(),
        upload,
        _sending: sending,
    })
}

impl Claimed<'_> {
    /// Move the file to `destination`, where the request would have
    /// streamed its body, forget the upload, and answer the file's size.
    pub(super) async fn take_to(
        self,
        project: &crate::project::Project,
        destination: &std::path::Path,
    ) -> Result<u64, ApiError> {
        let data_dir = project.data_dir().to_path_buf();
        let to = destination.to_path_buf();
        let id = self.id.clone();
        // A copy where the destination is on another filesystem, which for
        // a large file is not something to do on the async runtime.
        tokio::task::spawn_blocking(move || uploads::move_part(&data_dir, &id, &to))
            .await
            .map_err(|e| ApiError::internal("upload_failed", e.to_string()))?
            .map_err(|e| ApiError::internal("upload_failed", e))?;
        uploads::remove(project.documents(), project.data_dir(), &self.id)
            .map_err(|e| ApiError::internal("upload_failed", e))?;
        Ok(self.upload.size)
    }
}

/// A request's file: the named upload, or else its body, which is written to
/// `path` stopping at `room` bytes.
pub(super) async fn receive(
    claimed: Option<Claimed<'_>>,
    project: &crate::project::Project,
    body: axum::body::Body,
    path: &std::path::Path,
    room: u64,
    cap: u64,
) -> Result<u64, ApiError> {
    match claimed {
        Some(claimed) => claimed.take_to(project, path).await,
        None => super::lifecycle_routes::stream_to_file(body, path, room, cap).await,
    }
}

/// `Content-Length`, when the request's file is its body. An upload's bytes
/// are in the project already, and were measured as they arrived.
pub(super) fn declared_length(headers: &HeaderMap, claimed: Option<&Claimed<'_>>) -> Option<u64> {
    if claimed.is_some() {
        return None;
    }
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(value: &str) -> Option<(u64, u64, u64)> {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_RANGE, value.parse().unwrap());
        content_range(&headers)
    }

    #[test]
    fn content_ranges_are_read_strictly() {
        assert_eq!(range("bytes 0-99/1000"), Some((0, 99, 1000)));
        assert_eq!(range("bytes 999-999/1000"), Some((999, 999, 1000)));
        for bad in [
            "bytes 99-0/1000",
            "bytes */1000",
            "bytes 0-99/*",
            "0-99/1000",
            "items 0-99/1000",
            "bytes -1-99/1000",
        ] {
            assert_eq!(range(bad), None, "{bad}");
        }
        assert_eq!(content_range(&HeaderMap::new()), None);
    }
}
