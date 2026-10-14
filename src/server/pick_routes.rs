//! The pick manager's view of a project's interpretations (#178, #162).
//!
//! One row per stored interpretation -- live, on an older revision, left
//! behind by a radargram that is no longer served, or archived by a
//! removal -- described by numbers and never by its picks. Whose rows a
//! caller sees follows the role: a picker their own, an operator and an
//! admin everyone's. Reading the picks behind a row is still the existing
//! routes' decision, under the `Picks` download scope.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::{self, readable_project};
use super::routes::ApiError;
use crate::identity::{RadargramId, UserId};
use crate::interp::carry;
use crate::project::interpretations::{self, InterpretationError, StoredInterpretation};
use crate::project::pick_summary::{self, DocumentStats};
use crate::project::roles::{DownloadScope, Role};
use crate::project::{overrides, revisions, Project};

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
                        let axes = current_axes.get_or_insert_with(|| {
                            match state.absolute_path(entry) {
                                Ok(path) => interp_routes::current_axes(project, &path, entry),
                                Err(e) => {
                                    tracing::warn!(
                                        radargram = radargram.as_str(),
                                        error = e.as_str(),
                                        "cannot carry picks for the pick manager"
                                    );
                                    crate::interp::anchors::Axes::default()
                                }
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
        // A path names its user, so these are everyone's too.
        unreadable: if everyone {
            scan.unreadable
        } else {
            Vec::new()
        },
    }))
}
