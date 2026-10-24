//! HTTP-level tests for `POST /api/v1/datasets/{id}/figure` (#142).
//!
//! The figure itself is tested in `render::figure`; these are about what the
//! route lets a caller draw, which has to match what the routes serving the
//! same data already allow.

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};

use super::access_tests::{activated, put, scoped};
use super::derived_routes_tests::{app_with_picks, derived_set, item, RADARGRAM};
use super::test_support::{json_request, password_hash, send, sign_in, Response};
use crate::project::roles::{DownloadScope, Role};

async fn figure(app: &Router, spec: &Value, format: &str, session: Option<&str>) -> Response {
    let uri = format!("/api/v1/datasets/{RADARGRAM}/figure?format={format}");
    send(app, json_request("POST", &scoped(&uri), spec, session)).await
}

/// The number of picked lines in an SVG figure: the test layer has no colour
/// of its own, so each is drawn in the default.
fn pick_lines(svg: &str) -> usize {
    svg.matches(r##"stroke="#ffcc00""##).count()
}

fn small(extra: Value) -> Value {
    let mut spec = json!({"size": {"width": 300, "unit": "px", "dpi": 72}});
    spec.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    spec
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_operator_draws_everyones_picks_and_the_consensus() {
    let hash = password_hash();
    let (_dir, app) = app_with_picks(
        vec![activated(
            "op",
            Role::Operator,
            DownloadScope::Derived,
            &hash,
        )],
        &[("alice", 2.0), ("bob", 4.0)],
    );
    let op = sign_in(&app, "op").await;
    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("bed_median", "median(bed)", json!("project"))])),
        Some(&op),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);

    let spec = small(json!({
        "picks": {"items": [{}]},
        "derived": {"items": ["bed_median"]},
    }));
    let svg = figure(&app, &spec, "svg", Some(&op)).await;
    assert_eq!(svg.status, StatusCode::OK, "{}", svg.text);
    assert_eq!(svg.content_type.as_deref(), Some("image/svg+xml"));
    assert!(svg
        .disposition
        .as_deref()
        .unwrap()
        .contains("line-01-figure.svg"));
    assert_eq!(pick_lines(&svg.text), 2 + 1, "two picks and the median");

    let png = figure(&app, &spec, "png", Some(&op)).await;
    assert_eq!(png.status, StatusCode::OK);
    assert_eq!(png.content_type.as_deref(), Some("image/png"));

    // The test picks name no revision, so they are drawn as stored, the
    // median is evaluated over them as stored (#434), and the response says
    // both.
    let warnings: Vec<String> = serde_json::from_str(
        &svg.header(super::figure_routes::WARNINGS_HEADER)
            .expect("a warnings header"),
    )
    .unwrap();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert_eq!(
        warnings
            .iter()
            .filter(|w| w.contains("drawn as stored"))
            .count(),
        2,
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("derived layers use picks by alice, bob as stored")),
        "{warnings:?}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_picker_draws_only_their_own_picks() {
    let hash = password_hash();
    let (_dir, app) = app_with_picks(
        vec![activated("alice", Role::Picker, DownloadScope::All, &hash)],
        &[("alice", 2.0), ("bob", 4.0)],
    );
    let alice = sign_in(&app, "alice").await;

    let everyone = figure(
        &app,
        &small(json!({"picks": {"items": [{}]}})),
        "svg",
        Some(&alice),
    )
    .await;
    assert_eq!(everyone.status, StatusCode::OK, "{}", everyone.text);
    assert_eq!(pick_lines(&everyone.text), 1, "her own, not bob's");

    // Asking for someone else's by name is refused, not drawn without them.
    let bobs = figure(
        &app,
        &small(json!({"picks": {"items": [{"user": "bob"}]}})),
        "svg",
        Some(&alice),
    )
    .await;
    assert_eq!(bobs.status, StatusCode::FORBIDDEN, "{}", bobs.text);
    assert!(bobs.text.contains("not_your_picks"));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_figure_needs_the_derived_download_scope() {
    let hash = password_hash();
    let (_dir, app) = app_with_picks(
        vec![activated(
            "alice",
            Role::Picker,
            DownloadScope::Picks,
            &hash,
        )],
        &[],
    );
    let alice = sign_in(&app, "alice").await;
    let refused = figure(&app, &small(json!({})), "png", Some(&alice)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn bad_specs_formats_and_profiles_are_refused() {
    let hash = password_hash();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[],
    );
    let op = sign_in(&app, "op").await;

    let misspelt = figure(&app, &json!({"x_axis": {"spaceing": 1}}), "png", Some(&op)).await;
    assert_eq!(misspelt.status, StatusCode::BAD_REQUEST);
    assert!(
        misspelt.text.contains("invalid_figure_spec"),
        "{}",
        misspelt.text
    );

    let format = figure(&app, &small(json!({})), "gif", Some(&op)).await;
    assert_eq!(format.status, StatusCode::BAD_REQUEST);
    assert!(format.text.contains("invalid_format"));

    // A profile is a name here, never a file the server would read.
    let path = figure(
        &app,
        &small(json!({"profile": "/etc/passwd"})),
        "png",
        Some(&op),
    )
    .await;
    assert_eq!(path.status, StatusCode::BAD_REQUEST);
    assert!(path.text.contains("unknown_render_profile"));

    let unknown = figure(
        &app,
        &small(json!({"derived": {"items": ["nope"]}})),
        "png",
        Some(&op),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert!(unknown.text.contains("'nope'"), "{}", unknown.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_spec_for_another_revision_is_a_conflict() {
    let hash = password_hash();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[],
    );
    let op = sign_in(&app, "op").await;
    let stale = figure(
        &app,
        &small(json!({"revision_id": "someone-elses"})),
        "png",
        Some(&op),
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.text);
    assert!(stale.text.contains("revision_mismatch"));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_sidecar_of_a_figure_without_axes_is_json() {
    let hash = password_hash();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[],
    );
    let op = sign_in(&app, "op").await;
    let raw = small(json!({"axes": false, "extent": {"traces": [10, 30]}}));
    let sidecar = figure(&app, &raw, "json", Some(&op)).await;
    assert_eq!(sidecar.status, StatusCode::OK, "{}", sidecar.text);
    assert_eq!(sidecar.content_type.as_deref(), Some("application/json"));
    let body: Value = serde_json::from_str(&sidecar.text).unwrap();
    assert_eq!(body["traces"], json!([10.0, 30.0]));
    assert_eq!(body["x"]["unit"], "distance");
    assert_eq!(body["width"], 300);

    let with_axes = figure(&app, &small(json!({})), "json", Some(&op)).await;
    assert_eq!(with_axes.status, StatusCode::BAD_REQUEST);
}
