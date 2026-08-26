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
    processing_datetime: String,
    revision_id: String,
    shape: (usize, usize),
}

fn to_summary(entry: &super::catalog::CatalogEntry) -> DatasetSummary {
    DatasetSummary {
        radargram_id: entry.radargram_id.to_string(),
        effective_label: entry.effective_label(),
        display_name: entry.display_name.as_ref().map(|d| d.to_string()),
        group: entry.group.as_ref().map(|g| g.to_string()),
        relative_path: entry.relative_path.clone(),
        processing_datetime: entry.processing_datetime.clone(),
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

pub async fn index_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let entries: Vec<DatasetSummary> = state.catalog.entries.iter().map(to_summary).collect();
    let warnings: Vec<String> = state
        .catalog
        .warnings
        .iter()
        .map(|w| w.message.clone())
        .collect();
    let env = templates::environment();
    let tmpl = env
        .get_template("index.html.jinja")
        .expect("index template is always registered");
    let html = tmpl
        .render(minijinja::context! { entries => entries, warnings => warnings })
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
            processing_datetime => entry.processing_datetime.clone(),
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
