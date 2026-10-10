//! Drafts: processing a radargram's raw data on the server, to look at
//! before it becomes the radargram (#382, phase 5).
//!
//! ```text
//! POST   /api/v1/sources/{radargram_id}/drafts   process the source; answers the job
//! GET    /api/v1/drafts                          the caller's drafts
//! GET    /api/v1/drafts/{job}                    one, and how far it has got
//! DELETE /api/v1/drafts/{job}                    stop it, or throw it away
//! GET    /api/v1/drafts/{job}/views/{view}/overview   look at it
//! GET    /api/v1/drafts/{job}/download           the processed file
//! POST   /api/v1/drafts/{job}/commit             make it the radargram
//! ```
//!
//! As agreed in #382: only an `operator` makes drafts, a draft is private to
//! whoever made it, and downloading one needs the same scope as downloading
//! the radargram. A draft is processed by the site's queue
//! ([`super::processing`]) from the raw data the source names
//! ([`crate::project::raw`]).
//!
//! Committing goes through what already exists rather than beside it. For
//! a radargram that is served, it stages the draft as a replacement, which
//! answers the consequence report for every interpretation drawn on it;
//! the caller then commits that as any replacement is committed. For a new
//! id, it installs the draft as an upload. Either way it first checks that
//! the radargram is still the revision the draft was made against: someone
//! who committed in between is named, not overwritten.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::app::{AppState, OpenRadargram};
use super::auth::Caller;
use super::interp_routes::parse_radargram;
use super::processing::{JobRequest, JobState, JobStatus, Priority};
use super::routes::ApiError;
use crate::identity::RadargramId;
use crate::project::raw::{self, NamedFile};
use crate::project::roles::{DownloadScope, Role};

/// How long a draft is kept after it was made.
pub const DRAFT_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// One draft, as this project remembers it while the server runs.
struct Draft {
    owner: String,
    radargram: RadargramId,
    /// The revision served when the draft was asked for; `None` for an id
    /// with no processed file yet.
    base_revision: Option<String>,
    output: PathBuf,
    /// Where its raw files are laid out while it runs.
    work: PathBuf,
    /// Its raw files, kept from a sweep while it may still read them.
    hashes: Vec<String>,
    created: std::time::Instant,
    /// Opened on first look.
    view: Option<Arc<OpenRadargram>>,
}

#[derive(Default)]
pub struct Drafts {
    inner: Mutex<HashMap<u64, Draft>>,
}

impl Drafts {
    /// The raw files a draft that is not finished may still read.
    pub fn in_use(&self, queue: Option<&super::processing::ProcessingQueue>) -> HashSet<String> {
        let inner = self.inner.lock().expect("the drafts lock is not poisoned");
        inner
            .iter()
            .filter(|(id, _)| {
                queue
                    .and_then(|q| q.status(**id))
                    .is_none_or(|s| !s.state.is_finished())
            })
            .flat_map(|(_, d)| d.hashes.iter().cloned())
            .collect()
    }
}

fn queue(state: &AppState) -> Result<&Arc<super::processing::ProcessingQueue>, ApiError> {
    state.processing.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "processing_unavailable",
            "This server does not process radargrams.",
        )
    })
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
            "This catalog is not a Ridal project, so it has no raw data to process.",
        )
    })
}

fn not_found() -> ApiError {
    // The same answer for someone else's draft as for none: a draft is
    // private, and its existence is part of that.
    ApiError::not_found("no_such_draft", "You have no draft with that number.")
}

/// What `POST /api/v1/sources/{radargram_id}/drafts` takes.
#[derive(Debug, Deserialize)]
pub struct DraftBody {
    pub steps: Vec<String>,
    /// m/ns. Default: that of ice, as for `ridal process`.
    #[serde(default)]
    pub medium_velocity: Option<f32>,
    #[serde(default)]
    pub antenna_mhz_override: Option<f32>,
    #[serde(default)]
    pub antenna_separation_override: Option<f32>,
}

/// A draft as the API gives it.
#[derive(Debug, Serialize)]
pub struct DraftView {
    pub job: u64,
    pub radargram_id: String,
    pub base_revision: Option<String>,
    #[serde(flatten)]
    pub status: JobStatusView,
}

/// [`JobStatus`] without what is the server's business: where its files
/// are, and the key it was queued under.
#[derive(Debug, Serialize)]
pub struct JobStatusView {
    #[serde(flatten)]
    pub state: JobState,
    pub step: Option<super::processing::StepProgress>,
}

impl From<JobStatus> for JobStatusView {
    fn from(status: JobStatus) -> Self {
        Self {
            state: status.state,
            step: status.step,
        }
    }
}

fn view_of(state: &AppState, job: u64, draft: &Draft) -> DraftView {
    let status = state
        .processing
        .as_ref()
        .and_then(|q| q.status(job))
        .map(JobStatusView::from)
        .unwrap_or(JobStatusView {
            // The queue forgets finished jobs after a while; the file is
            // what says how it ended.
            state: if draft.output.is_file() {
                JobState::Done {
                    revision_id: revision_of(&draft.output)
                        .map(|r| r.to_string())
                        .unwrap_or_default(),
                }
            } else {
                JobState::Failed {
                    message: "the server no longer knows how this draft ended".to_string(),
                }
            },
            step: None,
        });
    DraftView {
        job,
        radargram_id: draft.radargram.to_string(),
        base_revision: draft.base_revision.clone(),
        status,
    }
}

fn revision_of(path: &std::path::Path) -> Option<crate::identity::RevisionId> {
    match crate::io::inspect_ridal_netcdf(path).ok()? {
        crate::io::RidalNetcdfKind::Supported(meta) => Some(meta.revision_id()),
        _ => None,
    }
}

/// Forget drafts past [`DRAFT_TTL`], and remove what finished drafts no
/// longer need laid out.
fn tidy(state: &AppState) {
    let mut inner = state
        .drafts
        .inner
        .lock()
        .expect("the drafts lock is not poisoned");
    let finished = |job: u64| {
        state
            .processing
            .as_ref()
            .and_then(|q| q.status(job))
            .is_none_or(|s| s.state.is_finished())
    };
    inner.retain(|job, draft| {
        if finished(*job) {
            let _ = std::fs::remove_dir_all(&draft.work);
            if draft.created.elapsed() > DRAFT_TTL {
                let _ = std::fs::remove_file(&draft.output);
                return false;
            }
        }
        true
    });
}

pub async fn create_draft(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(radargram_id): Path<String>,
    Json(body): Json<DraftBody>,
) -> Result<Response, ApiError> {
    let project = project_for(&state, &caller, "process a radargram")?;
    let queue = Arc::clone(queue(&state)?);
    let id = parse_radargram(&radargram_id)?;
    tidy(&state);

    let (source, source_version) = raw::read_source(project.documents(), &id)
        .map_err(|e| ApiError::internal("source_read_failed", e))?
        .ok_or_else(|| {
            ApiError::not_found(
                "no_source",
                format!("'{id}' has no raw data in this project to process."),
            )
        })?;
    let steps = body
        .steps
        .iter()
        .map(|step| crate::steps::parse_one(step).map(|parsed| parsed.canonical))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ApiError::bad_request("invalid_steps", e))?;
    let velocity = body.medium_velocity.unwrap_or(0.168);
    if !(velocity.is_finite() && velocity > 0.) {
        return Err(ApiError::bad_request(
            "invalid_velocity",
            "The medium velocity must be a positive number of m/ns.",
        ));
    }

    // What it is made against, and what it is called: the served
    // radargram's names, so a committed draft keeps them.
    let catalog = state.catalog();
    let entry = catalog.find_entry(id.as_str());
    let base_revision = entry.map(|e| e.revision_id.to_string());
    let display_name = entry.and_then(|e| e.display_name.as_ref().map(|n| n.to_string()));
    let group_name = entry.and_then(|e| e.group_name.as_ref().map(|n| n.to_string()));
    let group_id = entry.and_then(|e| e.group_id.as_ref().map(|g| g.to_string()));
    drop(catalog);

    // One job for one person asking for one thing twice. The key is not
    // the revision the job will produce: that is only known once the
    // positions are projected, which is the job's work.
    let mut hasher = blake3::Hasher::new();
    for part in [
        caller.display_name(),
        id.as_str(),
        source_version.as_str(),
        &steps.join("\n"),
        &velocity.to_string(),
        &format!("{:?}", body.antenna_mhz_override),
        &format!("{:?}", body.antenna_separation_override),
    ] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let key = hasher.finalize().to_hex().to_string();

    let data_dir = project.data_dir().to_path_buf();
    let name = super::lifecycle_routes::uuid();
    let work = data_dir.join(raw::RAW_DIR).join("jobs").join(&name);
    let drafts_dir = project.cache_dir().join("drafts");
    std::fs::create_dir_all(&drafts_dir)
        .map_err(|e| ApiError::internal("draft_failed", e.to_string()))?;
    let output = drafts_dir.join(format!("{name}.nc"));

    let recordings: Vec<Vec<NamedFile>> = source
        .bundles
        .iter()
        .map(|bundle| {
            bundle
                .files
                .iter()
                .map(|f| NamedFile {
                    name: f.file.name.clone(),
                    blake3: f.file.blake3.clone(),
                })
                .collect()
        })
        .collect();
    let laid_out = {
        let data_dir = data_dir.clone();
        let work = work.clone();
        tokio::task::spawn_blocking(move || raw::materialize(&data_dir, &recordings, &work))
            .await
            .map_err(|e| ApiError::internal("draft_failed", e.to_string()))?
            .map_err(|e| ApiError::conflict("source_unavailable", e))?
    };
    // A processing job holds the whole radargram and works on copies of
    // it: a generous multiple of the raw data is the estimate.
    let estimate_bytes = source
        .bundles
        .iter()
        .flat_map(|b| &b.files)
        .filter_map(|f| raw::file_path(&data_dir, &f.file.blake3))
        .filter_map(|path| path.metadata().ok())
        .map(|meta| meta.len())
        .sum::<u64>()
        .saturating_mul(8);

    let spec = crate::job::JobSpec {
        inputs: laid_out
            .into_iter()
            .map(|resolved| resolved.header)
            .collect(),
        output: output.clone(),
        steps,
        medium_velocity: velocity,
        crs: source.crs.clone(),
        dem: source
            .dem
            .as_ref()
            .and_then(|dem| raw::file_path(&data_dir, &dem.blake3)),
        cor: None,
        antenna_mhz_override: body.antenna_mhz_override,
        antenna_separation_override: body.antenna_separation_override,
        radargram_id: id.to_string(),
        display_name,
        group_name,
        group_id,
    };
    let job = queue
        .submit(JobRequest {
            key,
            owner: caller.display_name().to_string(),
            priority: Priority::Draft,
            spec,
            estimate_bytes,
        })
        .map_err(|e| match e {
            super::processing::SubmitError::TooMany { .. } => {
                ApiError::too_many_requests("too_many_jobs", e.to_string())
            }
            super::processing::SubmitError::TooLarge { .. } => {
                ApiError::bad_request("job_too_large", e.to_string())
            }
        })?;

    let mut inner = state
        .drafts
        .inner
        .lock()
        .expect("the drafts lock is not poisoned");
    match inner.entry(job) {
        // The same draft asked for again: it is already on its way.
        std::collections::hash_map::Entry::Occupied(_) => {
            let _ = std::fs::remove_dir_all(&work);
        }
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(Draft {
                owner: caller.display_name().to_string(),
                radargram: id,
                base_revision,
                output,
                work,
                hashes: source.hashes().map(str::to_string).collect(),
                created: std::time::Instant::now(),
                view: None,
            });
        }
    }
    let view = view_of(&state, job, &inner[&job]);
    Ok((StatusCode::ACCEPTED, Json(view)).into_response())
}

#[derive(Debug, Serialize)]
pub struct DraftList {
    pub drafts: Vec<DraftView>,
}

pub async fn list_drafts(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<Json<DraftList>, ApiError> {
    caller.require(Role::Operator, "see your drafts")?;
    tidy(&state);
    let inner = state
        .drafts
        .inner
        .lock()
        .expect("the drafts lock is not poisoned");
    let mut drafts: Vec<DraftView> = inner
        .iter()
        .filter(|(_, d)| d.owner == caller.display_name())
        .map(|(job, d)| view_of(&state, *job, d))
        .collect();
    drafts.sort_by_key(|d| d.job);
    Ok(Json(DraftList { drafts }))
}

/// The caller's draft `job`, through `f`.
fn with_draft<T>(
    state: &AppState,
    caller: &Caller,
    job: u64,
    f: impl FnOnce(&mut Draft) -> T,
) -> Result<T, ApiError> {
    caller.require(Role::Operator, "use a draft")?;
    let mut inner = state
        .drafts
        .inner
        .lock()
        .expect("the drafts lock is not poisoned");
    match inner.get_mut(&job) {
        Some(draft) if draft.owner == caller.display_name() => Ok(f(draft)),
        _ => Err(not_found()),
    }
}

pub async fn get_draft(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(job): Path<u64>,
) -> Result<Json<DraftView>, ApiError> {
    let view = with_draft(&state, &caller, job, |draft| view_of(&state, job, draft))?;
    Ok(Json(view))
}

pub async fn delete_draft(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(job): Path<u64>,
) -> Result<StatusCode, ApiError> {
    with_draft(&state, &caller, job, |_| ())?;
    if let Some(queue) = &state.processing {
        queue.cancel(job);
    }
    let draft = state
        .drafts
        .inner
        .lock()
        .expect("the drafts lock is not poisoned")
        .remove(&job);
    if let Some(draft) = draft {
        // Its render service goes with the entry; the reader pool holds it
        // only weakly.
        drop(draft.view);
        let _ = std::fs::remove_file(&draft.output);
        let _ = std::fs::remove_dir_all(&draft.work);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The finished draft's file, or why there is none yet.
fn finished_output(state: &AppState, caller: &Caller, job: u64) -> Result<PathBuf, ApiError> {
    let (output, view) = with_draft(state, caller, job, |draft| {
        (draft.output.clone(), view_of(state, job, draft))
    })?;
    match view.status.state {
        JobState::Done { .. } if output.is_file() => Ok(output),
        JobState::Queued | JobState::Running => Err(ApiError::conflict(
            "draft_not_ready",
            "This draft is still being processed.",
        )),
        _ => Err(ApiError::conflict(
            "draft_failed",
            "This draft did not finish, so there is nothing to show.",
        )),
    }
}

pub async fn download_draft(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(job): Path<u64>,
) -> Result<Response, ApiError> {
    caller.require_download(DownloadScope::All, "a draft of a radargram")?;
    let output = finished_output(&state, &caller, job)?;
    let radargram = with_draft(&state, &caller, job, |d| d.radargram.to_string())?;
    let file = tokio::fs::File::open(&output)
        .await
        .map_err(|e| ApiError::internal("draft_open_failed", e.to_string()))?;
    let length = file
        .metadata()
        .await
        .map(|m| m.len())
        .map_err(|e| ApiError::internal("draft_open_failed", e.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-netcdf".to_string()),
            (header::CONTENT_LENGTH, length.to_string()),
            super::routes::attachment(&format!("{radargram}-draft-{job}.nc")),
        ],
        axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}

pub async fn draft_overview(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path((job, view)): Path<(u64, String)>,
    Query(query): Query<super::routes::ProfileQuery>,
) -> Result<Response, ApiError> {
    let output = finished_output(&state, &caller, job)?;
    let dataset_view = super::routes::lookup_view(&view)?;
    let profile = super::routes::lookup_profile(query.profile.as_deref().unwrap_or("default"))?;

    let opened = with_draft(&state, &caller, job, |draft| draft.view.clone())?;
    let radargram = match opened {
        Some(radargram) => radargram,
        None => {
            let revision = revision_of(&output).ok_or_else(|| {
                ApiError::internal("draft_unreadable", "The draft cannot be read.")
            })?;
            let config = state.render_config();
            let (service, shape) =
                super::render_service::RenderService::open_lazily(&output, revision, &config)
                    .map_err(|e| ApiError::internal("draft_unreadable", e))?;
            let radargram = Arc::new(OpenRadargram {
                service: Arc::new(std::sync::Mutex::new(service)),
                shape,
            });
            with_draft(&state, &caller, job, |draft| {
                draft.view.get_or_insert(radargram).clone()
            })?
        }
    };

    let (source_height, width) = radargram.shape;
    let range = crate::render::topo::ElevationRange::default();
    let height = super::routes::resolve_view_height(
        state.render_config().open_readers,
        Arc::clone(&radargram),
        dataset_view,
        source_height,
        range,
    )
    .await?;
    let spec = crate::render::grid::OverviewSpec::new(width, height, 512);
    let bytes = super::routes::get_or_build_overview(
        Arc::clone(&state),
        radargram,
        spec,
        dataset_view,
        profile.clone(),
        range,
    )
    .await?;
    Ok(super::routes::image_response(bytes, &profile))
}

pub async fn commit_draft(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Path(job): Path<u64>,
) -> Result<Response, ApiError> {
    project_for(&state, &caller, "commit a draft")?;
    let output = finished_output(&state, &caller, job)?;
    let (radargram, base) = with_draft(&state, &caller, job, |d| {
        (d.radargram.clone(), d.base_revision.clone())
    })?;

    let current = state
        .catalog()
        .find_entry(radargram.as_str())
        .map(|e| e.revision_id.to_string());
    if current != base {
        return Err(ApiError::conflict(
            "radargram_moved",
            match (&base, &current) {
                (Some(_), Some(now)) => format!(
                    "'{radargram}' has been replaced since this draft was made (it is now \
                     revision {now}). Make the draft again from what is there now."
                ),
                (Some(_), None) => format!(
                    "'{radargram}' was removed since this draft was made, so there is \
                     nothing for it to replace."
                ),
                (None, _) => format!(
                    "'{radargram}' was added since this draft was made. Make the draft \
                     again, so it is measured against what is there now."
                ),
            },
        ));
    }

    let file = tokio::fs::File::open(&output)
        .await
        .map_err(|e| ApiError::internal("draft_open_failed", e.to_string()))?;
    let length = file.metadata().await.map(|m| m.len()).unwrap_or_default();
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_LENGTH, length.into());
    let filename = Some(format!("draft {job} of {radargram}"));

    let response = match current {
        // Staged as any replacement is: the answer is the consequence
        // report and the token that commits it.
        Some(_) => super::replace_routes::stage_replacement(
            State(Arc::clone(&state)),
            caller,
            Path(radargram.to_string()),
            Query(super::replace_routes::ReplaceQuery { filename }),
            headers,
            body,
        )
        .await?
        .into_response(),
        None => super::lifecycle_routes::upload_dataset(
            State(Arc::clone(&state)),
            caller,
            Query(super::lifecycle_routes::UploadQuery { filename }),
            headers,
            body,
        )
        .await?
        .into_response(),
    };
    Ok(response)
}
