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
use crate::project::revisions::{self, ledger};
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
    /// How many interpretations sit in the archive under this id, from an
    /// earlier removal. Almost always zero. When it is not, the operator has
    /// just reused an id that someone drew picks on, and only they know
    /// whether this is the same line coming back or a different one taking
    /// its name.
    archived_interpretations: usize,
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
    let meta = match inspection {
        RidalNetcdfKind::Supported(meta) => meta,
        RidalNetcdfKind::Legacy(version) => {
            return Err(ApiError::bad_request(
                "ridal_file_too_old",
                format!(
                    "{} was {}",
                    query.filename.as_deref().unwrap_or("the upload"),
                    crate::io::legacy_reason(&version)
                ),
            ));
        }
        RidalNetcdfKind::NotRidal => {
            return Err(ApiError::bad_request(
                "not_a_ridal_radargram",
                format!(
                    "{} is a NetCDF file but not one Ridal processed: it has no \
                     radargram id. Process it with `ridal process` first.",
                    query.filename.as_deref().unwrap_or("the upload")
                ),
            ));
        }
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

    // Read before the install, so a failure here refuses the add rather than
    // leaving it installed with the report missing. Not a reason to refuse
    // the id: picks in the archive are not attached to anything and nothing
    // reattaches them on its own. It is a reason to say so, which the
    // response and the log both do.
    let archived_interpretations =
        interpretations::count_archived(project.documents(), &meta.radargram_id)
            .map_err(|e| ApiError::internal("archive_read_failed", e.to_string()))?;

    // The name comes out of the uploaded file, so this is the one place a
    // path is built from something the client controls. `RadargramId` is a
    // validated slug -- lowercase ASCII, digits, '-' and '_', and nothing
    // else -- so it cannot hold a separator or a '.', and traversal is not
    // reachable. Checked again anyway, because "safe because of a type
    // defined in another module" is not a property you want a path sink to
    // depend on, and CodeQL cannot see through the newtype to agree.
    let installed = destination.join(format!("{}.nc", meta.radargram_id));
    if installed.parent() != Some(destination.as_path()) {
        return Err(ApiError::bad_request(
            "invalid_radargram_id",
            format!(
                "'{}' does not name a file inside the radargram directory.",
                meta.radargram_id
            ),
        ));
    }
    std::fs::rename(&temporary, &installed).map_err(|e| {
        ApiError::internal(
            "install_failed",
            format!("Could not install {}: {e}", installed.display()),
        )
    })?;
    cleanup.installed();

    // The id now has a current revision, and the ledger has to say so. Two
    // things depend on it: `/revisions` can only report what is current if
    // something records it, and the axis checksum has to be on file
    // *before* a second file with the same processing datetime arrives, or
    // the collision it exists to detect has nothing to be detected against.
    //
    // `note_current_again` rather than `note_current`, because this id may
    // have been removed before. Re-uploading the same revision makes it
    // current once more, and a record still marked superseded would
    // contradict the catalog that is serving it.
    let revision_id =
        crate::identity::RevisionId::fingerprint_v1(&meta.radargram_id, &meta.processing_datetime)
            .to_string();
    let baseline = crate::interp::anchors::snapshot_values(
        &crate::interp::source::read_axis_declarations(&installed),
    )
    .map(|values| {
        revisions::AxisSnapshot {
            radargram_id: meta.radargram_id.to_string(),
            revision_id: revision_id.clone(),
            y_anchor: values.y_anchor,
            y_values: values.y_values,
            x_values: values.x_values,
            y_alternate: values.y_alternate,
        }
        .checksum()
    });
    if let Err(e) = ledger::update(project.documents(), |l| {
        ledger::note_current_again(
            l,
            meta.radargram_id.as_str(),
            &revision_id,
            baseline.clone(),
        );
    }) {
        // The file is installed. Refusing now would report a failure that
        // did not happen, the same reason the audit log does not fail its
        // own operation.
        eprintln!(
            "Warning: could not record the revision of '{}': {e}",
            meta.radargram_id
        );
    }

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
            note: match (&query.filename, archived_interpretations) {
                (name, 0) => name.clone(),
                (Some(name), n) => Some(format!(
                    "{name}; {n} archived interpretation(s) already under this id"
                )),
                (None, n) => Some(format!(
                    "{n} archived interpretation(s) already under this id"
                )),
            },
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
            archived_interpretations,
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

    // The axes go first, while the file is still there to read them from.
    //
    // A removal is a supersession with nothing on the other side (#148), and
    // the same rule applies: take the snapshot unconditionally. Nothing
    // needs to reference this revision *yet* for the snapshot to matter --
    // someone with the viewer open has not saved, and their `PUT` arrives
    // after the file is gone.
    //
    // Fatal only when the file is about to be deleted. An ignore leaves it
    // where it is and can be lifted, so there is nothing irreversible to
    // protect and refusing would be refusing for no gain.
    match snapshot_axes(project, &id, &path, &revision) {
        Ok(()) => {}
        Err(problem) if in_project => {
            return Err(ApiError::internal(
                "snapshot_failed",
                format!(
                    "{problem}. The removal was refused, because deleting the file \
                     now would lose the only copy of the mapping that carries its \
                     interpretations forward."
                ),
            ))
        }
        Err(problem) => eprintln!("Warning: {problem}"),
    }

    // Archived before the file goes. If the deletion fails afterwards the
    // catalog still has the radargram and the picks are one directory over,
    // which is recoverable; the other order could lose them outright.
    let archive = interpretations::archive_all(project.documents(), &id, &at)
        .map_err(|e| ApiError::internal("archive_failed", e.to_string()))?;
    let archived = archive.moved;
    // Left in place because the filename is not a valid user id (#213). Said
    // out loud, because the point of archiving is that no orphan stays behind
    // to reattach to a different radargram under the same id.
    for stem in &archive.skipped {
        tracing::warn!(
            radargram = id.as_str(),
            stem = stem.as_str(),
            "interpretation left unarchived: its filename is not a valid user id"
        );
    }

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

    // Recorded only now, because until this point the removal could still
    // fail. `remove_file` and the ignore-list write each return an error
    // that leaves the radargram served -- and a supersession written
    // before them would have `/revisions` reporting, permanently, that a
    // radargram everyone can still see has no current revision.
    if let Err(e) = ledger::update(project.documents(), |l| {
        ledger::supersede(l, id.as_str(), &revision, None, &at);
    }) {
        // Not fatal, for the same reason the audit log is not: the removal
        // has happened, and a failure to write the history is not a reason
        // to report that it did not.
        eprintln!("Warning: could not record the supersession: {e}");
    }

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

    // Whatever is back is current again. The ignore marked its revision
    // superseded, and leaving that standing would have the history say a
    // radargram is gone while the catalog serves it. Read from the
    // rebuilt catalog rather than the ignore record, because the file may
    // have been reprocessed while it was out of sight -- in which case what
    // came back is a *different* revision, and marking the old one current
    // would be the wrong correction.
    if let Some(entry) = state.catalog().find_entry(id.as_str()) {
        let revision = entry.revision_id.to_string();
        if let Err(e) = ledger::update(project.documents(), |l| {
            ledger::note_current_again(l, id.as_str(), &revision, None);
        }) {
            eprintln!("Warning: could not record the restoration of '{id}': {e}");
        }
    }

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
/// Which directory that is comes from `[radargrams] roots` rather than
/// being the built-in name (#187): a project declares where its radargrams
/// live, and an upload that landed somewhere else would sit outside every
/// directory the project says to scan. `Project::relative_upload_dir`
/// returns it relative to the root, so the join below still starts from a
/// resolved path.
///
/// Both sides are canonicalized, so a `radargrams` replaced by a symlink to
/// somewhere else resolves and is caught. Without this, `create_dir_all`
/// and `rename` follow the link and the upload lands outside the project —
/// the one thing #147 says never happens.
///
/// The root is canonicalized *before* anything is joined onto it, so every
/// path here descends from a resolved one. The order is not only tidiness:
/// the project root traces back to the `ridal gui <PATH>` argument, and
/// CodeQL reads a `create_dir_all` on a path joined onto an unresolved
/// value as a path-injection sink however the root got there. The same
/// join-then-canonicalize sequence, in the same function as the sink, is
/// what it recognises as validated — see `AppState::resolve_absolute_path`,
/// which documents the same thing for the same reason.
fn writable_destination(project: &crate::project::Project) -> Result<std::path::PathBuf, ApiError> {
    let root = project.root().canonicalize().map_err(|e| {
        ApiError::internal(
            "upload_failed",
            format!("Could not resolve the project root: {e}"),
        )
    })?;
    let destination = root.join(project.relative_upload_dir());
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

/// Keep this revision's axes before its file becomes unreachable.
///
/// # Why a failure here can refuse the removal
///
/// Two outcomes look alike and are not. A radargram that **does not declare
/// its axes** has no mapping to keep: there is nothing to lose, the removal
/// proceeds, and the note says so. A radargram that declares them and
/// cannot have them **written** — a full disk, a read-only
/// `revisions/` — is about to have the only copy of that mapping deleted,
/// and with it the ability to carry any document drawn on it onto a later
/// revision. That is not a record of the thing; it is part of the thing.
///
/// So this reports which happened, and the caller decides. A removal that
/// deletes the file aborts; an ignore, which leaves the file where it is
/// and can be lifted again, does not.
fn snapshot_axes(
    project: &crate::project::Project,
    id: &RadargramId,
    path: &std::path::Path,
    revision: &str,
) -> Result<(), String> {
    let declared = crate::interp::source::read_axis_declarations(path);

    // The axes have to belong to the revision they are about to be filed
    // under. `revision` came from the catalog snapshot and these values
    // come from the file as it is right now; if something rewrote it in
    // place since discovery, storing them would pair one revision's
    // mapping with another's id -- which is the exact cross-revision
    // mistake snapshots exist to prevent, committed by the snapshot.
    //
    // The reader is lenient by design, so a file that cannot be opened at
    // all returns defaults and would otherwise look like a radargram that
    // simply declares nothing. Requiring the datetime to match tells those
    // two apart: no datetime means the file did not read.
    let same_revision = declared.processing_datetime.as_deref().is_some_and(|when| {
        crate::identity::RevisionId::fingerprint_v1(id, when).as_str() == revision
    });
    if !same_revision {
        return Err(format!(
            "'{id}' on disk is not the revision the catalog has ({revision}); it was \
             changed or became unreadable since it was discovered, so its axes cannot \
             be kept under that id"
        ));
    }

    let Some(values) = crate::interp::anchors::snapshot_values(&declared) else {
        eprintln!(
            "Note: '{id}' does not declare its axes, so no snapshot was kept. \
             Interpretations drawn on it cannot be carried onto a later revision."
        );
        return Ok(());
    };
    let snapshot = revisions::AxisSnapshot {
        radargram_id: id.to_string(),
        revision_id: revision.to_string(),
        y_anchor: values.y_anchor,
        y_values: values.y_values,
        x_values: values.x_values,
        y_alternate: values.y_alternate,
    };
    let checksum = snapshot.checksum();
    revisions::put(project.documents(), id, &snapshot)
        .map_err(|e| format!("'{id}' declares axes but they could not be kept: {e}"))?;
    // The ledger is a record *of* the snapshot, which is now safely on
    // disk, so a failure here is the audit log's kind of failure rather
    // than the snapshot's: worth saying, not worth refusing over.
    if let Err(e) = ledger::update(project.documents(), |l| {
        ledger::note_current(l, id.as_str(), revision, Some(checksum.clone()));
    }) {
        eprintln!("Warning: could not record the axis checksum of '{id}': {e}");
    }
    Ok(())
}

pub(super) fn too_large(needed: u64, room: u64, cap: u64) -> ApiError {
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
pub(super) struct TempFile(pub(super) std::path::PathBuf);

impl TempFile {
    pub(super) fn installed(mut self) {
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

pub(super) fn uuid() -> String {
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
pub(super) async fn stream_to_file(
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

/// One revision a radargram has been through.
#[derive(Debug, Serialize)]
pub struct RevisionSummary {
    revision_id: String,
    /// Whether it is the one currently behind the id.
    current: bool,
    superseded_at: Option<String>,
    superseded_by: Option<String>,
    /// Whether this revision's axes were kept, which is what decides
    /// whether picks drawn on it can be carried onto a later one.
    has_axes: bool,
    /// Sizes from the snapshot, so the history says what changed between
    /// revisions without opening anything.
    n_traces: Option<usize>,
    n_samples: Option<usize>,
    y_anchor: Option<String>,
}

/// `GET /api/v1/datasets/{id}/revisions` — what this id has been.
///
/// Reads the ledger rather than the files: the whole point of #148 is that
/// a superseded revision's file may be gone while its mapping is not.
///
/// `viewer` and above. Which revisions a radargram has had is the same kind
/// of fact as its processing date -- provenance about data people are
/// already being shown, not an operator's working notes.
pub async fn list_revisions(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(Role::Viewer, "read a radargram's history")?;
    let project = state.project.as_ref().ok_or_else(|| {
        ApiError::not_found(
            "not_a_project",
            "This catalog is not a Ridal project, so it keeps no revision history.",
        )
    })?;
    let id = RadargramId::new(&radargram_id)
        .map_err(|e| ApiError::bad_request("invalid_radargram_id", e))?;

    let (history, _) = ledger::read(project.documents())
        .map_err(|e| ApiError::internal("ledger_read_failed", e.to_string()))?;
    let current = history.current(id.as_str()).map(|r| r.revision_id.clone());
    let records = history
        .radargrams
        .get(id.as_str())
        .cloned()
        .unwrap_or_default();

    let summaries = records
        .into_iter()
        .map(|record| {
            // Opened per revision rather than listed once: the header is a
            // few hundred bytes and a history is a handful of revisions, so
            // the simpler shape costs nothing worth saving.
            let snapshot = revisions::get(project.documents(), &id, &record.revision_id)
                .ok()
                .flatten();
            RevisionSummary {
                // From the ledger's own definition rather than re-derived
                // here, so there is one answer to "which is current".
                current: current.as_deref() == Some(record.revision_id.as_str()),
                superseded_at: record.superseded_at,
                superseded_by: record.superseded_by,
                has_axes: snapshot.is_some(),
                n_traces: snapshot.as_ref().map(|s| s.n_traces()),
                n_samples: snapshot.as_ref().map(|s| s.n_samples()),
                y_anchor: snapshot.and_then(|s| s.y_anchor),
                revision_id: record.revision_id,
            }
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({ "revisions": summaries })))
}
