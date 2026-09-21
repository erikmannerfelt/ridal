//! Changing which revision sits behind a radargram id (#148, F3).
//!
//! ```text
//! POST   /api/v1/datasets/{id}/replace              stage a new revision, report
//! POST   /api/v1/datasets/{id}/replace/{token}      commit it
//! DELETE /api/v1/datasets/{id}/replace/{token}      discard it
//! ```
//!
//! `operator` and above.
//!
//! # The documents are not rewritten
//!
//! The instinct is to migrate the interpretations — re-anchor them and
//! write them back — and #148 sets out why that is the wrong shape. It
//! launders an approximation into ground truth (§8.3 forbids presenting a
//! re-anchored `y` as exact); it compounds, because a second replace
//! re-anchors the output of the first with the original gone; and it
//! cannot be undone.
//!
//! So a replace changes the *file* and nothing else. Every interpretation
//! stays exactly as drawn, against the revision it was drawn on, and
//! `source.revision_id` stops being a staleness marker and becomes what it
//! should be: a permanent statement of which coordinate space those
//! numbers live in. Showing them on the new revision is
//! [`crate::interp::carry`], derived on demand.
//!
//! That is what makes a replace reversible. The ledger entry can be
//! dropped and every document is still what somebody drew. Only the
//! superseded NetCDF is gone, and re-uploading it brings the id back to
//! where it was.
//!
//! # Two requests, because the answer arrives too late otherwise
//!
//! What a replace will do to the picks cannot be known until the new file
//! has been read, and by then it has been uploaded. So the upload stages
//! the file and answers with a report; a second request commits or
//! discards it. The alternative — commit and then report — tells the
//! operator what happened rather than what is about to, which for an
//! operation that deletes the old file is the wrong order.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::{http::header, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::routes::ApiError;
use crate::identity::{RadargramId, RevisionId};
use crate::interp::carry::{CarryReport, Severity};
use crate::io::RidalNetcdfKind;
use crate::project::revisions::{self, ledger};
use crate::project::users::Role;
use crate::project::{audit, interpretations, Project};

/// Where a staged replacement waits between the two requests, under the
/// project's data directory (#187).
///
/// Inside the project rather than the system temporary directory: the file
/// is about to be installed into `radargrams/`, and a rename across
/// filesystems is a copy that can half-finish. It also means a staged file
/// counts against the project's size cap, which is the honest accounting —
/// it is occupying the project's disk. A `[project] data_dir` pointed at
/// another filesystem gives the install a cross-device rename, which fails
/// with the system's message rather than half-succeeding.
///
/// **Dot-prefixed, and that is load-bearing.** Discovery walks the whole
/// project tree looking for `.nc` files, and a staged replacement is a
/// `.nc` file with the same radargram id as the one it would replace — so
/// without this it would be catalogued, immediately, as a duplicate of its
/// own target. `is_excluded_dir_name` skips dot-prefixed directories, and
/// a test holds that rule in place.
const STAGING_DIR: &str = ".staging";

/// How long a staged replacement is offered before it is swept.
///
/// Long enough to read a report and decide; short enough that an
/// abandoned browser tab does not leave a radargram-sized file in the
/// project forever. Swept on the next replace rather than by a timer,
/// because a background task that deletes files is a background task that
/// deletes files.
const STAGING_MAX_AGE_SECS: u64 = 6 * 60 * 60;

#[derive(Debug, Deserialize)]
pub struct ReplaceQuery {
    #[serde(default)]
    filename: Option<String>,
}

/// What replacing this radargram would do to one interpretation.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentConsequence {
    pub user: String,
    /// The carry that *would* be shown after the replace. Not applied —
    /// nothing is written to the document either now or at commit.
    #[serde(flatten)]
    pub carry: CarryReport,
}

/// How the two revisions' grids compare.
#[derive(Debug, Clone, Serialize)]
pub struct ShapeChange {
    pub from_traces: usize,
    pub from_samples: usize,
    pub to_traces: usize,
    pub to_samples: usize,
    pub changed: bool,
}

/// Everything a replace would do, before it does any of it.
#[derive(Debug, Clone, Serialize)]
pub struct ConsequenceReport {
    pub radargram_id: String,
    pub from_revision: String,
    pub to_revision: String,
    /// `true` when the incoming file fingerprints to the revision id the
    /// outgoing one already has.
    ///
    /// `RevisionId` is `hash(radargram_id + processing_datetime)` and says
    /// nothing about contents, so a file edited in another tool can carry
    /// the same datetime and produce the same id. Every cache and
    /// staleness check would then believe nothing changed. Ridal cannot
    /// produce this itself, which is exactly why it has to be reported
    /// rather than assumed away.
    pub revision_id_collision: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<ShapeChange>,
    /// Whether the outgoing revision's mapping could be kept. Without it
    /// no document drawn on it can ever be shown again (SPEC §8.1 forbids
    /// falling back to the raw index), so a replace that cannot snapshot
    /// is refused at commit.
    pub outgoing_axes_kept: bool,
    pub documents: Vec<DocumentConsequence>,
    /// The worst tier across every document, or `current` when there are
    /// none. What the dialog leads with.
    pub worst: Severity,
    pub headline: String,
}

#[derive(Debug, Serialize)]
pub struct Staged {
    token: String,
    bytes: u64,
    report: ConsequenceReport,
}

#[derive(Debug, Serialize)]
pub struct Replaced {
    radargram_id: String,
    from_revision: String,
    to_revision: String,
}

fn project_for<'a>(
    state: &'a AppState,
    caller: &Caller,
    action: &str,
) -> Result<&'a Project, ApiError> {
    caller.require(Role::Operator, action)?;
    state.project.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "not_a_project",
            "This catalog is not a Ridal project, so there is nothing to replace \
             a radargram in.",
        )
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// The staging directory, created and checked to be inside the project's
/// data directory.
///
/// Checked against the *data* directory rather than the project root,
/// because a project may put its data elsewhere entirely (`[project]
/// data_dir`). Both sides are canonicalized, so a `.staging` replaced by a
/// symlink to somewhere else resolves and is caught — which is the property
/// the check is for.
fn staging_dir(project: &Project) -> Result<std::path::PathBuf, ApiError> {
    let root = project.data_dir().canonicalize().map_err(|e| {
        ApiError::internal(
            "replace_failed",
            format!("Could not resolve the project's data directory: {e}"),
        )
    })?;
    let dir = root.join(STAGING_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| {
        ApiError::internal(
            "replace_failed",
            format!("Could not prepare {}: {e}", dir.display()),
        )
    })?;
    let resolved = dir.canonicalize().map_err(|e| {
        ApiError::internal(
            "replace_failed",
            format!("Could not resolve {}: {e}", dir.display()),
        )
    })?;
    if !resolved.starts_with(&root) {
        return Err(ApiError::conflict(
            "staging_outside_project",
            "The staging directory resolves outside the project, so the upload \
             was refused. Ridal does not write outside the project."
                .to_string(),
        ));
    }
    Ok(resolved)
}

/// Delete staged files nobody came back for.
///
/// Swept here rather than on a timer. A background task whose job is to
/// delete files in the project is a thing that can go wrong while nobody
/// is watching; doing it on the next replace means it only ever runs when
/// an operator is already staging something.
fn sweep_staging(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age.as_secs() > STAGING_MAX_AGE_SECS);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// What the operator was shown, kept beside the staged file.
///
/// A consequence report describes a *transition*: from the revision on
/// disk when the report was made, to the one in the staged file. Committing
/// without checking the first half means another replace landing in between
/// turns an approved A → C into an unapproved B → C — the operator read
/// what would happen to picks drawn on A, and B's picks get it instead.
///
/// Written next to the staged `.nc` rather than held in memory, so it
/// survives a restart exactly as the staged file does, and is swept with
/// it.
#[derive(Debug, Serialize, Deserialize)]
struct StagedFor {
    /// The revision the report was made against.
    from_revision: String,
}

fn staged_note_path(dir: &std::path::Path, token: &str) -> Result<std::path::PathBuf, ApiError> {
    Ok(staged_path(dir, token)?.with_extension("from"))
}

/// A staging token, validated as a bare slug so it can name a file.
///
/// Tokens are minted here and handed back to the browser, so this only
/// ever rejects a request that did not come from one — but it is the one
/// value in this module that reaches a path from the client, and a check
/// next to the join is worth more than a promise made elsewhere.
fn staged_path(dir: &std::path::Path, token: &str) -> Result<std::path::PathBuf, ApiError> {
    let ok = !token.is_empty()
        && token.len() <= 64
        && token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !ok {
        return Err(ApiError::bad_request(
            "invalid_staging_token",
            "That is not a staging token from this server.",
        ));
    }
    let path = dir.join(format!("{token}.nc"));
    if path.parent() != Some(dir) {
        return Err(ApiError::bad_request(
            "invalid_staging_token",
            "That is not a staging token from this server.",
        ));
    }
    Ok(path)
}

/// Build the report without changing anything.
fn consequences(
    project: &Project,
    radargram: &RadargramId,
    from_revision: &RevisionId,
    from_path: &std::path::Path,
    to_path: &std::path::Path,
    to_revision: &RevisionId,
) -> Result<ConsequenceReport, ApiError> {
    let from_declared = crate::interp::source::read_axis_declarations(from_path);
    let to_declared = crate::interp::source::read_axis_declarations(to_path);

    // The mapping of the revision that is about to go. Computed here so the
    // report can say whether the replace is even permissible: if the
    // outgoing axes cannot be described, every document drawn on them
    // becomes unshowable the moment the file is deleted.
    let outgoing_axes_kept = crate::interp::anchors::snapshot_values(&from_declared).is_some();

    let to_axes = crate::interp::anchors::axes_from_declarations(&to_declared);

    let shape = (!from_declared.time.is_empty() && !to_declared.time.is_empty()).then(|| {
        let (from_traces, to_traces) = (from_declared.time.len(), to_declared.time.len());
        let (from_samples, to_samples) = (from_declared.n_samples, to_declared.n_samples);
        ShapeChange {
            from_traces,
            from_samples,
            to_traces,
            to_samples,
            changed: from_traces != to_traces || from_samples != to_samples,
        }
    });

    // Skip and report (#213) rather than skip in silence. This one plans a
    // carry across a replacement, so a pick left out of the plan is a pick
    // that silently does not get carried.
    let (users, unreadable) =
        interpretations::list_users_checked(project.documents(), radargram)
            .map_err(|e| ApiError::internal("interpretations_read_failed", e.to_string()))?;
    for stem in &unreadable {
        tracing::warn!(
            radargram = radargram.as_str(),
            stem = stem.as_str(),
            "not carrying an interpretation: its filename is not a valid user id"
        );
    }

    let mut documents = Vec::new();
    for user_id in users {
        let Some(stored) = interpretations::read(project.documents(), radargram, &user_id)
            .map_err(|e| ApiError::internal("interpretations_read_failed", e.to_string()))?
        else {
            continue;
        };
        let carried = crate::interp::carry::carry(&stored.document, &to_axes, to_revision.as_str());
        documents.push(DocumentConsequence {
            user: user_id.as_str().to_string(),
            carry: carried.report,
        });
    }

    // Worst first: the dialog leads with the most serious thing that would
    // happen to anybody's picks, not with an average of them.
    let worst = documents
        .iter()
        .map(|d| d.carry.severity)
        .max_by_key(|s| match s {
            Severity::Current => 0,
            Severity::Carried => 1,
            Severity::Approximate => 2,
            Severity::Partial => 3,
            Severity::Refused => 4,
        })
        .unwrap_or(Severity::Current);

    let revision_id_collision = from_revision == to_revision;

    let headline = if revision_id_collision {
        "The new file has the same processing date as the current one, so Ridal \
         cannot tell them apart: both produce the same revision id, and every \
         cache and staleness check will believe nothing changed. Reprocess it so \
         it gets its own date."
            .to_string()
    } else if !outgoing_axes_kept && !documents.is_empty() {
        "The current revision does not describe its axes, so its mapping cannot be \
         kept. Replacing it would leave every pick drawn on it impossible to place \
         on anything ever again."
            .to_string()
    } else if documents.is_empty() {
        "Nobody has interpreted this radargram, so replacing it affects no picks.".to_string()
    } else {
        match worst {
            Severity::Refused => format!(
                "{} of picks cannot be shown on the new revision at all — there is no \
                 anchor axis both revisions share. They are kept exactly as drawn, but \
                 nothing will draw them.",
                // Only the refused ones. `documents.len()` said every set
                // was affected whenever any one of them was, which
                // overstates the damage on the line the dialog leads with.
                plural(
                    documents
                        .iter()
                        .filter(|d| d.carry.severity == Severity::Refused)
                        .count(),
                    "set",
                    "sets"
                ),
            ),
            Severity::Partial => "Some picks fall outside the new revision and will not be shown. \
                 Nothing is deleted: they stay as drawn, against the revision they \
                 were drawn on."
                .to_string(),
            Severity::Approximate => {
                "Every pick can be shown on the new revision, but they move. Nothing \
                 is rewritten — what is stored stays exactly as drawn."
                    .to_string()
            }
            _ => "Every pick carries onto the new revision without visibly moving. \
                  Nothing is rewritten."
                .to_string(),
        }
    };

    Ok(ConsequenceReport {
        radargram_id: radargram.to_string(),
        from_revision: from_revision.to_string(),
        to_revision: to_revision.to_string(),
        revision_id_collision,
        shape,
        outgoing_axes_kept,
        documents,
        worst,
        headline,
    })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// `POST /api/v1/datasets/{id}/replace` — stage a new revision and report.
///
/// Nothing is installed. The upload lands in `staging/`, is validated, and
/// is measured against every interpretation of this id; the answer says
/// what committing would do.
pub async fn stage_replacement(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
    Query(query): Query<ReplaceQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "replace a radargram")?;
    let _lifecycle = state.lifecycle_lock().await;

    let catalog = state.catalog();
    let entry = super::routes::lookup_dataset(&catalog, &radargram_id)?;
    let radargram = entry.radargram_id.clone();
    let from_revision = entry.revision_id.clone();
    if !state.is_writable(entry) {
        return Err(ApiError::conflict(
            "not_in_project",
            format!(
                "'{radargram}' lives outside the project, which Ridal never writes to. \
                 Replace it where it is, or add a copy to the project first."
            ),
        ));
    }
    let from_path = state
        .absolute_path(entry)
        .map_err(|e| ApiError::internal("path_resolve_failed", e))?;
    drop(catalog);

    let dir = staging_dir(project)?;
    sweep_staging(&dir);

    let cap = project.max_bytes();
    let room = cap.saturating_sub(project.size_bytes());
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|d| d > room) {
        return Err(super::lifecycle_routes::too_large(
            declared.unwrap_or_default(),
            room,
            cap,
        ));
    }

    let token = super::lifecycle_routes::uuid();
    let staged = staged_path(&dir, &token)?;
    let cleanup = super::lifecycle_routes::TempFile(staged.clone());
    let bytes = super::lifecycle_routes::stream_to_file(body, &staged, room, cap).await?;

    let inspection = crate::io::inspect_ridal_netcdf(&staged).map_err(|e| {
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
                    "{} is a NetCDF file but not one Ridal processed.",
                    query.filename.as_deref().unwrap_or("the upload")
                ),
            ));
        }
    };

    // A replacement has to be a replacement *of this radargram*. Without
    // this, replacing `line-01` with a file whose id is `line-02` installs
    // it at `line-01.nc`, and the catalog then disagrees with the file
    // about what it is.
    if meta.radargram_id != radargram {
        return Err(ApiError::conflict(
            "wrong_radargram",
            format!(
                "That file is '{}', not '{radargram}'. Replacing a radargram needs a \
                 new revision of the same one — process it with \
                 `--radargram-id {radargram}` if it really is this line.",
                meta.radargram_id
            ),
        ));
    }

    let to_revision = RevisionId::fingerprint_v1(&meta.radargram_id, &meta.processing_datetime);
    let report = consequences(
        project,
        &radargram,
        &from_revision,
        &from_path,
        &staged,
        &to_revision,
    )?;

    // Which revision this report describes, so the commit can refuse if
    // the radargram has moved on since.
    let note = serde_json::to_string(&StagedFor {
        from_revision: from_revision.to_string(),
    })
    .map_err(|e| ApiError::internal("replace_failed", e.to_string()))?;
    std::fs::write(staged_note_path(&dir, &token)?, note).map_err(|e| {
        ApiError::internal(
            "replace_failed",
            format!("Could not record what this replacement was measured against: {e}"),
        )
    })?;

    // Kept for the commit, so the guard must not remove it.
    cleanup.installed();

    Ok((
        StatusCode::OK,
        Json(Staged {
            token,
            bytes,
            report,
        }),
    ))
}

/// `POST /api/v1/datasets/{id}/replace/{token}` — do it.
pub async fn commit_replacement(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path((radargram_id, token)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "replace a radargram")?;
    let _lifecycle = state.lifecycle_lock().await;

    let catalog = state.catalog();
    let entry = super::routes::lookup_dataset(&catalog, &radargram_id)?;
    let radargram = entry.radargram_id.clone();
    let from_revision = entry.revision_id.clone();
    if !state.is_writable(entry) {
        return Err(ApiError::conflict(
            "not_in_project",
            format!("'{radargram}' lives outside the project, which Ridal never writes to."),
        ));
    }
    let from_path = state
        .absolute_path(entry)
        .map_err(|e| ApiError::internal("path_resolve_failed", e))?;
    drop(catalog);

    let dir = staging_dir(project)?;
    let staged = staged_path(&dir, &token)?;
    if !staged.is_file() {
        return Err(ApiError::not_found(
            "staging_expired",
            "That staged replacement is no longer here. Staged files are swept after \
             a few hours; upload it again.",
        ));
    }
    let cleanup = super::lifecycle_routes::TempFile(staged.clone());

    // The report the operator read described a transition *from* a
    // particular revision. If another replace has landed since, this is a
    // different transition and they have not seen what it would do.
    let note_path = staged_note_path(&dir, &token)?;
    let measured_against: Option<StagedFor> = std::fs::read_to_string(&note_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());
    match measured_against {
        Some(note) if note.from_revision == from_revision.to_string() => {}
        Some(note) => {
            return Err(ApiError::conflict(
                "report_is_stale",
                format!(
                    "'{radargram}' was measured against revision {} and is now on {}, so \
                     something replaced it while this one was staged. What you were shown \
                     describes a change that is no longer the one that would happen. \
                     Upload it again to see the current answer.",
                    note.from_revision, from_revision
                ),
            ))
        }
        None => {
            return Err(ApiError::conflict(
                "report_is_stale",
                "There is no record of what this staged replacement was measured \
                 against, so it cannot be committed. Upload it again.",
            ))
        }
    }

    let RidalNetcdfKind::Supported(meta) = crate::io::inspect_ridal_netcdf(&staged)
        .map_err(|e| ApiError::internal("staging_unreadable", e.to_string()))?
    else {
        return Err(ApiError::internal(
            "staging_unreadable",
            "The staged file is no longer a Ridal radargram.",
        ));
    };
    if meta.radargram_id != radargram {
        return Err(ApiError::conflict(
            "wrong_radargram",
            format!(
                "That staged file is '{}', not '{radargram}'.",
                meta.radargram_id
            ),
        ));
    }
    let to_revision = RevisionId::fingerprint_v1(&meta.radargram_id, &meta.processing_datetime);

    // Refused here, not only in the dialog. The browser disables the button
    // for both of these, but a disabled button is a courtesy and this
    // request deletes a file: anything that can be checked on the way in
    // has to be checked on the way in.
    if to_revision == from_revision {
        return Err(ApiError::conflict(
            "revision_id_collision",
            format!(
                "The new file has the same processing date as the current one, so both                  produce revision {to_revision}. Ridal could not tell them apart                  afterwards, and every cache and staleness check would believe nothing                  had changed. Reprocess it so it gets its own date."
            ),
        ));
    }

    let at = now();

    // The outgoing mapping, before the file carrying it is gone. This is
    // the step that buys the right to delete it: a few hundred bytes stand
    // in for the only part of the old revision anyone still needs, and
    // without them every pick drawn on it becomes unplaceable — §8.1 does
    // not permit falling back to the raw index.
    //
    // Fatal, unlike the audit log. A radargram that genuinely declares no
    // axes has no mapping to lose and proceeds; one that declares them and
    // cannot have them written does not.
    let from_declared = crate::interp::source::read_axis_declarations(&from_path);
    if let Some(values) = crate::interp::anchors::snapshot_values(&from_declared) {
        let snapshot = revisions::AxisSnapshot {
            radargram_id: radargram.to_string(),
            revision_id: from_revision.to_string(),
            y_anchor: values.y_anchor,
            y_values: values.y_values,
            x_values: values.x_values,
            y_alternate: values.y_alternate,
        };
        let checksum = snapshot.checksum();
        revisions::put(project.documents(), &radargram, &snapshot).map_err(|e| {
            ApiError::internal(
                "snapshot_failed",
                format!(
                    "'{radargram}' declares axes but they could not be kept: {e}. The \
                     replace was refused, because installing the new revision now \
                     would leave every pick drawn on the old one unplaceable."
                ),
            )
        })?;
        if let Err(e) = ledger::update(project.documents(), |l| {
            ledger::note_current(
                l,
                radargram.as_str(),
                from_revision.as_str(),
                Some(checksum.clone()),
            );
        }) {
            eprintln!("Warning: could not record the axis checksum of '{radargram}': {e}");
        }
    } else {
        // No mapping to keep. Harmless when nobody has drawn on it, and
        // not at all harmless when somebody has: the file about to be
        // deleted is the only thing that relates their coordinates to
        // anything, and §8.1 does not permit falling back to the raw
        // index. So the replace stops, and the operator can export the
        // picks or reprocess the current revision so it declares its axes.
        let picked = interpretations::list_users(project.documents(), &radargram)
            .map_err(|e| ApiError::internal("interpretations_read_failed", e.to_string()))?;
        if !picked.is_empty() {
            return Err(ApiError::conflict(
                "no_outgoing_axes",
                format!(
                    "'{radargram}' does not describe its axes, so the mapping for the                      current version cannot be kept — and {} of picks were drawn on it.                      Replacing it now would leave them impossible to place on anything,                      ever. Download them first, or reprocess the current version with a                      Ridal that records its axes.",
                    plural(picked.len(), "set", "sets"),
                ),
            ));
        }
        eprintln!(
            "Note: '{radargram}' does not declare its axes, so no snapshot was kept. \
             Nobody has interpreted it, so there is nothing to carry."
        );
    }

    // Let go of the handle before the rename lands on it: the render
    // service holds the outgoing file open, and Windows refuses to replace
    // a file that is still open.
    state.close_radargram(radargram.as_str());
    std::fs::rename(&staged, &from_path).map_err(|e| {
        ApiError::internal(
            "install_failed",
            format!("Could not install the new revision: {e}"),
        )
    })?;
    cleanup.installed();
    let _ = std::fs::remove_file(&note_path);

    // The interpretations are deliberately untouched. #148: a document
    // stays as drawn, against the revision it was drawn on, and is carried
    // on read. Rewriting here would launder an approximation into ground
    // truth, compound on the next replace, and make this irreversible.
    if let Err(e) = ledger::update(project.documents(), |l| {
        ledger::supersede(
            l,
            radargram.as_str(),
            from_revision.as_str(),
            Some(to_revision.as_str()),
            &at,
        );
    }) {
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
            action: audit::Action::Replaced,
            radargram_id: radargram.to_string(),
            revision_id: Some(to_revision.to_string()),
            note: Some(format!("superseded {from_revision}")),
        },
    );

    Ok(Json(Replaced {
        radargram_id: radargram.to_string(),
        from_revision: from_revision.to_string(),
        to_revision: to_revision.to_string(),
    }))
}

/// `DELETE /api/v1/datasets/{id}/replace/{token}` — change your mind.
pub async fn discard_replacement(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path((_radargram_id, token)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "replace a radargram")?;
    // The same lock staging and committing take. Without it a discard can
    // remove the staged path after the commit has validated it and before
    // the rename, turning a commit that was going to work into
    // `install_failed` -- and the browser sends a discard on `pagehide`,
    // so the two really can interleave.
    let _lifecycle = state.lifecycle_lock().await;
    let dir = staging_dir(project)?;
    let staged = staged_path(&dir, &token)?;
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_file(staged_note_path(&dir, &token)?);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn consequence(severities: &[Severity]) -> ConsequenceReport {
        let documents = severities
            .iter()
            .enumerate()
            .map(|(i, severity)| DocumentConsequence {
                user: format!("user-{i}"),
                carry: CarryReport {
                    severity: *severity,
                    from_revision: Some("rev-a".to_string()),
                    to_revision: "rev-b".to_string(),
                    x_anchor: None,
                    y_anchor: None,
                    kept: 0,
                    dropped: Vec::new(),
                    moved: None,
                    refusal: None,
                    headline: String::new(),
                },
            })
            .collect();
        ConsequenceReport {
            radargram_id: "line-01".to_string(),
            from_revision: "rev-a".to_string(),
            to_revision: "rev-b".to_string(),
            revision_id_collision: false,
            shape: None,
            outgoing_axes_kept: true,
            documents,
            worst: Severity::Refused,
            headline: String::new(),
        }
    }

    #[test]
    fn the_headline_counts_only_the_sets_that_cannot_be_shown() {
        // It reported `documents.len()`, so one refused set among four
        // said all four were affected -- on the line the dialog leads
        // with, about the most serious thing that would happen.
        let report = consequence(&[
            Severity::Refused,
            Severity::Carried,
            Severity::Approximate,
            Severity::Partial,
        ]);
        let refused = report
            .documents
            .iter()
            .filter(|d| d.carry.severity == Severity::Refused)
            .count();
        assert_eq!(refused, 1);
        assert_eq!(plural(refused, "set", "sets"), "1 set");
        assert_eq!(
            plural(report.documents.len(), "set", "sets"),
            "4 sets",
            "which is what it used to say"
        );
    }
}
