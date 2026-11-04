//! Changing a radargram's id, as three steps that each stand alone (#500).
//!
//! ```text
//! POST /api/v1/datasets/{id}/copy       the same radargram under another id
//! POST /api/v1/datasets/{id}/transfer   move its picks onto another radargram
//! GET  /api/v1/datasets/{id}/activity   who is working on it now
//! ```
//!
//! An id is never changed in place: it is hashed into the revision id and
//! keys every file that belongs to the radargram (see
//! [`crate::project::transfer`]). The catalog's "Change id" makes a new
//! radargram under the new id, transfers the picks onto it and removes the
//! old one, which archives anything left behind.
//!
//! # Unsaved work
//!
//! Only saved picks can be transferred. A picker open on the old radargram
//! holds its edits in the browser until it saves, so before the removal the
//! page asks [`get_activity`] who has saved recently (`date_modified`, which
//! the picker stamps on every save and a transfer keeps; not the file's
//! time, which the transfer itself renews) and who opened the viewer recently
//! ([`RecentlyOpened`], the only sign of someone who has started picking but
//! not saved), and scales its confirmation to the answer. A save that
//! arrives after the removal is refused with a message naming the new id
//! (`interp_routes::put_interpretation`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::parse_radargram;
use super::lifecycle_routes::Removed;
use super::needs::{Needs, Who};
use super::routes::{entry_path, ApiError, ErrorBody};
use crate::identity::{RadargramId, UserId};
use crate::project::roles::Role;
use crate::project::{audit, transfer, Project};

/// How long an opened viewer counts as someone working on a radargram.
const OPENED_FOR: chrono::TimeDelta = chrono::TimeDelta::hours(24);

/// Who opened which radargram's viewer, and when last (#500).
///
/// In memory: it answers "is anyone picking on this right now", and a
/// restart closes every connection that question is about anyway. Only
/// people who may pick are recorded.
#[derive(Debug, Default)]
pub struct RecentlyOpened(Mutex<HashMap<String, HashMap<String, chrono::DateTime<chrono::Utc>>>>);

impl RecentlyOpened {
    /// Note that `user` opened `radargram` now, and forget anything older
    /// than [`OPENED_FOR`], so the map holds one day of viewers at most.
    pub fn note(&self, radargram: &str, user: &UserId) {
        let now = chrono::Utc::now();
        let mut opened = self.0.lock().unwrap_or_else(|e| e.into_inner());
        for users in opened.values_mut() {
            users.retain(|_, at| now - *at < OPENED_FOR);
        }
        opened.retain(|_, users| !users.is_empty());
        opened
            .entry(radargram.to_string())
            .or_default()
            .insert(user.as_str().to_string(), now);
    }

    /// Who opened `radargram` within [`OPENED_FOR`], and when.
    pub fn of(&self, radargram: &str) -> HashMap<String, chrono::DateTime<chrono::Utc>> {
        let now = chrono::Utc::now();
        let opened = self.0.lock().unwrap_or_else(|e| e.into_inner());
        opened
            .get(radargram)
            .map(|users| {
                users
                    .iter()
                    .filter(|(_, at)| now - **at < OPENED_FOR)
                    .map(|(user, at)| (user.clone(), *at))
                    .collect()
            })
            .unwrap_or_default()
    }
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
            "This catalog is not a Ridal project, so it has no picks to move.",
        )
    })
}

/// `POST /api/v1/datasets/{radargram_id}/copy`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct CopyRequest {
    /// The new radargram's id. It must not be in use or ignored.
    to: String,
}

/// `POST /api/v1/datasets/{radargram_id}/copy`: the new radargram.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct Copied {
    radargram_id: String,
    revision_id: String,
    /// The radargram it is a copy of.
    from: String,
    /// The size of the new file.
    bytes: u64,
    /// What was copied besides the file: `catalog_settings` (display name,
    /// group and the rest of the catalog dialog), `raw_source` and
    /// `processing_profile`, where the original had them.
    copied: Vec<String>,
    /// What could not be copied. The radargram is added either way.
    warnings: Vec<String>,
}

/// Add a copy of this radargram under another id: the same processed file
/// with only its id changed, with its catalog settings, raw data and
/// processing profile. Its picks stay where they are; see `transfer`.
#[utoipa::path(
    post,
    path = "/api/v1/datasets/{radargram_id}/copy",
    tag = "Radargrams",
    request_body = CopyRequest,
    responses(
        (status = 201, description = "The copy is in the project.", body = Copied),
        (status = 400, description = "`invalid_radargram_id`.", body = ErrorBody),
        (status = 404, description = "No such radargram.", body = ErrorBody),
        (status = 409, description = "`radargram_exists` or `radargram_ignored`: the new id is taken.", body = ErrorBody),
        (status = 413, description = "`project_full`: the project's size limit would be exceeded.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
// The first of a rename's three steps (#500). A copy rather than a
// reprocess, because raw data are often not attached to a processed
// radargram, and the grids of a copy are identical by construction, so its
// picks move across as current. `ridal_radargram_id` is the only place the
// id is written in the file, and the revision id follows from it.
//
// Installed exactly as an upload is (`lifecycle_routes::install_locked`),
// under the lifecycle lock: the same collision, ignore and size checks,
// ledger record and audit entry.
pub(super) async fn copy_dataset(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
    Json(request): Json<CopyRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "copy a radargram")?;
    let to = parse_radargram(&request.to)?;
    let destination = super::lifecycle_routes::writable_destination(project)?;

    let _lifecycle = state.lifecycle_lock().await;
    let (from, source) = {
        let catalog = state.catalog();
        let entry = super::routes::lookup_dataset(&catalog, &radargram_id)?;
        (entry.radargram_id.clone(), entry_path(&state, entry)?)
    };
    // Checked before a byte is copied, and in words about a new id rather
    // than the upload's advice on `ridal process`, which `install_locked`
    // would give: the person is typing an id into a dialog.
    if state.catalog().find_entry(to.as_str()).is_some() {
        return Err(ApiError::conflict(
            "radargram_exists",
            format!("'{to}' is already the id of another radargram here. Choose another id."),
        ));
    }
    let (stored, _) = crate::project::overrides::read(project.documents())
        .map_err(|e| ApiError::internal("overrides_read_failed", e.to_string()))?;
    if stored.ignored.contains_key(&to) {
        return Err(ApiError::conflict(
            "radargram_ignored",
            format!(
                "'{to}' belongs to a radargram this project no longer serves (see Ignored \
                 radargrams). Choose another id, or restore that radargram and remove it first."
            ),
        ));
    }
    let bytes = std::fs::metadata(&source)
        .map_err(|e| ApiError::internal("read_failed", format!("Could not read the file: {e}")))?
        .len();
    let cap = project.max_bytes();
    let room = cap.saturating_sub(project.size_bytes());
    if bytes > room {
        return Err(super::lifecycle_routes::too_large(bytes, room, cap));
    }

    // Discovery skips `upload-*.tmp.nc`, as for an upload.
    let temporary = destination.join(format!("upload-{}.tmp.nc", super::lifecycle_routes::uuid()));
    let cleanup = super::lifecycle_routes::TempFile(temporary.clone());
    let meta = {
        let (source, temporary, to) = (source.clone(), temporary.clone(), to.clone());
        tokio::task::spawn_blocking(move || copy_with_id(&source, &temporary, &to))
            .await
            .map_err(|e| ApiError::internal("copy_failed", e.to_string()))??
    };
    let added = super::lifecycle_routes::install_locked(
        &state,
        project,
        &caller,
        &temporary,
        meta,
        bytes,
        Some(&format!("copied from {from}")),
    )?;
    cleanup.installed();

    let (copied, warnings) = copy_settings(&state, project, &from, &to);
    Ok((
        axum::http::StatusCode::CREATED,
        Json(Copied {
            radargram_id: added.radargram_id().to_string(),
            revision_id: added.revision_id().to_string(),
            from: from.to_string(),
            bytes,
            copied,
            warnings,
        }),
    ))
}

/// Copy the processed file at `source` to `temporary`, with its radargram
/// id changed to `to`, and read back what the copy declares.
fn copy_with_id(
    source: &std::path::Path,
    temporary: &std::path::Path,
    to: &RadargramId,
) -> Result<crate::io::RidalNetcdfMetadata, ApiError> {
    let failed = |e: &dyn std::fmt::Display| {
        ApiError::internal("copy_failed", format!("Could not copy the radargram: {e}"))
    };
    std::fs::copy(source, temporary).map_err(|e| failed(&e))?;
    {
        let mut file = netcdf::append(temporary).map_err(|e| failed(&e))?;
        file.add_attribute("ridal_radargram_id", to.as_str())
            .map_err(|e| failed(&e))?;
    }
    match crate::io::inspect_ridal_netcdf(temporary).map_err(|e| failed(&e))? {
        crate::io::RidalNetcdfKind::Supported(meta) if meta.radargram_id == *to => Ok(meta),
        _ => Err(failed(&"the copy does not read back as the new radargram")),
    }
}

/// Give `to` what `from` has besides its file: its catalog settings, raw
/// source and processing profile, so it can be shown and reprocessed as
/// the original could. Returns what was copied and what could not be. Not
/// fatal: the radargram is installed by now, and each of these can be set
/// again by hand.
fn copy_settings(
    state: &AppState,
    project: &Project,
    from: &RadargramId,
    to: &RadargramId,
) -> (Vec<String>, Vec<String>) {
    use crate::project::{overrides, processing, raw};
    let store = project.documents();
    let mut copied = Vec::new();
    let mut warnings = Vec::new();

    match overrides::update(store, |stored| {
        let Some(settings) = stored.radargrams.get(from).cloned() else {
            return Ok(false);
        };
        stored.radargrams.entry(to.clone()).or_insert(settings);
        Ok(true)
    }) {
        Ok(true) => copied.push("catalog_settings".to_string()),
        Ok(false) => {}
        Err(e) => warnings.push(format!("The catalog settings were not copied: {e}")),
    }

    match raw::read_source(store, from) {
        Ok(Some((source, _))) => {
            match raw::write_source(
                store,
                to,
                &source,
                &crate::project::store::Expectation::Absent,
            ) {
                Ok(_) => copied.push("raw_source".to_string()),
                Err(e) => warnings.push(format!("The raw data were not attached: {e}")),
            }
        }
        Ok(None) => {}
        Err(e) => warnings.push(format!("The raw data were not attached: {e}")),
    }

    match processing::read_assignments(store) {
        Ok((assignments, _)) => {
            if let Some(assignment) = assignments.radargrams.get(from) {
                match processing::assign(
                    store,
                    to,
                    Some((assignment.profile.clone(), assignment.pre_steps.clone())),
                ) {
                    Ok(_) => copied.push("processing_profile".to_string()),
                    Err(e) => {
                        warnings.push(format!("The processing profile was not assigned: {e}"))
                    }
                }
            }
        }
        Err(e) => warnings.push(format!("The processing profile was not assigned: {e}")),
    }

    if copied.iter().any(|c| c == "catalog_settings") {
        // An override changes what the catalog says, so it is re-resolved
        // rather than rediscovered, as an edit in the catalog dialog is.
        if let Err(e) = super::overrides_routes::refresh_catalog(state) {
            warnings.push(format!("The catalog was not refreshed: {}", e.message()));
        }
    }
    (copied, warnings)
}

/// `POST /api/v1/datasets/{radargram_id}/transfer`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct TransferRequest {
    /// The radargram to move the picks onto. It must be in the catalog.
    to: String,
    /// Also remove this radargram, or stop serving it when it is in a
    /// read-only root, in the same step. Picks that were not moved are
    /// archived, as with any removal.
    #[serde(default)]
    remove_source: bool,
}

/// One interpretation now on the new radargram.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct MovedInterpretation {
    user: String,
    lines: usize,
    /// The revision the document now names, if it names one.
    #[schema(required = true)]
    revision_id: Option<String>,
    /// Whether that is the new radargram's current revision. When it is
    /// not, the picks are carried onto it and their owner adopts them, as
    /// after a reprocess.
    current: bool,
    /// The document's version, for `If-Match`.
    version: String,
}

/// `POST /api/v1/datasets/{radargram_id}/transfer`: what was moved.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct Transferred {
    from: String,
    to: String,
    /// Whether the two current revisions have the same axes. When they do,
    /// picks drawn on the old one's current revision are current on the new
    /// one at once.
    same_grid: bool,
    moved: Vec<MovedInterpretation>,
    /// Users who already had picks on `to`. Theirs on `from` were not moved
    /// and are never merged.
    occupied: Vec<String>,
    /// Files under `from` whose names are not a valid user id, left where
    /// they are.
    unreadable: Vec<String>,
    /// What became of `from`, when `remove_source` asked for its removal.
    #[schema(required = true)]
    removed: Option<Removed>,
}

/// Copy every saved interpretation of this radargram onto another one, and
/// optionally remove this one in the same step. Moving anyone else's picks
/// needs an administrator.
#[utoipa::path(
    post,
    path = "/api/v1/datasets/{radargram_id}/transfer",
    tag = "Radargrams",
    request_body = TransferRequest,
    responses(
        (status = 200, description = "Transferred.", body = Transferred),
        (status = 400, description = "`invalid_radargram_id`, or `same_radargram`.", body = ErrorBody),
        (status = 404, description = "Either radargram is not in the catalog.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
// The second of a rename's three steps (#500). Written only where the
// owner has nothing on `to` yet, as a restore from the archive is (#435),
// so it never replaces picks and can be retried: a retry finds every
// document already moved and goes on to the removal.
//
// Operator when every picks file is the caller's own, which is what a
// `ridal gui` user, an operator with nobody else, needs to rename at all.
// Anyone else's needs an administrator, as restoring them does: it writes
// documents in their name.
//
// One hold of the lifecycle lock covers the transfer and the removal, so a
// save cannot land on `from` between the two and be missed: it either
// lands before the transfer, and is moved, or after the removal, and is
// refused with the new id.
pub(super) async fn transfer_interpretations(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
    Json(request): Json<TransferRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "transfer picks")?;
    let from = parse_radargram(&radargram_id)?;
    let to = parse_radargram(&request.to)?;
    if from == to {
        return Err(ApiError::bad_request(
            "same_radargram",
            "The picks would be moved onto the radargram they are already on.",
        ));
    }

    let _lifecycle = state.lifecycle_lock().await;
    let ((from_revision, from_path), (to_revision, to_path)) = {
        let catalog = state.catalog();
        let served = |id: &RadargramId| -> Result<_, ApiError> {
            let entry = super::routes::lookup_dataset(&catalog, id.as_str())?;
            Ok((entry.revision_id.to_string(), entry_path(&state, entry)?))
        };
        (served(&from)?, served(&to)?)
    };

    let store = project.documents();
    let (owners, unreadable) = transfer::owners(store, &from)
        .map_err(|e| ApiError::internal("store_failed", e.to_string()))?;
    let others: Vec<&UserId> = owners
        .iter()
        .filter(|owner| caller.user.as_ref() != Some(*owner))
        .collect();
    if !others.is_empty() || !unreadable.is_empty() {
        caller.require(Role::Admin, "move other people's picks")?;
    }

    // Both current revisions' axes, so the grids can be compared and the
    // picks drawn on `from` carried from its axes once its file is gone.
    for (id, path, revision) in [
        (&from, &from_path, &from_revision),
        (&to, &to_path, &to_revision),
    ] {
        super::lifecycle_routes::snapshot_axes(project, id, path, revision).map_err(|problem| {
            ApiError::internal(
                "snapshot_failed",
                format!("{problem}. Nothing was transferred."),
            )
        })?;
    }
    let same_grid = transfer::same_grid(store, &from, &from_revision, &to, &to_revision)
        .map_err(|e| ApiError::internal("snapshot_read_failed", e.to_string()))?;

    let at = chrono::Utc::now().to_rfc3339();
    let by = caller.audit_name();
    let report = transfer::transfer(
        store,
        &transfer::Plan {
            from: &from,
            from_revision: &from_revision,
            to: &to,
            to_revision: &to_revision,
            same_grid,
            at: &at,
            by: &by,
        },
    )
    .map_err(|e| ApiError::internal("transfer_failed", e.to_string()))?;

    // Before the removal, so the id change is on record even when the
    // removal fails and is retried.
    let users: Vec<&str> = report.moved.iter().map(|m| m.user.as_str()).collect();
    audit::record(
        store,
        audit::Entry {
            at: at.clone(),
            user: by,
            action: audit::Action::Transferred,
            radargram_id: from.to_string(),
            revision_id: Some(from_revision.clone()),
            note: Some(match users.len() {
                0 => "no interpretations to move".to_string(),
                n => format!(
                    "moved {n} interpretation(s) ({}){}",
                    users.join(", "),
                    if same_grid {
                        ""
                    } else {
                        "; the grids differ, so they are carried"
                    }
                ),
            }),
            to_radargram_id: Some(to.to_string()),
        },
    );

    let removed = if request.remove_source {
        Some(super::lifecycle_routes::remove_locked(
            &state,
            project,
            &caller,
            from.as_str(),
        )?)
    } else {
        None
    };

    Ok(Json(Transferred {
        from: from.to_string(),
        to: to.to_string(),
        same_grid,
        moved: report
            .moved
            .into_iter()
            .map(|m| MovedInterpretation {
                user: m.user.to_string(),
                lines: m.lines,
                revision_id: m.revision_id,
                current: m.current,
                version: m.version.to_string(),
            })
            .collect(),
        occupied: report.occupied.iter().map(UserId::to_string).collect(),
        unreadable: report.unreadable,
        removed,
    }))
}

/// One person's recent work on a radargram.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct PersonActivity {
    /// Who, for an administrator; `null` for anyone else, who is told how
    /// many but not who.
    #[schema(required = true)]
    user: Option<String>,
    /// When their picks were last saved, RFC 3339.
    #[schema(required = true)]
    saved_at: Option<String>,
    /// When they last opened the viewer, RFC 3339. Kept in memory since the
    /// server started.
    #[schema(required = true)]
    opened_at: Option<String>,
}

/// `GET /api/v1/datasets/{radargram_id}/activity`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(super) struct Activity {
    radargram_id: String,
    /// The caller's own picks and viewer, whatever their age.
    you: PersonActivity,
    /// Everyone else who saved or opened the viewer in the last 24 hours,
    /// most recent first.
    others: Vec<PersonActivity>,
    /// Picks files with no save time, such as those written by a script or a
    /// converter. Nothing says how recent they are.
    undated: usize,
    /// How firmly to confirm a change that leaves an open picker behind:
    /// `none` when nobody else was active in the last 24 hours, `day` when
    /// someone was, `hour` when someone was in the last hour.
    #[schema(value_type = String)]
    level: &'static str,
    /// Whether the caller may move this radargram's picks: an
    /// administrator, or anyone when every picks file is their own.
    may_transfer: bool,
}

/// Who has saved picks on this radargram or opened its viewer recently, for
/// a confirmation before it is removed or renamed. Names are given to
/// administrators only.
#[utoipa::path(
    get,
    path = "/api/v1/datasets/{radargram_id}/activity",
    tag = "Radargrams",
    responses(
        (status = 200, description = "Recent activity.", body = Activity),
        (status = 404, description = "No such radargram.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
pub(super) async fn get_activity(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_for(&state, &caller, "see who is working on a radargram")?;
    let radargram = {
        let catalog = state.catalog();
        super::routes::lookup_dataset(&catalog, &radargram_id)?
            .radargram_id
            .clone()
    };
    let store = project.documents();
    let (owners, unreadable) = transfer::owners(store, &radargram)
        .map_err(|e| ApiError::internal("store_failed", e.to_string()))?;

    let now = chrono::Utc::now();
    let mut saved: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let mut undated = unreadable.len();
    for owner in &owners {
        let Ok(Some(stored)) = crate::project::interpretations::read(store, &radargram, owner)
        else {
            continue;
        };
        match stored
            .document
            .date_modified
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        {
            Some(at) => {
                saved.insert(owner.to_string(), at.to_utc());
            }
            None => undated += 1,
        }
    }
    let opened = state.recently_opened.of(radargram.as_str());

    let me = caller.user.as_ref().map(|u| u.as_str().to_string());
    let names = caller.may(Role::Admin);
    let may_transfer =
        names || (unreadable.is_empty() && owners.iter().all(|o| caller.user.as_ref() == Some(o)));
    // Each other person and the latest of their save and their viewer, the
    // save only when it is recent enough to be someone working now.
    type Time = chrono::DateTime<chrono::Utc>;
    let mut others: Vec<(String, Option<Time>, Option<Time>)> = saved
        .keys()
        .chain(opened.keys())
        .filter(|user| Some(*user) != me.as_ref())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|user| {
            (
                user.clone(),
                saved.get(user).copied().filter(|at| now - *at < OPENED_FOR),
                opened.get(user).copied(),
            )
        })
        .filter(|(_, saved, opened)| saved.is_some() || opened.is_some())
        .collect();
    others.sort_by_key(|(_, saved, opened)| std::cmp::Reverse((*saved).max(*opened)));
    let level = match others.iter().filter_map(|(_, s, o)| (*s).max(*o)).max() {
        Some(at) if now - at < chrono::TimeDelta::hours(1) => "hour",
        Some(_) => "day",
        None => "none",
    };

    let you = PersonActivity {
        user: me.clone(),
        saved_at: me
            .as_ref()
            .and_then(|me| saved.get(me))
            .map(|at| at.to_rfc3339()),
        opened_at: me
            .as_ref()
            .and_then(|me| opened.get(me))
            .map(|at| at.to_rfc3339()),
    };
    Ok(Json(Activity {
        radargram_id: radargram.to_string(),
        you,
        others: others
            .into_iter()
            .map(|(user, saved_at, opened_at)| PersonActivity {
                user: names.then_some(user),
                saved_at: saved_at.map(|at| at.to_rfc3339()),
                opened_at: opened_at.map(|at| at.to_rfc3339()),
            })
            .collect(),
        undated,
        level,
        may_transfer,
    }))
}
