//! Batches: reprocess every stale radargram of a processing profile, and
//! commit each without review (#382, phase 6).
//!
//! ```text
//! POST /api/v1/processing/profiles/{profile}/reprocess   start one
//! GET  /api/v1/processing/batches                        recent batches
//! GET  /api/v1/processing/batches/{batch}                one, item by item
//! ```
//!
//! What #382 decided, as built:
//!
//! - A batch takes every member that is `stale` or `not_processed` (and,
//!   when asked, `older_processing`). A member with no raw data is listed
//!   as skipped, and never counted as stale.
//! - It is processed at the profile version it started with, as `Batch`
//!   jobs, which the queue runs after drafts. They are submitted
//!   [`IN_FLIGHT`] at a time rather than all at once, so a batch neither
//!   fills the queue nor hits the per-person cap, and a draft waits at most
//!   for the batch items already running.
//! - Each finished item is committed with no review: the replace's report
//!   is not shown to anyone, but its refusals still hold. Before committing,
//!   the radargram must still be the revision the item started from;
//!   someone else's commit in between makes the item `skipped`, not lost.
//! - A newer version of the profile supersedes the items not yet started.
//! - Every batch, and how each item ended, is appended to
//!   `processing/batches/audit.jsonl`, so the history outlives the server.
//!   A restart stops the batches running; their unfinished items read as
//!   `interrupted`, and are stale again for the next batch.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::draft_routes::{prepare, submit_error, Prepared, Processing};
use super::processing::{JobState, Priority};
use super::routes::ApiError;
use crate::identity::{ProfileId, RadargramId};
use crate::project::processing::{self, ProcessingState};
use crate::project::roles::Role;
use crate::project::store::DocumentStore;

/// How many of a batch's items are queued or running at once.
pub const IN_FLIGHT: usize = 2;
/// How often a batch looks at its jobs.
const TICK: Duration = Duration::from_millis(500);

/// How one item of a batch ended, or where it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Waiting,
    Processing {
        job: u64,
    },
    Committed {
        revision_id: String,
    },
    /// Processed to exactly what is served already.
    AlreadyCurrent,
    Skipped {
        reason: String,
    },
    Failed {
        message: String,
    },
    /// The server stopped before it finished.
    Interrupted,
}

impl Outcome {
    fn is_finished(&self) -> bool {
        !matches!(self, Outcome::Waiting | Outcome::Processing { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub radargram_id: RadargramId,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub id: String,
    pub profile: ProfileId,
    /// The profile version the batch processes with.
    pub version: u32,
    pub started_by: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub items: Vec<Item>,
}

/// One line of the batch history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Entry {
    Started {
        batch: Batch,
    },
    Item {
        batch_id: String,
        radargram_id: RadargramId,
        outcome: Outcome,
    },
    Finished {
        batch_id: String,
        at: String,
    },
}

fn ledger(project: &crate::project::Project) -> DocumentStore {
    DocumentStore::new(project.data_dir().join("processing").join("batches"))
}

fn append(project: &crate::project::Project, entry: &Entry) {
    if let Err(e) = crate::project::jsonl::append(&ledger(project), entry) {
        tracing::warn!(error = %e, "batch history not written");
    }
}

/// The batches this project is running, and what their jobs read.
#[derive(Default)]
pub struct Batches {
    inner: Mutex<HashMap<String, Live>>,
}

struct Live {
    batch: Batch,
    /// The raw files of the items being processed.
    hashes: HashMap<RadargramId, Vec<String>>,
}

impl Batches {
    /// The raw files a running batch item may still read.
    pub fn in_use(&self) -> HashSet<String> {
        let inner = self.inner.lock().expect("the batches lock is not poisoned");
        inner
            .values()
            .flat_map(|live| live.hashes.values().flatten().cloned())
            .collect()
    }

    fn get(&self, id: &str) -> Option<Batch> {
        let inner = self.inner.lock().expect("the batches lock is not poisoned");
        inner.get(id).map(|live| live.batch.clone())
    }
}

/// Every batch in the history, newest first, with the running ones as
/// they are now.
fn history(state: &AppState, project: &crate::project::Project) -> Vec<Batch> {
    let entries: Vec<Entry> = crate::project::jsonl::read(&ledger(project)).unwrap_or_default();
    let mut batches: Vec<Batch> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        match entry {
            Entry::Started { batch } => {
                index.insert(batch.id.clone(), batches.len());
                batches.push(batch);
            }
            Entry::Item {
                batch_id,
                radargram_id,
                outcome,
            } => {
                if let Some(batch) = index.get(&batch_id).map(|i| &mut batches[*i]) {
                    if let Some(item) = batch
                        .items
                        .iter_mut()
                        .find(|item| item.radargram_id == radargram_id)
                    {
                        item.outcome = outcome;
                    }
                }
            }
            Entry::Finished { batch_id, at } => {
                if let Some(batch) = index.get(&batch_id).map(|i| &mut batches[*i]) {
                    batch.finished_at = Some(at);
                }
            }
        }
    }
    for batch in &mut batches {
        match state.batches.get(&batch.id) {
            Some(live) => *batch = live,
            None if batch.finished_at.is_none() => {
                for item in &mut batch.items {
                    if !item.outcome.is_finished() {
                        item.outcome = Outcome::Interrupted;
                    }
                }
            }
            None => {}
        }
    }
    batches.reverse();
    batches
}

#[derive(Debug, Default, Deserialize)]
pub struct ReprocessBody {
    /// Also reprocess what an older Ridal processed exactly as the profile
    /// says (`older_processing`). Off by default: that is not stale.
    #[serde(default)]
    pub include_older: bool,
}

pub async fn reprocess(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(profile): Path<String>,
    Json(body): Json<ReprocessBody>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require(Role::Operator, "reprocess a profile's radargrams")?;
    let project = state.project.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "not_a_project",
            "This catalog is not a Ridal project, so it has nothing to reprocess.",
        )
    })?;
    if state.processing.is_none() {
        return Err(ApiError::conflict(
            "processing_unavailable",
            "This server does not process radargrams.",
        ));
    }
    let profile_id =
        ProfileId::new(&profile).map_err(|e| ApiError::bad_request("invalid_profile_id", e))?;
    let (profile, _) = processing::read_profile(project.documents(), &profile_id)
        .map_err(super::processing_routes::processing_error)?
        .ok_or_else(|| {
            super::processing_routes::processing_error(processing::ProcessingError::NoSuchProfile(
                profile_id.clone(),
            ))
        })?;

    let mut items = Vec::new();
    for status in super::processing_routes::statuses(&state, project, None).await? {
        if status.profile != profile_id {
            continue;
        }
        let outcome = match status.state {
            ProcessingState::Stale { .. } | ProcessingState::NotProcessed => Outcome::Waiting,
            ProcessingState::OlderProcessing { .. } if body.include_older => Outcome::Waiting,
            ProcessingState::NoRawData => Outcome::Skipped {
                reason: "no raw data in the project".to_string(),
            },
            ProcessingState::Current { .. } | ProcessingState::OlderProcessing { .. } => continue,
        };
        items.push(Item {
            radargram_id: status.radargram_id,
            outcome,
        });
    }
    if !items.iter().any(|item| item.outcome == Outcome::Waiting) {
        return Err(ApiError::conflict(
            "nothing_to_reprocess",
            format!(
                "Every radargram in '{}' that can be processed is up to date.",
                profile.id
            ),
        ));
    }

    let batch = Batch {
        id: super::lifecycle_routes::uuid(),
        profile: profile.id.clone(),
        version: profile.current().version,
        started_by: caller.audit_name(),
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        items,
    };
    append(
        project,
        &Entry::Started {
            batch: batch.clone(),
        },
    );
    for item in batch.items.iter().filter(|i| i.outcome.is_finished()) {
        append(
            project,
            &Entry::Item {
                batch_id: batch.id.clone(),
                radargram_id: item.radargram_id.clone(),
                outcome: item.outcome.clone(),
            },
        );
    }
    state
        .batches
        .inner
        .lock()
        .expect("the batches lock is not poisoned")
        .insert(
            batch.id.clone(),
            Live {
                batch: batch.clone(),
                hashes: HashMap::new(),
            },
        );
    tokio::spawn(drive(Arc::clone(&state), caller, batch.id.clone()));
    Ok((StatusCode::ACCEPTED, Json(batch)))
}

/// An item being processed, and what its commit needs.
struct Running {
    job: u64,
    prepared_base: Option<String>,
    output: std::path::PathBuf,
    work: std::path::PathBuf,
}

/// Run batch `id` to its end.
async fn drive(state: Arc<AppState>, caller: Caller, id: String) {
    let Some(project) = state.project.as_ref() else {
        return;
    };
    let Some(queue) = state.processing.clone() else {
        return;
    };
    let Some(batch) = state.batches.get(&id) else {
        return;
    };
    let mut waiting: VecDeque<RadargramId> = batch
        .items
        .iter()
        .filter(|i| i.outcome == Outcome::Waiting)
        .map(|i| i.radargram_id.clone())
        .collect();
    let mut running: HashMap<RadargramId, Running> = HashMap::new();

    let set = |radargram: &RadargramId, outcome: Outcome| {
        let mut inner = state
            .batches
            .inner
            .lock()
            .expect("the batches lock is not poisoned");
        if let Some(live) = inner.get_mut(&id) {
            if let Some(item) = live
                .batch
                .items
                .iter_mut()
                .find(|i| &i.radargram_id == radargram)
            {
                item.outcome = outcome.clone();
            }
            if outcome.is_finished() {
                live.hashes.remove(radargram);
            }
        }
        if outcome.is_finished() {
            append(
                project,
                &Entry::Item {
                    batch_id: id.clone(),
                    radargram_id: radargram.clone(),
                    outcome,
                },
            );
        }
    };

    while !(waiting.is_empty() && running.is_empty()) {
        // A newer version of the profile supersedes what has not started.
        let now_at = processing::read_profile(project.documents(), &batch.profile)
            .ok()
            .flatten()
            .map(|(p, _)| p.current().version);
        if now_at.is_some_and(|v| v != batch.version) {
            for radargram in waiting.drain(..) {
                set(
                    &radargram,
                    Outcome::Skipped {
                        reason: format!(
                            "superseded by version {} of the profile",
                            now_at.unwrap_or_default()
                        ),
                    },
                );
            }
        }

        while running.len() < IN_FLIGHT {
            let Some(radargram) = waiting.pop_front() else {
                break;
            };
            match start(&state, project, &caller, &batch, &radargram).await {
                Ok((prepared, hashes)) => {
                    let Prepared {
                        request,
                        base_revision,
                        output,
                        work,
                        ..
                    } = prepared;
                    match queue.submit(request) {
                        Ok(job) => {
                            state
                                .batches
                                .inner
                                .lock()
                                .expect("the batches lock is not poisoned")
                                .get_mut(&id)
                                .map(|live| live.hashes.insert(radargram.clone(), hashes));
                            set(&radargram, Outcome::Processing { job });
                            running.insert(
                                radargram,
                                Running {
                                    job,
                                    prepared_base: base_revision,
                                    output,
                                    work,
                                },
                            );
                        }
                        Err(super::processing::SubmitError::TooMany { .. }) => {
                            // The person's own drafts are using their share;
                            // try again on the next tick.
                            let _ = std::fs::remove_dir_all(&work);
                            waiting.push_front(radargram);
                            break;
                        }
                        Err(e) => {
                            let _ = std::fs::remove_dir_all(&work);
                            set(
                                &radargram,
                                Outcome::Failed {
                                    message: submit_error(e).message().to_string(),
                                },
                            );
                        }
                    }
                }
                Err(message) => set(&radargram, Outcome::Failed { message }),
            }
        }

        let mut finished = Vec::new();
        for (radargram, item) in &running {
            let Some(status) = queue.status(item.job) else {
                finished.push((radargram.clone(), Outcome::Interrupted));
                continue;
            };
            let outcome = match status.state {
                JobState::Queued | JobState::Running => continue,
                JobState::Done { .. } => commit(&state, &caller, radargram, item).await,
                JobState::Failed { message } => Outcome::Failed { message },
                JobState::TimedOut => Outcome::Failed {
                    message: "stopped at the processing timeout".to_string(),
                },
                JobState::Cancelled => Outcome::Skipped {
                    reason: "cancelled".to_string(),
                },
            };
            finished.push((radargram.clone(), outcome));
        }
        for (radargram, outcome) in finished {
            if let Some(item) = running.remove(&radargram) {
                let _ = std::fs::remove_dir_all(&item.work);
                let _ = std::fs::remove_file(&item.output);
            }
            set(&radargram, outcome);
        }
        tokio::time::sleep(TICK).await;
    }

    let at = chrono::Utc::now().to_rfc3339();
    append(
        project,
        &Entry::Finished {
            batch_id: id.clone(),
            at: at.clone(),
        },
    );
    state
        .batches
        .inner
        .lock()
        .expect("the batches lock is not poisoned")
        .remove(&id);
    tracing::info!(
        batch = id.as_str(),
        profile = batch.profile.as_str(),
        "batch finished"
    );
}

/// Prepare one item's job at the batch's profile version.
async fn start(
    state: &AppState,
    project: &crate::project::Project,
    caller: &Caller,
    batch: &Batch,
    radargram: &RadargramId,
) -> Result<(Prepared, Vec<String>), String> {
    let (profile, _) = processing::read_profile(project.documents(), &batch.profile)
        .map_err(|e| e.to_string())?
        .ok_or("the profile has been deleted")?;
    let version = profile
        .versions
        .iter()
        .find(|v| v.version == batch.version)
        .ok_or("the profile version has gone")?;
    let (assignments, _) =
        processing::read_assignments(project.documents()).map_err(|e| e.to_string())?;
    let assignment = assignments
        .radargrams
        .get(radargram)
        .filter(|a| a.profile == batch.profile)
        .ok_or("no longer in the profile")?;
    let steps = assignment
        .pre_steps
        .iter()
        .chain(version.steps.iter())
        .cloned()
        .collect();
    let prepared = prepare(
        state,
        project,
        radargram,
        Processing {
            steps,
            medium_velocity: version.medium_velocity,
            antenna_mhz_override: None,
            antenna_separation_override: None,
        },
        caller.display_name(),
        Priority::Batch,
        &project.cache_dir().join("batches"),
    )
    .await
    .map_err(|e| e.message().to_string())?;
    let hashes = prepared.hashes.clone();
    Ok((prepared, hashes))
}

/// Install a finished item, through the same routes a person uses.
async fn commit(
    state: &Arc<AppState>,
    caller: &Caller,
    radargram: &RadargramId,
    item: &Running,
) -> Outcome {
    let current = state
        .catalog()
        .find_entry(radargram.as_str())
        .map(|e| e.revision_id.to_string());
    if current != item.prepared_base {
        return Outcome::Skipped {
            reason: "changed by someone else after the batch started".to_string(),
        };
    }
    let file = match tokio::fs::File::open(&item.output).await {
        Ok(file) => file,
        Err(e) => {
            return Outcome::Failed {
                message: format!("the processed file cannot be read: {e}"),
            }
        }
    };
    let length = file.metadata().await.map(|m| m.len()).unwrap_or_default();
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_LENGTH, length.into());
    let filename = Some(format!("batch item {radargram}"));

    let staged = match current {
        Some(_) => super::replace_routes::stage_replacement(
            State(Arc::clone(state)),
            caller.clone(),
            Path(radargram.to_string()),
            axum::extract::Query(super::replace_routes::ReplaceQuery { filename }),
            headers,
            body,
        )
        .await
        .map(IntoResponse::into_response),
        None => {
            let added = super::lifecycle_routes::upload_dataset(
                State(Arc::clone(state)),
                caller.clone(),
                axum::extract::Query(super::lifecycle_routes::UploadQuery { filename }),
                headers,
                body,
            )
            .await
            .map(IntoResponse::into_response);
            return match added {
                Ok(response) => match json_of(response).await {
                    Some(body) => Outcome::Committed {
                        revision_id: body["revision_id"].as_str().unwrap_or_default().to_string(),
                    },
                    None => Outcome::Failed {
                        message: "the new radargram was added, but its answer was unreadable"
                            .to_string(),
                    },
                },
                Err(e) => Outcome::Failed {
                    message: e.message().to_string(),
                },
            };
        }
    };
    let staged = match staged {
        Ok(response) => json_of(response).await,
        Err(e) => {
            return Outcome::Failed {
                message: e.message().to_string(),
            }
        }
    };
    let Some(staged) = staged else {
        return Outcome::Failed {
            message: "the replacement's report was unreadable".to_string(),
        };
    };
    if staged["report"]["revision_id_collision"] == serde_json::Value::Bool(true) {
        let token = staged["token"].as_str().unwrap_or_default().to_string();
        let _ = super::replace_routes::discard_replacement(
            State(Arc::clone(state)),
            caller.clone(),
            Path((radargram.to_string(), token)),
        )
        .await;
        return Outcome::AlreadyCurrent;
    }
    let token = staged["token"].as_str().unwrap_or_default().to_string();
    match super::replace_routes::commit_replacement(
        State(Arc::clone(state)),
        caller.clone(),
        Path((radargram.to_string(), token)),
    )
    .await
    {
        Ok(response) => {
            let body = json_of(response.into_response()).await.unwrap_or_default();
            Outcome::Committed {
                revision_id: body["to_revision"].as_str().unwrap_or_default().to_string(),
            }
        }
        Err(e) => Outcome::Failed {
            message: e.message().to_string(),
        },
    }
}

async fn json_of(response: axum::response::Response) -> Option<serde_json::Value> {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[derive(Debug, Serialize)]
pub struct BatchList {
    pub batches: Vec<Batch>,
}

pub async fn list_batches(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<BatchList>, ApiError> {
    caller.require(Role::Operator, "see the batch history")?;
    let project = super::interp_routes::readable_project(&state)?;
    let mut batches = history(&state, project);
    batches.truncate(50);
    Ok(Json(BatchList { batches }))
}

pub async fn get_batch(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(batch): Path<String>,
) -> Result<Json<Batch>, ApiError> {
    caller.require(Role::Operator, "see a batch")?;
    let project = super::interp_routes::readable_project(&state)?;
    history(&state, project)
        .into_iter()
        .find(|b| b.id == batch)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("no_such_batch", "There is no batch with that id."))
}
