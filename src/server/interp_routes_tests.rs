//! HTTP-level tests for the interpretation and layer write routes.
//!
//! Driven through the real Axum router with `ServiceExt::oneshot`, so the
//! status codes, `ETag`/`If-Match` handling and error envelopes are the ones
//! a browser will actually see -- not just the store behaviour underneath,
//! which `project::store` already covers.
//!
//! Every test here builds an `AppState`, which creates and opens a NetCDF.
//! netcdf-c is not thread-safe, so they carry the same
//! `#[serial_test::serial(netcdf)]` guard as the tests in `app.rs`; without
//! it they pass alone and flake in a full run.

use std::path::Path as StdPath;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::Value;
use tower::ServiceExt;

use super::app::{build_router, AccessOptions, AppState};
use crate::project::Project;
use crate::server::render_service::RenderServiceConfig;

const RADARGRAM: &str = "line-01";

/// Where a project keeps everything Ridal owns, since #187 moved it out of
/// the project root. Tests reach past the API to assert on files on disk --
/// the layout is part of the contract -- so they need the same answer the
/// code has.
fn data(root: &StdPath) -> std::path::PathBuf {
    root.join(crate::project::DEFAULT_DATA_DIR)
}

/// Where a freshly initialized project keeps its radargrams, and where an
/// upload from the browser lands.
fn radargrams(root: &StdPath) -> std::path::PathBuf {
    data(root).join(crate::project::DEFAULT_RADARGRAM_DIR)
}

fn write_test_nc(path: &StdPath, radargram_id: &str) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", 8).unwrap();
    file.add_dimension("x", 40).unwrap();
    let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    // Varying, not constant: the renderer refuses an array whose amplitude
    // percentiles collapse to a single value, because there is no contrast
    // to stretch. A flat fixture is not a realistic radargram anyway.
    let data: Vec<f32> = (0..(8 * 40)).map(|i| (i % 97) as f32).collect();
    var.put_values(&data, ..).unwrap();
    file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
        .unwrap();
    file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
        .unwrap();
    file.add_attribute("ridal_radargram_id", radargram_id)
        .unwrap();
}

/// A valid NetCDF file with none of ridal's attributes at all -- genuinely
/// unrelated, as opposed to old ridal output (#167). Distinct from posting
/// non-NetCDF bytes: this exercises the branch reached only once a file
/// parses successfully and still isn't recognized.
pub(super) fn write_unrelated_test_nc(path: &StdPath) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("x", 3).unwrap();
    let mut var = file.add_variable::<f32>("temperature", &["x"]).unwrap();
    var.put_values(&[1.0f32, 2.0, 3.0], ..).unwrap();
}

/// A ridal file old enough to predate radargram ids (#116): the pre-rename
/// unprefixed `program_version` attribute and none of the `ridal_*` ones,
/// mirroring the shape of real pre-0.6 output (#167).
pub(super) fn write_legacy_test_nc(path: &StdPath, version: &str) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", 8).unwrap();
    file.add_dimension("x", 40).unwrap();
    let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    let data: Vec<f32> = (0..(8 * 40)).map(|i| (i % 97) as f32).collect();
    var.put_values(&data, ..).unwrap();
    file.add_attribute("processing_datetime", "2020-01-01T00:00:00Z")
        .unwrap();
    file.add_attribute("program_version", version).unwrap();
}

/// A radargram with the coordinate variables a level 2 export needs.
///
/// `write_test_nc` deliberately writes the bare minimum the catalog
/// recognises; deriving level 2 additionally needs distance, travel time,
/// depth and positions, so those are written here rather than bloating the
/// fixture every other test uses.
pub(super) fn write_test_nc_with_axes(path: &StdPath, radargram_id: &str, group: Option<&str>) {
    write_test_nc_with_axes_at(path, radargram_id, group, "2020-01-01T00:00:00Z")
}

/// The same, with the processing datetime chosen.
///
/// The revision id is `hash(radargram_id + processing_datetime)`, so this
/// is what makes two fixtures two *revisions* of one radargram rather than
/// two files that collide on one id.
pub(super) fn write_test_nc_with_axes_at(
    path: &StdPath,
    radargram_id: &str,
    group: Option<&str>,
    processing_datetime: &str,
) {
    write_test_nc_full(path, radargram_id, group, processing_datetime, 4.0, 4.0)
}

/// A revision on which no zero correction has ever run.
///
/// `twtt_time_zero` of zero is the sentinel for "never located", so this
/// has no travel-time anchor at all — only the recording clock. It is the
/// ordinary state of a radargram between acquisition and the first
/// correction, and the case where replacing one used to be refused.
pub(super) fn write_test_nc_uncorrected(
    path: &StdPath,
    radargram_id: &str,
    processing_datetime: &str,
) {
    write_test_nc_full(path, radargram_id, None, processing_datetime, 0.0, 0.0)
}

fn write_test_nc_full(
    path: &StdPath,
    radargram_id: &str,
    group: Option<&str>,
    processing_datetime: &str,
    crop_ns: f64,
    time_zero_ns: f64,
) {
    let (n_samples, n_traces) = (8usize, 40usize);
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", n_samples).unwrap();
    file.add_dimension("x", n_traces).unwrap();
    let mut data = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    let values: Vec<f32> = (0..(n_samples * n_traces))
        .map(|i| (i % 97) as f32)
        .collect();
    data.put_values(&values, ..).unwrap();

    // 1 m between traces, running due east, so expected values are obvious.
    let mut put = |name: &str, values: Vec<f64>| {
        let mut var = file.add_variable::<f64>(name, &["x"]).unwrap();
        var.put_values(&values, ..).unwrap();
    };
    put("distance", (0..n_traces).map(|i| i as f64).collect());
    put(
        "easting",
        (0..n_traces).map(|i| 400_000.0 + i as f64).collect(),
    );
    put("northing", vec![8_700_000.0; n_traces]);
    put(
        "longitude",
        (0..n_traces).map(|i| 15.0 + i as f64 * 1e-5).collect(),
    );
    put("latitude", vec![78.0; n_traces]);
    // Track reading needs per-trace acquisition time as well as position:
    // it uses the time gaps to decide where a profile breaks.
    put(
        "time",
        (0..n_traces).map(|i| 1_677_501_559.0 + i as f64).collect(),
    );

    let mut twtt = file.add_variable::<f64>("twtt", &["y"]).unwrap();
    twtt.put_values(
        &(0..n_samples).map(|i| i as f64 * 0.4).collect::<Vec<f64>>(),
        ..,
    )
    .unwrap();
    let mut depth = file.add_variable::<f64>("depth", &["y"]).unwrap();
    depth
        .put_values(
            &(0..n_samples)
                .map(|i| i as f64 * 0.04)
                .collect::<Vec<f64>>(),
            ..,
        )
        .unwrap();

    file.add_attribute("ridal_processing_datetime", processing_datetime)
        .unwrap();
    file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
        .unwrap();
    file.add_attribute("ridal_radargram_id", radargram_id)
        .unwrap();
    file.add_attribute("crs", "EPSG:32633").unwrap();

    // What #144 added, so this fixture is a radargram processed by a
    // current Ridal: the anchor name on the axis, and where sample 0 and
    // time zero sit on the recording clock. Without these the radargram
    // cannot describe its own axes and #146 emits nothing, which is the
    // case `write_test_nc` covers.
    {
        let mut twtt = file.variable_mut("twtt").unwrap();
        twtt.put_attribute("anchor_name", "twtt").unwrap();
    }
    // Equal, which is what a zero correction leaves: sample 0 *is* time
    // zero, so its travel time is zero. `time_zero_ns` of `0.0` would
    // instead mean time zero was never located -- see
    // `write_test_nc_uncorrected`.
    let mut crop = file.add_variable::<f64>("twtt_crop", &[]).unwrap();
    crop.put_value(crop_ns, ()).unwrap();
    let mut zero = file.add_variable::<f64>("twtt_time_zero", &[]).unwrap();
    zero.put_value(time_zero_ns, ()).unwrap();
    // Written while the file is being created. Both attributes are needed:
    // `resolve_group` treats a bare id as no group at all, since the id only
    // exists to give the name a URL-safe form.
    if let Some(group) = group {
        file.add_attribute("ridal_group_name", group).unwrap();
        file.add_attribute("ridal_group_id", group).unwrap();
    }
}

/// A writable project with two radargrams in one group, both carrying
/// coordinate axes -- the shape a merged download is about.
fn group_app() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    let radargrams = radargrams(dir.path());
    for id in ["line-01", "line-02"] {
        write_test_nc_with_axes(&radargrams.join(format!("{id}.nc")), id, Some("survey"));
    }
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

/// A catalog holding both a group and a radargram belonging to no group.
///
/// The catalog scope has to cover both, which a fixture where everything is
/// grouped could not tell apart from "every group, merged".
fn mixed_catalog_app() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    let radargrams = radargrams(dir.path());
    for id in ["line-01", "line-02"] {
        write_test_nc_with_axes(&radargrams.join(format!("{id}.nc")), id, Some("survey"));
    }
    write_test_nc_with_axes(&radargrams.join("loose-01.nc"), "loose-01", None);
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

/// A writable project whose radargram carries full coordinate axes.
fn project_app_with_axes() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc_with_axes(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM, None);
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

/// A project containing one radargram, served writable unless stated.
///
/// No `users.json`, so this is the unconfigured case: every request is the
/// local `default` user with the `operator` role, which is how Ridal behaved
/// before authentication existed and what every test written then assumes.
/// The authenticated cases build their own state in `auth_routes_tests`.
fn project_app(writable: bool) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM);
    let app = app_for(dir.path(), writable);
    (dir, app)
}

/// A router over a project directory that already exists.
///
/// Separate from `project_app` so a test can edit `ridal.toml` by hand and
/// then open it, which is the only way to reach the config-reading paths:
/// the API refuses to store what those have to survive.
fn app_for(dir: &StdPath, writable: bool) -> Router {
    let project = Project::discover(dir).unwrap().unwrap();
    let state = Arc::new(
        AppState::build_with_project(
            dir,
            &RenderServiceConfig::default(),
            Some(project),
            AccessOptions {
                read_only: !writable,
                ..AccessOptions::default()
            },
        )
        .unwrap(),
    );
    build_router(state)
}

/// A bare directory of radargrams: the pre-existing read-only arrangement.
fn bare_app() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    write_test_nc(&dir.path().join("line-01.nc"), RADARGRAM);
    let state = Arc::new(
        AppState::build_with_project(
            dir.path(),
            &RenderServiceConfig::default(),
            None,
            AccessOptions::default(),
        )
        .unwrap(),
    );
    (dir, build_router(state))
}

fn document(key: &str) -> Value {
    serde_json::json!({
        "key": key,
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [30.0, 3.0]]},
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    })
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, etag, body)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Option<String>, Value) {
    send(
        app,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

async fn put(
    app: &Router,
    uri: &str,
    body: &Value,
    if_match: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut builder = Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(value) = if_match {
        builder = builder.header(header::IF_MATCH, value);
    }
    send(app, builder.body(Body::from(body.to_string())).unwrap()).await
}

const URI: &str = "/api/v1/datasets/line-01/interpretations/default";

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_interpretation_is_created_then_updated() {
    let (_dir, app) = project_app(true);

    let (status, etag, _) = put(&app, URI, &document(RADARGRAM), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let etag = etag.expect("a write must return an ETag for the next If-Match");

    let (status, get_etag, body) = get(&app, URI).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(get_etag.as_deref(), Some(etag.as_str()));
    assert_eq!(body["key"], RADARGRAM);
    assert_eq!(body["features"][0]["properties"]["label"], "bed");

    // Updating an existing document is 200, not 201.
    let (status, _, _) = put(&app, URI, &document(RADARGRAM), Some(&etag)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_stale_if_match_is_refused_with_412() {
    // The two-tab case. The second save must not silently win.
    let (_dir, app) = project_app(true);
    let (_, first, _) = put(&app, URI, &document(RADARGRAM), None).await;
    let first = first.unwrap();

    let mut changed = document(RADARGRAM);
    changed["features"] = serde_json::json!([]);
    put(&app, URI, &changed, Some(&first)).await;

    let (status, _, body) = put(&app, URI, &document(RADARGRAM), Some(&first)).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(body["error"]["code"], "version_conflict");

    // The intervening edit survived.
    let (_, _, stored) = get(&app, URI).await;
    assert_eq!(stored["features"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn if_none_match_star_refuses_to_clobber() {
    let (_dir, app) = project_app(true);
    put(&app, URI, &document(RADARGRAM), None).await;

    let request = Request::builder()
        .method("PUT")
        .uri(URI)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::IF_NONE_MATCH, "*")
        .body(Body::from(document(RADARGRAM).to_string()))
        .unwrap();
    let (status, _, _) = send(&app, request).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_document_naming_another_radargram_is_rejected() {
    let (_dir, app) = project_app(true);
    let (status, _, body) = put(&app, URI, &document("some-other-line"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "key_mismatch");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn writing_to_an_unknown_radargram_is_404_not_a_stray_directory() {
    let (dir, app) = project_app(true);
    let (status, _, _) = put(
        &app,
        "/api/v1/datasets/not-here/interpretations/default",
        &document("not-here"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        !data(dir.path()).join("interpretations/not-here").exists(),
        "a typo in the URL must not create an interpretation directory"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_user_name_cannot_escape_the_interpretations_directory() {
    let (_dir, app) = project_app(true);
    // Axum's path matching already rejects most of these; the point is that
    // nothing reaches the filesystem regardless of how it is encoded.
    for user in ["..", "%2e%2e", "not_a_slug!"] {
        let uri = format!("/api/v1/datasets/line-01/interpretations/{user}");
        let (status, _, _) = put(&app, &uri, &document(RADARGRAM), None).await;
        assert!(
            status.is_client_error(),
            "user '{user}' produced {status}, expected a client error"
        );
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn deleting_removes_the_document_and_then_404s() {
    let (_dir, app) = project_app(true);
    put(&app, URI, &document(RADARGRAM), None).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(URI)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let (status, _, _) = get(&app, URI).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn listing_reports_users_and_whether_writes_are_possible() {
    let (_dir, app) = project_app(true);
    put(&app, URI, &document(RADARGRAM), None).await;

    let (status, _, body) = get(&app, "/api/v1/datasets/line-01/interpretations").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["users"], serde_json::json!(["default"]));
    assert_eq!(body["writable"], true);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_read_only_server_refuses_writes_but_still_serves_reads() {
    let (_dir, writable) = project_app(true);
    put(&writable, URI, &document(RADARGRAM), None).await;

    let (_dir2, app) = project_app(false);
    let (status, _, body) = put(&app, URI, &document(RADARGRAM), None).await;
    // A 403 rather than a 409 since #131: --read-only is a cap on the
    // caller's role, not a separate switch on the write routes, so this is
    // "you may not" rather than "the server is not in a state to". The code
    // still names the flag, because the operator of the server is the one
    // who can change it.
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "read_only");

    let (status, _, body) = get(&app, "/api/v1/datasets/line-01/interpretations").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["writable"], false);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_catalog_that_is_not_a_project_explains_itself() {
    // The pre-existing read-only arrangement must keep working, and must say
    // why saving is unavailable rather than failing obscurely.
    let (_dir, app) = bare_app();

    let (status, _, body) = put(&app, URI, &document(RADARGRAM), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "not_a_project");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ridal project init"),
        "the error should say how to fix it: {body}"
    );

    // Layers still answer, so the viewer's page load does not fail.
    let (status, _, body) = get(&app, "/api/v1/layers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["layers"], serde_json::json!([]));
    assert_eq!(body["writable"], false);
}

fn overhanging_document() -> Value {
    serde_json::json!({
        "key": RADARGRAM,
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString",
                         "coordinates": [[5.0, 2.0], [30.0, 3.0], [20.0, 5.0]]},
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    })
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_overhanging_line_is_refused_by_default() {
    // The guardrail is on for a layer nobody has opted out of -- including
    // one the vocabulary does not define at all.
    let (_dir, app) = project_app(true);
    let (status, _, body) = put(&app, URI, &overhanging_document(), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "overhang");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("f-0001"), "{message}");
    assert!(message.contains("bed"), "{message}");

    let (status, _, _) = get(&app, URI).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a refused save must not have stored anything"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_layer_that_allows_overhangs_accepts_one() {
    let (_dir, app) = project_app(true);
    let layers = serde_json::json!([
        {"id": "bed", "name": "Bed", "allow_overhangs": true}
    ]);
    let (status, _, _) = put(&app, "/api/v1/layers", &layers, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = put(&app, URI, &overhanging_document(), None).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn turning_the_guardrail_off_and_on_again_changes_what_is_accepted() {
    // The setting is read at save time, not baked in at startup, so a
    // project can tighten the rule without a restart.
    let (_dir, app) = project_app(true);
    put(
        &app,
        "/api/v1/layers",
        &serde_json::json!([{"id": "bed", "name": "Bed", "allow_overhangs": true}]),
        None,
    )
    .await;
    assert_eq!(
        put(&app, URI, &overhanging_document(), None).await.0,
        StatusCode::CREATED
    );

    put(
        &app,
        "/api/v1/layers",
        &serde_json::json!([{"id": "bed", "name": "Bed"}]),
        None,
    )
    .await;
    assert_eq!(
        put(&app, URI, &overhanging_document(), None).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_overhang_flag_round_trips_and_defaults_to_off() {
    let (_dir, app) = project_app(true);
    let layers = serde_json::json!([
        {"id": "bed", "name": "Bed"},
        {"id": "crevasse", "name": "Crevasse", "allow_overhangs": true}
    ]);
    put(&app, "/api/v1/layers", &layers, None).await;

    let (_, _, body) = get(&app, "/api/v1/layers").await;
    // Absent rather than `false`: the default is not written out.
    assert!(body["layers"][0].get("allow_overhangs").is_none());
    assert_eq!(body["layers"][1]["allow_overhangs"], true);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn layers_round_trip_through_the_api() {
    let (_dir, app) = project_app(true);

    let (status, _, body) = get(&app, "/api/v1/layers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["layers"], serde_json::json!([]));

    // A bare array is accepted: the GUI only ever has the list.
    let layers = serde_json::json!([
        {"id": "bed", "name": "Bed", "color": "#e6194b"},
        {"id": "internal", "name": "Internal reflector", "color": "#3cb44b"}
    ]);
    let (status, etag, _) = put(&app, "/api/v1/layers", &layers, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(etag.is_some());

    let (_, _, body) = get(&app, "/api/v1/layers").await;
    assert_eq!(body["layers"][0]["id"], "bed");
    assert_eq!(body["layers"][1]["name"], "Internal reflector");
    assert_eq!(body["writable"], true);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_layer_id_that_would_not_survive_export_is_rejected() {
    let (_dir, app) = project_app(true);
    let layers = serde_json::json!([{"id": "Bed Layer", "name": "Bed"}]);
    let (status, _, body) = put(&app, "/api/v1/layers", &layers, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_layers");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn duplicate_layer_ids_are_rejected() {
    let (_dir, app) = project_app(true);
    let layers = serde_json::json!([
        {"id": "bed", "name": "Bed"},
        {"id": "bed", "name": "Bed again"}
    ]);
    let (status, _, _) = put(&app, "/api/v1/layers", &layers, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

async fn page(app: &Router, uri: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_viewer_offers_picking_only_where_it_can_be_saved() {
    let (_dir, app) = project_app(true);
    let (status, html) = page(&app, "/view/line-01").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("writable: true"), "config flag missing");
    assert!(html.contains(r#"id="pick-toggle""#), "no picking control");
    assert!(html.contains(r#"id="pick-save""#), "no save control");
    assert!(
        html.contains(r#"id="pick-selection""#),
        "no selection panel"
    );
    assert!(
        html.contains("/static/picker.js"),
        "picker script not loaded"
    );
    assert!(html.contains(r#"user: "default""#), "no author for saves");

    // Read-only: the toolbar says why rather than vanishing, so a missing
    // control never reads as a missing feature.
    let (_dir2, read_only) = project_app(false);
    let (_, html) = page(&read_only, "/view/line-01").await;
    assert!(html.contains("read-only"), "{html}");
    assert!(!html.contains(r#"id="pick-toggle""#));

    let (_dir3, bare) = bare_app();
    let (_, html) = page(&bare, "/view/line-01").await;
    assert!(html.contains("ridal project init"), "{html}");
    assert!(!html.contains(r#"id="pick-toggle""#));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_level2_download_derives_from_the_saved_interpretation() {
    let (_dir, app) = project_app_with_axes();
    let (status, _, _) = put(&app, URI, &document(RADARGRAM), None).await;
    assert_eq!(status, StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{URI}/level2?spacing=vertices&format=csv"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let disposition = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        disposition.contains("line-01-default-level2.csv"),
        "{disposition}"
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let csv = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        csv.starts_with("radargram_id,revision_id,layer,line_index,point_index,"),
        "{csv}"
    );
    assert!(csv.contains("line-01,"), "{csv}");
    assert!(csv.contains(",bed,0,0,f-0001,"), "{csv}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn downloading_before_anything_is_saved_says_so() {
    let (_dir, app) = project_app_with_axes();
    let (status, _, body) = get(&app, &format!("{URI}/level2")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Save some picks first"),
        "{body}"
    );
}

async fn raw(app: &Router, uri: &str) -> (StatusCode, Option<String>, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let disposition = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, disposition, bytes.to_vec())
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_group_track_merges_every_member_and_names_each() {
    let (_dir, app) = group_app();
    let (status, disposition, bytes) = raw(&app, "/api/v1/groups/survey/track.geojson").await;
    assert_eq!(status, StatusCode::OK);
    assert!(disposition.unwrap().contains("survey-tracks.geojson"));

    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let ids: Vec<&str> = body["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["properties"]["radargram_id"].as_str().unwrap())
        .collect();
    // The merge is reversible: every feature says where it came from.
    assert!(ids.contains(&"line-01"), "{ids:?}");
    assert!(ids.contains(&"line-02"), "{ids:?}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_group_level2_merges_points_that_name_their_radargram() {
    let (_dir, app) = group_app();
    for id in ["line-01", "line-02"] {
        let uri = format!("/api/v1/datasets/{id}/interpretations/default");
        let (status, _, _) = put(&app, &uri, &document(id), None).await;
        assert_eq!(status, StatusCode::CREATED, "{id}");
    }

    let (status, disposition, bytes) = raw(
        &app,
        "/api/v1/groups/survey/level2?spacing=vertices&format=csv",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(disposition.unwrap().contains("survey-level2.csv"));

    let csv = String::from_utf8(bytes).unwrap();
    let mut lines = csv.lines();
    // One header for the whole file, radargram first.
    assert!(lines
        .next()
        .unwrap()
        .starts_with("radargram_id,revision_id,layer,"));
    let rows: Vec<&str> = lines.collect();
    assert!(rows.iter().any(|r| r.starts_with("line-01,")), "{csv}");
    assert!(rows.iter().any(|r| r.starts_with("line-02,")), "{csv}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_group_level2_says_which_members_it_left_out() {
    // Half a survey being unpicked is normal; a merged file that quietly
    // omitted it would look complete.
    let (_dir, app) = group_app();
    let (status, _, _) = put(
        &app,
        "/api/v1/datasets/line-01/interpretations/default",
        &document("line-01"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/groups/survey/level2?format=csv")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let warning = response
        .headers()
        .get(header::WARNING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(warning.contains("line-02"), "{warning}");
    // A person reads this now -- the browser shows it beside the download
    // rather than discarding it with the rest of the headers -- so it has
    // to say why the file is short, not just which radargram is missing.
    assert!(warning.contains("interpreted"), "{warning}");
    assert!(
        warning.contains("of the"),
        "counting them makes 'is this file complete?' answerable at a \
         glance: {warning}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_group_with_nothing_interpreted_says_so() {
    let (_dir, app) = group_app();
    let (status, _, body) = get(&app, "/api/v1/groups/survey/level2").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has been interpreted yet"),
        "{body}"
    );

    let (status, _, body) = get(&app, "/api/v1/groups/nope/track.geojson").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "group_not_found");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_catalog_track_covers_grouped_and_ungrouped_alike() {
    let (_dir, app) = mixed_catalog_app();
    let (status, disposition, bytes) = raw(&app, "/api/v1/catalog/track.geojson").await;
    assert_eq!(status, StatusCode::OK);
    assert!(disposition.unwrap().contains("catalog-tracks.geojson"));

    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let ids: Vec<&str> = body["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["properties"]["radargram_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"line-01"), "{ids:?}");
    assert!(ids.contains(&"line-02"), "{ids:?}");
    // The one that belongs to no group is the point of this test: a catalog
    // download that only covered groups would silently drop it.
    assert!(ids.contains(&"loose-01"), "{ids:?}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_catalog_level2_merges_across_group_boundaries() {
    let (_dir, app) = mixed_catalog_app();
    for id in ["line-01", "loose-01"] {
        let uri = format!("/api/v1/datasets/{id}/interpretations/default");
        let (status, _, _) = put(&app, &uri, &document(id), None).await;
        assert_eq!(status, StatusCode::CREATED, "{id}");
    }

    let (status, disposition, bytes) =
        raw(&app, "/api/v1/catalog/level2?spacing=vertices&format=csv").await;
    assert_eq!(status, StatusCode::OK);
    assert!(disposition.unwrap().contains("catalog-level2.csv"));

    let csv = String::from_utf8(bytes).unwrap();
    let rows: Vec<&str> = csv.lines().skip(1).collect();
    assert!(rows.iter().any(|r| r.starts_with("line-01,")), "{csv}");
    assert!(rows.iter().any(|r| r.starts_with("loose-01,")), "{csv}");
    // Same schema as the group and single-radargram products, so the three
    // are concatenable and tell one story.
    assert!(csv.starts_with("radargram_id,revision_id,layer,"), "{csv}");

    // line-02 was never picked, and is named rather than silently missing --
    // the same rule the group scope follows, because it is the same code.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/catalog/level2?spacing=vertices&format=csv")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let warning = response
        .headers()
        .get(header::WARNING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(warning.contains("line-02"), "{warning}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_catalog_with_nothing_interpreted_says_so() {
    let (_dir, app) = mixed_catalog_app();
    let (status, _, body) = get(&app, "/api/v1/catalog/level2").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("this catalog"),
        "{body}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn single_level2_points_name_their_radargram_too() {
    // The same field, from the same place: it is on the point, not on the
    // file, which is what lets a merged file work at all.
    let (_dir, app) = project_app_with_axes();
    put(&app, URI, &document(RADARGRAM), None).await;
    let (_, _, bytes) = raw(&app, &format!("{URI}/level2?spacing=vertices")).await;
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["features"][0]["properties"]["radargram_id"], RADARGRAM);
    assert_eq!(body["ridal"]["sources"][0]["radargram_id"], RADARGRAM);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_radargram_downloads_byte_for_byte() {
    let (dir, app) = project_app(true);
    let (status, disposition, bytes) = raw(&app, "/api/v1/datasets/line-01/download").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        disposition.as_deref(),
        Some("attachment; filename=\"line-01.nc\"")
    );

    let source = std::fs::read(radargrams(dir.path()).join("line-01.nc")).unwrap();
    assert_eq!(
        bytes, source,
        "the download must be the file, not a re-write"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_track_downloads_as_geojson_naming_its_radargram() {
    let (_dir, app) = project_app_with_axes();
    let (status, disposition, bytes) = raw(&app, "/api/v1/datasets/line-01/track.geojson").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        disposition.unwrap().contains("line-01-track.geojson"),
        "downloads should be named after their radargram"
    );

    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["type"], "FeatureCollection");
    let feature = &body["features"][0];
    assert_eq!(feature["geometry"]["type"], "LineString");
    // The field Erik asked for, plus enough to tell segments apart.
    assert_eq!(feature["properties"]["radargram_id"], "line-01");
    assert!(feature["properties"]["trace_start"].is_number());
    assert!(feature["properties"]["n_traces"].is_number());
    // WGS84, per RFC 7946: longitude first.
    let first = &feature["geometry"]["coordinates"][0];
    assert!(first[0].as_f64().unwrap().abs() <= 180.0);
    assert!(first[1].as_f64().unwrap().abs() <= 90.0);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn raw_picks_download_exactly_what_is_stored() {
    // Not a re-serialisation: a field this version does not model must
    // survive a round trip out to another tool.
    let (_dir, app) = project_app(true);
    let mut document = document(RADARGRAM);
    document["from_a_future_version"] = serde_json::json!({"keep": "me"});
    put(&app, URI, &document, None).await;

    let (status, disposition, bytes) = raw(&app, &format!("{URI}/raw")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(disposition
        .unwrap()
        .contains("line-01-default.gprinterp.json"));
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["from_a_future_version"]["keep"], "me");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_image_renders_at_the_requested_width() {
    let (_dir, app) = project_app(true);
    // The fixture is 40 traces by 8 samples.
    let (status, disposition, bytes) = raw(
        &app,
        "/api/v1/datasets/line-01/views/standard/image?width=20",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        disposition.unwrap().contains("20x4.png"),
        "size in the name"
    );
    // PNG signature, then width and height from the IHDR chunk.
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    assert_eq!((width, height), (20, 4));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_image_is_never_upscaled_past_the_source() {
    // A wider image than there are traces carries no more information, and
    // asking for one is more likely a slip than an intent.
    let (_dir, app) = project_app(true);
    let (status, disposition, _) = raw(
        &app,
        "/api/v1/datasets/line-01/views/standard/image?width=5000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(disposition.unwrap().contains("40x8.png"));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_image_refuses_sizes_it_cannot_produce() {
    let (_dir, app) = project_app(true);
    let base = "/api/v1/datasets/line-01/views/standard/image";
    for (query, code) in [
        ("?width=0", "invalid_width"),
        ("?width=99999", "invalid_width"),
        ("?format=tiff", "invalid_format"),
    ] {
        let (status, _, body) = get(&app, &format!("{base}{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["error"]["code"], code, "{query}");
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn dataset_downloads_work_without_a_project() {
    // A radargram, its track and an image belong to the catalog, not to an
    // interpretation, so a bare directory still serves them.
    let (_dir, app) = bare_app();
    for uri in [
        "/api/v1/datasets/line-01/download",
        "/api/v1/datasets/line-01/views/standard/image?width=10",
    ] {
        let (status, _, _) = raw(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_default_profile_round_trips_through_the_settings_api() {
    let (dir, app) = project_app(true);

    let (status, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["project"], true);
    // Split since #131: what the Project section may do is an operator
    // question, and it is not the same question as whether this person may
    // edit their own preferences.
    assert_eq!(body["can_edit_project"], true);
    assert!(body["default_profile"].is_null(), "unset to begin with");
    assert!(
        body["profiles"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("abslog")),
        "the page needs the list to populate its select: {body}"
    );

    let (status, _, _) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_profile": "abslog"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(body["default_profile"], "abslog");

    // And on disk, so it survives a restart.
    let marker = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    assert!(marker.contains("default_profile = \"abslog\""), "{marker}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_export_against_the_wrong_radargram_is_refused() {
    // The CLI refused this and the HTTP route did not, so the same inputs
    // gave an error on one path and a plausible, wrong file on the other.
    // Needs the fixture with real axes: without a CRS, `read_geometry`
    // fails first and the identity check is never reached.
    let (_dir, app) = project_app_with_axes();

    // Written straight to disk, because the API will not store a mismatch:
    // `interpretations::write` already refuses a document whose key names a
    // different radargram. So the only way in is a hand-edited or moved
    // file -- which is exactly the case the export path has to survive.
    let mut doc = document("line-01");
    doc["key"] = serde_json::json!("some-other-line");
    let dir = data(_dir.path()).join("interpretations/line-01");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("default.gprinterp.json"),
        serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();

    let (status, _, body) = get(
        &app,
        "/api/v1/datasets/line-01/interpretations/default/level2",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "radargram_mismatch");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("drawn on radargram"),
        "{body}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_stale_delete_cannot_erase_newer_picks() {
    // Writing was version-checked and deleting was not, so a client
    // holding an old ETag could remove picks drawn after it last read.
    let (_dir, app) = project_app(true);
    let uri = "/api/v1/datasets/line-01/interpretations/default";

    let (status, etag, _) = put(&app, uri, &document("line-01"), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let first = etag.expect("a write returns an ETag");

    // Someone else edits. The document must genuinely differ: the version
    // is a content hash, so saving identical bytes leaves it unchanged and
    // there would be nothing stale about the first ETag.
    let mut newer = document("line-01");
    newer["features"][0]["properties"]["id"] = serde_json::json!("f-edited");
    let (status, _, _) = put(&app, uri, &newer, Some(&first)).await;
    assert_eq!(status, StatusCode::OK);

    // The stale holder tries to delete.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .header(header::IF_MATCH, &first)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);

    // And the picks are still there.
    let (status, _, _) = get(&app, uri).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the document survived the stale delete"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn if_match_star_requires_the_document_to_already_exist() {
    // `If-Match: *` asserts "replace what is there". Mapping it to the
    // store's `Any` made it satisfied by an absent document too, so a
    // client asserting an existence precondition would instead create one
    // and be told 201.
    let (_dir, app) = project_app(true);
    let uri = "/api/v1/datasets/line-01/interpretations/default";

    let (status, _, body) = put(&app, uri, &document("line-01"), Some("*")).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(body["error"]["code"], "version_conflict");

    // Once it exists, the same header succeeds whatever the version is --
    // that is what distinguishes `*` from naming a version.
    let (status, _, _) = put(&app, uri, &document("line-01"), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _) = put(&app, uri, &document("line-01"), Some("*")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_write_cannot_target_another_users_interpretation() {
    // The path parameter must not be the authorisation. Without this,
    // `PUT .../interpretations/alice` writes Alice's picks for anyone who
    // asks -- harmless with one user, a hole the moment logins exist.
    let (dir, app) = project_app(true);

    let (status, _, body) = put(
        &app,
        "/api/v1/datasets/line-01/interpretations/someone-else",
        &document("line-01"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "not_your_interpretation");
    // Nothing was created for them.
    assert!(
        !data(dir.path())
            .join("interpretations/line-01/someone-else.gprinterp.json")
            .exists(),
        "a refused write must not leave a document behind"
    );

    // Deleting someone else's is refused the same way.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/datasets/line-01/interpretations/someone-else")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // Writing as yourself still works, so the guard is not simply refusing
    // everything.
    let (status, _, _) = put(
        &app,
        "/api/v1/datasets/line-01/interpretations/default",
        &document("line-01"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn another_users_interpretation_can_still_be_read() {
    // Per-user means "you cannot change mine", not "you cannot see mine":
    // reads keep naming the user in the path.
    let (_dir, app) = project_app(true);
    put(
        &app,
        "/api/v1/datasets/line-01/interpretations/default",
        &document("line-01"),
        None,
    )
    .await;

    for uri in [
        "/api/v1/datasets/line-01/interpretations/default",
        "/api/v1/datasets/line-01/interpretations/default/raw",
    ] {
        let (status, _, _) = get(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_default_horizontal_scale_round_trips_and_reaches_the_viewer() {
    let (dir, app) = project_app(true);

    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    assert!(body["default_xscale"].is_null(), "unset to begin with");
    assert!(
        body["xscales"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["value"] == 2.0),
        "the page needs the offered factors to populate its select: {body}"
    );

    let (status, _, _) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_xscale": 2.0}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(body["default_xscale"], 2.0);
    let marker = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    assert!(marker.contains("default_xscale = 2.0"), "{marker}");

    // The point of the setting: the viewer opens already stretched, with
    // that option selected rather than 1x.
    let (status, html) = page(&app, "/view/line-01").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains(r#"<option value="2" selected>"#),
        "the viewer should preselect the project default"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn one_times_is_stored_as_no_preference_and_odd_scales_are_refused() {
    let (dir, app) = project_app(true);
    put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_xscale": 4.0}),
        None,
    )
    .await;

    // Back to 1x. It is the neutral value, not a preference, so the key
    // leaves the file rather than being written as 1.0.
    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_xscale": 1.0}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["default_xscale"].is_null(), "{body}");
    // Comment lines only -- `ridal project init` documents the key by name.
    let marker = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    let live = marker
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter(|l| l.contains("default_xscale"))
        .count();
    assert_eq!(live, 0, "{marker}");

    // A factor the viewer does not offer would leave every radargram
    // stretched with no dropdown entry to undo it.
    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_xscale": 3.7}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "unknown_xscale");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_default_profile_is_what_pages_render_with() {
    // The whole point of the setting: a page opened with no profile in its
    // address uses the project's, not the built-in one.
    let (_dir, app) = project_app(true);
    put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_profile": "abslog"}),
        None,
    )
    .await;

    for uri in ["/", "/view/line-01", "/layers", "/settings"] {
        let (status, html) = page(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(
            html.contains("?profile=abslog"),
            "{uri} did not pick up the project default"
        );
    }

    // An explicit request still wins.
    let (_, html) = page(&app, "/?profile=positive").await;
    assert!(html.contains("value=\"positive\" selected"), "{html}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_unknown_default_profile_is_refused() {
    // Storing a name nothing renders would leave every page failing with
    // no obvious cause.
    let (_dir, app) = project_app(true);
    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_profile": "nope"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "unknown_profile");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_default_profile_can_be_cleared() {
    let (_dir, app) = project_app(true);
    let settings = "/api/v1/project/settings";
    put(
        &app,
        settings,
        &serde_json::json!({"default_profile": "abslog"}),
        None,
    )
    .await;
    put(
        &app,
        settings,
        &serde_json::json!({"default_profile": null}),
        None,
    )
    .await;

    let (_, _, body) = get(&app, settings).await;
    assert!(body["default_profile"].is_null(), "{body}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn settings_are_read_only_where_writes_are() {
    let (_dir, app) = project_app(false);
    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_profile": "abslog"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "read_only");

    // Reading still works, and the page says why the form is inert.
    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(body["can_edit_project"], false);
    let (status, html) = page(&app, "/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("read-only"), "{html}");
}

/// One valid basemap, as the settings page would send it.
fn a_basemap(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": format!("Basemap {id}"),
        "url": format!("https://tile.example.org/{id}/{{z}}/{{x}}/{{y}}.png"),
    })
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn basemaps_round_trip_through_the_settings_api() {
    let (dir, app) = project_app(true);

    // A project that has defined none still offers the built-in, or every
    // map in the GUI would draw nothing.
    let (status, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["basemaps"].as_array().unwrap().len(), 1);
    assert_eq!(body["basemaps"][0]["id"], "esri-world-imagery");
    assert_eq!(body["project_basemaps"].as_array().unwrap().len(), 0);
    assert_eq!(body["built_in_basemap"], true);

    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({
            "basemaps": [a_basemap("osm")],
            "default_basemap": "osm",
        }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    // Both lists: what may be chosen (with the built-in first) and what may
    // be edited (the project's own).
    let offered: Vec<&str> = body["basemaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|map| map["id"].as_str().unwrap())
        .collect();
    assert_eq!(offered, vec!["esri-world-imagery", "osm"]);
    assert_eq!(body["project_basemaps"].as_array().unwrap().len(), 1);
    assert_eq!(body["default_basemap"], "osm");
    // The browser needs every optional value resolved, so it keeps no
    // defaults of its own.
    assert_eq!(body["basemaps"][1]["tile_size"], 256);
    assert_eq!(body["basemaps"][1]["max_zoom"], 18);

    // And on disk, so it survives a restart.
    let marker = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    assert!(marker.contains("[[basemaps]]"), "{marker}");
    assert!(marker.contains("default_basemap = \"osm\""), "{marker}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_basemap_that_could_not_be_drawn_is_refused() {
    let (_dir, app) = project_app(true);
    for (entry, code) in [
        (
            serde_json::json!({"id": "bad", "name": "Bad", "url": "javascript:alert(1)"}),
            "invalid_basemap",
        ),
        (
            serde_json::json!({"id": "Bad Id", "name": "Bad", "url": "https://t.example.org/{z}/{x}/{y}.png"}),
            "invalid_basemap",
        ),
        (
            // The built-in's id is reserved: two answers to "the built-in"
            // would depend on which was found first.
            serde_json::json!({"id": "esri-world-imagery", "name": "Mine", "url": "https://t.example.org/{z}/{x}/{y}.png"}),
            "invalid_basemap",
        ),
    ] {
        let (status, _, body) = put(
            &app,
            "/api/v1/project/settings",
            &serde_json::json!({"basemaps": [entry]}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], code, "{body}");
    }

    // And nothing was stored on the way to refusing.
    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(body["project_basemaps"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_default_basemap_nothing_offers_is_refused() {
    let (_dir, app) = project_app(true);
    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_basemap": "nope"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "unknown_basemap");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn each_half_of_the_settings_page_leaves_the_other_alone() {
    // Two forms save this one document. Absent must mean "unchanged" rather
    // than "clear it", or saving a basemap would quietly drop the project's
    // default profile -- and nobody would connect the two.
    let (_dir, app) = project_app(true);
    let settings = "/api/v1/project/settings";

    put(
        &app,
        settings,
        &serde_json::json!({"default_profile": "abslog", "default_xscale": 2.0}),
        None,
    )
    .await;
    put(
        &app,
        settings,
        &serde_json::json!({"basemaps": [a_basemap("osm")], "default_basemap": "osm"}),
        None,
    )
    .await;

    let (_, _, body) = get(&app, settings).await;
    assert_eq!(body["default_profile"], "abslog", "{body}");
    assert_eq!(body["default_xscale"], 2.0, "{body}");

    // And the other way around: saving the render defaults must not remove
    // the basemaps.
    put(
        &app,
        settings,
        &serde_json::json!({"default_profile": "positive", "default_xscale": null}),
        None,
    )
    .await;
    let (_, _, body) = get(&app, settings).await;
    assert_eq!(
        body["project_basemaps"].as_array().unwrap().len(),
        1,
        "{body}"
    );
    assert_eq!(body["default_basemap"], "osm", "{body}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn removing_a_basemap_that_was_the_default_does_not_fail() {
    // An unset default means the first offered, which is where a dangling
    // one would land anyway -- so this is a removal, not a conflict.
    let (_dir, app) = project_app(true);
    let settings = "/api/v1/project/settings";
    put(
        &app,
        settings,
        &serde_json::json!({"basemaps": [a_basemap("osm")], "default_basemap": "osm"}),
        None,
    )
    .await;

    let (status, _, body) = put(&app, settings, &serde_json::json!({"basemaps": []}), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["default_basemap"].is_null(), "{body}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_hand_broken_basemap_costs_that_basemap_and_not_the_page() {
    // `ridal.toml` is meant to be hand-edited. One bad entry must not leave
    // every map blank, and the settings page has to be able to say why the
    // entry is missing.
    let (dir, app) = project_app(true);
    drop(app);
    let marker = dir.path().join("ridal.toml");
    let text = std::fs::read_to_string(&marker).unwrap();
    std::fs::write(
        &marker,
        format!(
            "{text}\n[[basemaps]]\nid = \"good\"\nname = \"Good\"\n\
             url = \"https://tile.example.org/{{z}}/{{x}}/{{y}}.png\"\n\
             \n[[basemaps]]\nid = \"bad\"\nname = \"Bad\"\nurl = \"nonsense\"\n"
        ),
    )
    .unwrap();

    // Reopened, because the config is read when the project is opened.
    let app = app_for(dir.path(), true);

    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    let offered: Vec<&str> = body["basemaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|map| map["id"].as_str().unwrap())
        .collect();
    assert_eq!(offered, vec!["esri-world-imagery", "good"], "{body}");
    let problems = body["basemap_problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{body}");
    assert!(problems[0].as_str().unwrap().contains("'bad'"), "{body}");

    // The catalog page still draws maps, on what is left.
    let (status, html) = page(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("data-basemaps="), "{html}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn overlays_round_trip_and_reach_every_page_with_a_map() {
    // #177's overlay half: a GeoJSON added in the settings becomes a layer
    // the catalog and the viewer can switch on.
    let (dir, app) = project_app(true);

    let (status, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["overlays"].as_array().unwrap().len(), 0);

    let stakes = serde_json::json!({
        "id": "stakes",
        "name": "Mass balance stakes",
        "url": "https://static.example.org/shapes/stakes.geojson",
        // The two popup dials: the property naming each feature -- the
        // generalisation of the hardcoded `properties.Stake` this came from
        // -- and the one describing it.
        "name_field": "Stake",
        "description_field": "notes",
    });
    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({ "overlays": [stakes] }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["overlays"][0]["name_field"], "Stake");

    // Delivered to both map-bearing pages, with the colour resolved so the
    // browser keeps no defaults of its own.
    for uri in ["/", "/view/line-01"] {
        let (status, html) = page(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(html.contains("data-overlays="), "{uri} carries no overlays");
        assert!(html.contains("Mass balance stakes"), "{uri}: {html}");
        assert!(html.contains("3aa3e3"), "{uri} did not resolve the colour");
    }

    let marker = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    assert!(marker.contains("[[overlays]]"), "{marker}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_overlay_that_could_not_be_drawn_is_refused() {
    let (_dir, app) = project_app(true);
    for entry in [
        // Not something a browser will fetch a document from.
        serde_json::json!({"id": "bad", "name": "Bad", "url": "javascript:alert(1)"}),
        // A colour that is not a colour reaches the map as a style.
        serde_json::json!({
            "id": "bad", "name": "Bad",
            "url": "https://example.org/x.geojson",
            "color": "url(http://tracker.example/x.png)"
        }),
        // A property called nothing would look like a popup that does not
        // work, rather than like a mistake.
        serde_json::json!({
            "id": "bad", "name": "Bad",
            "url": "https://example.org/x.geojson", "name_field": ""
        }),
    ] {
        let (status, _, body) = put(
            &app,
            "/api/v1/project/settings",
            &serde_json::json!({ "overlays": [entry] }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_overlay", "{body}");
    }

    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(body["overlays"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn saving_overlays_leaves_the_basemaps_alone_and_the_other_way_round() {
    // Three sections of one settings page write one file. Each sends only
    // its own half, and must not clear the others'.
    let (_dir, app) = project_app(true);
    let settings = "/api/v1/project/settings";

    put(
        &app,
        settings,
        &serde_json::json!({"basemaps": [a_basemap("osm")], "default_basemap": "osm"}),
        None,
    )
    .await;
    put(
        &app,
        settings,
        &serde_json::json!({"overlays": [{
            "id": "stakes", "name": "Stakes",
            "url": "https://static.example.org/shapes/stakes.geojson"
        }]}),
        None,
    )
    .await;

    let (_, _, body) = get(&app, settings).await;
    assert_eq!(
        body["project_basemaps"].as_array().unwrap().len(),
        1,
        "{body}"
    );
    assert_eq!(body["default_basemap"], "osm", "{body}");
    assert_eq!(body["overlays"].as_array().unwrap().len(), 1, "{body}");

    // And a basemap save afterwards leaves the overlay in place.
    put(&app, settings, &serde_json::json!({"basemaps": []}), None).await;
    let (_, _, body) = get(&app, settings).await;
    assert_eq!(body["overlays"].as_array().unwrap().len(), 1, "{body}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_hand_broken_overlay_costs_that_overlay_and_not_the_page() {
    let (dir, app) = project_app(true);
    drop(app);
    let marker = dir.path().join("ridal.toml");
    let text = std::fs::read_to_string(&marker).unwrap();
    std::fs::write(
        &marker,
        format!(
            "{text}\n[[overlays]]\nid = \"good\"\nname = \"Good\"\n\
             url = \"https://static.example.org/good.geojson\"\n\
             \n[[overlays]]\nid = \"bad\"\nname = \"Bad\"\nurl = \"nonsense\"\n"
        ),
    )
    .unwrap();

    let app = app_for(dir.path(), true);
    let (_, _, body) = get(&app, "/api/v1/project/settings").await;
    // Both are listed for editing -- the broken one has to be reachable to
    // be fixed -- while only the usable one is served to the maps.
    assert_eq!(body["overlays"].as_array().unwrap().len(), 2, "{body}");
    let problems = body["overlay_problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{body}");
    assert!(problems[0].as_str().unwrap().contains("'bad'"), "{body}");

    let (status, html) = page(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Good"), "{html}");
    assert!(!html.contains("nonsense"), "the broken one is not served");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_download_defaults_round_trip_and_reach_both_dialogs() {
    // #166: the dialogs should open on what the project agreed on, rather
    // than on Ridal's answer every time.
    let (dir, app) = project_app(true);

    let (status, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["default_spacing"].is_null(), "unset to begin with");
    assert!(
        body["spacings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["value"] == "10"),
        "the page needs the list to populate its select: {body}"
    );

    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_spacing": "10", "default_format": "csv"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["default_spacing"], "10");

    // Both dialogs open on it: the viewer's own, and the catalog's merged
    // download. Pinned by the rendered `selected`, since that is the thing
    // a person actually gets.
    for uri in ["/", "/view/line-01"] {
        let (status, html) = page(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(
            html.contains("value=\"10\" selected"),
            "{uri} did not open on the project's spacing"
        );
        assert!(
            html.contains("value=\"csv\" selected"),
            "{uri} did not open on the project's format"
        );
    }

    // And on disk, so it survives a restart.
    let marker = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    assert!(marker.contains("default_spacing = \"10\""), "{marker}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_download_default_the_dialogs_do_not_offer_is_refused() {
    // Storing one would open every download on a choice with no entry to
    // change it back -- the same reason an unknown profile is refused.
    let (_dir, app) = project_app(true);
    for (body, code) in [
        (
            serde_json::json!({"default_spacing": "17"}),
            "unknown_spacing",
        ),
        (
            serde_json::json!({"default_format": "shapefile"}),
            "unknown_format",
        ),
    ] {
        let (status, _, answer) = put(&app, "/api/v1/project/settings", &body, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert_eq!(answer["error"]["code"], code, "{answer}");
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_neutral_download_defaults_are_stored_as_absence() {
    // `auto` and WGS84 GeoJSON are what Ridal does anyway, so choosing them
    // means "no preference" -- and storing them would quietly opt the
    // project out of a later change to either.
    let (dir, app) = project_app(true);
    let (status, _, body) = put(
        &app,
        "/api/v1/project/settings",
        &serde_json::json!({"default_spacing": "auto", "default_format": "geojson"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["default_spacing"].is_null(), "{body}");
    assert!(body["default_format"].is_null(), "{body}");

    let marker = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    let live = marker
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter(|l| l.contains("default_spacing") || l.contains("default_format"))
        .count();
    assert_eq!(live, 0, "{marker}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_viewer_opens_with_the_picks_drawn_unless_told_otherwise() {
    // #143. The toolbar's toggle changes it from there; this is the state
    // the page arrives in.
    let (_dir, app) = project_app(true);
    let (status, html) = page(&app, "/view/line-01").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("showPicks: true"), "{html}");
    assert!(html.contains("id=\"pick-visibility\""), "{html}");
    // The button's own label and pressed state are rendered from the same
    // setting rather than hardcoded, so the markup is never briefly wrong
    // -- including for a screen reader, and with JavaScript unavailable.
    assert!(html.contains("aria-pressed=\"true\""), "{html}");
    assert!(html.contains(">Hide picks</button>"), "{html}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_page_follows_the_device_until_a_theme_is_chosen() {
    // #141. No attribute at all is what `prefers-color-scheme` needs to
    // stay in charge, so its absence is the feature rather than an omission.
    let (_dir, app) = project_app(true);
    for uri in ["/", "/view/line-01", "/settings", "/layers"] {
        let (status, html) = page(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(html.contains("<html lang=\"en\">"), "{uri}: {html}");
        assert!(!html.contains("data-theme"), "{uri} should carry no theme");
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_bare_catalog_has_nothing_to_configure() {
    let (_dir, app) = bare_app();
    let (status, _, body) = get(&app, "/api/v1/project/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["project"], false);

    let (status, html) = page(&app, "/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("ridal project init"), "{html}");
    assert!(!html.contains("id=\"settings-form\""), "no form to offer");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn layer_usage_counts_features_and_flags_undefined_labels() {
    let (_dir, app) = project_app(true);
    put(
        &app,
        "/api/v1/layers",
        &serde_json::json!([{"id": "bed", "name": "Bed"}]),
        None,
    )
    .await;

    let document = serde_json::json!({
        "key": RADARGRAM,
        "features": [
            {"type": "Feature",
             "geometry": {"type": "LineString", "coordinates": [[1.0, 1.0], [10.0, 2.0]]},
             "properties": {"id": "f-1", "label": "bed"}},
            {"type": "Feature",
             "geometry": {"type": "LineString", "coordinates": [[12.0, 1.0], [20.0, 2.0]]},
             "properties": {"id": "f-2", "label": "bed"}},
            {"type": "Feature",
             "geometry": {"type": "LineString", "coordinates": [[22.0, 1.0], [30.0, 2.0]]},
             "properties": {"id": "f-3", "label": "englacial"}}
        ]
    });
    put(&app, URI, &document, None).await;

    let (status, _, body) = get(&app, "/api/v1/layers/usage").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["counts"]["bed"], 2);
    // A label nobody defined is reported separately, not silently counted.
    assert_eq!(body["undefined"]["englacial"], 1);
    assert!(body["counts"].get("englacial").is_none());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_defined_layer_nobody_uses_reports_zero_rather_than_vanishing() {
    // So the page can say "0 features" before a delete, instead of nothing.
    let (_dir, app) = project_app(true);
    put(
        &app,
        "/api/v1/layers",
        &serde_json::json!([{"id": "unused", "name": "Unused"}]),
        None,
    )
    .await;

    let (_, _, body) = get(&app, "/api/v1/layers/usage").await;
    assert_eq!(body["counts"]["unused"], 0);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_layers_page_renders_for_a_project_and_for_a_bare_catalog() {
    let (_dir, app) = project_app(true);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/layers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("writable: true"), "{html}");
    assert!(html.contains("Add a layer"));

    // A catalog with no project still answers, explaining why rather than
    // 404ing on "show me the layers".
    let (_dir2, bare) = bare_app();
    let response = bare
        .oneshot(
            Request::builder()
                .uri("/layers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("ridal project init"), "{html}");
    assert!(
        !html.contains("Add a layer"),
        "read-only page must not offer a form"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn malformed_json_is_a_client_error_with_the_standard_envelope() {
    let (_dir, app) = project_app(true);
    let (status, _, body) = put(&app, URI, &serde_json::json!({"no_key": true}), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_interpretation");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn validation_warnings_are_reported_without_refusing_the_save() {
    // The format is permissive by design: a feature with no stable id is
    // worth mentioning but must still save.
    let (_dir, app) = project_app(true);
    let document = serde_json::json!({
        "key": RADARGRAM,
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [30.0, 3.0]]},
            "properties": {"label": "bed"}
        }]
    });
    let (status, _, body) = put(&app, URI, &document, None).await;
    assert_eq!(status, StatusCode::CREATED);
    let warnings = body["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w.as_str().unwrap().contains("id")),
        "{warnings:?}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn unknown_fields_survive_a_save_and_reload_over_http() {
    let (_dir, app) = project_app(true);
    let mut document = document(RADARGRAM);
    document["from_a_future_version"] = serde_json::json!({"nested": [1, 2]});

    put(&app, URI, &document, None).await;
    let (_, _, body) = get(&app, URI).await;
    assert_eq!(
        body["from_a_future_version"],
        serde_json::json!({"nested": [1, 2]})
    );
}

/// The `axes:` entry of `window.RIDAL_VIEWER`, as one string.
///
/// Not by line: the jinja comment above it ends `-#}`, which eats the
/// newline, so the entry does not start a line of its own.
pub(super) fn axes_line(html: &str) -> Option<String> {
    let start = html.find("axes: ")?;
    let rest = &html[start..];
    Some(rest[..rest.find('\n')?].trim_end().to_string())
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_viewer_hands_the_picker_the_axes_to_save() {
    // The picker writes the document, but only the server has read the
    // radargram. This is where the axes cross over, and without them every
    // document saved is stuck on the revision it was drawn on forever.
    let (_dir, app) = project_app_with_axes();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/view/{RADARGRAM}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .to_string();

    let axes = axes_line(&html).expect("the viewer must hand the picker an axes block");
    assert!(axes.contains("trace_time"), "{axes}");
    assert!(axes.contains("tiepoints"), "{axes}");
    assert!(axes.contains("\"twtt\""), "{axes}");
    assert!(axes.contains("regular"), "{axes}");
    // The fixture's crop landed on time zero, so sample 0 is travel time 0.
    assert!(axes.contains("\"t0\":0.0"), "{axes}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_radargram_that_cannot_describe_its_axes_offers_none() {
    // Processed before #144, so it has no anchor name and no time-zero
    // variables. Half an axis block would invite a consumer to believe it
    // had a mapping, so the viewer offers null and the picker leaves
    // `coordinates` out entirely -- which is where every interpretation
    // already was, not a regression.
    let (_dir, app) = project_app(true);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/view/{RADARGRAM}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .to_string();

    let axes = axes_line(&html).expect("the key is always present, even when empty");
    assert_eq!(axes, "axes: null,", "{axes}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_anchor_name_cannot_break_out_of_the_script_block() {
    // The anchor name is read straight from a NetCDF attribute, and #147
    // lets an operator upload the file it comes from -- so this is a
    // stored-XSS path. `serde_json` escapes what JSON requires and `<` is
    // not on that list, so without escaping, a name containing
    // `</script><script>` closes the block and what follows executes when
    // anyone opens the viewer.
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    let path = radargrams(dir.path()).join("nasty.nc");
    write_test_nc_with_axes(&path, "nasty", None);
    {
        let mut file = netcdf::append(&path).unwrap();
        let mut twtt = file.variable_mut("twtt").unwrap();
        twtt.put_attribute("anchor_name", "twtt</script><script>alert(1)</script>")
            .unwrap();
    }
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
    let app = build_router(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/view/nasty")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .to_string();

    let axes = axes_line(&html).expect("the radargram declares an anchor, so there are axes");
    assert!(
        !axes.contains("</script>"),
        "the block can be closed from inside: {axes}"
    );
    assert!(
        axes.contains("\\u003c/script"),
        "expected the escaped form: {axes}"
    );
    // The *page* must have exactly the script tags it was written with --
    // no extra opening tag smuggled in through the data.
    assert_eq!(
        html.matches("<script").count(),
        html.matches("</script>").count(),
        "unbalanced script tags"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_document_carrying_the_axes_the_viewer_offered_can_be_saved() {
    // The test that was missing, and whose absence let #157 ship a shape
    // gprinterp rejects outright -- so every save of a radargram that
    // *could* describe its axes returned 400, on exactly the radargrams
    // the feature existed for.
    //
    // Everything else checked that the server offered the right numbers.
    // Nothing checked that a document containing them could be stored.
    let (_dir, app) = project_app_with_axes();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/view/{RADARGRAM}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .to_string();
    let line = axes_line(&html).expect("axes");
    let json = line
        .trim_start_matches("axes: ")
        .trim_end_matches(',')
        .to_string();
    let axes: Value = serde_json::from_str(&json).expect("the page carries valid JSON");

    // Exactly what picker.js assembles around it.
    let mut doc = document(RADARGRAM);
    doc["coordinates"] = serde_json::json!({ "axes": axes });

    let (status, _, body) = put(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/interpretations/default"),
        &doc,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // And it comes back with the anchors intact, rather than having been
    // accepted and quietly emptied.
    let (status, _, stored) = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/interpretations/default"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stored}");
    let axes = &stored["coordinates"]["axes"];
    assert_eq!(axes["x"]["anchor"][0]["name"], "trace_time", "{stored}");
    assert_eq!(axes["y"]["anchor"][0]["name"], "twtt", "{stored}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn axes_are_withheld_when_the_file_changed_under_the_catalog() {
    // The revision id beside the axes comes from the catalog snapshot; the
    // axes come from the file as it is now. Reprocess it in place between
    // the two and the page would hand the picker one revision's mapping
    // labelled with another's id -- the exact cross-revision mistake the
    // axes exist to prevent, produced by the feature itself.
    let (dir, app) = project_app_with_axes();

    // The catalog is built. Now put a different file at the same path, the
    // way reprocessing in place does -- written elsewhere and renamed over,
    // rather than reopened, because the render service still holds the
    // original and HDF5 will not open it for writing twice.
    {
        let staging = tempfile::tempdir().unwrap();
        let replacement = staging.path().join("newer.nc");
        write_test_nc_with_axes(&replacement, RADARGRAM, None);
        {
            let mut file = netcdf::append(&replacement).unwrap();
            file.add_attribute("ridal_processing_datetime", "2099-01-01T00:00:00Z")
                .unwrap();
        }
        std::fs::rename(&replacement, radargrams(dir.path()).join("line-01.nc")).unwrap();
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/view/{RADARGRAM}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "the page still renders");
    let html = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .to_string();

    let axes = axes_line(&html).expect("the key is always present");
    assert_eq!(
        axes, "axes: null,",
        "no mapping is better than one belonging to a different revision: {axes}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_save_waits_while_a_radargram_is_being_removed() {
    // Removing a radargram archives its interpretations and then deletes the
    // file, and those two steps are not one instant. A save arriving between
    // them passes the catalog check -- the entry is still there -- and then
    // writes a document into the directory the archive has just emptied.
    // What is left is picks for a radargram that no longer exists, outside
    // the archive, in exactly the place a later radargram taking that id
    // would find them.
    //
    // Rather than race the two and hope, this holds the lifecycle lock the
    // way a removal in progress does, and checks that the save does not
    // proceed until it is released.
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM);
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
    let app = build_router(Arc::clone(&state));

    let removal = state.lifecycle_lock().await;

    let saving = tokio::spawn(async move {
        let request = Request::builder()
            .method("PUT")
            .uri(URI)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(document(RADARGRAM).to_string()))
            .unwrap();
        app.oneshot(request).await.unwrap().status()
    });

    // Long enough that a save which ignored the lock would have finished:
    // once the lock is free the same request completes immediately, as the
    // second timeout below shows with the same budget.
    let mut saving = saving;
    let budget = std::time::Duration::from_millis(250);
    assert!(
        tokio::time::timeout(budget, &mut saving).await.is_err(),
        "the save went through while a removal held the lifecycle lock"
    );
    assert!(
        !data(dir.path()).join("interpretations/line-01").exists(),
        "and it wrote nothing in the meantime"
    );

    drop(removal);

    let status = tokio::time::timeout(budget, saving)
        .await
        .expect("the save should proceed once the removal is done")
        .unwrap();
    assert_eq!(status, StatusCode::CREATED);
}

/// The anchor axes the viewer page hands the picker, parsed back out of it.
///
/// Through the page rather than rebuilt here, so these tests exercise the
/// same values a browser would actually save.
async fn offered_axes(app: &Router) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/view/{RADARGRAM}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html = String::from_utf8_lossy(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .to_string();
    let line = axes_line(&html).expect("the page offers axes");
    let json = line.trim_start_matches("axes: ").trim_end_matches(',');
    serde_json::from_str(json).expect("the page carries valid JSON")
}

/// The revision the catalog is currently serving.
async fn revision_of(app: &Router) -> String {
    let (_, _, body) = send(
        app,
        Request::builder()
            .uri(format!("/api/v1/datasets/{RADARGRAM}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    body["revision_id"]
        .as_str()
        .expect("a revision id")
        .to_string()
}

/// A document with one line, authored against `revision`.
fn document_with_axes(axes: &Value, points: &[[f64; 2]], revision: impl Into<String>) -> Value {
    serde_json::json!({
        "key": RADARGRAM,
        "source": {"radargram_id": RADARGRAM, "revision_id": revision.into()},
        "coordinates": {"space": "index", "axes": axes},
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": points},
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    })
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_document_from_this_revision_is_shown_as_drawn() {
    // The ordinary case, and the one that has to stay quiet: a banner that
    // announced a migration every time anyone opened a radargram would be
    // ignored by the time it mattered.
    let (_dir, app) = project_app_with_axes();
    let uri = "/api/v1/datasets/line-01/interpretations/default";

    let axes = offered_axes(&app).await;
    let document = document_with_axes(&axes, &[[5.0, 2.0], [30.0, 3.0]], revision_of(&app).await);
    let (status, _, _) = put(&app, uri, &document, None).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _, body) = send(
        &app,
        Request::builder()
            .uri(format!("{uri}/carried"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["report"]["severity"], "current");
    assert!(body["report"]["moved"].is_null(), "nothing was carried");
    // And what comes back is what was stored, vertex for vertex.
    assert_eq!(
        body["document"]["features"][0]["geometry"]["coordinates"],
        serde_json::json!([[5.0, 2.0], [30.0, 3.0]])
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_document_from_another_revision_is_carried_and_the_stored_one_is_untouched() {
    // The whole safety property of #148's read half: the view moves, the
    // document does not. What is on disk is still what somebody drew, and
    // the carried coordinates exist only in the response.
    let (dir, app) = project_app_with_axes();
    let uri = "/api/v1/datasets/line-01/interpretations/default";

    let axes = offered_axes(&app).await;
    // Authored against a revision this radargram has never had.
    let document = document_with_axes(&axes, &[[5.0, 2.0], [30.0, 3.0]], "rev-from-elsewhere");
    let (status, _, body) = put(&app, uri, &document, None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _, body) = send(
        &app,
        Request::builder()
            .uri(format!("{uri}/carried"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(
        body["report"]["severity"], "current",
        "a different revision is not the current one: {body}"
    );
    assert_eq!(body["report"]["from_revision"], "rev-from-elsewhere");
    assert!(
        body["report"]["headline"].as_str().unwrap().len() > 20,
        "the banner has something to say: {body}"
    );

    // The file on disk still says what it said.
    let stored: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            data(dir.path()).join("interpretations/line-01/default.gprinterp.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(stored["source"]["revision_id"], "rev-from-elsewhere");
    assert_eq!(
        stored["features"][0]["geometry"]["coordinates"],
        serde_json::json!([[5.0, 2.0], [30.0, 3.0]]),
        "the authored coordinates were not rewritten"
    );
}
