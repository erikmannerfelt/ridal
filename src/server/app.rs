//! Shared Axum application and typed state (#120).
//!
//! Route handlers here should primarily compose the already-tested
//! catalog (M3) and render-service (M4/M5) components -- no new NetCDF,
//! catalog, or rendering logic belongs in this module.

use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Mutex;

use axum::routing::get;
use axum::Router;

use super::catalog::{Catalog, RevisionId};
use super::render::service::{RenderService, RenderServiceConfig};
use super::source::SourceReader;
use crate::identity::RadargramId;

/// One open radargram: its render service plus the metadata needed to
/// answer dataset-detail and viewer-page requests without re-inspecting
/// the file.
pub struct OpenRadargram {
    pub service: Mutex<RenderService>,
    pub shape: (usize, usize),
}

pub struct AppState {
    pub catalog: Catalog,
    pub radargrams: HashMap<String, OpenRadargram>,
}

impl AppState {
    /// Discover the catalog under `root` and eagerly open a
    /// [`RenderService`] for every entry. Eager rather than lazy: the
    /// issue's target catalog size is ~100 files (#122/#123), and eager
    /// construction means a broken file surfaces as a clear startup
    /// warning rather than a request-time surprise.
    pub fn build(root: &StdPath, config: &RenderServiceConfig) -> Result<Self, String> {
        let catalog = Catalog::discover(root);
        let mut radargrams = HashMap::new();

        for entry in &catalog.entries {
            let absolute_path = Self::resolve_absolute_path(root, entry);
            let reader = match SourceReader::open(&absolute_path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "Warning: could not open {} for rendering: {e}",
                        entry.relative_path
                    );
                    continue;
                }
            };
            let shape = reader.shape();
            let revision_id: RevisionId = entry.revision_id.clone();
            let service = RenderService::new(reader, revision_id, config);
            radargrams.insert(
                entry.radargram_id.as_str().to_string(),
                OpenRadargram {
                    service: Mutex::new(service),
                    shape,
                },
            );
        }

        Ok(Self {
            catalog,
            radargrams,
        })
    }

    fn resolve_absolute_path(root: &StdPath, entry: &super::catalog::CatalogEntry) -> PathBuf {
        if root.is_file() {
            root.to_path_buf()
        } else {
            root.join(&entry.relative_path)
        }
    }

    pub fn find_entry(&self, radargram_id: &str) -> Option<&super::catalog::CatalogEntry> {
        self.catalog
            .entries
            .iter()
            .find(|e| e.radargram_id.as_str() == radargram_id)
    }
}

/// Build the complete Axum application over `state`.
pub fn build_router(state: std::sync::Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(super::routes::index_page))
        .route("/view/{radargram_id}", get(super::routes::viewer_page))
        .route("/static/leaflet.js", get(super::assets::leaflet_js))
        .route("/static/leaflet.css", get(super::assets::leaflet_css))
        .route(
            "/static/images/marker-icon.png",
            get(super::assets::marker_icon),
        )
        .route(
            "/static/images/marker-icon-2x.png",
            get(super::assets::marker_icon_2x),
        )
        .route(
            "/static/images/marker-shadow.png",
            get(super::assets::marker_shadow),
        )
        .route("/static/images/layers.png", get(super::assets::layers_png))
        .route(
            "/static/images/layers-2x.png",
            get(super::assets::layers_2x_png),
        )
        .route("/api/v1/health", get(super::routes::health))
        .route("/api/v1/profiles", get(super::routes::list_profiles))
        .route("/api/v1/datasets", get(super::routes::list_datasets))
        .route(
            "/api/v1/datasets/{radargram_id}",
            get(super::routes::dataset_detail),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/views/{view}/overview",
            get(super::routes::overview_image),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/views/{view}/chunks/{profile}/{x}/{y}",
            get(super::routes::chunk_image),
        )
        .with_state(state)
}

/// A radargram ID from the URL is validated the same way an explicit
/// `--radargram-id` would be, so a malformed ID (never a real dataset)
/// fails fast with a clear reason rather than a generic "not found".
pub fn validate_radargram_id(raw: &str) -> Result<RadargramId, String> {
    RadargramId::new(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    fn write_test_nc(path: &StdPath, radargram_id: &str) {
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", 20).unwrap();
        file.add_dimension("x", 300).unwrap();
        let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        let data: Vec<f32> = (0..(20 * 300)).map(|i| (i % 100) as f32).collect();
        var.put_values(&data, ..).unwrap();
        file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
            .unwrap();
        file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
            .unwrap();
        file.add_attribute("ridal_radargram_id", radargram_id)
            .unwrap();
    }

    fn test_app(dir: &StdPath) -> Router {
        let config = RenderServiceConfig::default();
        let state = std::sync::Arc::new(AppState::build(dir, &config).unwrap());
        build_router(state)
    }

    async fn get(app: &Router, uri: &str) -> (StatusCode, axum::body::Bytes) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, bytes)
    }

    #[tokio::test]
    async fn health_route_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_app(dir.path());
        let (status, body) = get(&app, "/api/v1/health").await;
        assert_eq!(status, StatusCode::OK);
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn full_route_suite_against_a_real_catalog() {
        // A single #[test] (not #[tokio::test]) driving a manually built
        // runtime, so every route in this suite shares one netcdf-serial
        // guard -- otherwise each #[tokio::test] would need its own,
        // fighting the file-per-test isolation this suite wants to test.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "route-test-a");
            let app = test_app(dir.path());

            // Catalog listing.
            let (status, body) = get(&app, "/api/v1/datasets").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["entries"].as_array().unwrap().len(), 1);
            assert_eq!(json["entries"][0]["radargram_id"], "route-test-a");

            // Dataset detail: known and unknown.
            let (status, _) = get(&app, "/api/v1/datasets/route-test-a").await;
            assert_eq!(status, StatusCode::OK);
            let (status, body) = get(&app, "/api/v1/datasets/does-not-exist").await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "dataset_not_found");

            // An invalid radargram ID (uppercase) is a 400, not a 404 --
            // structurally invalid vs. legitimately absent (#118's
            // distinction, applied here to dataset lookup too).
            let (status, body) = get(&app, "/api/v1/datasets/Not-Valid").await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "invalid_radargram_id");

            // Profiles.
            let (status, body) = get(&app, "/api/v1/profiles").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert!(json
                .as_array()
                .unwrap()
                .contains(&Value::String("default".into())));

            // Overview image.
            let (status, body) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/overview",
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(image::load_from_memory(&body).is_ok());

            // Unknown dataset view.
            let (status, body) =
                get(&app, "/api/v1/datasets/route-test-a/views/bogus/overview").await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "unknown_dataset_view");

            // Unknown profile.
            let (status, _) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/overview?profile=nonexistent",
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            // A valid chunk.
            let (status, body) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/chunks/default/0/0",
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let decoded = image::load_from_memory(&body).unwrap();
            assert_eq!(
                decoded.width(),
                super::super::render::grid::CHUNK_SIZE as u32
            );

            // Structurally invalid chunk coordinate (not a number) -> 400.
            let (status, body) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/chunks/default/abc/0",
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "invalid_chunk_coordinate");

            // Well-formed but out-of-grid chunk coordinate -> 404, not 400.
            let (status, body) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/chunks/default/999/999",
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "image_chunk_not_found");

            // Pages: index and viewer.
            let (status, body) = get(&app, "/").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(html.contains("route-test-a"));

            let (status, body) = get(&app, "/view/route-test-a").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(html.contains("route-test-a"));

            let (status, _) = get(&app, "/view/does-not-exist").await;
            assert_eq!(status, StatusCode::NOT_FOUND);

            // Static assets are embedded, not proxied to a filesystem path.
            let (status, body) = get(&app, "/static/leaflet.js").await;
            assert_eq!(status, StatusCode::OK);
            assert!(!body.is_empty());
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn one_unreadable_file_does_not_break_the_whole_catalog_at_startup() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("garbage.nc"), b"not a netcdf file").unwrap();
            write_test_nc(&dir.path().join("good.nc"), "still-works");

            let app = test_app(dir.path());
            let (status, body) = get(&app, "/api/v1/datasets").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["entries"].as_array().unwrap().len(), 1);
            assert!(!json["warnings"].as_array().unwrap().is_empty());

            let (status, _) =
                get(&app, "/api/v1/datasets/still-works/views/standard/overview").await;
            assert_eq!(status, StatusCode::OK);
        });
    }
}
