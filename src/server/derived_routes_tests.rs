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
    disposition: Option<String>,
}

async fn send(app: &Router, request: Request<Body>) -> Response {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let disposition = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
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
        disposition,
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

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_contributor_route_respects_visibility() {
    // The layer panel needs other contributors' lines for its overlay, but
    // the obvious route (#212) takes no Caller and cannot decide what a caller
    // may see. This route must return the caller's own document always, and
    // everyone's only to someone who may see cross-user results.
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

    let as_op = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/contributors"),
        Some(&op),
    )
    .await;
    assert_eq!(as_op.status, StatusCode::OK, "{}", as_op.text);
    assert_eq!(as_op.body["can_see_others"], json!(true));
    assert_eq!(as_op.body["documents"].as_array().unwrap().len(), 2);

    let as_alice = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/contributors"),
        Some(&alice),
    )
    .await;
    assert_eq!(as_alice.status, StatusCode::OK, "{}", as_alice.text);
    assert_eq!(as_alice.body["can_see_others"], json!(false));
    let documents = as_alice.body["documents"].as_array().unwrap();
    assert_eq!(documents.len(), 1, "{}", as_alice.text);
    assert_eq!(documents[0]["user"], json!("alice"));
    assert_eq!(documents[0]["own"], json!(true));
}

async fn post(app: &Router, uri: &str, body: &Value, session: Option<&str>) -> Response {
    send(
        app,
        request("POST", uri, session)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

/// The preview route must not let its caller choose whose picks it evaluates
/// over.
///
/// `Audience::Released` grants the cross-user pick set unconditionally,
/// because on a *stored* item it can only have been put there by an admin.
/// Accepting it from a request body turns that admin decision into a
/// self-service one: a picker asks for `released` and gets everyone's data.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_picker_cannot_ask_the_preview_route_for_a_cross_user_evaluation() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("alice", Role::Picker, DownloadScope::Results, &hash),
            activated("bob", Role::Picker, DownloadScope::Results, &hash),
            activated("op", Role::Operator, DownloadScope::All, &hash),
        ],
        &[("alice", 2.0), ("bob", 4.0)],
    );
    let alice = sign_in(&app, "alice").await;
    let op = sign_in(&app, "op").await;

    let ask = |session: String, body: Value| {
        let app = app.clone();
        async move {
            let response = post(
                &app,
                &format!("/api/v1/datasets/{RADARGRAM}/derived/preview"),
                &body,
                Some(&session),
            )
            .await;
            assert_eq!(response.status, StatusCode::OK, "{}", response.text);
            response.body["values"][0].as_f64().unwrap()
        }
    };

    // Her own pick, which is the honest answer.
    let own = ask(
        alice.clone(),
        json!({"expression": "median(bed)", "unit": "meters"}),
    )
    .await;
    assert!(
        (own - depth(2.0)).abs() < 1e-6,
        "expected her own pick, got {own}"
    );

    // The same request, asking for the released pick set. It must not change
    // the answer: nothing in a request body is an admin decision.
    let claimed = ask(
        alice,
        json!({"expression": "median(bed)", "unit": "meters", "audience": "released"}),
    )
    .await;
    assert!(
        (claimed - depth(2.0)).abs() < 1e-6,
        "a picker asking for audience=released must still get only her own picks, got {claimed} \
         (the consensus of 2 and 4 would be {})",
        depth(3.0)
    );

    // An operator gets the cross-user result without asking for anything,
    // which is rule 3 and is why the body field buys nothing.
    let operator = ask(op, json!({"expression": "median(bed)", "unit": "meters"})).await;
    assert!(
        (operator - depth(3.0)).abs() < 1e-6,
        "an operator previews over everyone, got {operator}"
    );
}

/// Another contributor's picks are raw picks whatever route they leave by.
///
/// `get_interpretation_raw` serves the same bytes behind `DownloadScope::Picks`;
/// two routes disclosing identical data under different gates is how #212
/// happened. Role stays the first gate, so this only narrows what an operator
/// may have — and the caller's own picks are never gated, because they already
/// have them.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_contributor_overlay_needs_the_picks_download_scope() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            // An operator who may see everyone by role, but whose project
            // says nothing leaves the server.
            activated("restricted", Role::Operator, DownloadScope::Results, &hash),
            activated("op", Role::Operator, DownloadScope::Picks, &hash),
            activated("bob", Role::Picker, DownloadScope::Picks, &hash),
        ],
        &[("restricted", 2.0), ("bob", 4.0)],
    );

    let restricted = sign_in(&app, "restricted").await;
    let response = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/contributors"),
        Some(&restricted),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    assert_eq!(
        response.body["can_see_others"],
        json!(false),
        "a scope below Picks must not unlock the contributor overlay"
    );
    let documents = response.body["documents"].as_array().unwrap();
    assert_eq!(documents.len(), 1, "only their own: {}", response.text);
    assert_eq!(documents[0]["user"], json!("restricted"));
    assert_eq!(
        documents[0]["own"],
        json!(true),
        "their own picks are never gated"
    );

    // One rung up, the same role sees everyone.
    let op = sign_in(&app, "op").await;
    let allowed = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/contributors"),
        Some(&op),
    )
    .await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.text);
    assert_eq!(allowed.body["can_see_others"], json!(true));
    assert_eq!(allowed.body["documents"].as_array().unwrap().len(), 2);
}

/// Saving must not delete items the caller could not see.
///
/// `GET /api/v1/derived` returns only what the caller may see, and the editor
/// saves back what it loaded, so a whole-document write silently drops every
/// other user's private item. The panel is the only way to author an
/// expression, which makes this the normal path rather than an edge case.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn saving_preserves_items_the_caller_cannot_see() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("alice", Role::Picker, DownloadScope::All, &hash),
            activated("op", Role::Operator, DownloadScope::All, &hash),
        ],
        &[("alice", 2.0), ("op", 4.0)],
    );
    let alice = sign_in(&app, "alice").await;
    let op = sign_in(&app, "op").await;

    // Alice keeps a private expression.
    let hers = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item(
            "alice_only",
            "median(bed)",
            json!({"private": {"user": "alice"}})
        )])),
        Some(&alice),
    )
    .await;
    assert_eq!(hers.status, StatusCode::OK, "{}", hers.text);

    // The operator cannot see it, and saves an unrelated project-wide item —
    // exactly what the panel does: load what you can see, save it back.
    let visible = get(&app, "/api/v1/derived", Some(&op)).await;
    assert_eq!(
        visible.body["items"].as_array().unwrap().len(),
        0,
        "the operator must not see her private item: {}",
        visible.text
    );
    let theirs = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("shared", "median(bed)", json!("project"))])),
        Some(&op),
    )
    .await;
    assert_eq!(theirs.status, StatusCode::OK, "{}", theirs.text);

    // Alice's item must still be there.
    let after = get(&app, "/api/v1/derived", Some(&alice)).await;
    let ids: Vec<String> = after.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids.contains(&"alice_only".to_string()),
        "the operator's save deleted alice's private item; ids are {ids:?}"
    );
}

/// An id already used by an invisible item is refused, not merged over.
///
/// The caller cannot see what they would be overwriting, so there is no way
/// for them to have meant it. The message does not name the owner: that would
/// turn a save into a way to enumerate who keeps what.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_id_taken_by_an_invisible_item_is_refused() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("alice", Role::Picker, DownloadScope::All, &hash),
            activated("op", Role::Operator, DownloadScope::All, &hash),
        ],
        &[("alice", 2.0)],
    );
    let alice = sign_in(&app, "alice").await;
    let op = sign_in(&app, "op").await;

    let hers = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item(
            "shared_name",
            "median(bed)",
            json!({"private": {"user": "alice"}})
        )])),
        Some(&alice),
    )
    .await;
    assert_eq!(hers.status, StatusCode::OK, "{}", hers.text);

    let clash = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("shared_name", "mean(bed)", json!("project"))])),
        Some(&op),
    )
    .await;
    assert_eq!(
        clash.status,
        StatusCode::CONFLICT,
        "a taken id must be refused: {}",
        clash.text
    );
    assert!(
        !clash.text.contains("alice"),
        "the refusal must not name the owner: {}",
        clash.text
    );

    // And hers is untouched.
    let after = get(&app, "/api/v1/derived", Some(&alice)).await;
    assert_eq!(after.body["items"][0]["expression"], json!("median(bed)"));
}

/// Deleting an item another item depends on is refused, naming the dependent.
///
/// A delete is a `PUT` that omits the item. The client cannot check this
/// itself -- the dependent may be a private item it cannot see -- so the
/// server checks the stored graph, where the item being deleted still exists.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn deleting_an_item_another_depends_on_is_refused() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );
    let op = sign_in(&app, "op").await;

    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([
            item("base", "median(bed)", json!("project")),
            item("dependent", "median(base) + 1.0", json!("project")),
        ])),
        Some(&op),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);

    // Omit `base` but keep `dependent`: a delete that would orphan it.
    let refused = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item(
            "dependent",
            "median(base) + 1.0",
            json!("project")
        )])),
        Some(&op),
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::CONFLICT,
        "deleting a depended-on item must be refused: {}",
        refused.text
    );
    assert!(
        refused.text.contains("dependent"),
        "the refusal must name the dependent: {}",
        refused.text
    );

    // Deleting both together is fine.
    let both = put(&app, "/api/v1/derived", &derived_set(json!([])), Some(&op)).await;
    assert_eq!(both.status, StatusCode::OK, "{}", both.text);
    let after = get(&app, "/api/v1/derived", Some(&op)).await;
    assert_eq!(after.body["items"].as_array().unwrap().len(), 0);
}

/// One unreadable filename must not hide every readable one (#213).
///
/// `UserId` rejects anything outside its charset, and both derived routes used
/// to turn that into a 500 for the whole request — so a single badly-named file
/// produced an empty contributor list and a consensus over nothing, which reads
/// as "the feature is broken" rather than "one file is named wrong".
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn one_unreadable_filename_does_not_hide_the_readable_ones() {
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );

    // A pick whose filename is not a valid user id, placed beside a good one.
    let bad = dir
        .path()
        .join(crate::project::DEFAULT_DATA_DIR)
        .join("interpretations")
        .join(RADARGRAM)
        .join("NotAUserId.gprinterp.json");
    std::fs::write(
        &bad,
        format!(r#"{{"schema":"gprinterp","key":"{RADARGRAM}","features":[]}}"#),
    )
    .unwrap();

    let op = sign_in(&app, "op").await;
    let response = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/contributors"),
        Some(&op),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "a bad filename must not fail the request: {}",
        response.text
    );
    let documents = response.body["documents"].as_array().unwrap();
    assert_eq!(
        documents.len(),
        1,
        "the good pick survives: {}",
        response.text
    );
    assert_eq!(documents[0]["user"], json!("op"));
    assert_eq!(
        response.body["unreadable"],
        json!(["NotAUserId"]),
        "and the skipped one is reported rather than swallowed: {}",
        response.text
    );

    // The consensus is still computed, over the readable picks.
    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("bed_median", "median(bed)", json!("project"))])),
        Some(&op),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);
    let value = first_value(&app, "bed_median", &op).await;
    assert!((value - depth(2.0)).abs() < 1e-6, "got {value}");
}

/// Derived points are wide: every visible item -- layer or attribute -- is a
/// property of one point per position, and an unlisted layer is left out
/// unless `include_unlisted` brings it back.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn derived_points_are_wide_and_skip_unlisted_layers_by_default() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );
    let op = sign_in(&app, "op").await;

    let mut unlisted = item("hidden_a", "median(bed)", json!("project"));
    unlisted["listed"] = json!(false);
    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([
            item("line_a", "median(bed)", json!("project")),
            item("count_a", "count(bed)", json!("project")),
            unlisted,
        ])),
        Some(&op),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);

    let url = format!("/api/v1/datasets/{RADARGRAM}/derived/level2?format=csv");
    let response = get(&app, &url, Some(&op)).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);

    let header = response.text.lines().next().unwrap();
    // A derived layer is a position, so it is written in all three vertical
    // units; an attribute is a number in its own. Both are columns of the
    // same point.
    for column in ["line_a_m", "line_a_ns", "line_a_samples", "count_a_m"] {
        assert!(header.contains(column), "missing {column}: {header}");
    }
    assert!(
        !header.contains("hidden_a"),
        "an unlisted layer must be excluded by default: {header}"
    );
    // One row per grid position, not one per item: this radargram has 40
    // traces at 1 m, and the derived rows are on the same arc grid.
    assert_eq!(response.text.lines().count(), 1 + 40, "{}", response.text);

    let response = get(&app, &format!("{url}&include_unlisted=true"), Some(&op)).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    assert!(
        response.text.contains("hidden_a"),
        "include_unlisted must bring it back: {}",
        response.text
    );
}

/// A property name that would displace a point's own field is refused when the
/// item is saved, while the author can still pick another id.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_property_that_shadows_a_base_field_is_refused_at_save() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );
    let op = sign_in(&app, "op").await;

    let mut colliding = item("easting", "count(bed)", json!("project"));
    colliding["unit"] = json!("dimensionless");
    let response = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([colliding])),
        Some(&op),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert!(
        response.text.contains("invalid_derived_items") && response.text.contains("easting"),
        "{}",
        response.text
    );
}

/// A collision that reached the store anyway (written by hand or by an older
/// build) is refused at export rather than written as a dropped or duplicate
/// property.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_stored_collision_is_refused_at_export() {
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );
    let op = sign_in(&app, "op").await;

    let stored = json!({
        "schema": "ridal-derived",
        "schema_version": "1",
        "items": [{
            "id": "easting",
            "name": "easting",
            "expression": "count(bed)",
            "unit": "dimensionless",
            "scope": "project",
        }]
    });
    let derived_dir = dir
        .path()
        .join(crate::project::DEFAULT_DATA_DIR)
        .join(crate::project::DERIVED_DIR);
    std::fs::create_dir_all(&derived_dir).unwrap();
    std::fs::write(
        derived_dir.join("derived.json"),
        serde_json::to_string(&stored).unwrap(),
    )
    .unwrap();

    let response = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/derived/level2?format=csv"),
        Some(&op),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert!(
        response.text.contains("derived_property_collision"),
        "{}",
        response.text
    );
}

/// A derived layer and a picked layer are sampled on the same radargram-wide
/// arc grid, so their distances line up node for node.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn derived_and_picked_points_share_the_arc_grid() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );
    let op = sign_in(&app, "op").await;
    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("line_a", "median(bed)", json!("project"))])),
        Some(&op),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);

    let distances = |text: &str, column: usize| -> Vec<String> {
        text.lines()
            .skip(1)
            .filter(|line| !line.is_empty())
            .map(|line| line.split(',').nth(column).unwrap().to_string())
            .collect()
    };

    let picked = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/interpretations/op/level2?format=csv&spacing=5"),
        Some(&op),
    )
    .await;
    assert_eq!(picked.status, StatusCode::OK, "{}", picked.text);

    let derived = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/derived/level2?format=csv&spacing=5"),
        Some(&op),
    )
    .await;
    assert_eq!(derived.status, StatusCode::OK, "{}", derived.text);

    // The derived CSV's `distance_m` is the fourth column; the picked CSV's
    // is the ninth. Both must be 0, 5, 10, ... on the shared grid.
    let picked_distances = distances(&picked.text, 8);
    let derived_distances = distances(&derived.text, 3);
    assert_eq!(
        derived_distances,
        ["0", "5", "10", "15", "20", "25", "30", "35"]
    );
    assert_eq!(
        picked_distances, derived_distances,
        "picked and derived points must share one arc grid"
    );
}

/// Downloading every user's picks is an admin decision; a picker is refused
/// even though they may download their own points.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn every_user_points_need_the_admin_role() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![
            activated("admin", Role::Admin, DownloadScope::All, &hash),
            activated("alice", Role::Picker, DownloadScope::All, &hash),
            activated("bob", Role::Picker, DownloadScope::All, &hash),
        ],
        &[("alice", 2.0), ("bob", 4.0)],
    );
    let alice = sign_in(&app, "alice").await;
    let admin = sign_in(&app, "admin").await;

    let refused = get(
        &app,
        &format!(
            "/api/v1/datasets/{RADARGRAM}/interpretations/alice/level2?format=csv&every_user=true"
        ),
        Some(&alice),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text);

    let allowed = get(
        &app,
        &format!(
            "/api/v1/datasets/{RADARGRAM}/interpretations/admin/level2?format=csv&every_user=true"
        ),
        Some(&admin),
    )
    .await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.text);
    assert!(
        allowed.text.contains("alice") && allowed.text.contains("bob"),
        "an admin's every-user file must name each contributor: {}",
        allowed.text
    );
}

/// The catalog's picked and derived downloads must not share a filename, or
/// the browser saves the second under the first's name and the user cannot
/// tell which is which.
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn catalog_picked_and_derived_downloads_are_named_apart() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );
    let op = sign_in(&app, "op").await;
    let created = put(
        &app,
        "/api/v1/derived",
        &derived_set(json!([item("line_a", "median(bed)", json!("project"))])),
        Some(&op),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);

    let picked = get(&app, "/api/v1/catalog/level2?format=geojson", Some(&op)).await;
    assert_eq!(picked.status, StatusCode::OK, "{}", picked.text);
    assert!(
        picked
            .disposition
            .as_deref()
            .unwrap_or("")
            .contains("picked-layer-points"),
        "{:?}",
        picked.disposition
    );

    let derived = get(
        &app,
        "/api/v1/catalog/level2?format=geojson&derived=true",
        Some(&op),
    )
    .await;
    assert_eq!(derived.status, StatusCode::OK, "{}", derived.text);
    assert!(
        derived
            .disposition
            .as_deref()
            .unwrap_or("")
            .contains("derived-layer-points"),
        "{:?}",
        derived.disposition
    );
    assert_ne!(picked.disposition, derived.disposition);
    // And the bodies really are different products: the picked one carries
    // the picked layer, the derived one the derived item.
    assert!(picked.text.contains("bed"), "{}", picked.text);
    assert!(derived.text.contains("line_a"), "{}", derived.text);
}

/// A merged *derived* download names no user, so it must not demand one.
/// (The picked path still does: "download my points" needs to know whose.)
#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_merged_derived_download_needs_no_user() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_picks(
        vec![activated("op", Role::Operator, DownloadScope::All, &hash)],
        &[("op", 2.0)],
    );

    // Anonymous (no session), which is the case that was refused.
    let picked = get(&app, "/api/v1/catalog/level2?format=geojson", None).await;
    assert_eq!(picked.status, StatusCode::BAD_REQUEST, "{}", picked.text);
    assert!(picked.text.contains("user_required"), "{}", picked.text);

    let derived = get(
        &app,
        "/api/v1/catalog/level2?format=geojson&derived=true",
        None,
    )
    .await;
    assert_eq!(derived.status, StatusCode::OK, "{}", derived.text);
    assert!(
        derived
            .disposition
            .as_deref()
            .unwrap_or("")
            .contains("derived-layer-points"),
        "{:?}",
        derived.disposition
    );
}
