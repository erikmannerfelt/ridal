//! The ingest planner (#382 phase 7): many recordings in, the radargrams
//! they would make out.
//!
//! ```text
//! POST /api/v1/ingest/plan   group uploaded recordings into outputs, and name them
//! ```
//!
//! Stateless: the browser sends the whole request on every edit, and the
//! server keeps nothing between calls but a cache of what each recording's
//! header and positions say. There is no plan document to expire, lock or
//! clean up, and no route that creates: the page creates with
//! `PUT /sources/{id}` (with `If-None-Match: *`), the processing assignment
//! and batch routes, one call at a time, so each refusal reaches its output.
//!
//! A plan with problems still answers in full, so the page can show each
//! where it is; creating is for a plan with none.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::app::AppState;
use super::auth::Caller;
use super::needs::{Needs, Who};
use super::raw_routes::project_for;
use super::routes::{ApiError, OrInternal};
use crate::id_pattern::{BatchFacts, OutputFacts, Pattern, PatternKind};
use crate::identity::{DisplayName, RadargramId};
use crate::processing::batch::Split;
use crate::processing::grouping::{GroupingEdits, GroupingRecording};
use crate::project::raw::{self, NamedFile};
use crate::project::roles::Role;

/// More than any survey; enough to keep one request's reads bounded.
const MAX_RECORDINGS: usize = 5000;
/// Vertices kept of each recording's track: enough to draw it on a map of
/// the whole survey, little enough that a plan of hundreds stays small.
const TRACK_VERTICES: usize = 64;
/// Reads kept between plans before the cache starts over.
const CACHE_ENTRIES: usize = 8192;

/// One recording as the request names it.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct RecordingFiles {
    /// The files it is made of, as `POST /api/v1/raw` answered.
    pub files: Vec<NamedFile>,
}

/// The body of `POST /api/v1/ingest/plan`. Recordings are named by their
/// key, a hash of their files' contents that the plan gives, so that an edit
/// sticks to its recording however the list around it changes.
// The key is `recording_key`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::ToSchema)]
pub struct PlanRequest {
    /// Every uploaded recording, in any order.
    pub recordings: Vec<RecordingFiles>,
    /// The gap above which recordings split; `null` never merges.
    #[serde(default)]
    pub merge_threshold_seconds: Option<f64>,
    /// Recordings to leave out.
    #[serde(default)]
    pub excluded: Vec<String>,
    /// Recordings to keep with the one before them in time, whatever the
    /// gap.
    #[serde(default)]
    pub joins: Vec<String>,
    /// Recordings that start a new output although they would merge.
    #[serde(default)]
    pub splits: Vec<String>,
    /// As `ridal batch-process --id-pattern` takes it (#368). Without it,
    /// each output is named after its first recording's file.
    #[serde(default)]
    pub id_pattern: Option<String>,
    /// As `ridal batch-process --display-name-pattern` takes it.
    #[serde(default)]
    pub display_name_pattern: Option<String>,
    /// Ids and display names typed over what the patterns give, by the key
    /// of the output's first recording.
    #[serde(default)]
    pub ids: BTreeMap<String, String>,
    #[serde(default)]
    pub display_names: BTreeMap<String, String>,
    /// The catalog group the outputs go in, which `{group}` and
    /// `{group_name}` render.
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    /// As for a source: an EPSG code, and one of the project's DEMs by id.
    #[serde(default)]
    pub crs: Option<String>,
    #[serde(default)]
    pub dem: Option<String>,
    /// Overrides by antenna, as the headers name it (#409): applied to
    /// every recording with that antenna, before grouping, so `{freq}`,
    /// `per=freq` and merging see them, and carried to the sources made.
    #[serde(default)]
    pub antennas: BTreeMap<String, AntennaOverride>,
    /// Whether to send each recording's track. A page that has them
    /// already may leave them out of a re-plan.
    #[serde(default = "yes")]
    pub include_tracks: bool,
}

fn yes() -> bool {
    true
}

/// What to take one antenna to be, where its files do not say or are
/// wrong. Either may be left to the files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, utoipa::ToSchema)]
pub struct AntennaOverride {
    /// MHz.
    #[serde(default)]
    pub mhz: Option<f32>,
    /// m.
    #[serde(default)]
    pub separation: Option<f32>,
}

/// `POST /api/v1/ingest/plan`.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct IngestPlan {
    #[schema(required = true)]
    pub merge_threshold_seconds: Option<f64>,
    /// The CRS positions were read in: the request's, else the project's
    /// `[processing] crs`, else `null`, each track in its own UTM zone.
    #[schema(required = true)]
    pub crs: Option<String>,
    /// In time order; those that cannot be read last.
    pub recordings: Vec<PlannedRecording>,
    /// In time order.
    pub outputs: Vec<PlannedOutput>,
    /// Each antenna the recordings name, in name order.
    pub antennas: Vec<PlannedAntenna>,
    pub problems: Vec<PlanProblem>,
}

/// One antenna the recordings name, what their files say of it, and what
/// the request overrides.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct PlannedAntenna {
    /// As the headers name it, which `antennas` in the request is keyed by.
    pub antenna: String,
    /// The frequency the name gives, in MHz; `null` when it gives none,
    /// and an override must.
    #[schema(required = true)]
    pub antenna_mhz: Option<f32>,
    /// The smallest and largest separation its recordings' files give, in
    /// m; `null` when none of them could be read.
    #[schema(required = true)]
    pub separation_min: Option<f32>,
    #[schema(required = true)]
    pub separation_max: Option<f32>,
    /// The request's overrides for it.
    #[schema(required = true)]
    pub mhz_override: Option<f32>,
    #[schema(required = true)]
    pub separation_override: Option<f32>,
    /// How many recordings have it.
    pub recordings: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct PlannedRecording {
    /// A hash of its files' contents ([`recording_key`]).
    pub key: String,
    /// Its header file's name.
    pub name: String,
    pub files: Vec<String>,
    /// RFC 3339; `null` when it cannot be read.
    #[schema(required = true)]
    pub start_time: Option<String>,
    #[schema(required = true)]
    pub stop_time: Option<String>,
    /// The antenna as its header names it; `null` when it cannot be read.
    #[schema(required = true)]
    pub antenna: Option<String>,
    /// MHz, after any override.
    #[schema(required = true)]
    pub antenna_mhz: Option<f32>,
    #[schema(required = true)]
    pub traces: Option<u32>,
    /// Simplified, WGS84 `[lon, lat]`; `null` when not asked for or not
    /// read.
    #[schema(required = true, value_type = Option<Vec<super::openapi::NumberPair>>)]
    pub track: Option<Vec<[f64; 2]>>,
    /// Which output it is in; `null` when excluded or unreadable.
    #[schema(required = true)]
    pub output: Option<usize>,
    pub excluded: bool,
    /// Why it cannot be read, when it cannot.
    #[schema(required = true)]
    pub unreadable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct PlannedOutput {
    /// Its first recording's key, which `ids` and `display_names` are keyed
    /// by.
    pub key: String,
    /// In merge order.
    pub recordings: Vec<String>,
    /// `null` when the pattern or the override gives no valid id.
    #[schema(required = true)]
    pub radargram_id: Option<String>,
    #[schema(required = true)]
    pub display_name: Option<String>,
    /// Whether the id and the display name were typed rather than rendered.
    pub id_overridden: bool,
    pub display_name_overridden: bool,
    pub start_time: String,
    pub stop_time: String,
    /// MHz, after any override.
    pub antenna_mhz: f32,
    /// What its source is to say of the antenna: the overrides of its
    /// recordings' antennas. Its recordings all have the same frequency
    /// and separation once overridden, or they would not be one output, so
    /// one value holds for every one of them.
    #[schema(required = true)]
    pub antenna_mhz_override: Option<f32>,
    #[schema(required = true)]
    pub antenna_separation_override: Option<f32>,
    /// Why it is not part of the output before it; `null` for the first.
    #[schema(required = true)]
    pub split: Option<Split>,
}

/// Something that stops the plan being created, with where it is.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct PlanProblem {
    /// `invalid_pattern`, `invalid_group`, `invalid_id`, `id_collision`,
    /// `id_taken`, `join_refused`, `unreadable`, or
    /// `antenna_frequency_unknown`: a recording whose antenna gives no
    /// frequency, kept out of the plan until `antennas` gives one.
    pub kind: String,
    pub message: String,
    /// The output it is about, by index.
    #[schema(required = true)]
    pub output: Option<usize>,
    /// The recording it is about, by key.
    #[schema(required = true)]
    pub recording: Option<String>,
    /// For `invalid_pattern`: `id_pattern` or `display_name_pattern`.
    #[schema(required = true)]
    pub pattern: Option<String>,
    /// Where in the pattern, in characters, `[start, end)`, when the
    /// problem is in one place of it.
    #[schema(required = true, min_items = 2, max_items = 2)]
    pub span: Option<[usize; 2]>,
    /// For `id_taken`: whether the id is a served radargram, and whether it
    /// has raw data already. Either makes it taken.
    #[schema(required = true)]
    pub served: Option<bool>,
    #[schema(required = true)]
    pub has_source: Option<bool>,
}

impl PlanProblem {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            message: message.into(),
            output: None,
            recording: None,
            pattern: None,
            span: None,
            served: None,
            has_source: None,
        }
    }
}

/// What a recording's header and positions say, kept between plans.
#[derive(Debug, Clone, PartialEq)]
pub struct Read {
    pub key: String,
    pub name: String,
    pub outcome: Result<ReadRecording, Unread>,
}

/// A recording as its files have it: no override in `grouping` but a
/// frequency where the files give none.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadRecording {
    pub grouping: GroupingRecording,
    /// The antenna as the header names it.
    pub antenna: String,
    /// What the name gives, in MHz; `None` when it gives none and the
    /// recording was read with an override for it.
    pub antenna_mhz_parsed: Option<f32>,
    pub traces: u32,
    pub track: Vec<[f64; 2]>,
}

/// Why a recording is not read.
#[derive(Debug, Clone, PartialEq)]
pub enum Unread {
    Unreadable(String),
    /// Read but for its frequency, which the antenna's name does not give
    /// and no override does yet.
    FrequencyUnknown {
        antenna: String,
        reason: String,
    },
}

impl Unread {
    fn reason(&self) -> &str {
        match self {
            Unread::Unreadable(reason) | Unread::FrequencyUnknown { reason, .. } => reason,
        }
    }

    fn reason_mut(&mut self) -> &mut String {
        match self {
            Unread::Unreadable(reason) | Unread::FrequencyUnknown { reason, .. } => reason,
        }
    }
}

/// Reads by everything that decides them: the files, the CRS and the DEM.
/// Re-planning after a pattern edit reads nothing.
#[derive(Debug, Default)]
pub struct ReadCache(Mutex<HashMap<String, Read>>);

impl ReadCache {
    fn get(&self, key: &str) -> Option<Read> {
        self.0.lock().expect("not poisoned").get(key).cloned()
    }

    fn put(&self, key: String, read: Read) {
        let mut cache = self.0.lock().expect("not poisoned");
        if cache.len() >= CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(key, read);
    }
}

fn cache_key(
    files: &[NamedFile],
    crs: Option<&str>,
    dem: Option<&str>,
    antenna_mhz: Option<f32>,
) -> String {
    let mut parts: Vec<String> = files
        .iter()
        .map(|f| format!("{}\u{0}{}", f.name, f.blake3))
        .collect();
    parts.sort();
    format!(
        "{}\u{1}{}\u{1}{}\u{1}{}",
        parts.join("\u{0}"),
        crs.unwrap_or_default(),
        dem.unwrap_or_default(),
        antenna_mhz.map(|mhz| mhz.to_string()).unwrap_or_default()
    )
}

/// Read one recording: lay it out under `work`, which is removed after.
/// `antenna_mhz` only for one whose antenna gives no frequency.
fn read_recording(
    data_dir: &StdPath,
    files: &[NamedFile],
    work: &StdPath,
    crs: Option<&String>,
    dem: Option<&PathBuf>,
    antenna_mhz: Option<f32>,
) -> Result<Read, String> {
    let resolved = raw::materialize(data_dir, &[files.to_vec()], work)?
        .pop()
        .expect("one recording in, one out");
    let header_name = resolved
        .header
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let key = recording_key(files);
    let outcome = (|| {
        let (meta, location) = crate::processing::grouping::read_header_and_positions(
            &resolved,
            crs,
            dem,
            antenna_mhz,
        )
        .map_err(|e| match e {
            crate::processing::grouping::HeaderReadError::UnknownFrequency(unknown) => {
                Unread::FrequencyUnknown {
                    reason: unknown.to_string(),
                    antenna: unknown.antenna,
                }
            }
            crate::processing::grouping::HeaderReadError::Other(reason) => {
                Unread::Unreadable(reason)
            }
        })?;
        let grouping = GroupingRecording::of(&meta, &location)
            .ok_or_else(|| Unread::Unreadable("it has no positions".to_string()))?;
        let track = crate::server::track::Track::from_location(&location)
            .map_err(Unread::Unreadable)?
            .segments
            .into_iter()
            .flat_map(|segment| segment.vertices)
            .map(|v| [v.lon, v.lat])
            .collect::<Vec<_>>();
        Ok(ReadRecording {
            grouping,
            antenna: meta.antenna.clone(),
            antenna_mhz_parsed: antenna_mhz.is_none().then_some(meta.antenna_mhz),
            traces: meta.last_trace,
            track: thin(track, TRACK_VERTICES),
        })
    })();
    Ok(Read {
        key,
        name: header_name,
        outcome,
    })
}

/// `text` with every path under `dir` given as its file name alone.
fn without_dir(text: &str, dir: &StdPath) -> String {
    let prefix = dir.display().to_string();
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(&prefix) {
        out.push_str(&rest[..at]);
        let after = &rest[at + prefix.len()..];
        // Up to the end of the path: a quote, whitespace or the end.
        let end = after
            .find(|c: char| c == '"' || c == '\'' || c.is_whitespace())
            .unwrap_or(after.len());
        let path = StdPath::new(&after[..end]);
        out.push_str(
            &path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().to_string()),
        );
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// What names a recording in a plan: a hash of its files' hashes, sorted.
///
/// Not the header's hash alone: two RAMAC recordings with the same settings
/// and length have byte-identical `.rad` files. Not the names either, so
/// that the same data under two names is one recording twice, which a plan
/// refuses.
pub fn recording_key(files: &[NamedFile]) -> String {
    let mut hashes: Vec<&str> = files.iter().map(|f| f.blake3.as_str()).collect();
    hashes.sort_unstable();
    blake3::hash(hashes.join("\n").as_bytes()).to_hex()[..32].to_string()
}

/// At most `n` of `points`, evenly by index, keeping the first and last.
fn thin(points: Vec<[f64; 2]>, n: usize) -> Vec<[f64; 2]> {
    if points.len() <= n || n < 2 {
        return points;
    }
    let last = points.len() - 1;
    (0..n)
        .map(|i| points[(i * last + (n - 1) / 2) / (n - 1)])
        .collect()
}

/// Which ids are taken, and how.
#[derive(Debug, Clone, Default)]
pub struct Taken {
    pub served: HashSet<String>,
    pub sources: HashSet<String>,
}

/// Plan many uploaded recordings as the radargrams they would make, creating
/// nothing. Answers every recording, every output, every antenna, and the
/// problems with the plan, which is answered in full even when there are some.
/// Each call keeps its recordings' files from being deleted for another hour.
#[utoipa::path(
    post,
    path = "/api/v1/ingest/plan",
    tag = "Raw data",
    request_body = PlanRequest,
    responses(
        (status = 200, description = "The plan.", body = IngestPlan),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Operator))))),
)]
pub async fn plan_ingest(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Json(request): Json<PlanRequest>,
) -> Result<Json<IngestPlan>, ApiError> {
    let project = project_for(&state, &caller, "plan adding raw data")?;
    if request.recordings.len() > MAX_RECORDINGS {
        return Err(ApiError::bad_request(
            "too_many_recordings",
            format!("A plan can hold at most {MAX_RECORDINGS} recordings."),
        ));
    }
    let crs = match &request.crs {
        Some(text) => Some(raw::epsg_code(text).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_crs",
                format!("'{text}' is not an EPSG code; write it as `EPSG:32633`."),
            )
        })?),
        None => project
            .processing_crs()
            .map_err(|e| ApiError::conflict("invalid_project_crs", e))?,
    };
    let dem = request
        .dem
        .as_deref()
        .map(|id| super::dem_routes::link_to(project, &super::dem_routes::parse_dem(id)?))
        .transpose()?;
    for (antenna, given) in &request.antennas {
        super::raw_routes::antenna_override(
            given.mhz,
            "invalid_antenna_mhz",
            &format!("antenna frequency (MHz) for '{antenna}'"),
        )?;
        super::raw_routes::antenna_override(
            given.separation,
            "invalid_antenna_separation",
            &format!("antenna separation (m) for '{antenna}'"),
        )?;
    }

    let taken = Taken {
        served: state
            .catalog()
            .entries
            .iter()
            .map(|e| e.radargram_id.to_string())
            .collect(),
        sources: raw::source_ids(project.documents())
            .or_internal("source_read_failed")?
            .into_iter()
            .collect(),
    };

    let data_dir = project.data_dir().to_path_buf();
    let cache = Arc::clone(&state.ingest_reads);
    let recordings = request.recordings.clone();
    let antennas = request.antennas.clone();
    let plan_crs = crs.clone();
    let reads = tokio::task::spawn_blocking(move || {
        read_all(
            &data_dir,
            &recordings,
            plan_crs.as_ref(),
            dem.as_ref(),
            &antennas,
            &cache,
        )
    })
    .await
    .or_internal("plan_failed")?
    .or_internal("plan_failed")?;

    let mut seen = HashSet::new();
    for read in &reads {
        if !seen.insert(read.key.as_str()) {
            return Err(ApiError::bad_request(
                "duplicate_recording",
                format!("{} is in the plan twice.", read.name),
            ));
        }
    }
    Ok(Json(plan(&reads, &request, crs, &taken)))
}

/// Read every recording, from `cache` where it can, and make each one's
/// files young again: a recording being planned is named by no source
/// yet, and planning may take longer than the sweep waits.
///
/// Read as the files have it, so the cache does not depend on the
/// overrides, which [`plan`] applies. The one exception: a recording whose
/// antenna gives no frequency is read again with the frequency `antennas`
/// gives it, since without one it cannot be read at all.
pub fn read_all(
    data_dir: &StdPath,
    recordings: &[RecordingFiles],
    crs: Option<&String>,
    dem: Option<&raw::SourceDem>,
    antennas: &BTreeMap<String, AntennaOverride>,
    cache: &ReadCache,
) -> Result<Vec<Read>, String> {
    let dem_path = dem.and_then(|d| raw::file_path(data_dir, &d.file.blake3));
    let dem_hash = dem.map(|d| d.file.blake3.as_str());
    let work = raw::incoming_path(data_dir)?.with_extension("plan");
    let read_with = |index: usize, recording: &RecordingFiles, antenna_mhz: Option<f32>| {
        let key = cache_key(
            &recording.files,
            crs.map(String::as_str),
            dem_hash,
            antenna_mhz,
        );
        if let Some(read) = cache.get(&key) {
            return read;
        }
        let laid = work.join(index.to_string());
        let read = read_recording(
            data_dir,
            &recording.files,
            &laid,
            crs,
            dem_path.as_ref(),
            antenna_mhz,
        )
        .map(|mut read| {
            // Where it was laid out is the server's business: the
            // reason names the files as they were uploaded.
            if let Err(unread) = &mut read.outcome {
                let reason = unread.reason_mut();
                *reason = without_dir(reason, &laid);
            }
            read
        });
        let _ = std::fs::remove_dir_all(&laid);
        match read {
            Ok(read) => {
                cache.put(key, read.clone());
                read
            }
            // Not laid out: a file not uploaded yet, say. Not kept,
            // since uploading it changes the answer.
            Err(reason) => Read {
                key: recording_key(&recording.files),
                name: recording
                    .files
                    .first()
                    .map(|f| f.name.clone())
                    .unwrap_or_default(),
                outcome: Err(Unread::Unreadable(reason)),
            },
        }
    };
    let reads = recordings
        .iter()
        .enumerate()
        .map(|(index, recording)| {
            for file in &recording.files {
                if let Some(path) = raw::file_path(data_dir, &file.blake3) {
                    let _ = raw::refresh(&path);
                }
            }
            let read = read_with(index, recording, None);
            let given = match &read.outcome {
                Err(Unread::FrequencyUnknown { antenna, .. }) => {
                    antennas.get(antenna).and_then(|o| o.mhz)
                }
                _ => None,
            };
            match given {
                Some(mhz) => read_with(index, recording, Some(mhz)),
                None => read,
            }
        })
        .collect();
    let _ = std::fs::remove_dir_all(&work);
    Ok(reads)
}

/// The plan for `reads`, which are `request.recordings` read: pure, so
/// everything but the reading is tested without a server.
pub fn plan(
    reads: &[Read],
    request: &PlanRequest,
    crs: Option<String>,
    taken: &Taken,
) -> IngestPlan {
    let antennas = planned_antennas(reads, &request.antennas);
    // The overrides go in before anything is grouped or named, so a
    // recording is planned as what it will be processed as.
    let overridden: Vec<Read> = reads
        .iter()
        .map(|read| with_overrides(read, &request.antennas))
        .collect();
    let reads = overridden.as_slice();
    let excluded: HashSet<&str> = request.excluded.iter().map(String::as_str).collect();
    let mut problems = Vec::new();

    // What is grouped: every recording read and not left out.
    let grouped: Vec<usize> = (0..reads.len())
        .filter(|i| reads[*i].outcome.is_ok() && !excluded.contains(reads[*i].key.as_str()))
        .collect();
    let at: HashMap<&str, usize> = grouped
        .iter()
        .enumerate()
        .map(|(position, i)| (reads[*i].key.as_str(), position))
        .collect();
    let position_of = |keys: &[String]| -> HashSet<usize> {
        keys.iter()
            .filter_map(|key| at.get(key.as_str()).copied())
            .collect()
    };
    let edits = GroupingEdits {
        joins: position_of(&request.joins),
        splits: position_of(&request.splits),
    };
    let recordings: Vec<GroupingRecording> = grouped
        .iter()
        .map(|i| read_ok(&reads[*i]).grouping.clone())
        .collect();
    let grouping = crate::processing::grouping::group_chronologically(
        &recordings,
        request.merge_threshold_seconds,
        &edits,
    );
    for (position, reason) in &grouping.refused_joins {
        let read = &reads[grouped[*position]];
        problems.push(PlanProblem {
            recording: Some(read.key.clone()),
            ..PlanProblem::new(
                "join_refused",
                format!(
                    "{} cannot be joined to the radargram before it: {reason}",
                    read.name
                ),
            )
        });
    }

    let members: Vec<Vec<&Read>> = grouping
        .groups
        .iter()
        .map(|group| group.members.iter().map(|p| &reads[grouped[*p]]).collect())
        .collect();
    let facts: Vec<OutputFacts> = members
        .iter()
        .map(|reads| {
            let first = reads[0];
            OutputFacts {
                label: first.name.clone(),
                stem: StdPath::new(&first.name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                start_seconds: read_ok(first).grouping.start_seconds,
                antenna_mhz: read_ok(first).grouping.key.antenna_mhz,
                n_files: reads.len(),
            }
        })
        .collect();
    let keys: Vec<&str> = members.iter().map(|reads| reads[0].key.as_str()).collect();

    let batch = match crate::identity::resolve_group(
        request.group_name.as_deref(),
        request.group_id.as_deref(),
        None,
        None,
    ) {
        Ok(Some((name, id))) => BatchFacts {
            group_id: Some(id),
            group_name: Some(name),
        },
        Ok(None) => BatchFacts::default(),
        Err(e) => {
            problems.push(PlanProblem::new("invalid_group", e));
            BatchFacts::default()
        }
    };
    let existing: Vec<String> = taken.served.iter().chain(&taken.sources).cloned().collect();

    let parse = |text: &str, kind: PatternKind| Pattern::parse(text, kind);

    // Ids: the pattern's, or each first file's name, then what was typed.
    let id_pattern = match request.id_pattern.as_deref() {
        Some(text) => match parse(text, PatternKind::Id) {
            Ok(pattern) => Some(pattern),
            Err(e) => {
                problems.push(pattern_problem(
                    "id_pattern",
                    text,
                    e.message.clone(),
                    Some(e.span),
                ));
                None
            }
        },
        None => None,
    };
    let rendered: Vec<Result<RadargramId, String>> = match (&request.id_pattern, &id_pattern) {
        (None, _) => facts
            .iter()
            .map(|f| RadargramId::from_fallback(&f.stem))
            .collect(),
        (Some(_), None) => vec![Err(String::new()); facts.len()],
        (Some(text), Some(pattern)) => {
            match crate::id_pattern::render_each_id(pattern, &facts, &batch, &existing) {
                Ok(ids) => ids,
                Err(e) => {
                    problems.push(pattern_problem("id_pattern", text, e, None));
                    vec![Err(String::new()); facts.len()]
                }
            }
        }
    };
    let mut ids: Vec<Option<RadargramId>> = Vec::with_capacity(facts.len());
    let mut id_overridden = Vec::with_capacity(facts.len());
    for (index, (key, rendered)) in keys.iter().zip(rendered).enumerate() {
        let typed = request.ids.get(*key);
        id_overridden.push(typed.is_some());
        let id = match typed {
            Some(text) => RadargramId::new(text).map_err(|e| format!("'{text}' is not an id: {e}")),
            None => rendered,
        };
        match id {
            Ok(id) => ids.push(Some(id)),
            Err(message) => {
                // An empty message is a pattern that did not parse, which
                // is reported once, on the pattern.
                if !message.is_empty() {
                    problems.push(PlanProblem {
                        output: Some(index),
                        ..PlanProblem::new("invalid_id", message)
                    });
                }
                ids.push(None);
            }
        }
    }

    let mut by_id: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, id) in ids.iter().enumerate() {
        if let Some(id) = id {
            by_id.entry(id.as_str()).or_default().push(index);
        }
    }
    for (id, outputs) in &by_id {
        if outputs.len() > 1 {
            let names: Vec<&str> = outputs.iter().map(|i| facts[*i].label.as_str()).collect();
            for index in outputs {
                problems.push(PlanProblem {
                    output: Some(*index),
                    ..PlanProblem::new(
                        "id_collision",
                        format!(
                            "{} would all be '{id}'. Add something that tells them apart to \
                             the id pattern, such as `{{n}}`, or type another id.",
                            names.join(", ")
                        ),
                    )
                });
            }
        }
        let served = taken.served.contains(*id);
        let has_source = taken.sources.contains(*id);
        if served || has_source {
            for index in outputs {
                problems.push(PlanProblem {
                    output: Some(*index),
                    served: Some(served),
                    has_source: Some(has_source),
                    ..PlanProblem::new(
                        "id_taken",
                        format!("{id} already exists: choose another id."),
                    )
                });
            }
        }
    }

    // Display names: rendered beside the ids, so only once every output
    // has one.
    let mut display_names: Vec<Option<String>> = vec![None; facts.len()];
    if let Some(text) = request.display_name_pattern.as_deref() {
        match parse(text, PatternKind::DisplayName) {
            Err(e) => problems.push(pattern_problem(
                "display_name_pattern",
                text,
                e.message.clone(),
                Some(e.span),
            )),
            Ok(pattern) => {
                let all: Option<Vec<RadargramId>> = ids.iter().cloned().collect();
                // The id pattern numbers `{n}` unless the display name has
                // a counter of its own; with typed ids alone, there is no
                // id pattern, and its own counter is the one.
                let fallback = Pattern::parse("{n}", PatternKind::Id).expect("valid");
                if let Some(all) = all {
                    match crate::id_pattern::render_display_names(
                        &pattern,
                        id_pattern.as_ref().unwrap_or(&fallback),
                        &facts,
                        &batch,
                        &all,
                        &existing,
                    ) {
                        Ok(names) => {
                            display_names = names
                                .into_iter()
                                .map(|n| n.map(|n| n.to_string()))
                                .collect()
                        }
                        Err(e) => {
                            problems.push(pattern_problem("display_name_pattern", text, e, None))
                        }
                    }
                }
            }
        }
    }
    let mut display_name_overridden = vec![false; facts.len()];
    for (index, key) in keys.iter().enumerate() {
        if let Some(text) = request.display_names.get(*key) {
            display_names[index] = DisplayName::from_input(text).map(|n| n.to_string());
            display_name_overridden[index] = true;
        }
    }

    let mut output_of: HashMap<&str, usize> = HashMap::new();
    for (index, reads) in members.iter().enumerate() {
        for read in reads {
            output_of.insert(read.key.as_str(), index);
        }
    }
    let outputs: Vec<PlannedOutput> = grouping
        .groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let reads = &members[index];
            let first = &read_ok(reads[0]).grouping;
            let stop = reads
                .iter()
                .map(|r| read_ok(r).grouping.stop_seconds)
                .fold(f64::NEG_INFINITY, f64::max);
            let given: Vec<&AntennaOverride> = reads
                .iter()
                .filter_map(|r| request.antennas.get(&read_ok(r).antenna))
                .collect();
            PlannedOutput {
                key: keys[index].to_string(),
                recordings: reads.iter().map(|r| r.key.clone()).collect(),
                radargram_id: ids[index].as_ref().map(|id| id.to_string()),
                display_name: display_names[index].clone(),
                id_overridden: id_overridden[index],
                display_name_overridden: display_name_overridden[index],
                start_time: crate::tools::seconds_to_rfc3339(first.start_seconds),
                stop_time: crate::tools::seconds_to_rfc3339(stop),
                antenna_mhz: first.key.antenna_mhz,
                antenna_mhz_override: given
                    .iter()
                    .any(|o| o.mhz.is_some())
                    .then_some(first.key.antenna_mhz),
                antenna_separation_override: given
                    .iter()
                    .any(|o| o.separation.is_some())
                    .then_some(first.key.antenna_separation),
                split: group.split.clone(),
            }
        })
        .collect();

    let mut planned: Vec<(Option<f64>, usize, PlannedRecording)> = reads
        .iter()
        .enumerate()
        .map(|(index, read)| {
            let request_files = &request.recordings[index].files;
            let is_excluded = excluded.contains(read.key.as_str());
            let recording = match &read.outcome {
                Ok(ok) => PlannedRecording {
                    key: read.key.clone(),
                    name: read.name.clone(),
                    files: request_files.iter().map(|f| f.name.clone()).collect(),
                    start_time: Some(crate::tools::seconds_to_rfc3339(ok.grouping.start_seconds)),
                    stop_time: Some(crate::tools::seconds_to_rfc3339(ok.grouping.stop_seconds)),
                    antenna: Some(ok.antenna.clone()),
                    antenna_mhz: Some(ok.grouping.key.antenna_mhz),
                    traces: Some(ok.traces),
                    track: request.include_tracks.then(|| ok.track.clone()),
                    output: output_of.get(read.key.as_str()).copied(),
                    excluded: is_excluded,
                    unreadable: None,
                },
                Err(unread) => {
                    if !is_excluded {
                        let problem = match unread {
                            Unread::Unreadable(reason) => PlanProblem::new(
                                "unreadable",
                                format!("{} cannot be read: {reason}", read.name),
                            ),
                            Unread::FrequencyUnknown { antenna, .. } => PlanProblem::new(
                                "antenna_frequency_unknown",
                                format!(
                                    "{}: its antenna, '{antenna}', does not say its \
                                     frequency. Give one for that antenna.",
                                    read.name
                                ),
                            ),
                        };
                        problems.push(PlanProblem {
                            recording: Some(read.key.clone()),
                            ..problem
                        });
                    }
                    PlannedRecording {
                        key: read.key.clone(),
                        name: read.name.clone(),
                        files: request_files.iter().map(|f| f.name.clone()).collect(),
                        start_time: None,
                        stop_time: None,
                        antenna: match unread {
                            Unread::FrequencyUnknown { antenna, .. } => Some(antenna.clone()),
                            Unread::Unreadable(_) => None,
                        },
                        antenna_mhz: None,
                        traces: None,
                        track: None,
                        output: None,
                        excluded: is_excluded,
                        unreadable: Some(unread.reason().to_string()),
                    }
                }
            };
            let start = read
                .outcome
                .as_ref()
                .ok()
                .map(|ok| ok.grouping.start_seconds);
            (start, index, recording)
        })
        .collect();
    planned.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.total_cmp(&y).then(a.1.cmp(&b.1)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(&b.1),
    });

    IngestPlan {
        merge_threshold_seconds: request.merge_threshold_seconds,
        crs,
        recordings: planned.into_iter().map(|(_, _, r)| r).collect(),
        outputs,
        antennas,
        problems,
    }
}

/// `read` as it will be processed: its antenna's overrides in its key.
fn with_overrides(read: &Read, antennas: &BTreeMap<String, AntennaOverride>) -> Read {
    let mut read = read.clone();
    if let Ok(ok) = &mut read.outcome {
        if let Some(given) = antennas.get(&ok.antenna) {
            if let Some(mhz) = given.mhz {
                ok.grouping.key.antenna_mhz = mhz;
            }
            if let Some(separation) = given.separation {
                ok.grouping.key.antenna_separation = separation;
            }
        }
    }
    read
}

/// Each antenna `reads` name, from what their files say, before overrides.
fn planned_antennas(
    reads: &[Read],
    antennas: &BTreeMap<String, AntennaOverride>,
) -> Vec<PlannedAntenna> {
    let mut by_name: BTreeMap<&str, PlannedAntenna> = BTreeMap::new();
    for read in reads {
        let (antenna, mhz, separation) = match &read.outcome {
            Ok(ok) => (
                ok.antenna.as_str(),
                ok.antenna_mhz_parsed,
                Some(ok.grouping.key.antenna_separation),
            ),
            Err(Unread::FrequencyUnknown { antenna, .. }) => (antenna.as_str(), None, None),
            Err(Unread::Unreadable(_)) => continue,
        };
        let given = antennas.get(antenna).copied().unwrap_or_default();
        let entry = by_name.entry(antenna).or_insert_with(|| PlannedAntenna {
            antenna: antenna.to_string(),
            antenna_mhz: None,
            separation_min: None,
            separation_max: None,
            mhz_override: given.mhz,
            separation_override: given.separation,
            recordings: 0,
        });
        entry.recordings += 1;
        entry.antenna_mhz = entry.antenna_mhz.or(mhz);
        if let Some(separation) = separation {
            entry.separation_min = Some(
                entry
                    .separation_min
                    .map_or(separation, |s| s.min(separation)),
            );
            entry.separation_max = Some(
                entry
                    .separation_max
                    .map_or(separation, |s| s.max(separation)),
            );
        }
    }
    by_name.into_values().collect()
}

/// A pattern's problem; `span` in bytes, as [`Pattern::parse`] gives it.
fn pattern_problem(
    which: &str,
    text: &str,
    message: String,
    span: Option<(usize, usize)>,
) -> PlanProblem {
    let column = |byte: usize| text.get(..byte).map_or(0, |s| s.chars().count());
    PlanProblem {
        pattern: Some(which.to_string()),
        span: span.map(|(start, end)| [column(start), column(end)]),
        ..PlanProblem::new("invalid_pattern", message)
    }
}

fn read_ok(read: &Read) -> &ReadRecording {
    read.outcome
        .as_ref()
        .expect("only recordings that were read are grouped")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processing::grouping::MergeKey;

    /// A recording read, `start` and `stop` in seconds, named
    /// `DAT_<n>_A1.rad`.
    fn read(n: u32, start: f64, stop: f64, mhz: f32) -> Read {
        Read {
            key: format!("k{n}"),
            name: format!("DAT_{n:04}_A1.rad"),
            outcome: Ok(ReadRecording {
                grouping: GroupingRecording {
                    start_seconds: start,
                    stop_seconds: stop,
                    key: MergeKey {
                        crs: "EPSG:32633".to_string(),
                        antenna_mhz: mhz,
                        time_window: 500.,
                        samples: 1024,
                        antenna_separation: 4.,
                        medium_velocity: 0.168,
                    },
                },
                antenna: format!("{mhz} MHz"),
                antenna_mhz_parsed: Some(mhz),
                traces: 100,
                track: vec![[16.0, 78.0], [16.1, 78.1]],
            }),
        }
    }

    /// Three 100 MHz lines, the first two minutes apart, then an hour's
    /// gap, and an 800 MHz line straight after the last.
    fn survey() -> Vec<Read> {
        vec![
            read(1, 1_700_000_000., 1_700_000_100., 100.),
            read(2, 1_700_000_200., 1_700_000_300., 100.),
            read(3, 1_700_003_900., 1_700_004_000., 100.),
            read(4, 1_700_004_010., 1_700_004_100., 800.),
        ]
    }

    fn request(reads: &[Read]) -> PlanRequest {
        PlanRequest {
            recordings: reads
                .iter()
                .map(|r| RecordingFiles {
                    files: vec![NamedFile {
                        name: r.name.clone(),
                        blake3: r.key.clone(),
                    }],
                })
                .collect(),
            merge_threshold_seconds: Some(600.),
            include_tracks: true,
            ..Default::default()
        }
    }

    fn outputs(plan: &IngestPlan) -> Vec<Vec<&str>> {
        plan.outputs
            .iter()
            .map(|o| o.recordings.iter().map(String::as_str).collect())
            .collect()
    }

    fn ids(plan: &IngestPlan) -> Vec<Option<&str>> {
        plan.outputs
            .iter()
            .map(|o| o.radargram_id.as_deref())
            .collect()
    }

    fn kinds(plan: &IngestPlan) -> Vec<&str> {
        plan.problems.iter().map(|p| p.kind.as_str()).collect()
    }

    #[test]
    fn recordings_group_by_time_and_by_what_a_person_changes() {
        let reads = survey();
        let mut request = request(&reads);
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(outputs(&plan), [vec!["k1", "k2"], vec!["k3"], vec!["k4"]]);
        assert!(plan.problems.is_empty(), "{:?}", plan.problems);
        // Named after the first file without a pattern.
        assert_eq!(
            ids(&plan),
            [
                Some("dat_0001_a1"),
                Some("dat_0003_a1"),
                Some("dat_0004_a1")
            ]
        );
        assert_eq!(plan.recordings[1].output, Some(0));
        assert!(plan.outputs[2]
            .split
            .as_ref()
            .unwrap()
            .incompatible
            .is_some());

        // Split two that would merge, join one an hour later, and leave
        // one out.
        request.splits = vec!["k2".into()];
        request.joins = vec!["k3".into()];
        request.excluded = vec!["k1".into()];
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(outputs(&plan), [vec!["k2", "k3"], vec!["k4"]]);
        assert!(plan.recordings[0].excluded);
        assert_eq!(plan.recordings[0].output, None);
        // Splitting what is now the first does nothing to say.
        assert_eq!(plan.outputs[0].split, None);

        // A join the headers refuse is not made, and stops the plan.
        request.joins = vec!["k4".into()];
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(outputs(&plan).len(), 3);
        assert_eq!(kinds(&plan), ["join_refused"]);
        assert_eq!(plan.problems[0].recording.as_deref(), Some("k4"));
    }

    #[test]
    fn patterns_name_the_outputs_and_typed_names_win() {
        let reads = survey();
        let mut request = request(&reads);
        request.id_pattern = Some("{group}-{freq}mhz-{n:02, per=freq}".into());
        request.display_name_pattern = Some("{group_name} line {n}".into());
        request.group_name = Some("Drønbreen".into());
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert!(plan.problems.is_empty(), "{:?}", plan.problems);
        assert_eq!(
            ids(&plan),
            [
                Some("dronbreen-100mhz-01"),
                Some("dronbreen-100mhz-02"),
                Some("dronbreen-800mhz-01")
            ]
        );
        assert_eq!(
            plan.outputs[1].display_name.as_deref(),
            Some("Drønbreen line 2")
        );

        // Typed over, by the output's first recording.
        request.ids = [("k3".to_string(), "line-07".to_string())].into();
        request.display_names = [("k1".to_string(), "The first".to_string())].into();
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(plan.outputs[1].radargram_id.as_deref(), Some("line-07"));
        assert!(plan.outputs[1].id_overridden);
        assert_eq!(plan.outputs[0].display_name.as_deref(), Some("The first"));
        assert!(plan.outputs[0].display_name_overridden);
        assert!(!plan.outputs[1].display_name_overridden);

        // A typed id that is not one is a problem on its output.
        request.ids = [("k3".to_string(), "Line 7".to_string())].into();
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(kinds(&plan), ["invalid_id"]);
        assert_eq!(plan.problems[0].output, Some(1));
        assert_eq!(plan.outputs[1].radargram_id, None);
    }

    #[test]
    fn a_pattern_that_does_not_parse_says_where() {
        let reads = survey();
        let mut request = request(&reads);
        request.id_pattern = Some("ø-{freq}MHz".into());
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(kinds(&plan), ["invalid_pattern"]);
        let problem = &plan.problems[0];
        assert_eq!(problem.pattern.as_deref(), Some("id_pattern"));
        // In characters: `ø` is two bytes.
        assert_eq!(problem.span, Some([0, 1]));
        assert!(ids(&plan).iter().all(Option::is_none));

        request.id_pattern = Some("line-{freq}MHz".into());
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(plan.problems[0].span, Some([11, 14]));

        // `{group}` with no group is the whole pattern's problem.
        request.id_pattern = Some("{group}-{n}".into());
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(kinds(&plan), ["invalid_pattern"]);
        assert_eq!(plan.problems[0].span, None);
    }

    #[test]
    fn two_outputs_with_one_id_are_both_told() {
        let reads = survey();
        let mut request = request(&reads);
        request.id_pattern = Some("line-{freq}".into());
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(kinds(&plan), ["id_collision", "id_collision"]);
        let outputs: Vec<_> = plan.problems.iter().map(|p| p.output).collect();
        assert_eq!(outputs, [Some(0), Some(1)]);
        assert!(plan.problems[0]
            .message
            .contains("DAT_0001_A1.rad, DAT_0003_A1.rad"));
    }

    #[test]
    fn an_id_that_exists_is_taken_whether_served_or_only_raw_data() {
        let reads = survey();
        let mut request = request(&reads);
        request.id_pattern = Some("line-{n:02}".into());
        let taken = Taken {
            served: ["line-01".to_string()].into(),
            sources: ["line-02".to_string(), "line-01".to_string()].into(),
        };
        let plan = super::plan(&reads, &request, None, &taken);
        assert_eq!(kinds(&plan), ["id_taken", "id_taken"]);
        let first = &plan.problems[0];
        assert_eq!(
            (first.output, first.served, first.has_source),
            (Some(0), Some(true), Some(true))
        );
        assert_eq!(first.message, "line-01 already exists: choose another id.");
        let second = &plan.problems[1];
        assert_eq!(
            (second.output, second.served, second.has_source),
            (Some(1), Some(false), Some(true))
        );

        // `from=next` counts them as existing.
        request.id_pattern = Some("line-{n:02, from=next}".into());
        let plan = super::plan(&reads, &request, None, &taken);
        assert!(plan.problems.is_empty(), "{:?}", plan.problems);
        assert_eq!(plan.outputs[0].radargram_id.as_deref(), Some("line-03"));
    }

    #[test]
    fn an_unreadable_recording_stops_the_plan_until_it_is_left_out() {
        let mut reads = survey();
        reads.insert(
            1,
            Read {
                key: "bad".into(),
                name: "DAT_0099_A1.rad".into(),
                outcome: Err(Unread::Unreadable("no .cor".into())),
            },
        );
        let mut request = request(&reads);
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(kinds(&plan), ["unreadable"]);
        assert_eq!(outputs(&plan).len(), 3);
        // Listed last.
        let last = plan.recordings.last().unwrap();
        assert_eq!(last.key, "bad");
        assert_eq!(last.unreadable.as_deref(), Some("no .cor"));

        request.excluded = vec!["bad".into()];
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert!(plan.problems.is_empty());
        assert!(plan.recordings.last().unwrap().excluded);
    }

    #[test]
    fn a_reason_names_files_not_where_they_were_laid_out() {
        let dir = StdPath::new("/srv/p/raw/incoming/1.plan/4");
        assert_eq!(
            without_dir(
                "File not found: \"/srv/p/raw/incoming/1.plan/4/0/DAT_0099_A1.rd3\"",
                dir
            ),
            "File not found: \"DAT_0099_A1.rd3\""
        );
        assert_eq!(without_dir("no positions", dir), "no positions");
    }

    #[test]
    fn an_antenna_s_overrides_apply_before_grouping_and_naming() {
        let reads = survey();
        let mut request = request(&reads);
        request.id_pattern = Some("{freq}mhz-{n:02, per=freq}".into());
        // The 800 MHz line is really 100 MHz, 10 s after the third: it
        // joins it, and is named and numbered as a 100 MHz line.
        request.antennas = [(
            "800 MHz".to_string(),
            AntennaOverride {
                mhz: Some(100.),
                separation: None,
            },
        )]
        .into();
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert!(plan.problems.is_empty(), "{:?}", plan.problems);
        assert_eq!(ids(&plan), [Some("100mhz-01"), Some("100mhz-02")]);
        assert_eq!(plan.outputs[1].recordings, ["k3", "k4"]);
        assert_eq!(plan.outputs[1].antenna_mhz, 100.);
        // Its source says so for both recordings, which the files of one of
        // them already did; the first output has nothing to override.
        assert_eq!(plan.outputs[1].antenna_mhz_override, Some(100.));
        assert_eq!(plan.outputs[1].antenna_separation_override, None);
        assert_eq!(plan.outputs[0].antenna_mhz_override, None);
        let k4 = plan.recordings.iter().find(|r| r.key == "k4").unwrap();
        assert_eq!(k4.antenna.as_deref(), Some("800 MHz"));
        assert_eq!(k4.antenna_mhz, Some(100.));

        // The table: each antenna as its files have it, and what is given.
        assert_eq!(plan.antennas.len(), 2);
        assert_eq!(plan.antennas[0].antenna, "100 MHz");
        assert_eq!(plan.antennas[0].recordings, 3);
        assert_eq!(plan.antennas[1].antenna, "800 MHz");
        assert_eq!(plan.antennas[1].antenna_mhz, Some(800.));
        assert_eq!(plan.antennas[1].mhz_override, Some(100.));
        assert_eq!(plan.antennas[1].separation_min, Some(4.));

        // A separation that differs keeps it apart again.
        request.antennas.get_mut("800 MHz").unwrap().separation = Some(2.);
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert_eq!(outputs(&plan).len(), 3);
        assert_eq!(plan.outputs[2].antenna_separation_override, Some(2.));
    }

    #[test]
    fn an_antenna_that_gives_no_frequency_is_listed_and_kept_out() {
        let mut reads = survey();
        reads[3].outcome = Err(Unread::FrequencyUnknown {
            antenna: "RTA unshielded".to_string(),
            reason: "Could not read frequency from the antenna field".to_string(),
        });
        let plan = super::plan(&reads, &request(&reads), None, &Taken::default());
        assert_eq!(kinds(&plan), ["antenna_frequency_unknown"]);
        assert_eq!(plan.problems[0].recording.as_deref(), Some("k4"));
        assert!(plan.problems[0].message.contains("'RTA unshielded'"));
        assert_eq!(outputs(&plan).len(), 2);
        let unknown = plan
            .antennas
            .iter()
            .find(|a| a.antenna == "RTA unshielded")
            .unwrap();
        assert_eq!(unknown.antenna_mhz, None);
        assert_eq!(unknown.separation_min, None);
        assert_eq!(unknown.recordings, 1);
        let k4 = plan.recordings.iter().find(|r| r.key == "k4").unwrap();
        assert_eq!(k4.antenna.as_deref(), Some("RTA unshielded"));
        assert!(k4.unreadable.is_some());
        // Left out, it stops nothing.
        let mut request = request(&reads);
        request.excluded = vec!["k4".into()];
        let plan = super::plan(&reads, &request, None, &Taken::default());
        assert!(plan.problems.is_empty(), "{:?}", plan.problems);
    }

    #[test]
    fn a_recording_whose_antenna_gives_no_frequency_is_read_with_the_one_given() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();
        let stem = "dronbreen-20250327-DAT_0066_A1";
        let mangled = tempfile::tempdir().unwrap();
        let rad = mangled.path().join(format!("{stem}.rad"));
        std::fs::write(
            &rad,
            std::fs::read_to_string(format!("assets/mala/{stem}.rad"))
                .unwrap()
                .replace("ANTENNAS:25 MHz RTA=11", "ANTENNAS:RTA unshielded"),
        )
        .unwrap();
        let recordings = vec![RecordingFiles {
            files: vec![
                upload(data_dir, rad.to_str().unwrap()),
                upload(data_dir, &format!("assets/mala/{stem}.rd3")),
                upload(data_dir, &format!("assets/mala/{stem}.cor")),
            ],
        }];
        let cache = ReadCache::default();
        let none = read_all(data_dir, &recordings, None, None, &BTreeMap::new(), &cache).unwrap();
        match &none[0].outcome {
            Err(Unread::FrequencyUnknown { antenna, .. }) => assert_eq!(antenna, "RTA unshielded"),
            other => panic!("{other:?}"),
        }
        let given: BTreeMap<String, AntennaOverride> = [(
            "RTA unshielded".to_string(),
            AntennaOverride {
                mhz: Some(25.),
                separation: None,
            },
        )]
        .into();
        let read = read_all(data_dir, &recordings, None, None, &given, &cache).unwrap();
        let read = read[0].outcome.as_ref().unwrap();
        assert_eq!(read.antenna, "RTA unshielded");
        assert_eq!(read.antenna_mhz_parsed, None);
        assert_eq!(read.grouping.key.antenna_mhz, 25.);
        assert_eq!(read.grouping.key.antenna_separation, 6.2);
    }

    #[test]
    fn thinning_keeps_the_ends() {
        let points: Vec<[f64; 2]> = (0..1000).map(|i| [i as f64, 0.]).collect();
        let thinned = thin(points, 64);
        assert_eq!(thinned.len(), 64);
        assert_eq!(thinned[0], [0., 0.]);
        assert_eq!(thinned[63], [999., 0.]);
        assert_eq!(thin(vec![[1., 1.]; 3], 64).len(), 3);
    }

    fn upload(data_dir: &StdPath, path: &str) -> NamedFile {
        let incoming = raw::incoming_path(data_dir).unwrap();
        std::fs::copy(path, &incoming).unwrap();
        let name = StdPath::new(path).file_name().unwrap().to_string_lossy();
        let stored = raw::install(data_dir, &incoming, &name).unwrap();
        NamedFile {
            name: stored.name,
            blake3: stored.blake3,
        }
    }

    fn recording(data_dir: &StdPath, stem: &str) -> RecordingFiles {
        RecordingFiles {
            files: ["rad", "rd3", "cor"]
                .iter()
                .map(|ext| upload(data_dir, &format!("assets/mala/{stem}.{ext}")))
                .collect(),
        }
    }

    #[test]
    fn a_coords_csv_gives_a_recording_its_positions() {
        // #13: what Add data sends when a `.coords.csv` replaces the `.cor`,
        // the CSV first, as the browser may list it.
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();
        let stem = "dronbreen-20250327-DAT_0066_A1";
        let scratch = tempfile::tempdir().unwrap();
        let csv = scratch.path().join(format!("{stem}.coords.csv"));
        std::fs::write(
            &csv,
            "trace,time,lon,lat,alt\n\
             0,2025-03-27T12:10:20Z,16.6484,78.1019,715\n\
             3547,2025-03-27T12:22:10Z,16.6612,78.1034,720\n",
        )
        .unwrap();
        let mut files = vec![upload(data_dir, csv.to_str().unwrap())];
        files.extend(
            ["rad", "rd3"]
                .iter()
                .map(|ext| upload(data_dir, &format!("assets/mala/{stem}.{ext}"))),
        );
        let recordings = vec![RecordingFiles { files }];

        let reads = read_all(
            data_dir,
            &recordings,
            None,
            None,
            &BTreeMap::new(),
            &ReadCache::default(),
        )
        .unwrap();
        assert_eq!(reads[0].name, format!("{stem}.rad"));
        let read = reads[0].outcome.as_ref().unwrap();
        assert_eq!(read.traces, 3548);
        assert_eq!(read.grouping.start_seconds, 1_743_077_420.);
    }

    #[test]
    fn reads_are_kept_and_keep_their_files_from_the_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();
        let store = crate::project::store::DocumentStore::new(data_dir.to_path_buf());
        let recordings = vec![
            recording(data_dir, "dronbreen-20250327-DAT_0066_A1"),
            RecordingFiles {
                files: vec![NamedFile {
                    name: "DAT_0001_A1.rad".into(),
                    blake3: "a".repeat(64),
                }],
            },
        ];
        // Stored long enough ago to be swept.
        let hour_ago = std::time::SystemTime::now() - 2 * raw::SWEEP_MIN_AGE;
        for file in &recordings[0].files {
            let path = raw::file_path(data_dir, &file.blake3).unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(hour_ago)
                .unwrap();
        }

        let cache = ReadCache::default();
        let reads = read_all(data_dir, &recordings, None, None, &BTreeMap::new(), &cache).unwrap();
        let read = reads[0].outcome.as_ref().unwrap();
        // Keyed by every file, in any order.
        let mut reversed = recordings[0].files.clone();
        reversed.reverse();
        assert_eq!(reads[0].key, recording_key(&reversed));
        assert_ne!(reads[0].key, recording_key(&recordings[0].files[..2]));
        assert_eq!(reads[0].name, "dronbreen-20250327-DAT_0066_A1.rad");
        assert!(read.traces > 0);
        assert!(read.track.len() >= 2 && read.track.len() <= TRACK_VERTICES);
        assert!(read.grouping.key.crs.starts_with("EPSG:326"));
        assert!(reads[1]
            .outcome
            .as_ref()
            .unwrap_err()
            .reason()
            .contains("has not been uploaded"));
        // Only what could be laid out is kept.
        assert_eq!(cache.0.lock().unwrap().len(), 1);

        // Planning made them young again.
        raw::sweep(&store, data_dir, &HashSet::new(), raw::SWEEP_MIN_AGE).unwrap();
        for file in &recordings[0].files {
            assert!(raw::contains(data_dir, &file.blake3), "{}", file.name);
        }

        // Read again from the cache: the files could be gone.
        for file in &recordings[0].files {
            std::fs::remove_file(raw::file_path(data_dir, &file.blake3).unwrap()).unwrap();
        }
        let again = read_all(
            data_dir,
            &recordings[..1],
            None,
            None,
            &BTreeMap::new(),
            &cache,
        )
        .unwrap();
        assert_eq!(again[0], reads[0]);
        // A CRS is part of what a read is.
        let other = read_all(
            data_dir,
            &recordings[..1],
            Some(&"EPSG:32634".to_string()),
            None,
            &BTreeMap::new(),
            &cache,
        )
        .unwrap();
        assert!(other[0].outcome.is_err());
        // No incoming leftovers.
        let incoming = data_dir.join(raw::RAW_DIR).join("incoming");
        assert_eq!(std::fs::read_dir(incoming).unwrap().count(), 0);
    }
}
