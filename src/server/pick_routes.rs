//! The pick manager's view of a project's interpretations (#178, #162).
//!
//! One row per stored interpretation -- live, on an older revision, left
//! behind by a radargram that is no longer served, or archived by a
//! removal -- described by numbers and never by its picks. Whose rows a
//! caller sees follows the role: a picker their own, an operator and an
//! admin everyone's. Reading the picks behind a row is still the existing
//! routes' decision, under the `Picks` download scope.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::IntoResponse;
use axum::Json;

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::{self, expectation_from, parse_radargram, parse_user, readable_project};
use super::routes::ApiError;
use crate::identity::{RadargramId, UserId};
use crate::interp::carry;
use crate::project::interpretations::{self, InterpretationError, StoredInterpretation};
use crate::project::pick_summary::{self, DocumentStats};
use crate::project::roles::{DownloadScope, Role};
use crate::project::store::StoreError;
use crate::project::{audit, overrides, revisions, Project};

/// Where an interpretation stands relative to the radargram it was drawn on.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub(super) enum InterpretationState {
    /// Drawn on the revision the catalog serves now, or not saying which.
    Live,
    /// Drawn on an earlier revision. The viewer carries it onto the current
    /// one; promoting it rewrites it as drawn there.
    Stale,
    /// The radargram is in a read-only root and ignored. Lifting the ignore
    /// brings these back.
    Ignored,
    /// The radargram is not in the catalog and was not removed through
    /// Ridal, so nothing archived these picks: its file was moved or
    /// deleted on the server.
    Orphaned,
    /// Archived when the radargram was removed.
    Archived,
}

/// What an interpretation's coverage was measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum CoverageBasis {
    /// The current revision, which the picks were drawn on.
    Current,
    /// The current revision, after carrying picks drawn on an earlier one
    /// onto it, as the viewer draws them.
    Carried,
    /// The earlier revision the picks were drawn on, because they could not
    /// be carried onto the current one.
    Drawn,
    /// The current revision, with the picks as stored: they could not be
    /// carried and the revision they were drawn on was not kept. This is
    /// where the viewer draws another contributor's picks, and it is only
    /// right if reprocessing did not change which trace is which.
    Uncarried,
}

/// `GET /api/v1/interpretations`: one interpretation, as numbers.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(super) struct InterpretationSummary {
    radargram_id: String,
    /// The catalog's name for the radargram, when it is in the catalog.
    #[schema(required = true)]
    display_name: Option<String>,
    #[schema(required = true)]
    group_name: Option<String>,
    /// The account that drew it.
    user: String,
    state: InterpretationState,
    /// For an archived interpretation, the removal it was archived by: the
    /// removal time with `:` written as `-`, suffixed `-2`, `-3`... when
    /// removals shared a second.
    #[schema(required = true)]
    removal: Option<String>,
    /// The revision the document says it was drawn on, if it says.
    #[schema(required = true)]
    revision_id: Option<String>,
    /// Picked lines (features).
    features: usize,
    vertices: usize,
    /// Percent of the radargram's traces under at least one line, or `null`
    /// with no radargram to measure against.
    #[schema(required = true)]
    coverage: Option<f64>,
    /// The trace count `coverage` is measured against.
    #[schema(required = true)]
    traces: Option<usize>,
    /// What `coverage` was measured against, or `null` with no coverage.
    #[schema(required = true)]
    coverage_basis: Option<CoverageBasis>,
    /// One entry per layer in use, unlabelled lines last.
    layers: Vec<LayerSummary>,
    /// When the document says it was last modified.
    #[schema(required = true)]
    date_modified: Option<String>,
    /// The stored version, as sent in `ETag`. Absent for an archived
    /// interpretation, which cannot be written to.
    #[schema(required = true)]
    version: Option<String>,
    /// Size of the stored file.
    bytes: usize,
    /// Whether this archived interpretation can be restored: its radargram
    /// is in the catalog and its user has no interpretation of it now.
    /// Always `false` for one that is not archived.
    restorable: bool,
}

/// One layer within an [`InterpretationSummary`].
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(super) struct LayerSummary {
    /// The layer id the lines carry, or `null` for unlabelled lines.
    #[schema(required = true)]
    layer: Option<String>,
    features: usize,
    vertices: usize,
    #[schema(required = true)]
    coverage: Option<f64>,
}

/// A stored interpretation that could not be summarized.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(super) struct UnreadableInterpretation {
    /// The file or directory, relative to `interpretations/`.
    path: String,
    reason: String,
}

/// `GET /api/v1/interpretations`.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(super) struct InterpretationSummaryList {
    /// Ordered by radargram, then state, then user.
    interpretations: Vec<InterpretationSummary>,
    /// `true` when these are everyone's interpretations, `false` when only
    /// the caller's: everyone's takes the `operator` role.
    everyone: bool,
    /// Whether the caller may download a live interpretation as a file,
    /// which takes the `picks` download scope -- for their own as well.
    may_download: bool,
    /// Whether the caller may archive someone else's interpretation and
    /// purge the archive, which takes the `admin` role.
    may_archive: bool,
    /// Files that were skipped. Listed only to callers who see everyone's.
    unreadable: Vec<UnreadableInterpretation>,
}

/// Every stored interpretation, summarized, before any filtering.
#[derive(Default)]
pub(super) struct Scan {
    pub(super) rows: Vec<InterpretationSummary>,
    unreadable: Vec<UnreadableInterpretation>,
}

impl InterpretationSummary {
    pub(super) fn is_on_catalog(&self) -> bool {
        matches!(
            self.state,
            InterpretationState::Live | InterpretationState::Stale
        )
    }

    /// `(layer, feature count)` for every layer in this row.
    pub(super) fn layer_counts(&self) -> impl Iterator<Item = (Option<&str>, usize)> {
        self.layers
            .iter()
            .map(|layer| (layer.layer.as_deref(), layer.features))
    }
}

fn summary_row(
    radargram: &RadargramId,
    user: &UserId,
    stored: &StoredInterpretation,
    stats: DocumentStats,
    traces: Option<usize>,
) -> InterpretationSummary {
    InterpretationSummary {
        radargram_id: radargram.as_str().to_string(),
        display_name: None,
        group_name: None,
        user: user.as_str().to_string(),
        state: InterpretationState::Live,
        removal: None,
        revision_id: stored
            .document
            .source
            .as_ref()
            .and_then(|s| s.revision_id.clone()),
        features: stats.features,
        vertices: stats.vertices,
        coverage: stats.coverage,
        traces: traces.filter(|_| stats.coverage.is_some()),
        coverage_basis: None,
        layers: stats
            .layers
            .into_iter()
            .map(|layer| LayerSummary {
                layer: layer.layer,
                features: layer.features,
                vertices: layer.vertices,
                coverage: layer.coverage,
            })
            .collect(),
        date_modified: stored.document.date_modified.clone(),
        version: Some(stored.version.to_string()),
        bytes: stored.bytes,
        restorable: false,
    }
}

/// `drawn`'s counts with `carried`'s coverage.
///
/// Counts describe what was drawn, so they come from the stored document;
/// coverage describes what the viewer shows on the current revision, so it
/// comes from the carried one. A line carrying dropped covers nothing.
fn with_coverage_of(mut drawn: DocumentStats, carried: DocumentStats) -> DocumentStats {
    drawn.coverage = carried.coverage;
    for layer in &mut drawn.layers {
        layer.coverage = Some(
            carried
                .layers
                .iter()
                .find(|c| c.layer == layer.layer)
                .and_then(|c| c.coverage)
                .unwrap_or(0.0),
        );
    }
    drawn
}

impl Scan {
    fn skip(&mut self, path: String, reason: impl Into<String>) {
        let reason = reason.into();
        tracing::warn!(
            path = path.as_str(),
            reason = reason.as_str(),
            "skipping an interpretation"
        );
        self.unreadable
            .push(UnreadableInterpretation { path, reason });
    }

    /// A document that would not parse is skipped and reported, so one bad
    /// file cannot hide every other row (#213). Anything else is a store
    /// failure and fails the request.
    fn read_or_skip(
        &mut self,
        path: String,
        result: Result<Option<StoredInterpretation>, InterpretationError>,
    ) -> Result<Option<StoredInterpretation>, ApiError> {
        match result {
            Ok(stored) => Ok(stored),
            Err(e @ InterpretationError::Malformed { .. }) => {
                self.skip(path, e.to_string());
                Ok(None)
            }
            Err(e) => Err(ApiError::internal("store_failed", e.to_string())),
        }
    }
}

const NOT_A_USER_ID: &str = "the file name is not a valid user id";

/// Summarize every live interpretation directory: those of radargrams in
/// the catalog, and those left behind by radargrams that are not.
pub(super) fn scan_live(state: &AppState, project: &Project) -> Result<Scan, ApiError> {
    let store = project.documents();
    let catalog = state.catalog();
    let (stored_overrides, _) = overrides::read(store)
        .map_err(|e| ApiError::internal("overrides_read_failed", e.to_string()))?;
    let (radargrams, bad_directories) =
        interpretations::list_radargrams(store).map_err(store_failed)?;

    let mut scan = Scan::default();
    for name in bad_directories {
        scan.skip(name, "the directory name is not a valid radargram id");
    }
    for radargram in radargrams {
        let entry = catalog.find_entry(radargram.as_str());
        let (users, bad_files) =
            interpretations::list_users_checked(store, &radargram).map_err(store_failed)?;
        for stem in bad_files {
            scan.skip(
                format!("{}/{stem}{}", radargram.as_str(), interpretations::SUFFIX),
                NOT_A_USER_ID,
            );
        }
        // One file read per radargram and one snapshot read per older
        // revision, not one per document.
        let mut current_axes: Option<crate::interp::anchors::Axes> = None;
        let mut snapshot_traces: BTreeMap<String, Option<usize>> = BTreeMap::new();
        for user in users {
            let path = format!(
                "{}/{}{}",
                radargram.as_str(),
                user.as_str(),
                interpretations::SUFFIX
            );
            let result = interpretations::read(store, &radargram, &user);
            let Some(stored) = scan.read_or_skip(path, result)? else {
                continue;
            };
            let drawn_on = stored
                .document
                .source
                .as_ref()
                .and_then(|s| s.revision_id.as_deref());
            let (row_state, stats, traces, basis) = match entry {
                Some(entry) => match drawn_on {
                    Some(revision) if revision != entry.revision_id.as_str() => {
                        // Carried onto the current revision and measured
                        // there, which is what the viewer draws. Only when
                        // that is refused are they measured in the index
                        // space they were drawn in, against the old
                        // revision's kept axes.
                        let axes =
                            current_axes.get_or_insert_with(|| match state.absolute_path(entry) {
                                Ok(path) => interp_routes::current_axes(project, &path, entry),
                                Err(e) => {
                                    tracing::warn!(
                                        radargram = radargram.as_str(),
                                        error = e.as_str(),
                                        "cannot carry picks for the pick manager"
                                    );
                                    crate::interp::anchors::Axes::default()
                                }
                            });
                        let source = carry::with_kept_axes(project, &radargram, &stored.document);
                        let carried = carry::carry(&source, axes, entry.revision_id.as_str());
                        match carried.document {
                            Some(document) => {
                                let traces = entry.shape.1;
                                let stats = with_coverage_of(
                                    pick_summary::summarize(&stored.document, None),
                                    pick_summary::summarize(&document, Some(traces)),
                                );
                                (
                                    InterpretationState::Stale,
                                    stats,
                                    Some(traces),
                                    CoverageBasis::Carried,
                                )
                            }
                            None => {
                                let kept = *snapshot_traces
                                    .entry(revision.to_string())
                                    .or_insert_with(|| {
                                        revisions::get(store, &radargram, revision)
                                            .ok()
                                            .flatten()
                                            .map(|snapshot| snapshot.n_traces())
                                    });
                                // With no kept axes either, measured as the
                                // viewer draws another contributor's picks:
                                // as stored, on the current revision.
                                let (traces, basis) = match kept {
                                    Some(traces) => (traces, CoverageBasis::Drawn),
                                    None => (entry.shape.1, CoverageBasis::Uncarried),
                                };
                                (
                                    InterpretationState::Stale,
                                    pick_summary::summarize(&stored.document, Some(traces)),
                                    Some(traces),
                                    basis,
                                )
                            }
                        }
                    }
                    _ => {
                        let traces = entry.shape.1;
                        (
                            InterpretationState::Live,
                            pick_summary::summarize(&stored.document, Some(traces)),
                            Some(traces),
                            CoverageBasis::Current,
                        )
                    }
                },
                None if stored_overrides.ignored.contains_key(&radargram) => (
                    InterpretationState::Ignored,
                    pick_summary::summarize(&stored.document, None),
                    None,
                    CoverageBasis::Current,
                ),
                None => (
                    InterpretationState::Orphaned,
                    pick_summary::summarize(&stored.document, None),
                    None,
                    CoverageBasis::Current,
                ),
            };
            let mut row = summary_row(&radargram, &user, &stored, stats, traces);
            row.state = row_state;
            row.coverage_basis = row.coverage.map(|_| basis);
            if let Some(entry) = entry {
                row.display_name = entry.display_name.as_ref().map(|n| n.to_string());
                row.group_name = entry.group_name.as_ref().map(|n| n.to_string());
            }
            scan.rows.push(row);
        }
    }
    Ok(scan)
}

/// Add every archived interpretation to `scan`.
fn scan_archived(project: &Project, scan: &mut Scan) -> Result<(), ApiError> {
    let store = project.documents();
    let (archived, bad_files) = interpretations::list_archived(store).map_err(store_failed)?;
    for path in bad_files {
        scan.skip(format!("_archived/{path}"), NOT_A_USER_ID);
    }
    for item in archived {
        let path = format!(
            "_archived/{}/{}/{}{}",
            item.radargram.as_str(),
            item.removal,
            item.user.as_str(),
            interpretations::SUFFIX
        );
        let result = interpretations::read_archived(store, &item);
        let Some(stored) = scan.read_or_skip(path, result)? else {
            continue;
        };
        let stats = pick_summary::summarize(&stored.document, None);
        let mut row = summary_row(&item.radargram, &item.user, &stored, stats, None);
        row.state = InterpretationState::Archived;
        row.removal = Some(item.removal);
        row.version = None;
        scan.rows.push(row);
    }
    Ok(())
}

fn store_failed(e: InterpretationError) -> ApiError {
    ApiError::internal("store_failed", e.to_string())
}

/// `GET /api/v1/interpretations` -- every interpretation the caller may
/// know about, as numbers.
///
/// Scans every stored interpretation, as `/layers/usage` does: a directory
/// walk and a JSON parse per document, fine at the tens to hundreds of
/// radargrams a project has, and requested only by the pick manager.
pub async fn list_summaries(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let project = readable_project(&state)?;
    let mut scan = scan_live(&state, project)?;
    scan_archived(project, &mut scan)?;
    mark_restorable(&state, &mut scan.rows);

    // Statistics only, so seeing everyone's is a curation question, which
    // is the operator's; reading the picks behind a row is not decided here.
    let everyone = caller.may(Role::Operator);
    let mut rows = scan.rows;
    if !everyone {
        let own = caller.user.as_ref().map(UserId::as_str);
        rows.retain(|row| Some(row.user.as_str()) == own);
    }
    rows.sort_by(|a, b| {
        (&a.radargram_id, a.state, &a.removal, &a.user).cmp(&(
            &b.radargram_id,
            b.state,
            &b.removal,
            &b.user,
        ))
    });
    Ok(Json(InterpretationSummaryList {
        interpretations: rows,
        everyone,
        may_download: caller.may_download(DownloadScope::Picks),
        may_archive: caller.may(Role::Admin),
        // A path names its user, so these are everyone's too.
        unreadable: if everyone {
            scan.unreadable
        } else {
            Vec::new()
        },
    }))
}

/// Mark the archived rows that [`restore_archived`] would accept, and name
/// their radargram as the catalog does.
fn mark_restorable(state: &AppState, rows: &mut [InterpretationSummary]) {
    let occupied: BTreeSet<(String, String)> = rows
        .iter()
        .filter(|row| row.state != InterpretationState::Archived)
        .map(|row| (row.radargram_id.clone(), row.user.clone()))
        .collect();
    let catalog = state.catalog();
    for row in rows
        .iter_mut()
        .filter(|row| row.state == InterpretationState::Archived)
    {
        let Some(entry) = catalog.find_entry(&row.radargram_id) else {
            continue;
        };
        row.display_name = entry.display_name.as_ref().map(|n| n.to_string());
        row.group_name = entry.group_name.as_ref().map(|n| n.to_string());
        row.restorable = !occupied.contains(&(row.radargram_id.clone(), row.user.clone()));
    }
}

/// The project, if `caller` holds `role`; the same 401/403 split every
/// write uses, then the "not a project" answer reads give.
fn project_requiring<'a>(
    state: &'a AppState,
    caller: &Caller,
    role: Role,
    action: &str,
) -> Result<&'a Project, ApiError> {
    caller.require(role, action)?;
    readable_project(state)
}

fn interpretation_failed(error: InterpretationError) -> ApiError {
    match &error {
        InterpretationError::Store(StoreError::Conflict { .. }) => ApiError::precondition_failed(
            "version_conflict",
            "These picks were saved again after you loaded them, so nothing was \
             changed. Reload to see the current version.",
        ),
        _ => ApiError::internal("store_failed", error.to_string()),
    }
}

/// `POST …/interpretations/{user}/archive`: the interpretation was archived.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(super) struct ArchivedInterpretation {
    radargram_id: String,
    user: String,
    /// The archive entry it is now under, as in a summary row's `removal`.
    removal: String,
}

/// `POST /api/v1/datasets/{radargram_id}/interpretations/{user}/archive`
///
/// Move someone's interpretation into the archive (#178). This is what an
/// administrator gets instead of deleting another person's picks, which no
/// one may do (#131): the picks stop being drawn, counted and exported, and
/// stay attributed in the archive until purged. Conditional on `If-Match`
/// like a delete, so picks saved after the page loaded are not archived
/// unseen.
///
/// Works whether or not the radargram is served, which is how picks left
/// behind by a radargram that went away outside Ridal get archived too.
pub async fn archive_interpretation(
    State(state): State<Arc<AppState>>,
    Path((radargram_id, user)): Path<(String, String)>,
    caller: Caller,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_requiring(&state, &caller, Role::Admin, "archive someone's picks")?;
    let radargram = parse_radargram(&radargram_id)?;
    let user = parse_user(&user)?;
    let at = chrono::Utc::now().to_rfc3339();

    let Some(stored) = interpretations::read(project.documents(), &radargram, &user)
        .map_err(interpretation_failed)?
    else {
        return Err(not_found(&radargram, &user));
    };
    let destination = interpretations::archive_and_remove(
        project.documents(),
        &radargram,
        &user,
        &at,
        &expectation_from(&headers),
    )
    .map_err(interpretation_failed)?
    .ok_or_else(|| not_found(&radargram, &user))?;
    let removal = destination
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let lines = stored.document.features.len();
    audit::record(
        project.documents(),
        audit::Entry {
            at,
            user: caller.audit_name(),
            action: audit::Action::Archived,
            radargram_id: radargram.to_string(),
            revision_id: stored
                .document
                .source
                .as_ref()
                .and_then(|s| s.revision_id.clone()),
            note: Some(format!(
                "archived {}'s interpretation ({lines} {})",
                user.as_str(),
                if lines == 1 { "line" } else { "lines" }
            )),
        },
    );

    Ok(Json(ArchivedInterpretation {
        radargram_id: radargram.to_string(),
        user: user.to_string(),
        removal,
    }))
}

fn not_found(radargram: &RadargramId, user: &UserId) -> ApiError {
    ApiError::not_found(
        "interpretation_not_found",
        format!(
            "'{}' has no interpretation of '{}'.",
            user.as_str(),
            radargram.as_str()
        ),
    )
}

/// One archived interpretation, looked up among what the archive lists
/// rather than built from the path, so `removal` can only name an archive
/// entry that exists.
fn find_archived(
    project: &Project,
    radargram: &RadargramId,
    removal: &str,
    user: &UserId,
) -> Result<interpretations::Archived, ApiError> {
    let (archived, _) =
        interpretations::list_archived(project.documents()).map_err(store_failed)?;
    archived
        .into_iter()
        .find(|a| a.radargram == *radargram && a.removal == removal && a.user == *user)
        .ok_or_else(|| archived_not_found(radargram, removal, user))
}

fn archived_not_found(radargram: &RadargramId, removal: &str, user: &UserId) -> ApiError {
    ApiError::not_found(
        "archived_interpretation_not_found",
        format!(
            "There is no archived interpretation by '{}' of '{}' from '{removal}'.",
            user.as_str(),
            radargram.as_str()
        ),
    )
}

/// `GET /api/v1/interpretations/archived/{radargram_id}/{removal}/{user}`
///
/// One archived interpretation, byte for byte, as a download. Gated like
/// `…/interpretations/{user}/raw`: archived picks are still raw picks.
pub async fn download_archived(
    State(state): State<Arc<AppState>>,
    Path((radargram_id, removal, user)): Path<(String, String, String)>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_download(DownloadScope::Picks, "archived picks")?;
    let project = readable_project(&state)?;
    let radargram = parse_radargram(&radargram_id)?;
    let user = parse_user(&user)?;

    let item = find_archived(project, &radargram, &removal, &user)?;
    let bytes = project
        .documents()
        .read(&interpretations::archived_path(&item))
        .map_err(|e| ApiError::internal("store_failed", e.to_string()))?
        .ok_or_else(|| {
            ApiError::not_found(
                "archived_interpretation_not_found",
                "It was purged just now.",
            )
        })?;

    let filename = format!(
        "{}-{}-archived-{}{}",
        radargram.as_str(),
        user.as_str(),
        item.removal,
        interpretations::SUFFIX
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes.text,
    ))
}

/// `DELETE /api/v1/interpretations/archived/…`: what was purged.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(super) struct Purged {
    radargram_id: String,
    removal: String,
    /// Whose archived interpretations were destroyed.
    users: Vec<String>,
}

/// `DELETE /api/v1/interpretations/archived/{radargram_id}/{removal}`
///
/// Destroy every archived interpretation from one removal (#162).
pub async fn purge_removal(
    State(state): State<Arc<AppState>>,
    Path((radargram_id, removal)): Path<(String, String)>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    purge(&state, &caller, &radargram_id, &removal, None)
}

/// `DELETE /api/v1/interpretations/archived/{radargram_id}/{removal}/{user}`
///
/// Destroy one archived interpretation (#162).
pub async fn purge_one(
    State(state): State<Arc<AppState>>,
    Path((radargram_id, removal, user)): Path<(String, String, String)>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let user = parse_user(&user)?;
    purge(&state, &caller, &radargram_id, &removal, Some(&user))
}

/// Purging is the one operation on picks that destroys authored work, so it
/// sits with the role that owns the project (#162), and is audited with
/// whose work it was.
fn purge(
    state: &AppState,
    caller: &Caller,
    radargram_id: &str,
    removal: &str,
    user: Option<&UserId>,
) -> Result<Json<Purged>, ApiError> {
    let project = project_requiring(state, caller, Role::Admin, "purge archived picks")?;
    let radargram = parse_radargram(radargram_id)?;
    let purged = interpretations::purge_archived(project.documents(), &radargram, removal, user)
        .map_err(interpretation_failed)?;
    if purged.is_empty() {
        return Err(ApiError::not_found(
            "archived_interpretation_not_found",
            format!(
                "There is nothing archived {}of '{}' from '{removal}'.",
                user.map(|u| format!("by '{}' ", u.as_str()))
                    .unwrap_or_default(),
                radargram.as_str()
            ),
        ));
    }
    let users: Vec<String> = purged.iter().map(|u| u.to_string()).collect();
    audit::record(
        project.documents(),
        audit::Entry {
            at: chrono::Utc::now().to_rfc3339(),
            user: caller.audit_name(),
            action: audit::Action::Purged,
            radargram_id: radargram.to_string(),
            revision_id: None,
            note: Some(format!(
                "purged the archived picks of {} (archived {removal})",
                users.join(", ")
            )),
        },
    );
    Ok(Json(Purged {
        radargram_id: radargram.to_string(),
        removal: removal.to_string(),
        users,
    }))
}

/// `POST …/archived/{radargram_id}/{removal}/{user}/restore`: the
/// interpretation is back on its radargram.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(super) struct RestoredInterpretation {
    radargram_id: String,
    user: String,
    /// The archive entry it came from.
    removal: String,
    /// The revision the document says it was drawn on, if it says.
    #[schema(required = true)]
    revision_id: Option<String>,
    /// Whether that is the revision the catalog serves now. When it is not,
    /// the picks are carried onto it like any other stale interpretation,
    /// and their owner adopts them as usual.
    current: bool,
    /// The restored document's version, as sent in `ETag`.
    version: String,
}

/// `POST /api/v1/interpretations/archived/{radargram_id}/{removal}/{user}/restore`
///
/// Move an archived interpretation back onto its radargram (#435): what
/// #178's "modify orphaned picks" comes to once the radargram has been
/// added back. The document is restored as it was archived, so picks drawn
/// on an earlier revision go through the carry machinery (#148, #370) in
/// the viewer and are adopted by their owner, not rewritten here.
///
/// The administrator's, as archiving is: it undoes an archive. Refused when
/// the radargram is not in the catalog, and when the user has picks on it
/// already, which are never merged with or replaced by archived ones.
pub async fn restore_archived(
    State(state): State<Arc<AppState>>,
    Path((radargram_id, removal, user)): Path<(String, String, String)>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let project = project_requiring(&state, &caller, Role::Admin, "restore archived picks")?;
    let radargram = parse_radargram(&radargram_id)?;
    let user = parse_user(&user)?;

    // So a removal of the same radargram cannot archive between the check
    // that it is served and the restore.
    let _lifecycle = state.lifecycle_lock().await;
    let current = {
        let catalog = state.catalog();
        let entry = catalog.find_entry(radargram.as_str()).ok_or_else(|| {
            ApiError::conflict(
                "not_in_catalog",
                format!(
                    "'{}' is not in the catalog, so there is nothing to restore its picks \
                     onto. Add the radargram back first.",
                    radargram.as_str()
                ),
            )
        })?;
        entry.revision_id.as_str().to_string()
    };

    let item = find_archived(project, &radargram, &removal, &user)?;
    let stored = interpretations::read_archived(project.documents(), &item)
        .map_err(store_failed)?
        .ok_or_else(|| archived_not_found(&radargram, &removal, &user))?;
    let version = match interpretations::restore_archived(project.documents(), &item)
        .map_err(store_failed)?
    {
        interpretations::Restore::Done(version) => version,
        interpretations::Restore::NotArchived => {
            return Err(archived_not_found(&radargram, &removal, &user))
        }
        interpretations::Restore::Occupied => {
            return Err(ApiError::conflict(
                "interpretation_exists",
                format!(
                    "'{}' has picks on '{}' now, and archived picks are not merged \
                     into them. Nothing was restored.",
                    user.as_str(),
                    radargram.as_str()
                ),
            ))
        }
    };

    let revision_id = stored
        .document
        .source
        .as_ref()
        .and_then(|s| s.revision_id.clone());
    let lines = stored.document.features.len();
    audit::record(
        project.documents(),
        audit::Entry {
            at: chrono::Utc::now().to_rfc3339(),
            user: caller.audit_name(),
            action: audit::Action::Restored,
            radargram_id: radargram.to_string(),
            revision_id: revision_id.clone(),
            note: Some(format!(
                "restored {}'s interpretation ({lines} {}), archived {removal}",
                user.as_str(),
                if lines == 1 { "line" } else { "lines" }
            )),
        },
    );

    Ok(Json(RestoredInterpretation {
        radargram_id: radargram.to_string(),
        user: user.to_string(),
        removal: item.removal,
        // A document that does not say is drawn as if it were current.
        current: revision_id.as_deref().is_none_or(|r| r == current),
        revision_id,
        version: version.to_string(),
    }))
}

/// The most files one bulk download takes. Far more than a project's
/// interpretations today; it bounds the work a single request can ask for.
const MAX_BULK_FILES: usize = 10_000;

/// `POST /api/v1/interpretations/download`.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub(super) struct BulkDownloadRequest {
    /// The interpretations to include, as the pick manager lists them.
    items: Vec<BulkDownloadItem>,
}

/// One interpretation in a [`BulkDownloadRequest`]: a summary row's
/// `radargram_id`, `user` and, for an archived one, `removal`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, utoipa::ToSchema)]
pub(super) struct BulkDownloadItem {
    radargram_id: String,
    user: String,
    #[serde(default)]
    removal: Option<String>,
}

/// `POST /api/v1/interpretations/download`
///
/// Several interpretations, live or archived, byte for byte in one zip
/// (#435): the rows the pick manager's filters show. Which rows those are
/// is the page's to decide, so the request names them rather than the
/// server repeating the filters.
///
/// Gated like the one-file downloads, under the `picks` download scope.
/// The zip mirrors the layout under `interpretations/`, so it can be put
/// back into a project as it is, and `manifest.json` lists what it holds
/// and anything named that was not there, such as picks purged or archived
/// since the page loaded.
pub(super) async fn download_bulk(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Json(request): Json<BulkDownloadRequest>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_download(DownloadScope::Picks, "picks")?;
    readable_project(&state)?;
    if request.items.is_empty() {
        return Err(ApiError::bad_request(
            "nothing_selected",
            "No interpretations were named, so there is nothing to download.",
        ));
    }
    if request.items.len() > MAX_BULK_FILES {
        return Err(ApiError::payload_too_large(
            "too_many_files",
            format!(
                "{} interpretations were named; one download takes at most {MAX_BULK_FILES}.",
                request.items.len()
            ),
        ));
    }
    let items: BTreeSet<BulkDownloadItem> = request.items.into_iter().collect();
    for item in &items {
        parse_radargram(&item.radargram_id)?;
        parse_user(&item.user)?;
    }

    let bytes = tokio::task::spawn_blocking(move || {
        let project = readable_project(&state)?;
        bulk_zip(project, &items)
    })
    .await
    .map_err(|e| ApiError::internal("download_task_failed", e.to_string()))??;

    let filename = format!("picks-{}.zip", chrono::Utc::now().format("%Y-%m-%d"));
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    ))
}

/// One entry of a bulk download's `manifest.json`.
#[derive(serde::Serialize)]
struct ManifestEntry<'a> {
    radargram_id: &'a str,
    user: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    removal: Option<&'a str>,
    /// The file's path in the zip, absent for one that was not found.
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

fn bulk_zip(project: &Project, items: &BTreeSet<BulkDownloadItem>) -> Result<Vec<u8>, ApiError> {
    use std::io::Write;

    let store = project.documents();
    let (archived, _) = interpretations::list_archived(store).map_err(store_failed)?;
    let zip_failed = |e: zip::result::ZipError| ApiError::internal("zip_failed", e.to_string());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let mut files = Vec::new();
    let mut missing = Vec::new();
    for item in items {
        // Both checked by the caller.
        let radargram = parse_radargram(&item.radargram_id)?;
        let user = parse_user(&item.user)?;
        // Paths in the zip are made from the parsed ids and from removals
        // the archive lists, never from the request's strings.
        let path = match &item.removal {
            None => Some(interpretations::path_of(&radargram, &user)),
            Some(removal) => archived
                .iter()
                .find(|a| a.radargram == radargram && &a.removal == removal && a.user == user)
                .map(interpretations::archived_path),
        };
        let text = match &path {
            Some(path) => store
                .read(path)
                .map_err(|e| ApiError::internal("store_failed", e.to_string()))?
                .map(|stored| stored.text),
            None => None,
        };
        let entry = ManifestEntry {
            radargram_id: &item.radargram_id,
            user: &item.user,
            removal: item.removal.as_deref(),
            path: None,
        };
        let (Some(path), Some(text)) = (path, text) else {
            missing.push(entry);
            continue;
        };
        let name = path
            .strip_prefix(crate::project::INTERPRETATIONS_DIR)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        zip.start_file(name.as_str(), options).map_err(zip_failed)?;
        zip.write_all(text.as_bytes())
            .map_err(|e| ApiError::internal("zip_failed", e.to_string()))?;
        files.push(ManifestEntry {
            path: Some(name),
            ..entry
        });
    }

    let manifest = serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "files": files,
        "missing": missing,
    });
    zip.start_file("manifest.json", options)
        .map_err(zip_failed)?;
    zip.write_all(
        serde_json::to_string_pretty(&manifest)
            .expect("a manifest serializes")
            .as_bytes(),
    )
    .map_err(|e| ApiError::internal("zip_failed", e.to_string()))?;
    Ok(zip.finish().map_err(zip_failed)?.into_inner())
}
