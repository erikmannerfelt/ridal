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
    pub root: PathBuf,
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
            root: root.to_path_buf(),
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

    /// The absolute filesystem path for a catalog entry. Never exposed to
    /// HTTP clients directly (#122: "keep filesystem paths internal") --
    /// only used server-side, e.g. to re-open a file for track reading.
    pub fn absolute_path(&self, entry: &super::catalog::CatalogEntry) -> PathBuf {
        Self::resolve_absolute_path(&self.root, entry)
    }

    pub fn find_entry(&self, radargram_id: &str) -> Option<&super::catalog::CatalogEntry> {
        self.catalog
            .entries
            .iter()
            .find(|e| e.radargram_id.as_str() == radargram_id)
    }

    /// All entries sharing `group`, ordered like the catalog itself.
    pub fn entries_in_group(&self, group: &str) -> Vec<&super::catalog::CatalogEntry> {
        self.catalog
            .entries
            .iter()
            .filter(|e| e.group_id.as_ref().is_some_and(|g| g.as_str() == group))
            .collect()
    }
}

/// Build the complete Axum application over `state`.
pub fn build_router(state: std::sync::Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(super::routes::index_page))
        .route("/view/{radargram_id}", get(super::routes::viewer_page))
        .route("/static/leaflet.js", get(super::assets::leaflet_js))
        .route("/static/leaflet.css", get(super::assets::leaflet_css))
        .route("/static/app.css", get(super::assets::app_css))
        .route("/static/app.js", get(super::assets::app_js))
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
        .route("/static/images/logo.svg", get(super::assets::logo_svg))
        .route("/favicon.ico", get(super::assets::favicon))
        .route("/api/v1/health", get(super::routes::health))
        .route("/api/v1/profiles", get(super::routes::list_profiles))
        .route("/api/v1/datasets", get(super::routes::list_datasets))
        .route(
            "/api/v1/datasets/{radargram_id}",
            get(super::routes::dataset_detail),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/track",
            get(super::routes::dataset_track),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/attributes",
            get(super::routes::dataset_attributes),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/axes",
            get(super::routes::dataset_axes),
        )
        .route(
            "/api/v1/groups/{group}/tracks",
            get(super::routes::group_tracks),
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

    /// Like `write_test_nc`, but with real track variables (crs, easting,
    /// northing, time) so `dataset_track`/`group_tracks` have something to
    /// read, and an optional group.
    fn write_test_nc_with_track(path: &StdPath, radargram_id: &str, group: Option<&str>) {
        let n_traces = 50;
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", 5).unwrap();
        file.add_dimension("x", n_traces).unwrap();
        let mut data_var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        data_var
            .put_values(&vec![1.0f32; 5 * n_traces], ..)
            .unwrap();

        let mut easting_var = file.add_variable::<f64>("easting", &["x"]).unwrap();
        let easting: Vec<f64> = (0..n_traces).map(|i| 500000.0 + i as f64).collect();
        easting_var.put_values(&easting, ..).unwrap();

        let mut northing_var = file.add_variable::<f64>("northing", &["x"]).unwrap();
        northing_var
            .put_values(&vec![8_000_000.0f64; n_traces], ..)
            .unwrap();

        let mut time_var = file.add_variable::<f64>("time", &["x"]).unwrap();
        let time: Vec<f64> = (0..n_traces).map(|i| i as f64 * 0.1).collect();
        time_var.put_values(&time, ..).unwrap();

        file.add_attribute("crs", "EPSG:32633").unwrap();
        file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
            .unwrap();
        file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
            .unwrap();
        file.add_attribute("ridal_radargram_id", radargram_id)
            .unwrap();
        if let Some(group) = group {
            file.add_attribute("ridal_group_name", group).unwrap();
            file.add_attribute("ridal_group_id", group).unwrap();
        }
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
            for asset in [
                "/static/leaflet.js",
                "/static/leaflet.css",
                "/static/app.css",
                "/static/app.js",
                "/static/images/logo.svg",
                "/favicon.ico",
            ] {
                let (status, body) = get(&app, asset).await;
                assert_eq!(status, StatusCode::OK, "{asset}");
                assert!(!body.is_empty(), "{asset}");
            }
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn index_page_renders_lazy_overview_thumbnails() {
        // #121 requires an ~512px overview per catalog entry, and names
        // loading="lazy" as the mechanism bounding initial render work.
        // Both were missing when M7 was first reported complete, so they
        // are pinned here rather than left to visual inspection.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "thumb-test-a");
            let app = test_app(dir.path());

            let (status, body) = get(&app, "/").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();

            assert!(
                html.contains("/api/v1/datasets/thumb-test-a/views/standard/overview"),
                "index must embed the overview image URL"
            );
            assert!(
                html.contains("loading=\"lazy\""),
                "overview images must be lazily loaded"
            );
            // The viewer's back-link targets /#card-{id}; the anchor has to
            // survive the table -> card-grid restructuring.
            assert!(
                html.contains("id=\"card-thumb-test-a\""),
                "per-entry anchor must be preserved"
            );
            assert!(
                html.contains("/static/app.css"),
                "first-party stylesheet must be linked"
            );
            assert!(
                html.contains("/static/images/logo.svg"),
                "logo must be shown beside the wordmark in the shared header"
            );
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn track_attributes_and_group_routes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc_with_track(&dir.path().join("a.nc"), "track-a", Some("shared-group"));
            write_test_nc_with_track(&dir.path().join("b.nc"), "track-b", Some("shared-group"));
            write_test_nc_with_track(&dir.path().join("c.nc"), "track-c", None);
            let app = test_app(dir.path());

            // This radargram's own track.
            let (status, body) = get(&app, "/api/v1/datasets/track-a/track").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            let segments = json["segments"].as_array().unwrap();
            assert!(!segments.is_empty());
            let first_vertex = &segments[0]["vertices"][0];
            assert!(first_vertex["lon"].as_f64().is_some());
            assert!(first_vertex["lat"].as_f64().is_some());

            // Full raw attribute set, for the metadata dialog, plus the
            // curated display entries built from it.
            let (status, body) = get(&app, "/api/v1/datasets/track-a/attributes").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["raw"]["ridal_radargram_id"], "track-a");
            assert_eq!(json["raw"]["crs"], "EPSG:32633");
            let entries = json["entries"].as_array().unwrap();
            assert!(
                entries
                    .iter()
                    .any(|e| e["label"] == "CRS" && e["value"] == "EPSG:32633"),
                "{entries:?}"
            );
            // The revision checksum is server-computed, never a file
            // attribute, so it must still show up as a curated entry.
            assert!(
                entries.iter().any(|e| e["label"] == "Revision"),
                "{entries:?}"
            );
            // The fixture writes no original_filepaths attribute; the
            // dedicated field must still be present (as an empty array),
            // not missing or an error.
            assert_eq!(json["original_filepaths"].as_array().unwrap().len(), 0);

            // The `/axes` route degrades each axis to null independently
            // when the fixture never wrote distance/twtt/depth.
            let (status, body) = get(&app, "/api/v1/datasets/track-a/axes").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert!(json["distance"].is_null());
            assert!(json["twtt"].is_null());
            assert!(json["depth"].is_null());

            // Group tracks: both group members present, the ungrouped one absent.
            let (status, body) = get(&app, "/api/v1/groups/shared-group/tracks").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            let obj = json.as_object().unwrap();
            assert!(obj.contains_key("track-a"));
            assert!(obj.contains_key("track-b"));
            assert!(!obj.contains_key("track-c"));
            assert!(obj["track-a"]["track"]["segments"].is_array());

            // An empty/unknown group is an empty object, not an error.
            let (status, body) = get(&app, "/api/v1/groups/no-such-group/tracks").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json.as_object().unwrap().len(), 0);
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
