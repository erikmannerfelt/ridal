//! HTTP-level tests for the pick manager's summary route (#178).
//!
//! The unconfigured project used here makes every request the local
//! `default` user with the `operator` role, so these see every row. What a
//! picker sees is in `access_tests`, which signs people in.
//!
//! Every test builds an `AppState`, which opens a NetCDF, so they carry the
//! same `#[serial_test::serial(netcdf)]` guard as the other route tests.

use std::path::Path as StdPath;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::app::{build_router, AccessOptions, AppState};
use super::interp_routes_tests::write_test_nc_with_axes;
use crate::project::Project;
use crate::server::render_service::RenderServiceConfig;

const RADARGRAM: &str = "line-01";
const SUMMARY: &str = "/api/v1/interpretations";

fn data(root: &StdPath) -> std::path::PathBuf {
    root.join(crate::project::DEFAULT_DATA_DIR)
}

/// A project with one 40-trace radargram, `line-01`.
fn project_app() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc_with_axes(
        &data(dir.path())
            .join(crate::project::DEFAULT_RADARGRAM_DIR)
            .join("line-01.nc"),
        RADARGRAM,
        None,
    );
    let project = Project::discover(dir.path()).unwrap().unwrap();
    let state = Arc::new(
        AppState::build_with_project(
            dir.path(),
            &RenderServiceConfig::default(),
            Some(project),
            AccessOptions::default(),
        )
        .unwrap(),
    );
    (dir, build_router(state))
}

/// Two lines on `bed`, over traces 5..=14 and 16..=24, and one `surface`
/// line over 0..=9.
fn document(key: &str, revision: Option<&str>) -> Value {
    let mut document = json!({
        "key": key,
        "features": [
            {"type": "Feature",
             "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [14.0, 3.0]]},
             "properties": {"id": "f-0001", "label": "bed"}},
            {"type": "Feature",
             "geometry": {"type": "LineString", "coordinates": [[16.0, 2.0], [20.0, 2.5], [24.0, 3.0]]},
             "properties": {"id": "f-0002", "label": "bed"}},
            {"type": "Feature",
             "geometry": {"type": "LineString", "coordinates": [[0.0, 0.0], [9.0, 0.0]]},
             "properties": {"id": "f-0003", "label": "surface"}}
        ]
    });
    if let Some(revision) = revision {
        document["source"] = json!({"revision_id": revision});
    }
    document
}

/// Put a document on disk the way a hand copy or a converter would, past the
/// API's checks.
fn place(root: &StdPath, relative: &str, body: &str) {
    let path = data(root).join("interpretations").join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn summary(app: &Router) -> Value {
    let (status, body) = send(
        app,
        Request::builder().uri(SUMMARY).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn save(app: &Router, user: &str, body: &Value) {
    let (status, response) = send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/v1/datasets/{RADARGRAM}/interpretations/{user}"
            ))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert!(status.is_success(), "{status}: {response}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_saved_interpretation_is_summarized_with_its_coverage() {
    let (_dir, app) = project_app();
    save(&app, "default", &document(RADARGRAM, None)).await;

    let body = summary(&app).await;
    assert_eq!(body["everyone"], true);
    let rows = body["interpretations"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    let row = &rows[0];
    assert_eq!(row["radargram_id"], RADARGRAM);
    assert_eq!(row["user"], "default");
    assert_eq!(row["state"], "live");
    assert_eq!(row["features"], 3);
    assert_eq!(row["vertices"], 7);
    assert_eq!(row["traces"], 40);
    assert_eq!(row["coverage_basis"], "current");
    // 0..=14 and 16..=24 are under some line, trace 15 is not: 24 of 40.
    assert_eq!(row["coverage"], 60.0);
    assert_eq!(row["layers"][0]["layer"], "bed");
    assert_eq!(row["layers"][0]["features"], 2);
    // 10 + 9 of 40.
    assert_eq!(row["layers"][0]["coverage"], 47.5);
    assert_eq!(row["layers"][1]["layer"], "surface");
    assert_eq!(row["layers"][1]["coverage"], 25.0);
    assert!(row["version"].is_string());
    assert!(row["bytes"].as_u64().unwrap() > 0);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn picks_on_an_older_revision_are_stale_and_measured_against_it() {
    let (dir, app) = project_app();
    // These carry no axes of their own and no snapshot of `older` was
    // kept, so they cannot be carried.
    place(
        dir.path(),
        "line-01/default.gprinterp.json",
        &document(RADARGRAM, Some("older")).to_string(),
    );

    let body = summary(&app).await;
    let row = &body["interpretations"][0];
    assert_eq!(row["state"], "stale", "{body}");
    assert_eq!(row["revision_id"], "older");
    // Measured as stored, against the current 40 traces, which is where the
    // viewer draws them, and said to be uncarried.
    assert_eq!(row["coverage_basis"], "uncarried");
    assert_eq!(row["traces"], 40);
    assert_eq!(row["coverage"], 60.0);
    assert_eq!(row["features"], 3);

    // With the older revision's axes kept but no anchor to carry through,
    // coverage is measured as drawn, against its 20 traces: all but the gap
    // at trace 15, since the second bed line is clamped to the end.
    let project = Project::discover(dir.path()).unwrap().unwrap();
    crate::project::revisions::put(
        project.documents(),
        &crate::identity::RadargramId::new(RADARGRAM).unwrap(),
        &crate::project::revisions::AxisSnapshot {
            radargram_id: RADARGRAM.to_string(),
            revision_id: "older".to_string(),
            y_anchor: None,
            y_values: vec![0.0; 8],
            x_values: (0..20).map(f64::from).collect(),
            y_alternate: None,
        },
    )
    .unwrap();
    let body = summary(&app).await;
    let row = &body["interpretations"][0];
    assert_eq!(row["traces"], 20, "{body}");
    assert_eq!(row["coverage"], 95.0);
    assert_eq!(row["coverage_basis"], "drawn");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn picks_with_no_radargram_are_orphaned_and_a_bad_file_does_not_hide_them() {
    let (dir, app) = project_app();
    place(
        dir.path(),
        "gone-line/default.gprinterp.json",
        &document("gone-line", None).to_string(),
    );
    place(dir.path(), "gone-line/Not A User.gprinterp.json", "{}");
    place(dir.path(), "gone-line/broken.gprinterp.json", "not json");

    let body = summary(&app).await;
    let rows = body["interpretations"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["radargram_id"], "gone-line");
    assert_eq!(rows[0]["state"], "orphaned");
    assert_eq!(rows[0]["coverage"], Value::Null);

    let mut skipped: Vec<&str> = body["unreadable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["path"].as_str().unwrap())
        .collect();
    skipped.sort();
    assert_eq!(
        skipped,
        vec![
            "gone-line/Not A User.gprinterp.json",
            "gone-line/broken.gprinterp.json"
        ]
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn removing_a_radargram_turns_its_picks_into_archived_rows() {
    let (_dir, app) = project_app();
    save(&app, "default", &document(RADARGRAM, None)).await;

    let (status, body) = send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/datasets/{RADARGRAM}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");

    let body = summary(&app).await;
    let rows = body["interpretations"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    let row = &rows[0];
    assert_eq!(row["state"], "archived");
    assert_eq!(row["user"], "default");
    assert!(row["removal"].is_string(), "{row}");
    assert_eq!(row["version"], Value::Null);
    assert_eq!(row["features"], 3);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn layer_usage_still_counts_only_radargrams_in_the_catalog() {
    let (dir, app) = project_app();
    save(&app, "default", &document(RADARGRAM, None)).await;
    place(
        dir.path(),
        "gone-line/default.gprinterp.json",
        &document("gone-line", None).to_string(),
    );

    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/api/v1/layers/usage")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Neither layer is defined in a fresh project.
    assert_eq!(body["undefined"], json!({"bed": 2, "surface": 1}));
}
