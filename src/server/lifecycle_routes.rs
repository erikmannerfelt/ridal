//! Adding and removing radargrams while the server runs (#147).
//!
//! ```text
//! POST   /api/v1/datasets?filename=...   upload a processed .nc
//! DELETE /api/v1/datasets/{id}           remove it, or ignore it
//! POST   /api/v1/datasets/{id}/restore   lift an ignore
//! GET    /api/v1/catalog/ignored         what is ignored, and what is vestigial
//! ```
//!
//! `operator` and above throughout.
//!
//! # Remove means two different things
//!
//! A radargram in the project is a file Ridal owns and can delete. One in an
//! external root is not: Ridal never writes outside the project, so the most
//! it can do is stop serving it. Both are offered through the same route
//! because from the operator's side the intent is the same — "I do not want
//! this in the catalog" — and the response says which happened.
//!
//! # Nothing authored is destroyed
//!
//! Removing a radargram archives its interpretations rather than deleting
//! them. The hazard is not wasted space: it is that a *different* file
//! arrives later under the same id and orphaned picks silently reattach to
//! data they were never drawn on.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::header, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::routes::ApiError;
use crate::identity::RadargramId;
use crate::io::RidalNetcdfKind;
use crate::project::users::Role;
use crate::project::{audit, interpretations, overrides};

/// How much of an upload is read before the size cap is consulted again.
///
/// The cap is checked against `Content-Length` first, but that header is a
/// claim rather than a fact — a client can under-report it or omit it. So
/// the running total is checked as the body arrives, and this is how often.
const CHUNK_CHECK_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    /// What the file was called on the client. A display hint for error
    /// messages only — **never** used to build a path. The file lands at a
    /// name derived from the radargram id inside it.
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Added {
    radargram_id: String,
    revision_id: String,
    display_name: Option<String>,
    group_name: Option<String>,
    bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Removed {
    radargram_id: String,
    /// `removed` when the file was deleted, `ignored` when it lives in a
    /// read-only root and was only dropped from the catalog.
    outcome: &'static str,
    /// How many interpretations were archived. Zero is worth reporting: it
    /// is the difference between "nobody had picked this" and "your picks
    /// are somewhere".
    archived: usize,
}

#[derive(Debug, Serialize)]
pub struct IgnoredListing {
    ignored: Vec<IgnoredItem>,
    /// Ignore decisions with nothing to act on, because the radargram is
    /// not anywhere Ridal currently looks.
    vestigial: Vec<IgnoredItem>,
}

#[derive(Debug, Serialize)]
pub struct IgnoredItem {
    radargram_id: String,
    since: Option<String>,
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
            "This catalog is not a Ridal project, so there is nowhere to put a \
             radargram. Run `ridal project init` in the directory you are serving, \
             then restart.",
        )
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// `POST /api/v1/datasets` — install a processed radargram.
///
/// Streamed to a temporary file inside the destination directory and
/// validated before anything is installed, so a rejected upload leaves the
/// catalog exactly as it was. The rename at the end is the only moment the
/// project changes, and it is atomic.
pub async fn upload_dataset(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Query(query): Query<UploadQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "add a radargram")?;
    // Held until the catalog has been rebuilt. Measuring the project,
    // checking for a colliding id and installing the file are one decision,
    // and two uploads interleaving them each see room for one file and no
    // collision, then the second rename replaces the first.
    let _lifecycle = state.lifecycle_lock().await;
    let destination = writable_destination(project)?;

    let cap = project.max_bytes();
    let used = project.size_bytes();
    let room = cap.saturating_sub(used);

    // Refused before a byte is written where the client says how big it is.
    // A `Content-Length` that would not fit never needs a temporary file.
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|declared| declared > room) {
        return Err(too_large(declared.unwrap_or_default(), room, cap));
    }

    let temporary = destination.join(format!("upload-{}.tmp.nc", uuid()));
    // Before the transfer rather than after it. The guard already covered
    // an interrupted upload, because the `?` came a line later -- but only
    // incidentally, and a reordering that looks harmless would leave a
    // `.tmp.nc` in the radargram directory, where it would be discovered
    // as a radargram. Constructing it first makes the property structural.
    let cleanup = TempFile(temporary.clone());
    let bytes = stream_to_file(body, &temporary, room, cap).await?;

    let inspection = crate::io::inspect_ridal_netcdf(&temporary).map_err(|e| {
        ApiError::bad_request(
            "not_a_radargram",
            format!(
                "{} is not readable as a NetCDF file: {e}",
                query.filename.as_deref().unwrap_or("the upload")
            ),
        )
    })?;
    let RidalNetcdfKind::Supported(meta) = inspection else {
        return Err(ApiError::bad_request(
            "not_a_ridal_radargram",
            format!(
                "{} is a NetCDF file but not one Ridal processed: it has no \
                 radargram id. Process it with `ridal process` first.",
                query.filename.as_deref().unwrap_or("the upload")
            ),
        ));
    };

    // Refused here rather than left to become a duplicate-id warning after
    // the fact. The operator is standing right there and can rename it.
    if state
        .catalog()
        .find_entry(meta.radargram_id.as_str())
        .is_some()
    {
        return Err(ApiError::conflict(
            "radargram_exists",
            format!(
                "This catalog already has a radargram called '{}'. Give the new one \
                 a different id with `ridal process --radargram-id`, or remove the \
                 existing one first.",
                meta.radargram_id
            ),
        ));
    }

    // An ignored id is not a free id. `find_entry` only sees what is
    // served, so without this the upload was accepted, installed, and then
    // hidden again by the very ignore that made the id look available --
    // 201 Created for a radargram that never appeared.
    let (stored, _) = overrides::read(project.documents())
        .map_err(|e| ApiError::internal("overrides_read_failed", e.to_string()))?;
    if stored.ignored.contains_key(&meta.radargram_id) {
        return Err(ApiError::conflict(
            "radargram_ignored",
            format!(
                "'{}' is on this project's ignore list, so adding it would install a \
                 file the catalog then hides. Restore that id first if this is meant \
                 to replace it.",
                meta.radargram_id
            ),
        ));
    }

    let installed = destination.join(format!("{}.nc", meta.radargram_id));
    std::fs::rename(&temporary, &installed).map_err(|e| {
        ApiError::internal(
            "install_failed",
            format!("Could not install {}: {e}", installed.display()),
        )
    })?;
    cleanup.installed();

    state
        .rediscover()
        .map_err(|e| ApiError::internal("rediscover_failed", e))?;

    audit::record(
        project.documents(),
        audit::Entry {
            at: now(),
            user: caller.display_name().to_string(),
            action: audit::Action::Added,
            radargram_id: meta.radargram_id.to_string(),
            revision_id: Some(
                crate::identity::RevisionId::fingerprint_v1(
                    &meta.radargram_id,
                    &meta.processing_datetime,
                )
                .to_string(),
            ),
            note: query.filename.clone(),
        },
    );

    Ok((
        StatusCode::CREATED,
        Json(Added {
            radargram_id: meta.radargram_id.to_string(),
            revision_id: crate::identity::RevisionId::fingerprint_v1(
                &meta.radargram_id,
                &meta.processing_datetime,
            )
            .to_string(),
            display_name: meta.display_name.map(|n| n.to_string()),
            group_name: meta.group_name.map(|n| n.to_string()),
            bytes,
        }),
    ))
}

/// `DELETE /api/v1/datasets/{id}` — remove it, or stop serving it.
pub async fn remove_dataset(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "remove a radargram")?;
    let _lifecycle = state.lifecycle_lock().await;
    let catalog = state.catalog();
    let entry = super::routes::lookup_dataset(&catalog, &radargram_id)?;
    let id = entry.radargram_id.clone();
    let revision = entry.revision_id.to_string();
    let in_project = state.is_writable(entry);
    let path = state
        .absolute_path(entry)
        .map_err(|e| ApiError::internal("path_resolve_failed", e))?;
    drop(catalog);

    let at = now();
    // Archived before the file goes. If the deletion fails afterwards the
    // catalog still has the radargram and the picks are one directory over,
    // which is recoverable; the other order could lose them outright.
    let archived = interpretations::archive_all(project.documents(), &id, &at)
        .map_err(|e| ApiError::internal("archive_failed", e.to_string()))?;

    let outcome = if in_project {
        // Let go of the file before unlinking it. Windows refuses to
        // delete a file that is still open, and the served radargram's
        // render service holds a NetCDF handle on this one -- so removing
        // an ordinary radargram would fail there and nowhere else.
        state.close_radargram(id.as_str());
        std::fs::remove_file(&path).map_err(|e| {
            ApiError::internal("remove_failed", format!("Could not remove the file: {e}"))
        })?;
        "removed"
    } else {
        // Outside the project, so the file is not Ridal's to delete. The
        // decision is recorded instead, against the id: if that file is
        // later replaced with different content it stays ignored, because
        // something deliberately not shown should not become something
        // nobody remembers not showing.
        overrides::update(project.documents(), |stored| {
            stored.ignored.insert(
                id.clone(),
                overrides::IgnoredRadargram {
                    since: Some(at.clone()),
                    revision_id: Some(revision.clone()),
                },
            );
            Ok(())
        })
        .map_err(|e| ApiError::internal("overrides_write_failed", e.to_string()))?;
        "ignored"
    };

    state
        .rediscover()
        .map_err(|e| ApiError::internal("rediscover_failed", e))?;

    audit::record(
        project.documents(),
        audit::Entry {
            at,
            user: caller.display_name().to_string(),
            action: if in_project {
                audit::Action::Removed
            } else {
                audit::Action::Ignored
            },
            radargram_id: id.to_string(),
            revision_id: Some(revision),
            note: (archived > 0).then(|| format!("{archived} interpretation(s) archived")),
        },
    );

    Ok(Json(Removed {
        radargram_id: id.to_string(),
        outcome,
        archived,
    }))
}

/// `POST /api/v1/datasets/{id}/restore` — lift an ignore.
///
/// Only ever undoes an *ignore*. A radargram whose file was deleted is not
/// restorable from here, and saying so is better than a button that looks
/// like undo and is not.
pub async fn restore_dataset(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "restore a radargram")?;
    let _lifecycle = state.lifecycle_lock().await;
    let id = RadargramId::new(&radargram_id)
        .map_err(|e| ApiError::bad_request("invalid_radargram_id", e))?;

    let lifted = overrides::update(project.documents(), |stored| {
        Ok(stored.ignored.remove(&id).is_some())
    })
    .map_err(|e| ApiError::internal("overrides_write_failed", e.to_string()))?;

    if !lifted {
        return Err(ApiError::not_found(
            "not_ignored",
            format!("'{radargram_id}' is not ignored, so there is nothing to restore."),
        ));
    }

    state
        .rediscover()
        .map_err(|e| ApiError::internal("rediscover_failed", e))?;

    audit::record(
        project.documents(),
        audit::Entry {
            at: now(),
            user: caller.display_name().to_string(),
            action: audit::Action::Unignored,
            radargram_id: id.to_string(),
            revision_id: None,
            note: None,
        },
    );

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/catalog/ignored` — what the project is not serving.
pub async fn list_ignored(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "see what is not being served")?;
    let (stored, _) = overrides::read(project.documents())
        .map_err(|e| ApiError::internal("overrides_read_failed", e.to_string()))?;
    let catalog = state.catalog();
    let vestigial: Vec<String> = catalog
        .vestigial_ignores()
        .iter()
        .map(|v| v.radargram_id.to_string())
        .collect();

    let (vestigial_items, ignored_items) = stored
        .ignored
        .iter()
        .map(|(id, entry)| IgnoredItem {
            radargram_id: id.to_string(),
            since: entry.since.clone(),
        })
        .partition(|item| vestigial.contains(&item.radargram_id));

    Ok(Json(IgnoredListing {
        ignored: ignored_items,
        vestigial: vestigial_items,
    }))
}

/// The project's radargram directory, checked to be inside the project.
///
/// Both sides are canonicalized, so a `radargrams` replaced by a symlink to
/// somewhere else resolves and is caught. Without this, `create_dir_all`
/// and `rename` follow the link and the upload lands outside the project —
/// the one thing #147 says never happens.
fn writable_destination(project: &crate::project::Project) -> Result<std::path::PathBuf, ApiError> {
    let destination = project.root().join(crate::project::DEFAULT_RADARGRAM_DIR);
    std::fs::create_dir_all(&destination).map_err(|e| {
        ApiError::internal(
            "upload_failed",
            format!("Could not prepare {}: {e}", destination.display()),
        )
    })?;
    let resolved = destination.canonicalize().map_err(|e| {
        ApiError::internal(
            "upload_failed",
            format!("Could not resolve {}: {e}", destination.display()),
        )
    })?;
    let root = project.root().canonicalize().map_err(|e| {
        ApiError::internal(
            "upload_failed",
            format!("Could not resolve the project root: {e}"),
        )
    })?;
    if !resolved.starts_with(&root) {
        return Err(ApiError::conflict(
            "destination_outside_project",
            format!(
                "{} resolves to {}, which is outside the project. Ridal does not \
                 write outside the project, so this upload was refused.",
                destination.display(),
                resolved.display()
            ),
        ));
    }
    Ok(resolved)
}

fn too_large(needed: u64, room: u64, cap: u64) -> ApiError {
    ApiError::payload_too_large(
        "project_full",
        format!(
            "That radargram is {} and the project has {} left of its {} limit. \
             Remove something, or raise the limit with `max_bytes` under \
             [radargrams] in ridal.toml.",
            human(needed),
            human(room),
            human(cap)
        ),
    )
}

fn human(bytes: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1024 * 1024 * 1024, "GB"),
        (1024 * 1024, "MB"),
        (1024, "kB"),
        (1, "bytes"),
    ];
    for (scale, unit) in UNITS {
        if bytes >= scale {
            return if scale == 1 {
                format!("{bytes} {unit}")
            } else {
                format!("{:.1} {unit}", bytes as f64 / scale as f64)
            };
        }
    }
    "0 bytes".to_string()
}

/// Removes its path on drop unless [`TempFile::installed`] was called.
///
/// An upload has half a dozen ways to be refused after the file exists —
/// unreadable, not a Ridal radargram, colliding id — and each one used to
/// need its own cleanup. Tying it to the scope means a new refusal cannot
/// forget.
struct TempFile(std::path::PathBuf);

impl TempFile {
    fn installed(mut self) {
        self.0.clear();
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.0.as_os_str().is_empty() {
            return;
        }
        let _ = std::fs::remove_file(&self.0);
    }
}

fn uuid() -> String {
    // Enough to not collide between two concurrent uploads, which is all
    // this needs: the file is renamed to its real name within the request.
    format!(
        "{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    )
}

/// Stream `body` into `path`, refusing once `room` bytes have been written.
///
/// The running total is what actually enforces the cap. `Content-Length` is
/// checked first because refusing before writing anything is better, but it
/// is a claim: a client can under-report it or omit it entirely, and a
/// chunked upload has none at all.
async fn stream_to_file(
    body: axum::body::Body,
    path: &std::path::Path,
    room: u64,
    cap: u64,
) -> Result<u64, ApiError> {
    use std::io::Write;

    let mut file = std::fs::File::create(path).map_err(|e| {
        ApiError::internal(
            "upload_failed",
            format!("Could not open {}: {e}", path.display()),
        )
    })?;

    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    let mut since_check = 0u64;
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        let chunk = chunk.map_err(|e| {
            ApiError::bad_request("upload_interrupted", format!("The upload stopped: {e}"))
        })?;
        written += chunk.len() as u64;
        since_check += chunk.len() as u64;
        if written > room && since_check >= CHUNK_CHECK_BYTES.min(written) {
            return Err(too_large(written, room, cap));
        }
        if since_check >= CHUNK_CHECK_BYTES {
            since_check = 0;
        }
        file.write_all(&chunk).map_err(|e| {
            ApiError::internal("upload_failed", format!("Could not write the upload: {e}"))
        })?;
    }
    if written > room {
        return Err(too_large(written, room, cap));
    }
    file.flush().map_err(|e| {
        ApiError::internal("upload_failed", format!("Could not finish the upload: {e}"))
    })?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(human(0), "0 bytes");
        assert_eq!(human(512), "512 bytes");
        assert_eq!(human(2048), "2.0 kB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn a_temporary_file_removes_itself_unless_it_was_installed() {
        // Six ways to refuse an upload after the file exists, and each one
        // used to need its own cleanup.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upload.tmp.nc");
        std::fs::write(&path, b"partial").unwrap();
        {
            let _cleanup = TempFile(path.clone());
        }
        assert!(!path.exists(), "a refused upload leaves nothing behind");

        std::fs::write(&path, b"complete").unwrap();
        TempFile(path.clone()).installed();
        assert!(path.exists(), "an installed one stays");
    }
}
