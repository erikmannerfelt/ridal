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
    group: Option<String>,
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
        group: entry.group.as_ref().map(|g| g.to_string()),
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

#[derive(serde::Serialize)]
struct GroupSummary {
    name: String,
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
    let mut group_names: Vec<&str> = state
        .catalog
        .entries
        .iter()
        .filter_map(|e| e.group.as_ref().map(|g| g.as_str()))
        .collect();
    group_names.sort_unstable();
    group_names.dedup();
    let groups: Vec<GroupSummary> = group_names
        .into_iter()
        .map(|name| GroupSummary {
            name: name.to_string(),
            entries: state
                .entries_in_group(name)
                .into_iter()
                .map(to_summary)
                .collect(),
        })
        .collect();
    let ungrouped: Vec<DatasetSummary> = state
        .catalog
        .entries
        .iter()
        .filter(|e| e.group.is_none())
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
            group => entry.group.as_ref().map(|g| g.to_string()),
            revision_id => entry.revision_id.to_string(),
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
        Float(v) => serde_json::json!(v),
        Floats(v) => serde_json::json!(v),
        Double(v) => serde_json::json!(v),
        Doubles(v) => serde_json::json!(v),
        Str(v) => serde_json::json!(v),
        Strs(v) => serde_json::json!(v),
    }
}

/// The complete raw global attribute set, for the viewer's metadata
/// dialog (a button opening a `<dialog>` with everything, per your
/// PFA-style preference -- see the planning conversation).
pub async fn dataset_attributes(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let entry = lookup_dataset(&state, &radargram_id)?;
    let path = state.absolute_path(entry);
    let file = netcdf::open(&path)
        .map_err(|e| ApiError::internal("attributes_read_failed", format!("{e}")))?;
    let mut out = serde_json::Map::new();
    for attr in file.attributes() {
        let name = attr.name().to_string();
        if let Ok(value) = attr.value() {
            out.insert(name, attribute_value_to_json(value));
        }
    }
    Ok(Json(serde_json::Value::Object(out)))
}

#[cfg(test)]
mod tests {
    use super::format_datetime_for_display;

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
}
