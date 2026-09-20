//! HTTP-level tests for the derived-item routes (#205).
//!
//! The property under test is the permission one, in three parts:
//!
//! 1. Anyone may see a result computed from **their own** picks.
//! 2. A **cross-user** result reaches a picker only once an admin has released
//!    it, and releasing needs the admin role rather than the operator role that
//!    authoring needs.
//! 3. An **operator** sees the cross-user result either way, so a consensus can
//!    be defined and watched while picking is still open.
//!
//! The mixed-download test exists because serving items of different audiences
//! from one evaluation is the obvious way to leak a consensus into an
//! unreleased item.

use std::path::Path as StdPath;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::app::{build_router, AccessOptions, AppState};
use crate::identity::{RadargramId, UserId};
use crate::project::interpretations;
use crate::project::store::Expectation;
use crate::project::users::{self, DownloadScope, Role, User, UserSet};
use crate::project::Project;
use crate::server::render_service::RenderServiceConfig;

const RADARGRAM: &str = "line-01";

fn password() -> &'static str {
    static PASSWORD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PASSWORD.get_or_init(|| {
        let mut bytes = [0u8; 12];
        getrandom::fill(&mut bytes).expect("system randomness");
        format!("passphrase-{:x?}", bytes)
    })
}

fn id(name: &str) -> UserId {
    UserId::new(name).unwrap()
}

fn radargram() -> RadargramId {
    RadargramId::new(RADARGRAM).unwrap()
}

fn activated(name: &str, role: Role, download: DownloadScope, hash: &str) -> User {
    let mut user = User::new(id(name), role, download);
    user.password_hash = Some(hash.to_string());
    user
}

/// A processed radargram with real depth, travel-time and coordinate axes.
fn write_geometry_nc(path: &StdPath, radargram_id: &str) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", 8).unwrap();
    file.add_dimension("x", 40).unwrap();

    let mut data = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    let values: Vec<f32> = (0..(8 * 40)).map(|i| (i % 97) as f32).collect();
    data.put_values(&values, ..).unwrap();

    let mut twtt = file.add_variable::<f32>("twtt", &["y"]).unwrap();
    twtt.put_values(&(0..8).map(|i| i as f32 * 0.4).collect::<Vec<_>>(), ..)
        .unwrap();
    twtt.put_attribute("anchor_name", "twtt_normal_incidence")
        .unwrap();

    let mut depth = file.add_variable::<f32>("depth", &["y"]).unwrap();
    depth
        .put_values(&(0..8).map(|i| i as f32 * 0.02).collect::<Vec<_>>(), ..)
        .unwrap();

    for (name, values) in [
        ("distance", (0..40).map(|i| i as f64).collect::<Vec<_>>()),
        ("easting", (0..40).map(|i| 400_000.0 + i as f64).collect()),
        ("northing", (0..40).map(|_| 8_700_000.0).collect()),
        (
            "longitude",
            (0..40).map(|i| 15.0 + i as f64 * 1e-5).collect(),
        ),
        ("latitude", (0..40).map(|_| 78.0).collect()),
    ] {
        let mut var = file.add_variable::<f64>(name, &["x"]).unwrap();
        var.put_values(&values, ..).unwrap();
    }

    file.add_attribute("ridal_radargram_id", radargram_id)
        .unwrap();
    file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
        .unwrap();
    file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
        .unwrap();
    file.add_attribute("crs", "EPSG:32633").unwrap();
}

/// A project with one radargram, the given accounts, and one pick per named
/// user at the given sample.
fn app_with_picks(users: Vec<User>, picks: &[(&str, f64)]) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    let data_dir = dir.path().join(crate::project::DEFAULT_DATA_DIR);
    let nc = data_dir
        .join(crate::project::DEFAULT_RADARGRAM_DIR)
        .join("line-01.nc");
    write_geometry_nc(&nc, RADARGRAM);

    let project = Project::discover(dir.path()).unwrap().unwrap();
    let set = UserSet {
        users,
        ..UserSet::default()
    };
    users::write(project.documents(), &set, &Expectation::Any).unwrap();

    for (name, sample) in picks {
        let document: gprinterp::Document = serde_json::from_value(json!({
            "key": RADARGRAM,
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[0.0, sample], [39.0, sample]]
                },
                "properties": {"id": "f-0", "label": "bed"}
            }]
        }))
        .unwrap();
        interpretations::write(
            project.documents(),
            &radargram(),
            &id(name),
            &document,
            &Expectation::Absent,
        )
        .unwrap();
    }

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

struct Response {
    status: StatusCode,
    cookie: Option<String>,
    body: Value,
    text: String,
}

async fn send(app: &Router, request: Request<Body>) -> Response {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    Response {
        status,
        cookie,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text,
    }
}

fn request(method: &str, uri: &str, session: Option<&str>) -> axum::http::request::Builder {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(session) = session {
        builder = builder.header(header::COOKIE, session);
    }
    builder
}

async fn get(app: &Router, uri: &str, session: Option<&str>) -> Response {
    send(
        app,
        request("GET", uri, session).body(Body::empty()).unwrap(),
    )
    .await
}

async fn put(app: &Router, uri: &str, body: &Value, session: Option<&str>) -> Response {
    send(
        app,
        request("PUT", uri, session)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn sign_in(app: &Router, name: &str) -> String {
    let response = send(
        app,
        request("POST", "/api/v1/auth/login", None)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"name": name, "password": password()}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    response
        .cookie
        .expect("a login must set a cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn derived_set(items: Value) -> Value {
    json!({
        "schema": "ridal-derived",
        "schema_version": "1",
        "items": items,
    })
}

fn item(id: &str, expression: &str, scope: Value) -> Value {
    json!({
        "id": id,
        "name": id,
        "expression": expression,
        "unit": "meters",
        "scope": scope,
    })
}

/// The trace-0 value of one derived item, as this session sees it.
async fn first_value(app: &Router, item: &str, session: &str) -> f64 {
    let response = get(
        app,
        &format!("/api/v1/datasets/{RADARGRAM}/derived/{item}"),
        Some(session),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    response.body["values"][0]
        .as_f64()
        .unwrap_or_else(|| panic!("no numeric trace-0 value for '{item}': {}", response.text))
}

/// An item whose cross-user result an admin has released to everyone.
fn released_item(id: &str, expression: &str) -> Value {
    json!({
        "id": id,
        "name": id,
        "expression": expression,
        "unit": "meters",
        "scope": "project",
        "audience": "released",
    })
}

/// The depth at a sample, for building expectations from the test geometry.
fn depth(sample: f64) -> f64 {
    sample * 0.02
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_operator_sees_the_consensus_where_a_picker_sees_their_own() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("alice", Role::Picker, DownloadScope::Derived, &hash),
            activated("bob", Role::Picker, DownloadScope::Derived, &hash),
            activated("op", Role::Operator, DownloadScope::Derived, &hash),
        ],
        &[("alice", 2.0), ("bob", 4.0)],
    );
    let op = sign_in(&app, "op").await;
    let alice = sign_in(&app, "alice").await;

    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("bed_median", "median(bed)", json!("project"))])),
        Some(&op),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);

    let as_op = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/derived/bed_median"),
        Some(&op),
    )
    .await;
    assert_eq!(as_op.status, StatusCode::OK, "{}", as_op.text);
    let op_value = as_op.body["values"][0].as_f64().unwrap();
    // The consensus of alice's 0.2 m and bob's 0.4 m.
    assert!((op_value - depth(3.0)).abs() < 1e-6, "{op_value}");

    let as_alice = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/derived/bed_median"),
        Some(&alice),
    )
    .await;
    assert_eq!(as_alice.status, StatusCode::OK, "{}", as_alice.text);
    let alice_value = as_alice.body["values"][0].as_f64().unwrap();
    // Only her own pick, not the other contributor's.
    assert!((alice_value - depth(2.0)).abs() < 1e-6, "{alice_value}");
    assert!(alice_value < op_value);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_picker_cannot_save_a_project_wide_item() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![activated(
            "alice",
            Role::Picker,
            DownloadScope::Derived,
            &hash,
        )],
        &[("alice", 2.0)],
    );
    let alice = sign_in(&app, "alice").await;

    let refused = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("shared", "median(bed)", json!("project"))])),
        Some(&alice),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_cycle_is_a_400_not_a_500() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );
    let op = sign_in(&app, "op").await;

    let response = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([
            item("a", "b", json!("project")),
            item("b", "a", json!("project")),
        ])),
        Some(&op),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert!(response.text.contains("a → b → a"), "{}", response.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn derived_results_need_the_derived_download_scope() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("alice", Role::Picker, DownloadScope::Derived, &hash),
            activated("none", Role::Picker, DownloadScope::None, &hash),
        ],
        &[("alice", 2.0)],
    );
    let alice = sign_in(&app, "alice").await;
    let none = sign_in(&app, "none").await;

    let results = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/derived"),
        Some(&alice),
    )
    .await;
    assert_eq!(results.status, StatusCode::OK, "{}", results.text);

    let refused = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/derived"),
        Some(&none),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text);

    // NOTE: the existing download ladder is `None < Picks < Derived < All`,
    // so a `Derived` scope *does* still permit raw picks; the plan's
    // "releasing results does not release picks" is not what #131 built. That
    // is a pre-existing design question, not something this branch changes.
    let raw = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/interpretations/alice/raw"),
        Some(&alice),
    )
    .await;
    assert_eq!(raw.status, StatusCode::OK, "{}", raw.text);
}

/// Rule 2 of the derived permission model: a cross-user result reaches
/// pickers only once an admin has released it. Before that the *same*
/// project-wide definition is a personal readout for each of them.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn releasing_an_item_is_what_lets_a_picker_see_the_consensus() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("alice", Role::Picker, DownloadScope::Derived, &hash),
            activated("bob", Role::Picker, DownloadScope::Derived, &hash),
            activated("boss", Role::Admin, DownloadScope::Derived, &hash),
        ],
        &[("alice", 2.0), ("bob", 4.0)],
    );
    let boss = sign_in(&app, "boss").await;
    let alice = sign_in(&app, "alice").await;

    // Unreleased: alice gets her own pick, not the consensus.
    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("bed_median", "median(bed)", json!("project"))])),
        Some(&boss),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);
    let before = first_value(&app, "bed_median", &alice).await;
    assert!(
        (before - depth(2.0)).abs() < 1e-6,
        "before release alice must see only her own pick, got {before}"
    );

    // Released: the very same expression now gives her the consensus.
    let released = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([released_item("bed_median", "median(bed)")])),
        Some(&boss),
    )
    .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.text);
    let after = first_value(&app, "bed_median", &alice).await;
    assert!(
        (after - depth(3.0)).abs() < 1e-6,
        "after release alice must see the consensus of 2 and 4, got {after}"
    );
}

/// Releasing publishes other people's work in aggregate, so it is an admin
/// decision -- not something the operator who maintains the vocabulary can do
/// as a side effect of editing an expression.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_operator_cannot_release_a_cross_user_result() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("op", Role::Operator, DownloadScope::Derived, &hash),
            activated("boss", Role::Admin, DownloadScope::Derived, &hash),
        ],
        &[("op", 2.0)],
    );
    let op = sign_in(&app, "op").await;
    let boss = sign_in(&app, "boss").await;

    let refused = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([released_item("bed_median", "median(bed)")])),
        Some(&op),
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::FORBIDDEN,
        "an operator must not release: {}",
        refused.text
    );

    // The same operator may still author the unreleased form.
    let allowed = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("bed_median", "median(bed)", json!("project"))])),
        Some(&op),
    )
    .await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.text);

    // And an admin may release it.
    let released = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([released_item("bed_median", "median(bed)")])),
        Some(&boss),
    )
    .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.text);
}

/// A download may mix audiences, and each item must come from an evaluation
/// over exactly its own pick set. Serving both from one wider evaluation is
/// the obvious way to leak a consensus into an unreleased item.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_mixed_download_keeps_each_item_to_its_own_pick_set() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("alice", Role::Picker, DownloadScope::Derived, &hash),
            activated("bob", Role::Picker, DownloadScope::Derived, &hash),
            activated("boss", Role::Admin, DownloadScope::Derived, &hash),
        ],
        &[("alice", 2.0), ("bob", 4.0)],
    );
    let boss = sign_in(&app, "boss").await;
    let alice = sign_in(&app, "alice").await;

    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([
            item("mine", "median(bed)", json!("project")),
            released_item("ours", "median(bed)"),
        ])),
        Some(&boss),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);

    let csv = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/derived"),
        Some(&alice),
    )
    .await;
    assert_eq!(csv.status, StatusCode::OK, "{}", csv.text);

    let value_for = |item: &str| -> f64 {
        csv.text
            .lines()
            .find(|line| {
                let mut fields = line.split(',');
                fields.nth(1) == Some(item) && fields.nth(2) == Some("0")
            })
            .and_then(|line| line.rsplit(',').next().map(str::to_string))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or_else(|| panic!("no trace-0 row for '{item}' in:\n{}", csv.text))
    };

    assert!(
        (value_for("mine") - depth(2.0)).abs() < 1e-6,
        "the unreleased item stays alice's own pick"
    );
    assert!(
        (value_for("ours") - depth(3.0)).abs() < 1e-6,
        "the released item is the consensus"
    );
}

/// What #205 actually asked for: release a consensus without releasing the
/// individual picks behind it.
///
/// This is the one place the download ladder inverts -- an aggregate over many
/// contributors discloses less than any one contributor's raw picks -- so it
/// needs its own rung below `Picks`. Before `DownloadScope::Results` existed
/// the property was simply not expressible: the lowest scope that permitted a
/// result also permitted every pick that went into it.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_results_scope_releases_a_consensus_without_the_picks_behind_it() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("alice", Role::Picker, DownloadScope::Results, &hash),
            activated("bob", Role::Picker, DownloadScope::Picks, &hash),
            activated("boss", Role::Admin, DownloadScope::All, &hash),
        ],
        &[("alice", 2.0), ("bob", 4.0)],
    );
    let boss = sign_in(&app, "boss").await;
    let alice = sign_in(&app, "alice").await;

    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([released_item("bed_median", "median(bed)")])),
        Some(&boss),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);

    // Alice may have the consensus...
    let consensus = first_value(&app, "bed_median", &alice).await;
    assert!(
        (consensus - depth(3.0)).abs() < 1e-6,
        "the released consensus must reach a Results-scope user, got {consensus}"
    );

    // ...but not the raw picks behind it, which is the whole point of the rung.
    // `/raw` rather than the plain interpretation route because that one takes
    // no `Caller` at all and is ungated for everyone -- pre-existing on main,
    // reported separately, and not this branch's to change.
    let picks = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/interpretations/bob/raw"),
        Some(&alice),
    )
    .await;
    assert_eq!(
        picks.status,
        StatusCode::FORBIDDEN,
        "a Results-scope user must not download another contributor's raw picks: {}",
        picks.text
    );

    // And the rung is a real restriction, not a relabelling: bob, one rung up
    // at `Picks`, may have them.
    let bob = sign_in(&app, "bob").await;
    let allowed = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/interpretations/alice/raw"),
        Some(&bob),
    )
    .await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.text);
}
