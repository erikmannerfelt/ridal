//! HTTP route handlers (#120). Composes catalog (M3) and render-service
//! (M4/M5) components; no NetCDF, catalog, or rendering logic here.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::app::{validate_radargram_id, AppState};
use super::render::grid::{ChunkGrid, OverviewSpec, ViewerRaster};
use super::render::profile::{DatasetView, RenderProfile};
use super::templates;

/// Stable JSON error envelope (#120): `{"error": {"code", "message"}}`.
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn internal(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": { "code": self.code, "message": self.message }
        });
        (self.status, Json(body)).into_response()
    }
}

/// Same error information, rendered as an HTML page for page routes
/// rather than JSON for API routes.
pub struct PageError(ApiError);

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        let env = templates::environment();
        let tmpl = env
            .get_template("error.html.jinja")
            .expect("error template is always registered");
        let html = tmpl
            .render(minijinja::context! {
                status => self.0.status.as_u16(),
                code => self.0.code,
                message => self.0.message,
            })
            .unwrap_or_else(|e| format!("<h1>Error</h1><p>{e}</p>"));
        (self.0.status, Html(html)).into_response()
    }
}

pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

pub async fn list_profiles() -> impl IntoResponse {
    let names: Vec<String> = RenderProfile::built_in_profiles()
        .into_iter()
        .map(|p| p.name)
        .collect();
    Json(names)
}

#[derive(serde::Serialize)]
struct DatasetSummary {
    radargram_id: String,
    effective_label: String,
    display_name: Option<String>,
    group_name: Option<String>,
    group_id: Option<String>,
    relative_path: String,
    /// The exact stored string. Kept verbatim because the revision
    /// fingerprint (#117) hashes it -- reformatting here would silently
    /// change identity.
    processing_datetime: String,
    /// A human-readable rendering of the same instant, for UI display
    /// only. The raw value carries nanosecond precision and a numeric
    /// offset, which is noise in a catalog listing and wraps badly in a
    /// narrow card.
    processing_datetime_display: String,
    revision_id: String,
    shape: (usize, usize),
}

/// Format an RFC3339 processing datetime for display as `YYYY-MM-DD HH:MM`.
///
/// Falls back to the input unchanged if it does not parse: a file written
/// by a future or third-party tool should still show *something* rather
/// than an empty cell or an error.
fn format_datetime_for_display(raw: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        Err(_) => raw.to_string(),
    }
}

fn to_summary(entry: &super::catalog::CatalogEntry) -> DatasetSummary {
    DatasetSummary {
        radargram_id: entry.radargram_id.to_string(),
        effective_label: entry.effective_label(),
        display_name: entry.display_name.as_ref().map(|d| d.to_string()),
        group_name: entry.group_name.as_ref().map(|g| g.to_string()),
        group_id: entry.group_id.as_ref().map(|g| g.to_string()),
        relative_path: entry.relative_path.clone(),
        processing_datetime: entry.processing_datetime.clone(),
        processing_datetime_display: format_datetime_for_display(&entry.processing_datetime),
        revision_id: entry.revision_id.to_string(),
        shape: entry.shape,
    }
}

pub async fn list_datasets(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let entries: Vec<DatasetSummary> = state.catalog.entries.iter().map(to_summary).collect();
    let warnings: Vec<String> = state
        .catalog
        .warnings
        .iter()
        .map(|w| w.message.clone())
        .collect();
    Json(serde_json::json!({ "entries": entries, "warnings": warnings }))
}

fn lookup_dataset<'a>(
    state: &'a AppState,
    radargram_id: &str,
) -> Result<&'a super::catalog::CatalogEntry, ApiError> {
    validate_radargram_id(radargram_id)
        .map_err(|e| ApiError::bad_request("invalid_radargram_id", e))?;
    state.find_entry(radargram_id).ok_or_else(|| {
        ApiError::not_found(
            "dataset_not_found",
            format!("No dataset with id '{radargram_id}'"),
        )
    })
}

pub async fn dataset_detail(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let entry = lookup_dataset(&state, &radargram_id)?;
    Ok(Json(to_summary(entry)))
}

fn lookup_view(view: &str) -> Result<DatasetView, ApiError> {
    match view {
        "standard" => Ok(DatasetView::Standard),
        _ => Err(ApiError::bad_request(
            "unknown_dataset_view",
            format!("Unknown dataset view '{view}'. Supported: standard."),
        )),
    }
}

fn lookup_profile(name: &str) -> Result<RenderProfile, ApiError> {
    RenderProfile::by_name(name).ok_or_else(|| {
        ApiError::bad_request(
            "unknown_render_profile",
            format!("Unknown render profile '{name}'."),
        )
    })
}

#[derive(Deserialize)]
pub struct ProfileQuery {
    profile: Option<String>,
}

fn image_response(bytes: Vec<u8>, profile: &RenderProfile) -> Response {
    (
        [(header::CONTENT_TYPE, profile.format.content_type())],
        bytes,
    )
        .into_response()
}

pub async fn overview_image(
    State(state): State<Arc<AppState>>,
    Path((radargram_id, view)): Path<(String, String)>,
    Query(query): Query<ProfileQuery>,
) -> Result<Response, ApiError> {
    let entry = lookup_dataset(&state, &radargram_id)?;
    let dataset_view = lookup_view(&view)?;
    let profile = lookup_profile(query.profile.as_deref().unwrap_or("default"))?;

    let radargram = state
        .radargrams
        .get(entry.radargram_id.as_str())
        .ok_or_else(|| {
            ApiError::internal(
                "dataset_unavailable",
                "Dataset is cataloged but its render service failed to initialize.",
            )
        })?;

    let (height, width) = radargram.shape;
    let spec = OverviewSpec::new(width, height, 512);
    let mut service = radargram.service.lock().map_err(|_| {
        ApiError::internal(
            "render_service_poisoned",
            "Render service lock was poisoned",
        )
    })?;
    let bytes = service
        .get_or_render_overview(&spec, dataset_view, &profile)
        .map_err(|e| ApiError::internal("render_failed", e))?;
    Ok(image_response(bytes, &profile))
}

pub async fn chunk_image(
    State(state): State<Arc<AppState>>,
    Path((radargram_id, view, profile_name, x_raw, y_raw)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
) -> Result<Response, ApiError> {
    let entry = lookup_dataset(&state, &radargram_id)?;
    let dataset_view = lookup_view(&view)?;
    let profile = lookup_profile(&profile_name)?;

    // Structurally invalid coordinates (not a non-negative integer) are a
    // 400, distinct from a well-formed but out-of-grid request (404) --
    // #118's explicit distinction.
    let x: usize = x_raw.parse().map_err(|_| {
        ApiError::bad_request("invalid_chunk_coordinate", format!("Invalid x: '{x_raw}'"))
    })?;
    let y: usize = y_raw.parse().map_err(|_| {
        ApiError::bad_request("invalid_chunk_coordinate", format!("Invalid y: '{y_raw}'"))
    })?;

    let radargram = state
        .radargrams
        .get(entry.radargram_id.as_str())
        .ok_or_else(|| {
            ApiError::internal(
                "dataset_unavailable",
                "Dataset is cataloged but its render service failed to initialize.",
            )
        })?;

    let (height, width) = radargram.shape;
    let raster = ViewerRaster::new(width, height);
    let grid = ChunkGrid::new(raster);
    let chunk = grid.chunk(x, y).ok_or_else(|| {
        ApiError::not_found(
            "image_chunk_not_found",
            "The requested image chunk is outside the radargram bounds.",
        )
    })?;

    let mut service = radargram.service.lock().map_err(|_| {
        ApiError::internal(
            "render_service_poisoned",
            "Render service lock was poisoned",
        )
    })?;
    let bytes = service
        .get_or_render_chunk(&chunk, dataset_view, &profile)
        .map_err(|e| ApiError::internal("render_failed", e))?;
    Ok(image_response(bytes, &profile))
}

/// One group's heading and members for the index page. `id` is also the
/// `data-group` value the group map's JS fetches
/// `/api/v1/groups/{id}/tracks` with, so it must stay the stable
/// [`crate::identity::GroupId`] -- never the free-form, possibly-changing
/// display name, which is `label` instead (#121 planning round: mirror the
/// radargram id/display-name split one level up).
#[derive(serde::Serialize)]
struct GroupSummary {
    id: String,
    label: String,
    entries: Vec<DatasetSummary>,
}

pub async fn index_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let entries: Vec<DatasetSummary> = state.catalog.entries.iter().map(to_summary).collect();
    let warnings: Vec<String> = state
        .catalog
        .warnings
        .iter()
        .map(|w| w.message.clone())
        .collect();

    // Grouped entries get one map each on the index page (#121); ungrouped
    // entries have no siblings to show together, so they stay in the
    // plain table only.
    let mut group_ids: Vec<&str> = state
        .catalog
        .entries
        .iter()
        .filter_map(|e| e.group_id.as_ref().map(|g| g.as_str()))
        .collect();
    group_ids.sort_unstable();
    group_ids.dedup();
    let groups: Vec<GroupSummary> = group_ids
        .into_iter()
        .map(|id| {
            let label = state
                .catalog
                .group_names
                .iter()
                .find(|(gid, _)| gid.as_str() == id)
                .map(|(_, name)| name.to_string())
                .unwrap_or_else(|| id.to_string());
            GroupSummary {
                id: id.to_string(),
                label,
                entries: state
                    .entries_in_group(id)
                    .into_iter()
                    .map(to_summary)
                    .collect(),
            }
        })
        .collect();
    let ungrouped: Vec<DatasetSummary> = state
        .catalog
        .entries
        .iter()
        .filter(|e| e.group_id.is_none())
        .map(to_summary)
        .collect();

    let env = templates::environment();
    let tmpl = env
        .get_template("index.html.jinja")
        .expect("index template is always registered");
    let html = tmpl
        .render(minijinja::context! {
            entries => entries,
            warnings => warnings,
            groups => groups,
            ungrouped => ungrouped,
        })
        .unwrap_or_else(|e| format!("<h1>Template error</h1><p>{e}</p>"));
    Html(html)
}

pub async fn viewer_page(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    Query(query): Query<ProfileQuery>,
) -> Result<impl IntoResponse, PageError> {
    let entry = lookup_dataset(&state, &radargram_id).map_err(PageError)?;
    let active_profile = query.profile.unwrap_or_else(|| "default".to_string());
    lookup_profile(&active_profile).map_err(PageError)?;

    let radargram = state
        .radargrams
        .get(entry.radargram_id.as_str())
        .ok_or_else(|| {
            PageError(ApiError::internal(
                "dataset_unavailable",
                "Dataset is cataloged but its render service failed to initialize.",
            ))
        })?;
    let (height, width) = radargram.shape;
    let raster = ViewerRaster::new(width, height);
    let grid = ChunkGrid::new(raster);

    let profiles: Vec<String> = RenderProfile::built_in_profiles()
        .into_iter()
        .map(|p| p.name)
        .collect();

    let env = templates::environment();
    let tmpl = env
        .get_template("viewer.html.jinja")
        .expect("viewer template is always registered");
    let html = tmpl
        .render(minijinja::context! {
            radargram_id => entry.radargram_id.to_string(),
            effective_label => entry.effective_label(),
            group_name => entry.group_name.as_ref().map(|g| g.to_string()),
            group_id => entry.group_id.as_ref().map(|g| g.to_string()),
            revision_id => entry.revision_id.to_string(),
            // First 7 hex characters, `git`-style, for the collapsed
            // banner row -- the full ID moves to the metadata dialog.
            revision_short => entry.revision_id.to_string().chars().take(7).collect::<String>(),
            processing_datetime => format_datetime_for_display(&entry.processing_datetime),
            shape_height => height,
            shape_width => width,
            profiles => profiles,
            active_profile => active_profile,
            chunk_size => super::render::grid::CHUNK_SIZE,
            n_cols => grid.n_cols,
            n_rows => grid.n_rows,
            viewer_width => raster.width,
            viewer_height => raster.height,
        })
        .map_err(|e| PageError(ApiError::internal("template_error", e.to_string())))?;
    Ok(Html(html))
}

#[derive(serde::Serialize)]
struct TrackVertexJson {
    trace_index: u32,
    lon: f64,
    lat: f64,
}

#[derive(serde::Serialize)]
struct TrackSegmentJson {
    segment_index: usize,
    trace_start: u32,
    trace_end: u32,
    n_traces: u32,
    length_m: f64,
    vertices: Vec<TrackVertexJson>,
}

#[derive(serde::Serialize)]
struct TrackJson {
    segments: Vec<TrackSegmentJson>,
}

fn track_to_json(track: &super::track::Track) -> TrackJson {
    TrackJson {
        segments: track
            .segments
            .iter()
            .map(|s| TrackSegmentJson {
                segment_index: s.segment_index,
                trace_start: s.trace_start,
                trace_end: s.trace_end,
                n_traces: s.n_traces,
                length_m: s.length_m,
                vertices: s
                    .vertices
                    .iter()
                    .map(|v| TrackVertexJson {
                        trace_index: v.trace_index,
                        lon: v.lon,
                        lat: v.lat,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// This radargram's own track (#121's cursor-sync feature).
pub async fn dataset_track(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let entry = lookup_dataset(&state, &radargram_id)?;
    let path = state.absolute_path(entry);
    let track = super::track::read_track_from_netcdf(&path)
        .map_err(|e| ApiError::internal("track_read_failed", e))?;
    Ok(Json(track_to_json(&track)))
}

/// Every radargram's track in one group, for sibling-track display on the
/// viewer map and the index page's per-group overview map. A track that
/// fails to read is silently skipped here rather than failing the whole
/// response -- one bad sibling should not break the rest, matching the
/// spirit of #122's "one bad candidate does not abort discovery."
pub async fn group_tracks(
    State(state): State<Arc<AppState>>,
    Path(group): Path<String>,
) -> impl IntoResponse {
    let mut out = serde_json::Map::new();
    for entry in state.entries_in_group(&group) {
        let path = state.absolute_path(entry);
        if let Ok(track) = super::track::read_track_from_netcdf(&path) {
            out.insert(
                entry.radargram_id.to_string(),
                serde_json::json!({
                    "effective_label": entry.effective_label(),
                    "track": track_to_json(&track),
                }),
            );
        }
    }
    Json(serde_json::Value::Object(out))
}

/// Widen an `f32` NetCDF attribute to `f64` via its shortest round-trip
/// decimal string, rather than a plain numeric cast. A plain cast (`v as
/// f64`) preserves the `f32`'s exact binary value, which `f64`'s extra
/// precision then renders as noise (`0.168_f32` -> `0.16799999773502350`).
/// `f32::to_string()` already produces the shortest decimal that
/// round-trips to the same `f32`, so re-parsing it as `f64` recovers the
/// value a human actually meant.
fn f32_to_f64_exact(v: f32) -> f64 {
    v.to_string().parse().unwrap_or(v as f64)
}

fn attribute_value_to_json(value: netcdf::AttributeValue) -> serde_json::Value {
    use netcdf::AttributeValue::*;
    match value {
        Uchar(v) => serde_json::json!(v),
        Uchars(v) => serde_json::json!(v),
        Schar(v) => serde_json::json!(v),
        Schars(v) => serde_json::json!(v),
        Ushort(v) => serde_json::json!(v),
        Ushorts(v) => serde_json::json!(v),
        Short(v) => serde_json::json!(v),
        Shorts(v) => serde_json::json!(v),
        Uint(v) => serde_json::json!(v),
        Uints(v) => serde_json::json!(v),
        Int(v) => serde_json::json!(v),
        Ints(v) => serde_json::json!(v),
        Ulonglong(v) => serde_json::json!(v),
        Ulonglongs(v) => serde_json::json!(v),
        Longlong(v) => serde_json::json!(v),
        Longlongs(v) => serde_json::json!(v),
        Float(v) => serde_json::json!(f32_to_f64_exact(v)),
        Floats(v) => {
            serde_json::json!(v.into_iter().map(f32_to_f64_exact).collect::<Vec<_>>())
        }
        Double(v) => serde_json::json!(v),
        Doubles(v) => serde_json::json!(v),
        Str(v) => serde_json::json!(v),
        Strs(v) => serde_json::json!(v),
    }
}

/// Attribute name -> display label overrides for cases the generic
/// strip-prefix/underscore-to-space/capitalize rule gets wrong (acronyms,
/// mainly).
fn label_override(name: &str) -> Option<&'static str> {
    match name {
        "crs" => Some("CRS"),
        "ridal_group_id" => Some("Group ID"),
        _ => None,
    }
}

/// `ridal_processing_datetime` -> "Processing datetime",
/// `original_filepaths` -> "Original filepaths": strip the `ridal_`
/// namespace prefix (meaningless to a human reader), replace underscores
/// with spaces, and capitalize only the first letter -- matching how the
/// rest of the dialog's prose is cased.
fn prettify_label(name: &str) -> String {
    if let Some(overridden) = label_override(name) {
        return overridden.to_string();
    }
    let stripped = name.strip_prefix("ridal_").unwrap_or(name);
    let mut words = stripped.split('_');
    let mut out = String::new();
    if let Some(first) = words.next() {
        let mut chars = first.chars();
        if let Some(c) = chars.next() {
            out.extend(c.to_uppercase());
        }
        out.push_str(chars.as_str());
    }
    for word in words {
        out.push(' ');
        out.push_str(word);
    }
    out
}

/// Round a float to 4 decimal places for display, trimming trailing zeros
/// (and a bare trailing `.`) so a whole number like `5.0` still reads as
/// `5`. Fixes the `f32`-precision-widening artifact at the point it
/// actually matters -- what a human reads -- on top of the exact-string
/// recovery `f32_to_f64_exact` already does at parse time.
fn format_rounded(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let rounded = (v * 10_000.0).round() / 10_000.0;
    let mut s = format!("{rounded:.4}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

fn plain_value_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(n) if n.is_f64() => format_rounded(n.as_f64().unwrap()),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(plain_value_string)
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(_) => value.to_string(),
    }
}

/// Format one attribute's value for display, appending its `*_unit`
/// sibling (if any) in parentheses rather than showing it as a separate
/// row -- "Time interval  0.3 (s)".
fn format_display_value(value: &serde_json::Value, unit: Option<&str>) -> String {
    let base = plain_value_string(value);
    match unit {
        Some(unit) => format!("{base} ({unit})"),
        None => base,
    }
}

#[derive(serde::Serialize)]
struct MetadataEntry {
    label: String,
    value: String,
}

/// Curated priority for known attribute keys: `(tier, order-within-tier)`.
/// Keys not listed fall into tier 3 ("everything else"), ordered
/// alphabetically by their prettified label -- the plan's "identity ->
/// acquisition -> processing -> everything else alphabetically".
///
/// `__start_stop_datetime` and `__shape` are synthetic keys for entries
/// this function builds itself rather than reading verbatim from `raw`.
fn curated_priority(key: &str) -> (u8, usize) {
    const IDENTITY: &[&str] = &[
        "ridal_radargram_id",
        "ridal_display_name",
        "ridal_group_name",
        "ridal_group_id",
        "__start_stop_datetime",
        "__shape",
    ];
    const ACQUISITION: &[&str] = &[
        "antenna",
        "antenna_separation",
        "frequency_steps",
        "vertical_sampling_frequency",
        "time_interval",
        "medium_velocity",
        "crs",
        "elevation_correction",
        "total_distance",
    ];
    const PROCESSING: &[&str] = &[
        "ridal_processing_datetime",
        "ridal_version",
        "original_filepaths",
    ];

    if let Some(i) = IDENTITY.iter().position(|&k| k == key) {
        return (0, i);
    }
    if let Some(i) = ACQUISITION.iter().position(|&k| k == key) {
        return (1, i);
    }
    if let Some(i) = PROCESSING.iter().position(|&k| k == key) {
        return (2, i);
    }
    (3, 0)
}

/// Attribute keys never shown as their own row: `processing_log` and
/// `processing_steps` get dedicated fields in the response instead (see
/// [`dataset_attributes`]); `start_datetime`/`stop_datetime` are merged
/// into one synthetic "Start/stop datetime" row;
/// `ridal_user_metadata_json` duplicates the flattened user-metadata
/// attributes already shown individually. `*_unit` keys are consumed by
/// their base attribute, not skipped by name here.
const SKIP_FROM_ENTRIES: &[&str] = &[
    "start_datetime",
    "stop_datetime",
    "processing_log",
    "processing_steps",
    "ridal_user_metadata_json",
];

fn build_metadata_entries(
    raw: &serde_json::Map<String, serde_json::Value>,
    shape: (usize, usize),
) -> Vec<MetadataEntry> {
    struct Entry {
        key: String,
        label: String,
        value: String,
    }
    let mut entries = Vec::new();

    if let (Some(start), Some(stop)) = (
        raw.get("start_datetime").and_then(|v| v.as_str()),
        raw.get("stop_datetime").and_then(|v| v.as_str()),
    ) {
        entries.push(Entry {
            key: "__start_stop_datetime".to_string(),
            label: "Start/stop datetime".to_string(),
            value: format!(
                "{} / {}",
                format_datetime_for_display(start),
                format_datetime_for_display(stop)
            ),
        });
    }

    entries.push(Entry {
        key: "__shape".to_string(),
        label: "Shape (samples \u{d7} traces)".to_string(),
        value: format!("{} \u{d7} {}", shape.0, shape.1),
    });

    for (key, value) in raw {
        if SKIP_FROM_ENTRIES.contains(&key.as_str()) || key.ends_with("_unit") {
            continue;
        }
        // The only other raw datetime attribute besides start/stop
        // (merged above): needs the same display formatting, not the raw
        // nanosecond-precision RFC3339 string.
        if key == "ridal_processing_datetime" {
            if let Some(raw_dt) = value.as_str() {
                entries.push(Entry {
                    key: key.clone(),
                    label: prettify_label(key),
                    value: format_datetime_for_display(raw_dt),
                });
                continue;
            }
        }
        let unit = raw.get(&format!("{key}_unit")).and_then(|v| v.as_str());
        entries.push(Entry {
            key: key.clone(),
            label: prettify_label(key),
            value: format_display_value(value, unit),
        });
    }

    entries.sort_by(|a, b| {
        curated_priority(&a.key)
            .cmp(&curated_priority(&b.key))
            .then_with(|| a.label.cmp(&b.label))
    });

    entries
        .into_iter()
        .map(|e| MetadataEntry {
            label: e.label,
            value: e.value,
        })
        .collect()
}

/// The viewer's metadata dialog: curated, human-readable `entries`
/// (prettified labels, merged units, rounded floats, merged start/stop,
/// curated order), `processing_steps`/`processing_log` as their own
/// fields (the log needs its per-step structure preserved, not squashed
/// into a single-line entry value), and the complete `raw` attribute set
/// as an escape hatch for anything the curated view doesn't surface.
pub async fn dataset_attributes(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let entry = lookup_dataset(&state, &radargram_id)?;
    let path = state.absolute_path(entry);
    let file = netcdf::open(&path)
        .map_err(|e| ApiError::internal("attributes_read_failed", format!("{e}")))?;
    let mut raw = serde_json::Map::new();
    for attr in file.attributes() {
        let name = attr.name().to_string();
        if let Ok(value) = attr.value() {
            raw.insert(name, attribute_value_to_json(value));
        }
    }

    let processing_steps: Vec<String> = raw
        .get("processing_steps")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let processing_log = raw
        .get("processing_log")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let entries = build_metadata_entries(&raw, entry.shape);

    Ok(Json(serde_json::json!({
        "entries": entries,
        "processing_steps": processing_steps,
        "processing_log": processing_log,
        "raw": raw,
    })))
}

/// Distance/TWTT/depth axes for the viewer's cursor readout (item 3 of the
/// planning round). `distance`/`twtt`/`depth` are written unconditionally
/// by `export.rs`, but small hand-built test fixtures
/// (`write_test_nc`/`write_test_nc_with_track`) do not write them -- so
/// each axis degrades independently to `null` rather than failing the
/// whole response.
#[derive(serde::Serialize)]
struct AxesJson {
    distance: Option<Vec<f64>>,
    twtt: Option<Vec<f64>>,
    depth: Option<Vec<f64>>,
}

pub async fn dataset_axes(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let entry = lookup_dataset(&state, &radargram_id)?;
    let path = state.absolute_path(entry);
    let file =
        netcdf::open(&path).map_err(|e| ApiError::internal("axes_read_failed", format!("{e}")))?;
    Ok(Json(AxesJson {
        distance: super::track::read_f64_variable(&file, "distance").ok(),
        twtt: super::track::read_f64_variable(&file, "twtt").ok(),
        depth: super::track::read_f64_variable(&file, "depth").ok(),
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        build_metadata_entries, f32_to_f64_exact, format_datetime_for_display, format_rounded,
        prettify_label,
    };

    #[test]
    fn display_datetime_drops_subsecond_noise() {
        // The real shape written by export.rs: chrono::Local::now()
        // to_rfc3339(), i.e. nanosecond precision plus a numeric offset.
        assert_eq!(
            format_datetime_for_display("2026-08-26T20:57:41.407887786+00:00"),
            "2026-08-26 20:57"
        );
    }

    #[test]
    fn display_datetime_handles_z_suffix_and_whole_seconds() {
        assert_eq!(
            format_datetime_for_display("2020-01-01T00:00:00Z"),
            "2020-01-01 00:00"
        );
    }

    #[test]
    fn display_datetime_falls_back_to_the_raw_value() {
        // A file from a future or third-party writer should still show
        // something rather than an empty cell.
        assert_eq!(
            format_datetime_for_display("not a datetime"),
            "not a datetime"
        );
        assert_eq!(format_datetime_for_display(""), "");
    }

    #[test]
    fn f32_widening_recovers_the_shortest_decimal() {
        // The exact reported artifact: 0.168_f32 cast plainly to f64
        // reads back as 0.16799999773502350.
        assert_eq!(f32_to_f64_exact(0.168_f32), 0.168_f64);
    }

    #[test]
    fn prettify_label_strips_ridal_prefix_and_title_cases() {
        assert_eq!(
            prettify_label("ridal_processing_datetime"),
            "Processing datetime"
        );
        assert_eq!(prettify_label("original_filepaths"), "Original filepaths");
        assert_eq!(prettify_label("crs"), "CRS");
    }

    #[test]
    fn format_rounded_trims_to_four_decimals() {
        assert_eq!(format_rounded(0.168_f32 as f64), "0.168");
        assert_eq!(format_rounded(5.0), "5");
        assert_eq!(format_rounded(1.0 / 3.0), "0.3333");
        assert_eq!(format_rounded(-0.00001), "0");
    }

    #[test]
    fn metadata_entries_merge_units_and_start_stop_and_curate_order() {
        let raw: serde_json::Map<String, serde_json::Value> = serde_json::json!({
            "ridal_radargram_id": "dronbreen-2022",
            "ridal_group_name": "Drønbreen",
            "ridal_group_id": "dronbreen",
            "ridal_processing_datetime": "2026-08-26T20:57:41.407887786+00:00",
            "start_datetime": "2022-03-29T00:00:00Z",
            "stop_datetime": "2022-03-29T01:00:00Z",
            "time_interval": 0.3,
            "time_interval_unit": "s",
            "processing_log": "step 1 (duration: 1s):\tdid a thing",
            "processing_steps": ["step 1"],
            "ridal_user_metadata_json": "{}",
            "crs": "EPSG:32633",
        })
        .as_object()
        .unwrap()
        .clone();

        let entries = build_metadata_entries(&raw, (400, 1200));
        let by_label: std::collections::HashMap<&str, &str> = entries
            .iter()
            .map(|e| (e.label.as_str(), e.value.as_str()))
            .collect();

        assert_eq!(
            by_label["Start/stop datetime"],
            "2022-03-29 00:00 / 2022-03-29 01:00"
        );
        assert_eq!(by_label["Time interval"], "0.3 (s)");
        assert_eq!(by_label["Shape (samples \u{d7} traces)"], "400 \u{d7} 1200");
        assert_eq!(by_label["Group name"], "Drønbreen");
        assert_eq!(by_label["Group ID"], "dronbreen");
        // ridal_processing_datetime gets the same display formatting as
        // start/stop, not the raw nanosecond-precision RFC3339 string.
        assert_eq!(by_label["Processing datetime"], "2026-08-26 20:57");
        // Merged-unit and internal-use attributes must not also appear as
        // their own separate rows.
        assert!(!by_label.contains_key("Time interval unit"));
        assert!(!by_label.contains_key("Processing log"));
        assert!(!by_label.contains_key("Processing steps"));
        assert!(!by_label.contains_key("User metadata json"));

        // Identity tier (radargram ID) sorts ahead of acquisition (CRS).
        let id_pos = entries
            .iter()
            .position(|e| e.label == "Radargram id")
            .unwrap();
        let crs_pos = entries.iter().position(|e| e.label == "CRS").unwrap();
        assert!(id_pos < crs_pos);
    }
}
