//! HTTP-level tests for authentication, roles and download scopes (#131).
//!
//! Driven through the real Axum router with `ServiceExt::oneshot`, so the
//! status codes, cookies and error envelopes are the ones a browser will
//! actually see -- and so the identity middleware is in the path, which is
//! where every one of these decisions is actually made.
//!
//! Every test builds an `AppState`, which creates and opens a NetCDF.
//! netcdf-c is not thread-safe, so they carry the same
//! `#[serial_test::serial(netcdf)]` guard as the tests in `app.rs`.

use std::path::Path as StdPath;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::app::{build_router, AccessOptions, AppState};
use crate::identity::UserId;
use crate::project::store::Expectation;
use crate::project::users::{self, DownloadScope, Invite, Role, User, UserSet};
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

/// The passphrase every account in this file is activated with.
///
/// Generated once per run rather than written down. Nothing here asserts
/// on the value -- only on what signing in with it and without it does --
/// and the code scanner over this repository cannot tell a fixture from a
/// credential shipped by mistake, so a literal costs a real alert
/// somewhere else its attention.
fn password() -> &'static str {
    static PASSWORD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PASSWORD.get_or_init(|| {
        let mut bytes = [0u8; 12];
        getrandom::fill(&mut bytes).expect("system randomness");
        format!("passphrase-{:x?}", bytes)
    })
}

fn write_test_nc(path: &StdPath, radargram_id: &str) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", 8).unwrap();
    file.add_dimension("x", 40).unwrap();
    let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    let data: Vec<f32> = (0..(8 * 40)).map(|i| (i % 97) as f32).collect();
    var.put_values(&data, ..).unwrap();
    file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
        .unwrap();
    file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
        .unwrap();
    file.add_attribute("ridal_radargram_id", radargram_id)
        .unwrap();
}

fn id(name: &str) -> UserId {
    UserId::new(name).unwrap()
}

/// An activated account. The hash is passed in so a test with several
/// accounts pays for Argon2id once rather than once per person.
fn activated(name: &str, role: Role, download: DownloadScope, hash: &str) -> User {
    let mut user = User::new(id(name), role, download);
    user.password_hash = Some(hash.to_string());
    user
}

/// A project with one radargram and the given accounts.
///
/// `users` being non-empty is what makes this an authenticated project:
/// writing the file at all is the opt-in, which is why the tests that need
/// today's unauthenticated behaviour use `interp_routes_tests` instead.
fn app_with(users: Vec<User>) -> (tempfile::TempDir, Router) {
    app_with_set(UserSet {
        users,
        ..UserSet::default()
    })
}

fn app_with_set(set: UserSet) -> (tempfile::TempDir, Router) {
    app_with_set_and_access(set, AccessOptions::default())
}

fn app_with_set_and_access(set: UserSet, access: AccessOptions) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM);
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(project.documents(), &set, &Expectation::Any).unwrap();

    let state = Arc::new(
        AppState::build_with_project(
            dir.path(),
            &RenderServiceConfig::default(),
            Some(project),
            access,
        )
        .unwrap(),
    );
    (dir, build_router(state))
}

/// The parts of a response these tests assert on.
struct Response {
    status: StatusCode,
    cookie: Option<String>,
    body: Value,
    text: String,
    /// The version the document is now at, for the routes that offer one.
    etag: Option<String>,
}

async fn send(app: &Router, request: Request<Body>) -> Response {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let etag = response
        .headers()
        .get(header::ETAG)
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
        etag,
    }
}

/// The `name=value` part of a `Set-Cookie`, ready to send back as `Cookie`.
fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
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

/// A POST carrying `If-Match`, for the routes that take a precondition.
async fn post_with_if_match(
    app: &Router,
    uri: &str,
    body: &Value,
    etag: &str,
    session: Option<&str>,
) -> Response {
    send(
        app,
        request("POST", uri, session)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::IF_MATCH, etag)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
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

async fn delete(app: &Router, uri: &str, session: Option<&str>) -> Response {
    send(
        app,
        request("DELETE", uri, session).body(Body::empty()).unwrap(),
    )
    .await
}

/// Sign in and return the cookie to send back.
async fn sign_in(app: &Router, name: &str) -> String {
    let response = post(
        app,
        "/api/v1/auth/login",
        &json!({"name": name, "password": password()}),
        None,
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    cookie_pair(&response.cookie.expect("a login must set a cookie"))
}

fn document(key: &str) -> Value {
    json!({
        "key": key,
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [30.0, 3.0]]},
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    })
}

fn interpretation_uri(user: &str) -> String {
    format!("/api/v1/datasets/{RADARGRAM}/interpretations/{user}")
}

// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_invite_is_the_only_way_a_password_is_ever_set() {
    // The whole account lifecycle, end to end: an administrator exists
    // (created from the command line, simulated here by writing the file),
    // creates an account, hands over a link, and the link is what sets the
    // password. No password is ever known to two people.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);
    let admin = sign_in(&app, "erik").await;

    let created = post(
        &app,
        "/api/v1/users",
        &json!({"name": "student", "role": "picker", "download": "derived"}),
        Some(&admin),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(created.body["user"]["activated"], false);
    assert_eq!(created.body["user"]["invite_pending"], true);

    // The token exists exactly here and nowhere else: only its hash is
    // stored. A path rather than a URL, because Ridal sits behind a proxy
    // and must not build one from a header a client controls.
    let path = created.body["invite_path"].as_str().unwrap().to_string();
    assert!(path.starts_with("/invite/"), "{path}");
    let token = path.trim_start_matches("/invite/").to_string();

    // Until it is redeemed there is no password, and saying so is refused
    // the same way a wrong password is.
    let refused = post(
        &app,
        "/api/v1/auth/login",
        &json!({"name": "student", "password": ""}),
        None,
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);

    // The page is reachable by anyone holding the link.
    let page = get(&app, &path, None).await;
    assert_eq!(page.status, StatusCode::OK);

    let redeemed = post(
        &app,
        "/api/v1/auth/invite",
        &json!({"token": token, "password": format!("{}-new", password())}),
        None,
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.text);
    // Signed in straight away: they just proved they hold the link and
    // chose a password, and asking for it again would be ceremony.
    let session = cookie_pair(&redeemed.cookie.expect("redeeming signs you in"));
    let me = get(&app, "/api/v1/auth/me", Some(&session)).await;
    assert_eq!(me.body["user"], "student");
    assert_eq!(me.body["role"], "picker");

    // And the token is consumed.
    let again = post(
        &app,
        "/api/v1/auth/invite",
        &json!({"token": token, "password": format!("{}-again", password())}),
        None,
    )
    .await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert!(
        again.text.contains("already have been used"),
        "{}",
        again.text
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn bulk_invites_are_atomic_and_store_only_token_hashes() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);
    let admin = sign_in(&app, "erik").await;
    let created = post(
        &app,
        "/api/v1/users/bulk/invites",
        &json!({"prefix":"student","count":3,"role":"picker","download":"all"}),
        Some(&admin),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(created.body["users"].as_array().unwrap().len(), 3);

    let listed = get(&app, "/api/v1/users", Some(&admin)).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.body["users"].as_array().unwrap().len(), 4);
    assert!(listed.text.contains("student-01"));
    assert!(!listed.text.contains("token_hash"));

    let continued = post(
        &app,
        "/api/v1/users/bulk/invites",
        &json!({"prefix":"student","count":2,"role":"picker"}),
        Some(&admin),
    )
    .await;
    assert_eq!(continued.status, StatusCode::CREATED, "{}", continued.text);
    assert_eq!(continued.body["users"][0]["name"], "student-04");
    assert_eq!(continued.body["users"][1]["name"], "student-05");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn bulk_passwords_require_acknowledgement_and_refuse_admins() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);
    let admin = sign_in(&app, "erik").await;
    let missing = post(
        &app,
        "/api/v1/users/bulk/passwords",
        &json!({"prefix":"student","count":2,"role":"viewer","acknowledge_risk":false}),
        Some(&admin),
    )
    .await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);

    let forbidden = post(
        &app,
        "/api/v1/users/bulk/passwords",
        &json!({"prefix":"admin","count":1,"role":"admin","acknowledge_risk":true}),
        Some(&admin),
    )
    .await;
    assert_eq!(forbidden.status, StatusCode::BAD_REQUEST);

    let created = post(
        &app,
        "/api/v1/users/bulk/passwords",
        &json!({"prefix":"student","count":2,"role":"viewer","acknowledge_risk":true}),
        Some(&admin),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);
    let users = created.body["users"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    assert!(users[0]["password"].as_str().unwrap().len() >= users::MIN_PASSWORD_LEN);

    let listed = get(&app, "/api/v1/users", Some(&admin)).await;
    for user in users {
        assert!(!listed.text.contains(user["password"].as_str().unwrap()));
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_expired_invite_is_refused() {
    let hash = users::hash_password(password()).unwrap();
    let mut stale = activated("student", Role::Picker, DownloadScope::All, &hash);
    stale.password_hash = None;
    stale.invite = Some(Invite {
        token_hash: blake3::hash(b"stale-token").to_hex().to_string(),
        // Long past.
        expires: 1,
    });
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Admin, DownloadScope::All, &hash),
        stale,
    ]);

    let response = post(
        &app,
        "/api/v1/auth/invite",
        &json!({"token": "stale-token", "password": format!("{}-new", password())}),
        None,
    )
    .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert!(response.text.contains("expired"), "{}", response.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_wrong_password_and_an_unknown_name_are_refused_identically() {
    // Otherwise the login form is a way to enumerate who has an account and
    // who has not signed up yet.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);

    let wrong = post(
        &app,
        "/api/v1/auth/login",
        &json!({"name": "erik", "password": format!("{}x", password())}),
        None,
    )
    .await;
    let unknown = post(
        &app,
        "/api/v1/auth/login",
        &json!({"name": "nobody", "password": password()}),
        None,
    )
    .await;

    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body, unknown.body, "the two refusals must not differ");
    assert!(wrong.cookie.is_none(), "a refused login must set no cookie");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_unknown_name_costs_the_same_as_a_wrong_password() {
    // The identical response was only half of it. An unknown name used to
    // return before Argon2id ran at all while a real one paid for a
    // verification, and that gap is measurable from outside -- which turns
    // the login form back into the account enumerator the unified message
    // was meant to close.
    //
    // Argon2id dominates both paths by design, so "same order of
    // magnitude" is the assertion that means something; a tight bound
    // would flake on a shared CI runner.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Admin, DownloadScope::All, &hash),
        // Created but never activated: the third path, which must also not
        // stand out.
        User::new(id("invited"), Role::Picker, DownloadScope::All),
    ]);

    let time = |name: &'static str| {
        let app = app.clone();
        async move {
            let start = std::time::Instant::now();
            let response = post(
                &app,
                "/api/v1/auth/login",
                &json!({"name": name, "password": format!("{}x", password())}),
                None,
            )
            .await;
            assert_eq!(response.status, StatusCode::UNAUTHORIZED);
            start.elapsed()
        }
    };

    // Warm anything lazy -- the decoy hash is built once per process, and
    // paying for it inside a measurement would look like a difference.
    time("nobody").await;

    let real = time("erik").await;
    let missing = time("nobody").await;
    let unactivated = time("invited").await;

    for (label, measured) in [("unknown name", missing), ("unactivated", unactivated)] {
        let ratio = measured.as_secs_f64() / real.as_secs_f64();
        assert!(
            (0.2..5.0).contains(&ratio),
            "{label} took {measured:?} against {real:?} for a real account \
             (ratio {ratio:.2}); one path is skipping the hash"
        );
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_password_hash_never_leaves_the_process() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);
    let admin = sign_in(&app, "erik").await;

    for uri in [
        "/api/v1/users",
        "/api/v1/auth/me",
        "/api/v1/project/settings",
    ] {
        let response = get(&app, uri, Some(&admin)).await;
        assert!(
            !response.text.contains("argon2"),
            "{uri}: {}",
            response.text
        );
        assert!(
            !response.text.contains("password_hash"),
            "{uri}: {}",
            response.text
        );
        assert!(
            !response.text.contains("token_hash"),
            "{uri}: {}",
            response.text
        );
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn picks_belong_to_the_person_who_drew_them_and_not_even_an_admin_may_edit_them() {
    // Per-user by design, and a property of the data rather than a
    // permission: there is deliberately no role that can overrule it.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Admin, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let student = sign_in(&app, "student").await;
    let admin = sign_in(&app, "erik").await;

    let own = put(
        &app,
        &interpretation_uri("student"),
        &document(RADARGRAM),
        Some(&student),
    )
    .await;
    assert_eq!(own.status, StatusCode::CREATED, "{}", own.text);

    // The administrator can read them...
    let read = get(&app, &interpretation_uri("student"), Some(&admin)).await;
    assert_eq!(read.status, StatusCode::OK);

    // ...and cannot write them, despite outranking everyone.
    let forged = put(
        &app,
        &interpretation_uri("student"),
        &document(RADARGRAM),
        Some(&admin),
    )
    .await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    assert_eq!(forged.body["error"]["code"], "not_your_interpretation");

    // Nor delete them.
    let removed = delete(&app, &interpretation_uri("student"), Some(&admin)).await;
    assert_eq!(removed.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_path_parameter_is_no_longer_the_authorisation() {
    // Without a session the path used to be all the authorisation there
    // was: `PUT .../interpretations/erik` would write Erik's document for
    // anyone who asked.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Picker,
        DownloadScope::All,
        &hash,
    )]);

    let anonymous = put(
        &app,
        &interpretation_uri("erik"),
        &document(RADARGRAM),
        None,
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.body["error"]["code"], "authentication_required");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_forged_cookie_is_not_a_session() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);

    // Everything a real cookie has except a signature made with the key.
    let forged = format!("ridal_session=erik.1.9999999999.{}", "a".repeat(64));
    let me = get(&app, "/api/v1/auth/me", Some(&forged)).await;
    assert_eq!(me.body["authenticated"], false);
    assert_eq!(me.body["role"], "viewer");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_viewer_reads_a_picker_writes_and_an_operator_curates() {
    // The ladder, at the three places it actually bites.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("watcher", Role::Viewer, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
        activated("erik", Role::Operator, DownloadScope::All, &hash),
    ]);
    let viewer = sign_in(&app, "watcher").await;
    let picker = sign_in(&app, "student").await;
    let operator = sign_in(&app, "erik").await;

    // Reading the catalog: everyone.
    for session in [&viewer, &picker, &operator] {
        let response = get(&app, "/api/v1/datasets", Some(session)).await;
        assert_eq!(response.status, StatusCode::OK);
    }

    // Writing an interpretation: picker and above.
    let refused = put(
        &app,
        &interpretation_uri("watcher"),
        &document(RADARGRAM),
        Some(&viewer),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.body["error"]["code"], "insufficient_role");
    assert!(refused.text.contains("picker"), "{}", refused.text);

    let allowed = put(
        &app,
        &interpretation_uri("student"),
        &document(RADARGRAM),
        Some(&picker),
    )
    .await;
    assert_eq!(allowed.status, StatusCode::CREATED, "{}", allowed.text);

    // The layer vocabulary: readable by a viewer, changed by an operator.
    // A picker *uses* layers but does not get to invent them.
    let layers = json!([{"id": "bed", "name": "Bed", "color": "#e6194b"}]);
    let read = get(&app, "/api/v1/layers", Some(&viewer)).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body["writable"], false);

    let refused = put(&app, "/api/v1/layers", &layers, Some(&picker)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(refused.text.contains("operator"), "{}", refused.text);

    let allowed = put(&app, "/api/v1/layers", &layers, Some(&operator)).await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.text);
    let read = get(&app, "/api/v1/layers", Some(&operator)).await;
    assert_eq!(read.body["writable"], true);

    // The project defaults: operator, not picker.
    let refused = put(
        &app,
        "/api/v1/project/settings",
        &json!({"default_profile": "abslog"}),
        Some(&picker),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let allowed = put(
        &app,
        "/api/v1/project/settings",
        &json!({"default_profile": "abslog"}),
        Some(&operator),
    )
    .await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.text);

    // Accounts: admin only, and an operator is not one.
    let refused = get(&app, "/api/v1/users", Some(&operator)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(refused.text.contains("admin"), "{}", refused.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_demotion_takes_effect_on_the_next_request_not_when_the_cookie_expires() {
    // What the credential version in the cookie buys: user management that
    // actually manages, rather than taking effect at some point in the next
    // fortnight.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Admin, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let admin = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;

    let before = get(&app, "/api/v1/auth/me", Some(&student)).await;
    assert_eq!(before.body["role"], "picker");

    let demoted = put(
        &app,
        "/api/v1/users/student",
        &json!({"role": "viewer"}),
        Some(&admin),
    )
    .await;
    assert_eq!(demoted.status, StatusCode::OK, "{}", demoted.text);

    // The same cookie, one request later.
    let after = get(&app, "/api/v1/auth/me", Some(&student)).await;
    assert_eq!(
        after.body["authenticated"], false,
        "the old cookie names a credential version the account no longer has"
    );

    let refused = put(
        &app,
        &interpretation_uri("student"),
        &document(RADARGRAM),
        Some(&student),
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);

    // Signing in again gets a cookie at the new version, with the new role.
    let student = sign_in(&app, "student").await;
    let now = get(&app, "/api/v1/auth/me", Some(&student)).await;
    assert_eq!(now.body["role"], "viewer");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_departed_users_picks_survive_the_account() {
    // Attributed scientific data. Removing the person must not destroy it.
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with(vec![
        activated("erik", Role::Admin, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let admin = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;

    put(
        &app,
        &interpretation_uri("student"),
        &document(RADARGRAM),
        Some(&student),
    )
    .await;
    // A preference, which is about the person rather than the survey.
    put(
        &app,
        "/api/v1/preferences",
        &json!({"render_profile": "abslog"}),
        Some(&student),
    )
    .await;

    let removed = delete(&app, "/api/v1/users/student", Some(&admin)).await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.text);

    // The account is gone and its session with it.
    let after = get(&app, "/api/v1/auth/me", Some(&student)).await;
    assert_eq!(after.body["authenticated"], false);

    // The picks are not.
    let stored = get(&app, &interpretation_uri("student"), Some(&admin)).await;
    assert_eq!(stored.status, StatusCode::OK, "{}", stored.text);
    let listed = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/interpretations"),
        Some(&admin),
    )
    .await;
    assert_eq!(listed.body["users"], json!(["student"]));

    // Their preferences are, because a preference is about a person.
    assert!(
        !data(dir.path()).join("preferences/student.json").exists(),
        "a departed account's preferences should not linger"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_last_administrator_cannot_be_demoted_or_removed() {
    // Either would lock the access settings away from everyone, with no way
    // back short of editing users.json by hand.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Admin, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let admin = sign_in(&app, "erik").await;

    let demote = put(
        &app,
        "/api/v1/users/erik",
        &json!({"role": "operator"}),
        Some(&admin),
    )
    .await;
    assert_eq!(demote.status, StatusCode::BAD_REQUEST);
    assert!(
        demote.text.contains("only administrator"),
        "{}",
        demote.text
    );

    let remove = delete(&app, "/api/v1/users/erik", Some(&admin)).await;
    assert_eq!(remove.status, StatusCode::BAD_REQUEST);

    // With a second administrator, both become possible.
    put(
        &app,
        "/api/v1/users/student",
        &json!({"role": "admin"}),
        Some(&admin),
    )
    .await;
    let demote = put(
        &app,
        "/api/v1/users/erik",
        &json!({"role": "operator"}),
        Some(&admin),
    )
    .await;
    assert_eq!(demote.status, StatusCode::OK, "{}", demote.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn download_scope_gates_the_downloads_and_not_the_viewer() {
    // The distinction the issue insists on: this stops casual bulk export
    // and states an intent. It cannot stop someone who can open the page,
    // and gating what the viewer draws with would break the viewer for
    // everyone below "all".
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("nothing", Role::Picker, DownloadScope::None, &hash),
        activated("picks", Role::Picker, DownloadScope::Picks, &hash),
        activated("derived", Role::Picker, DownloadScope::Derived, &hash),
        activated("everything", Role::Picker, DownloadScope::All, &hash),
    ]);

    let sessions = [
        ("nothing", sign_in(&app, "nothing").await),
        ("picks", sign_in(&app, "picks").await),
        ("derived", sign_in(&app, "derived").await),
        ("everything", sign_in(&app, "everything").await),
    ];

    // Something to download.
    put(
        &app,
        &interpretation_uri("picks"),
        &document(RADARGRAM),
        Some(&sessions[1].1),
    )
    .await;

    let raw = format!("/api/v1/datasets/{RADARGRAM}/interpretations/picks/raw");
    let image = format!("/api/v1/datasets/{RADARGRAM}/views/standard/image?width=20");
    let track = format!("/api/v1/datasets/{RADARGRAM}/track.geojson");
    let netcdf = format!("/api/v1/datasets/{RADARGRAM}/download");

    // What each level may take. The rows are the ladder.
    //
    // Asserted as "was this refused on permission grounds", not as "did it
    // return 200": the fixture radargram carries no coordinate variables,
    // so its track cannot be built and answers 500 even to someone allowed
    // it. Whether the scope let the request through is the question here,
    // and `track.rs` has its own tests for the rest.
    let expected = [
        // (session, raw, image, track+netcdf)
        (0usize, false, false, false),
        (1, true, false, false),
        (2, true, true, false),
        (3, true, true, true),
    ];
    for (index, picks_ok, derived_ok, all_ok) in expected {
        let (name, session) = &sessions[index];
        let permitted = |response: &Response| {
            !matches!(
                response.status,
                StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
            )
        };

        let response = get(&app, &raw, Some(session)).await;
        assert_eq!(permitted(&response), picks_ok, "{name}: raw picks");
        if picks_ok {
            assert_eq!(response.status, StatusCode::OK, "{name}: raw picks");
        }

        let response = get(&app, &image, Some(session)).await;
        assert_eq!(permitted(&response), derived_ok, "{name}: rendered image");
        if derived_ok {
            assert_eq!(response.status, StatusCode::OK, "{name}: rendered image");
        }

        for uri in [&track, &netcdf] {
            assert_eq!(
                permitted(&get(&app, uri, Some(session)).await),
                all_ok,
                "{name}: {uri}"
            );
        }
        if all_ok {
            assert_eq!(
                get(&app, &netcdf, Some(session)).await.status,
                StatusCode::OK,
                "{name}: the radargram itself"
            );
        }

        // The viewer keeps working at every level, including "none": the
        // chunks it draws from are not a download, and gating them would
        // make the scope setting break the thing it is meant to protect.
        let chunk = format!("/api/v1/datasets/{RADARGRAM}/views/standard/chunks/default/0/0");
        assert_eq!(
            get(&app, &chunk, Some(session)).await.status,
            StatusCode::OK,
            "{name}: the viewer must keep working"
        );
        assert_eq!(
            get(&app, &format!("/view/{RADARGRAM}"), Some(session))
                .await
                .status,
            StatusCode::OK,
            "{name}: the viewer page must keep opening"
        );
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_download_the_caller_may_not_have_is_not_offered() {
    // The API refusing is necessary and not sufficient. A menu entry is a
    // promise; one that answers with an error dialog teaches someone about
    // a permission in the worst possible way, and looks like a bug rather
    // than a policy.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("everything", Role::Picker, DownloadScope::All, &hash),
        activated("derived", Role::Picker, DownloadScope::Derived, &hash),
        activated("nothing", Role::Picker, DownloadScope::None, &hash),
    ]);

    let viewer_uri = format!("/view/{RADARGRAM}");

    let full = sign_in(&app, "everything").await;
    let page = get(&app, &viewer_uri, Some(&full)).await;
    for control in [
        "dl-points",
        "dl-raw",
        "dl-track",
        "dl-image",
        "dl-radargram",
    ] {
        assert!(page.text.contains(control), "{control} should be offered");
    }

    // `derived` keeps the level 2 points and the rendered image, and loses
    // the two that are the underlying data.
    let partial = sign_in(&app, "derived").await;
    let page = get(&app, &viewer_uri, Some(&partial)).await;
    for control in ["dl-points", "dl-raw", "dl-image"] {
        assert!(page.text.contains(control), "{control} should be offered");
    }
    for control in ["dl-track", "dl-radargram"] {
        assert!(!page.text.contains(control), "{control} should be hidden");
    }

    // `none` loses the menu itself rather than being shown an empty one.
    let barred = sign_in(&app, "nothing").await;
    let page = get(&app, &viewer_uri, Some(&barred)).await;
    assert!(!page.text.contains("id=\"download-menu\""), "{}", page.text);
    // And the page still works -- the viewer is not a download.
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text.contains("RIDAL_VIEWER"), "the viewer still loads");

    // The catalog's merged downloads follow the same rule.
    let index = get(&app, "/", Some(&barred)).await;
    assert!(!index.text.contains("data-download="), "{}", index.text);
    let index = get(&app, "/", Some(&full)).await;
    assert!(index.text.contains(r#"data-download="level2""#));
    assert!(index.text.contains(r#"data-download="track.geojson""#));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_anonymous_reader_is_not_offered_a_download_of_their_own_picks() {
    // A merged download covers one person's picks, and an anonymous reader
    // has no "mine" to resolve -- the route can only answer 400. Rather
    // than let the catalog offer a button that always fails, the control
    // is absent, which is what Erik saw and reported.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);

    let index = get(&app, "/", None).await;
    assert!(
        !index.text.contains(r#"data-download="level2""#),
        "a reader with no account cannot download 'their' picks: {}",
        index.text
    );
    // The track is theirs to take under the default anonymous scope, so
    // that one stays.
    assert!(index.text.contains(r#"data-download="track.geojson""#));

    // Signing in brings it back.
    let session = sign_in(&app, "erik").await;
    let index = get(&app, "/", Some(&session)).await;
    assert!(index.text.contains(r#"data-download="level2""#));

    // The viewer's own pick downloads follow the same rule.
    let page = get(&app, &format!("/view/{RADARGRAM}"), None).await;
    assert!(!page.text.contains("dl-points"), "{}", page.text);
    assert!(!page.text.contains("dl-raw"), "{}", page.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_anonymous_reader_downloads_what_the_project_allows_them() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_set(UserSet {
        anonymous_download: DownloadScope::Derived,
        users: vec![activated("erik", Role::Admin, DownloadScope::All, &hash)],
        ..UserSet::default()
    });

    // Public read is the default, so the catalog is open...
    assert_eq!(
        get(&app, "/api/v1/datasets", None).await.status,
        StatusCode::OK
    );
    // ...the rendered image is within "derived"...
    assert_eq!(
        get(
            &app,
            &format!("/api/v1/datasets/{RADARGRAM}/views/standard/image?width=20"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    // ...and the radargram itself is not. Anonymous gets a 401 rather than
    // a 403, because for them signing in is a thing that might help.
    let refused = get(
        &app,
        &format!("/api/v1/datasets/{RADARGRAM}/download"),
        None,
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);

    let admin = sign_in(&app, "erik").await;
    assert_eq!(
        get(
            &app,
            &format!("/api/v1/datasets/{RADARGRAM}/download"),
            Some(&admin)
        )
        .await
        .status,
        StatusCode::OK
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn requiring_a_login_to_read_hides_everything_but_the_way_in() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_set(UserSet {
        require_auth_to_read: true,
        users: vec![activated("erik", Role::Admin, DownloadScope::All, &hash)],
        ..UserSet::default()
    });

    // A page redirects, because someone who followed a link wants the page
    // rather than a status code.
    let redirected = get(&app, "/", None).await;
    assert_eq!(redirected.status, StatusCode::SEE_OTHER);

    // The API says so in the envelope every other failure uses.
    let refused = get(&app, "/api/v1/datasets", None).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.body["error"]["code"], "authentication_required");

    // The way in stays open, including the login page's own assets -- a
    // login screen that 401s its stylesheet is not a login screen.
    for uri in [
        "/login",
        "/static/app.css",
        "/static/login.js",
        "/favicon.ico",
        "/api/v1/health",
    ] {
        assert_eq!(get(&app, uri, None).await.status, StatusCode::OK, "{uri}");
    }

    let session = sign_in(&app, "erik").await;
    assert_eq!(get(&app, "/", Some(&session)).await.status, StatusCode::OK);
    assert_eq!(
        get(&app, "/api/v1/datasets", Some(&session)).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn preferences_sit_between_the_request_and_the_project_default() {
    // The cascade, all four layers, against the value a page actually
    // renders with.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Operator, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let erik = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;

    let viewer_uri = format!("/view/{RADARGRAM}");
    let renders_with =
        |text: &str, profile: &str| text.contains(&format!("value=\"{profile}\" selected"));

    // Built-in, with nothing set anywhere.
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(renders_with(&page.text, "default"), "{}", page.text);

    // The project default reaches everyone who has not chosen.
    put(
        &app,
        "/api/v1/project/settings",
        &json!({"default_profile": "abslog"}),
        Some(&erik),
    )
    .await;
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(renders_with(&page.text, "abslog"), "{}", page.text);

    // One person's own preference wins over it...
    let saved = put(
        &app,
        "/api/v1/preferences",
        &json!({"render_profile": "positive", "x_scale": 2.0}),
        Some(&student),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(renders_with(&page.text, "positive"), "{}", page.text);

    // ...for them alone.
    let page = get(&app, &viewer_uri, Some(&erik)).await;
    assert!(renders_with(&page.text, "abslog"), "{}", page.text);

    // And the request parameter wins over everything, for one page.
    let page = get(&app, &format!("{viewer_uri}?profile=positive"), Some(&erik)).await;
    assert!(renders_with(&page.text, "positive"), "{}", page.text);

    // `?xscale=` too, which fell out of naming the cascade.
    let page = get(&app, &format!("{viewer_uri}?xscale=4"), Some(&student)).await;
    assert!(page.text.contains("value=\"4\" selected"), "{}", page.text);

    // Clearing a preference falls back rather than storing the default,
    // which is what lets a later project change reach this person again.
    put(
        &app,
        "/api/v1/preferences",
        &json!({"render_profile": null, "x_scale": null}),
        Some(&student),
    )
    .await;
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(renders_with(&page.text, "abslog"), "{}", page.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn wanting_the_neutral_value_is_a_choice_rather_than_an_absence() {
    // #176: every dropdown in "My settings" offers "Project default", so
    // absence means *deferring*. 1x used to be collapsed to absence on the
    // grounds that it is the neutral value, which made "I want 1x"
    // unsayable in a project whose default is 2x.
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with(vec![
        activated("erik", Role::Operator, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let erik = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;
    let viewer_uri = format!("/view/{RADARGRAM}");

    put(
        &app,
        "/api/v1/project/settings",
        &json!({"default_xscale": 2.0}),
        Some(&erik),
    )
    .await;
    // Without a preference, the project's 2x applies.
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(page.text.contains("value=\"2\" selected"), "{}", page.text);

    let saved = put(
        &app,
        "/api/v1/preferences",
        &json!({"x_scale": 1.0}),
        Some(&student),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    assert_eq!(saved.body["x_scale"], 1.0, "an explicit 1x is a preference");
    let stored =
        std::fs::read_to_string(data(dir.path()).join("preferences/student.json")).unwrap();
    assert!(stored.contains("x_scale"), "{stored}");

    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(page.text.contains("value=\"1\" selected"), "{}", page.text);
    // And the project's default still applies to everyone else.
    let page = get(&app, &viewer_uri, Some(&erik)).await;
    assert!(page.text.contains("value=\"2\" selected"), "{}", page.text);

    // Choosing "Project default" -- an empty value -- defers again.
    put(
        &app,
        "/api/v1/preferences",
        &json!({"x_scale": null}),
        Some(&student),
    )
    .await;
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(page.text.contains("value=\"2\" selected"), "{}", page.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_preference_save_leaves_the_settings_it_does_not_mention_alone() {
    // Absent used to mean "clear it", which made the endpoint a trap for
    // anything but the one page that sends every field: `PUT
    // {"theme":"dark"}` silently wiped the profile and the scale with it.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    put(
        &app,
        "/api/v1/preferences",
        &json!({"render_profile": "abslog", "x_scale": 2.0, "level2_spacing": "10"}),
        Some(&erik),
    )
    .await;

    // A request about one setting changes that setting.
    let saved = put(
        &app,
        "/api/v1/preferences",
        &json!({"theme": "dark"}),
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    assert_eq!(saved.body["theme"], "dark");
    assert_eq!(saved.body["render_profile"], "abslog", "{}", saved.text);
    assert_eq!(saved.body["x_scale"], 2.0, "{}", saved.text);
    assert_eq!(saved.body["level2_spacing"], "10", "{}", saved.text);

    // And `null` still clears the one it names, which is what "Project
    // default" in the dropdown sends.
    let saved = put(
        &app,
        "/api/v1/preferences",
        &json!({"render_profile": null}),
        Some(&erik),
    )
    .await;
    assert!(saved.body["render_profile"].is_null(), "{}", saved.text);
    assert_eq!(saved.body["theme"], "dark", "{}", saved.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_signed_in_theme_reaches_the_login_page_too() {
    // It is the way *out* as much as the way in, so a person who chose dark
    // should not get one light screen on the way past it (#141).
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;
    put(
        &app,
        "/api/v1/preferences",
        &json!({"theme": "dark"}),
        Some(&erik),
    )
    .await;

    let page = get(&app, "/login", Some(&erik)).await;
    assert!(
        page.text.contains("<html lang=\"en\" data-theme=\"dark\">"),
        "{}",
        page.text
    );
    // And an anonymous visitor has no preference to apply, so the page
    // follows their device as it always did.
    let page = get(&app, "/login", None).await;
    assert!(!page.text.contains("data-theme"), "{}", page.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_chosen_theme_reaches_every_page_and_only_that_person() {
    // #141. Written by the server onto the root element rather than applied
    // by a script, so the page arrives in the right colours instead of
    // flashing the wrong ones -- which is the thing worth pinning.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Operator, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let erik = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;

    let saved = put(
        &app,
        "/api/v1/preferences",
        &json!({"theme": "dark"}),
        Some(&student),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);

    for uri in ["/", &format!("/view/{RADARGRAM}"), "/settings", "/layers"] {
        let page = get(&app, uri, Some(&student)).await;
        assert!(
            page.text.contains("<html lang=\"en\" data-theme=\"dark\">"),
            "{uri} did not carry the chosen theme"
        );
    }
    // For them alone: a theme is about a person, not about the project.
    let page = get(&app, "/", Some(&erik)).await;
    assert!(!page.text.contains("data-theme"), "{}", page.text);

    // Clearing it goes back to following the device, which is the absence
    // of the attribute rather than a third value.
    put(
        &app,
        "/api/v1/preferences",
        &json!({"theme": null}),
        Some(&student),
    )
    .await;
    let page = get(&app, "/", Some(&student)).await;
    assert!(!page.text.contains("data-theme"), "{}", page.text);

    // And a theme nothing renders is refused rather than stored.
    let refused = put(
        &app,
        "/api/v1/preferences",
        &json!({"theme": "sepia"}),
        Some(&student),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(refused.body["error"]["code"], "unknown_theme");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn hiding_the_interpretations_is_remembered_per_person() {
    // #143. Only "hidden" is stored: shown is what the viewer did before
    // the toggle existed, so it stays the answer for anyone who has not
    // chosen.
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with(vec![
        activated("erik", Role::Picker, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let erik = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;
    let viewer_uri = format!("/view/{RADARGRAM}");

    let saved = put(
        &app,
        "/api/v1/preferences",
        &json!({"show_picks": false}),
        Some(&student),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    assert_eq!(saved.body["show_picks"], false);

    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(page.text.contains("showPicks: false"), "{}", page.text);
    // The button arrives saying what pressing it will do, rather than
    // announcing the opposite until the script catches up.
    assert!(page.text.contains(">Show picks</button>"), "{}", page.text);
    assert!(
        page.text.contains("aria-pressed=\"false\""),
        "{}",
        page.text
    );
    let page = get(&app, &viewer_uri, Some(&erik)).await;
    assert!(page.text.contains("showPicks: true"), "{}", page.text);
    assert!(page.text.contains(">Hide picks</button>"), "{}", page.text);

    // Ticking it again stores nothing, so the shown default keeps applying
    // rather than being frozen in.
    put(
        &app,
        "/api/v1/preferences",
        &json!({"show_picks": true}),
        Some(&student),
    )
    .await;
    let stored =
        std::fs::read_to_string(data(dir.path()).join("preferences/student.json")).unwrap();
    assert!(!stored.contains("show_picks"), "{stored}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_download_defaults_cascade_from_the_project_to_the_person() {
    // #166, through the same cascade as every other setting: the project
    // says what a survey exports, a person may disagree, and the dialog
    // still wins for one download.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Operator, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let erik = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;
    let viewer_uri = format!("/view/{RADARGRAM}");
    let opens_on = |text: &str, value: &str| text.contains(&format!("value=\"{value}\" selected"));

    // Ridal's own answer, with nothing set anywhere.
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(opens_on(&page.text, "auto"), "{}", page.text);
    assert!(opens_on(&page.text, "geojson"), "{}", page.text);

    put(
        &app,
        "/api/v1/project/settings",
        &json!({"default_spacing": "25", "default_format": "csv"}),
        Some(&erik),
    )
    .await;
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(opens_on(&page.text, "25"), "{}", page.text);
    assert!(opens_on(&page.text, "csv"), "{}", page.text);

    // One person's own wins over it, for them alone.
    let saved = put(
        &app,
        "/api/v1/preferences",
        &json!({"level2_spacing": "5", "level2_format": "geojson-native"}),
        Some(&student),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(opens_on(&page.text, "5"), "{}", page.text);
    assert!(opens_on(&page.text, "geojson-native"), "{}", page.text);
    let page = get(&app, &viewer_uri, Some(&erik)).await;
    assert!(opens_on(&page.text, "25"), "{}", page.text);

    // A spacing the dialogs do not offer is refused rather than stored.
    let refused = put(
        &app,
        "/api/v1/preferences",
        &json!({"level2_spacing": "17"}),
        Some(&student),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(refused.body["error"]["code"], "unknown_spacing");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_basemap_resolves_through_the_same_cascade() {
    // #177 added a third preference, and the point of naming the cascade was
    // that adding one is a call rather than a fourth chance to get the order
    // wrong. This is that claim, checked against what a page carries.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Operator, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let erik = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;
    let viewer_uri = format!("/view/{RADARGRAM}");
    let opens_with = |text: &str, id: &str| text.contains(&format!("data-basemap=\"{id}\""));

    // Nothing defined anywhere: the built-in, as before #177.
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(
        opens_with(&page.text, "esri-world-imagery"),
        "{}",
        page.text
    );

    let two = json!([
        {"id": "osm", "name": "OpenStreetMap",
         "url": "https://tile.example.org/osm/{z}/{x}/{y}.png"},
        {"id": "topo", "name": "Topographic",
         "url": "https://tile.example.org/topo/{z}/{x}/{y}.png"},
    ]);
    let saved = put(
        &app,
        "/api/v1/project/settings",
        &json!({"basemaps": two, "default_basemap": "topo"}),
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);

    // The project default reaches whoever has not chosen...
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(opens_with(&page.text, "topo"), "{}", page.text);
    // ...and all three are offered, so the layer control can switch.
    assert!(page.text.contains("OpenStreetMap"), "{}", page.text);
    assert!(page.text.contains("ESRI World Imagery"), "{}", page.text);

    // One person's own wins, for them alone.
    let saved = put(
        &app,
        "/api/v1/preferences",
        &json!({"basemap": "osm"}),
        Some(&student),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    assert!(opens_with(
        &get(&app, &viewer_uri, Some(&student)).await.text,
        "osm"
    ));
    assert!(opens_with(
        &get(&app, &viewer_uri, Some(&erik)).await.text,
        "topo"
    ));

    // The catalog page carries them too -- it has one map per group.
    assert!(opens_with(
        &get(&app, "/", Some(&student)).await.text,
        "osm"
    ));

    // A preference for a basemap the project no longer offers falls back
    // rather than leaving that person's maps blank.
    put(
        &app,
        "/api/v1/project/settings",
        &json!({"basemaps": [], "default_basemap": null}),
        Some(&erik),
    )
    .await;
    let page = get(&app, &viewer_uri, Some(&student)).await;
    assert!(
        opens_with(&page.text, "esri-world-imagery"),
        "{}",
        page.text
    );
    // The stored preference is untouched: the project may well offer it
    // again, and forgetting it here would be a second, silent loss.
    let mine = get(&app, "/api/v1/preferences", Some(&student)).await;
    assert_eq!(mine.body["basemap"], "osm");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_basemap_preference_nothing_offers_is_refused() {
    // The same rule as an unknown profile: the layer control only lists what
    // is offered, so a stored id nothing matches would be unfixable from the
    // page that set it.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let refused = put(
        &app,
        "/api/v1/preferences",
        &json!({"basemap": "nope"}),
        Some(&erik),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.text);
    assert_eq!(refused.body["error"]["code"], "unknown_basemap");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_admin_setting_the_project_default_does_not_overwrite_anyone() {
    // The alternative -- a project default that stamps over personal
    // choices -- would make the operator's save button destructive in a way
    // nothing in the UI could warn about convincingly.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![
        activated("erik", Role::Operator, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let erik = sign_in(&app, "erik").await;
    let student = sign_in(&app, "student").await;

    put(
        &app,
        "/api/v1/preferences",
        &json!({"render_profile": "positive"}),
        Some(&student),
    )
    .await;
    put(
        &app,
        "/api/v1/project/settings",
        &json!({"default_profile": "abslog"}),
        Some(&erik),
    )
    .await;

    let mine = get(&app, "/api/v1/preferences", Some(&student)).await;
    assert_eq!(mine.body["render_profile"], "positive");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_anonymous_reader_has_nowhere_to_keep_a_preference() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);

    // A 401 rather than a silent no-op: the page would otherwise show a
    // saved setting that was never saved.
    let refused = put(
        &app,
        "/api/v1/preferences",
        &json!({"render_profile": "abslog"}),
        None,
    )
    .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert!(refused.text.contains("?profile="), "{}", refused.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_merged_download_asks_whose_picks_when_nobody_is_signed_in() {
    // "Mine" has no meaning for an anonymous reader, and guessing would
    // hand back an empty file that looks complete.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);

    let refused = get(&app, "/api/v1/catalog/level2", None).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(refused.body["error"]["code"], "user_required");
    assert!(refused.text.contains("?user="), "{}", refused.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn signing_out_clears_the_cookie_and_the_session_with_it() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);
    let session = sign_in(&app, "erik").await;
    assert_eq!(
        get(&app, "/api/v1/auth/me", Some(&session)).await.body["user"],
        "erik"
    );

    let out = post(&app, "/api/v1/auth/logout", &json!({}), Some(&session)).await;
    assert_eq!(out.status, StatusCode::OK);
    let cleared = out.cookie.expect("signing out must clear the cookie");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    assert!(cleared.starts_with("ridal_session=;"), "{cleared}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_session_survives_a_restart() {
    // The reason a signed cookie is the smaller option: the key is on disk,
    // so nothing about sessions has to be.
    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM);

    let build = || {
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
        build_router(state)
    };

    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Admin, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
    drop(project);

    let first = build();
    let session = sign_in(&first, "erik").await;

    // A second server over the same project, as a restart would be.
    let second = build();
    let me = get(&second, "/api/v1/auth/me", Some(&session)).await;
    assert_eq!(me.body["user"], "erik", "a restart signed everyone out");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_project_with_no_accounts_offers_no_login_and_still_writes() {
    // The migration rule: upgrading Ridal must not lock anyone out of their
    // own data, and `ridal gui` must keep working with no login step.
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
    let app = build_router(state);

    let me = get(&app, "/api/v1/auth/me", None).await;
    assert_eq!(me.body["user"], "default");
    assert_eq!(me.body["role"], "operator");
    assert_eq!(me.body["authentication_configured"], false);

    let saved = put(
        &app,
        &interpretation_uri("default"),
        &document(RADARGRAM),
        None,
    )
    .await;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.text);

    // No `users.json` was created by serving, only by an administrator
    // deciding to create one.
    assert!(!data(dir.path()).join("users.json").exists());
    // And no session key either: a project that never authenticates never
    // grows one.
    assert!(!data(dir.path()).join("session.key").exists());

    // The login page explains rather than offering a form that cannot work.
    let login = get(&app, "/login", None).await;
    assert_eq!(login.status, StatusCode::OK);
    assert!(login.text.contains("no accounts"), "{}", login.text);
    assert!(
        login.text.contains("ridal project user add"),
        "{}",
        login.text
    );
    // And the header offers no sign-in link, since there is nothing to sign
    // in to.
    let index = get(&app, "/", None).await;
    assert!(!index.text.contains(">Sign in<"), "{}", index.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_offline_session_signs_its_cookies_with_a_key_it_never_writes_down() {
    // `ridal gui` runs in the user's own survey directory, and that
    // directory gets zipped, synced and mailed around (#187). A signing key
    // in one is a leaked signing key, so offline mode keeps it in memory:
    // sessions work for as long as the server runs, and stop when it does.
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with_set_and_access(
        UserSet {
            users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        AccessOptions {
            persist_sessions: false,
            ..AccessOptions::default()
        },
    );

    let erik = sign_in(&app, "erik").await;
    let me = get(&app, "/api/v1/auth/me", Some(&erik)).await;
    assert_eq!(me.body["user"], "erik", "{}", me.text);

    assert!(
        !data(dir.path()).join("session.key").exists(),
        "offline mode must not leave a signing key in the project"
    );
    // The accounts file is the user's own decision and stays on disk; only
    // the secret Ridal generates is withheld.
    assert!(data(dir.path()).join("users.json").exists());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_served_project_keeps_its_signing_key_so_a_restart_does_not_sign_everyone_out() {
    // The other half: `ridal server start` is a deployment whose sessions
    // are expected to outlive a restart, and whose project directory is the
    // operator's rather than a survey directory being passed around.
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with_set_and_access(
        UserSet {
            users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        AccessOptions {
            persist_sessions: true,
            ..AccessOptions::default()
        },
    );

    sign_in(&app, "erik").await;

    // In the data directory, which is what the `.gitignore` written at init
    // covers -- and never in the project root.
    assert!(data(dir.path()).join("session.key").exists());
    assert!(!dir.path().join("session.key").exists());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_first_account_takes_effect_without_a_restart() {
    // What lets `ridal project user add` say there is nothing to restart.
    // The account file is read per request, so a server already serving an
    // open project becomes an authenticated one the moment the file
    // appears beneath it.
    let hash = users::hash_password(password()).unwrap();
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
    let app = build_router(state);

    // Open to begin with: everyone is the local default user and can write.
    let before = get(&app, "/api/v1/auth/me", None).await;
    assert_eq!(before.body["user"], "default");
    assert_eq!(before.body["role"], "operator");

    // An administrator appears, as the command line would create one --
    // without this server being told.
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Admin, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
    drop(project);

    let after = get(&app, "/api/v1/auth/me", None).await;
    assert_eq!(
        after.body["user"],
        Value::Null,
        "no longer the default user"
    );
    assert_eq!(after.body["role"], "viewer");
    assert_eq!(after.body["authentication_configured"], true);

    // And the same running server can sign that account in.
    let session = sign_in(&app, "erik").await;
    assert_eq!(
        get(&app, "/api/v1/auth/me", Some(&session)).await.body["user"],
        "erik"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_damaged_user_file_denies_everything_rather_than_opening_it() {
    // The failure mode worth naming: `UserSet::default()` looks like a
    // safe fallback and is the permissive one -- public read, anonymous
    // downloads of everything. A project that had required a login would
    // have started serving its catalog to anyone the moment its policy
    // file was damaged.
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM);
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(project.documents(), &UserSet::default(), &Expectation::Any).unwrap();
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

    // Readable while the file is intact.
    assert_eq!(
        get(&app, "/api/v1/datasets", None).await.status,
        StatusCode::OK
    );

    std::fs::write(data(dir.path()).join("users.json"), "{ not json at all").unwrap();

    // And closed once it is not: no public read, and no downloads.
    assert_eq!(
        get(&app, "/api/v1/datasets", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(
            &app,
            &format!("/api/v1/datasets/{RADARGRAM}/views/standard/image?width=20"),
            None
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    // The way in still renders, so the server is visibly refusing rather
    // than simply broken.
    assert_eq!(get(&app, "/login", None).await.status, StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_bind_that_cannot_carry_a_password_refuses_one_per_request() {
    // Sampling this at startup was not enough: a public --read-only server
    // with no accounts starts legitimately, and the first administrator
    // can be created while it runs. The guard has to be asked on the
    // request, not remembered from boot.
    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM);
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Admin, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
    let state = Arc::new(
        AppState::build_with_project(
            dir.path(),
            &RenderServiceConfig::default(),
            Some(project),
            AccessOptions {
                allow_password_login: false,
                ..AccessOptions::default()
            },
        )
        .unwrap(),
    );
    let app = build_router(state);

    let refused = post(
        &app,
        "/api/v1/auth/login",
        &json!({"name": "erik", "password": password()}),
        None,
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.body["error"]["code"], "insecure_transport");
    assert!(refused.text.contains("reverse proxy"), "{}", refused.text);
    assert!(refused.cookie.is_none(), "no session may be issued");

    // Redeeming an invite sends a password too, so it is guarded the same.
    let redeem = post(
        &app,
        "/api/v1/auth/invite",
        &json!({"token": "whatever", "password": format!("{}-new", password())}),
        None,
    )
    .await;
    assert_eq!(redeem.status, StatusCode::FORBIDDEN);
    assert_eq!(redeem.body["error"]["code"], "insecure_transport");

    // Reading is unaffected -- the guard is about passwords on the wire,
    // not about who may look.
    assert_eq!(
        get(&app, "/api/v1/datasets", None).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_invalid_invite_is_refused_without_hashing_the_password() {
    // Argon2id is deliberately expensive, so hashing before checking the
    // token let an unauthenticated caller spend a hash of the server's CPU
    // per request. Timed rather than asserted structurally, because the
    // property *is* the cost.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with(vec![activated(
        "erik",
        Role::Admin,
        DownloadScope::All,
        &hash,
    )]);

    let start = std::time::Instant::now();
    let refused = post(
        &app,
        "/api/v1/auth/invite",
        &json!({"token": "not-a-real-token", "password": format!("{}-new", password())}),
        None,
    )
    .await;
    let rejecting = start.elapsed();
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);

    // What one hash actually costs on this machine, measured rather than
    // assumed -- the parameters are the crate's defaults and will change.
    let start = std::time::Instant::now();
    users::hash_password(&format!("{}-cost", password())).unwrap();
    let hashing = start.elapsed();

    assert!(
        rejecting < hashing,
        "rejecting a bad token took {rejecting:?}, which is not comfortably \
         less than the {hashing:?} one hash costs -- the token is probably \
         being checked after the hash again"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_read_only_server_tells_an_anonymous_caller_the_truth() {
    // Signing in cannot make a write succeed on a read-only server, so
    // answering "sign in and try again" sends someone down a road with no
    // end. The refusal has to name the flag even for a caller who has no
    // account at all.
    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM);
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Picker, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
    let state = Arc::new(
        AppState::build_with_project(
            dir.path(),
            &RenderServiceConfig::default(),
            Some(project),
            AccessOptions {
                read_only: true,
                ..AccessOptions::default()
            },
        )
        .unwrap(),
    );
    let app = build_router(state);

    let refused = put(
        &app,
        &interpretation_uri("erik"),
        &document(RADARGRAM),
        None,
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.body["error"]["code"], "read_only");

    // Reading is untouched: `require` checks the effective role before it
    // looks at why that role is what it is.
    assert_eq!(
        get(&app, "/api/v1/datasets", None).await.status,
        StatusCode::OK
    );
    assert_eq!(
        get(&app, &format!("/view/{RADARGRAM}"), None).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_read_only_server_caps_even_an_administrator() {
    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&radargrams(dir.path()).join("line-01.nc"), RADARGRAM);
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Admin, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
    let state = Arc::new(
        AppState::build_with_project(
            dir.path(),
            &RenderServiceConfig::default(),
            Some(project),
            AccessOptions {
                read_only: true,
                ..AccessOptions::default()
            },
        )
        .unwrap(),
    );
    let app = build_router(state);

    let session = sign_in(&app, "erik").await;
    let me = get(&app, "/api/v1/auth/me", Some(&session)).await;
    assert_eq!(me.body["account_role"], "admin", "the account is unchanged");
    assert_eq!(me.body["role"], "viewer", "what they may do here is not");

    let refused = put(
        &app,
        &interpretation_uri("erik"),
        &document(RADARGRAM),
        Some(&session),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    // Names the flag, because the operator of the server is the one who can
    // change it -- "you are a viewer" would be true and useless.
    assert_eq!(refused.body["error"]["code"], "read_only");

    // Reading is unaffected.
    assert_eq!(
        get(&app, "/api/v1/datasets", Some(&session)).await.status,
        StatusCode::OK
    );
}

/// Builds an app with two radargrams, one of them unlisted, and the given
/// users.
fn app_with_an_unlisted_radargram(set: UserSet) -> (tempfile::TempDir, Router) {
    use crate::project::overrides::{self, RadargramOverride};

    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    // With coordinate axes, so the group map has tracks to draw: whether an
    // unlisted radargram reaches that map is one of the things this fixture
    // is for.
    for id in ["line-01", "line-02"] {
        super::interp_routes_tests::write_test_nc_with_axes(
            &radargrams(dir.path()).join(format!("{id}.nc")),
            id,
            None,
        );
    }
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(project.documents(), &set, &Expectation::Any).unwrap();
    overrides::update(project.documents(), |o| {
        o.radargrams.insert(
            crate::identity::RadargramId::new("line-02").unwrap(),
            RadargramOverride {
                unlisted: true,
                ..Default::default()
            },
        );
        Ok(())
    })
    .unwrap();

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

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_unlisted_radargram_is_absent_from_a_pickers_listing_and_present_for_an_operator() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![
            activated("student", Role::Picker, DownloadScope::All, &hash),
            activated("erik", Role::Operator, DownloadScope::All, &hash),
        ],
        ..UserSet::default()
    });

    let ids = |body: &Value| -> Vec<String> {
        body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["radargram_id"].as_str().unwrap().to_string())
            .collect()
    };

    let student = sign_in(&app, "student").await;
    let seen = ids(&get(&app, "/api/v1/datasets", Some(&student)).await.body);
    assert_eq!(seen, vec!["line-01"], "a picker sees only the listed one");

    // The operator is the person who can change it, so hiding it from them
    // would leave no way to find it again.
    let erik = sign_in(&app, "erik").await;
    let seen = ids(&get(&app, "/api/v1/datasets", Some(&erik)).await.body);
    assert_eq!(seen, vec!["line-01", "line-02"]);

    // Curation, not access control: the picker can still open it by id,
    // which is exactly what "unlisted" claims and all it claims.
    let response = get(&app, "/api/v1/datasets/line-02", Some(&student)).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_operator_can_rename_a_radargram_without_restarting_the_server() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![
            activated("student", Role::Picker, DownloadScope::All, &hash),
            activated("erik", Role::Operator, DownloadScope::All, &hash),
        ],
        ..UserSet::default()
    });
    let erik = sign_in(&app, "erik").await;

    // The dialog asks what the field would be without an override, which
    // is the whole reason "revert" can be offered honestly.
    let before = get(&app, "/api/v1/datasets/line-01/properties", Some(&erik)).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert!(before.body["effective"]["display_name"].is_null());
    assert_eq!(before.body["overridden"]["display_name"], false);

    let saved = put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"display_name": "A better name", "unlisted": false}),
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.text);

    // Visible immediately: the catalog is re-resolved, not read once at
    // startup.
    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let entry = listing.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["radargram_id"] == "line-01")
        .unwrap()
        .clone();
    assert_eq!(entry["effective_label"], "A better name");

    let after = get(&app, "/api/v1/datasets/line-01/properties", Some(&erik)).await;
    assert_eq!(after.body["overridden"]["display_name"], true);
    // And what it would go back to, which here is "nothing, so the id".
    assert!(after.body["from_file"]["display_name"].is_null());

    // Reverting leaves no trace, rather than an empty entry that reads as
    // "somebody configured this".
    let reverted = put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"display_name": null, "unlisted": false}),
        Some(&erik),
    )
    .await;
    assert_eq!(reverted.status, StatusCode::NO_CONTENT, "{}", reverted.text);
    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let entry = listing.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["radargram_id"] == "line-01")
        .unwrap()
        .clone();
    assert_eq!(entry["effective_label"], "line-01");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_operator_can_set_and_revert_an_elevation_range() {
    // #168: the elevation range lives on the same properties document as
    // display_name/group, edited and reverted the same way.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
        ..UserSet::default()
    });
    let erik = sign_in(&app, "erik").await;

    let before = get(&app, "/api/v1/datasets/line-01/properties", Some(&erik)).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert!(before.body["effective"]["elevation_min"].is_null());
    assert_eq!(before.body["overridden"]["elevation"], false);

    // min >= max is refused rather than silently accepted and then
    // excluding every trace once the corrected view tries to use it.
    let invalid = put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"unlisted": false, "elevation_min": 100.0, "elevation_max": 100.0}),
        Some(&erik),
    )
    .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST, "{}", invalid.text);
    assert_eq!(invalid.body["error"]["code"], "invalid_elevation_range");

    let saved = put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"unlisted": false, "elevation_min": 50.0, "elevation_max": 150.0}),
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.text);

    let after = get(&app, "/api/v1/datasets/line-01/properties", Some(&erik)).await;
    assert_eq!(after.body["effective"]["elevation_min"], 50.0);
    assert_eq!(after.body["effective"]["elevation_max"], 150.0);
    assert_eq!(after.body["overridden"]["elevation"], true);

    // Reverting (both bounds null) leaves no trace, exactly like
    // display_name.
    let reverted = put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"unlisted": false, "elevation_min": null, "elevation_max": null}),
        Some(&erik),
    )
    .await;
    assert_eq!(reverted.status, StatusCode::NO_CONTENT, "{}", reverted.text);
    let after_revert = get(&app, "/api/v1/datasets/line-01/properties", Some(&erik)).await;
    assert!(after_revert.body["effective"]["elevation_min"].is_null());
    assert_eq!(after_revert.body["overridden"]["elevation"], false);

    // This fixture has no `elevation` axis, so its refusal is the *file*
    // cause however the window is set -- which is worth pinning on its
    // own: a saved floor and cap must not turn an unsupported file into a
    // reported bad window. The `topo_window_invalid` counterpart needs a
    // fixture with real axes, and lives in `app.rs`.
    let geometry = get(
        &app,
        "/api/v1/datasets/line-01/views/topo/geometry",
        Some(&erik),
    )
    .await;
    assert_eq!(
        geometry.status,
        StatusCode::BAD_REQUEST,
        "{}",
        geometry.text
    );
    assert_eq!(geometry.body["error"]["code"], "topo_unavailable");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_bad_elevation_window_is_reported_as_a_window_problem_not_an_unsupported_file() {
    // The other half of the cause split (#168), on a fixture that *can*
    // support the corrected view. Without real `elevation`/`depth` axes a
    // radargram refuses for the file cause before the window is ever
    // examined, so only a fixture like this can pin the HTTP mapping for
    // `topo_window_invalid` -- which is the code the viewer keys its
    // visible warning off.
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();

    let path = radargrams(dir.path()).join("topo-line.nc");
    let (n_samples, n_traces) = (16usize, 32usize);
    {
        let mut file = netcdf::create(&path).unwrap();
        file.add_dimension("y", n_samples).unwrap();
        file.add_dimension("x", n_traces).unwrap();
        let mut data = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        // Varying, not constant: a flat array makes the percentile
        // estimate degenerate (low == high) and every render fails for a
        // reason that has nothing to do with what this test is about.
        let values: Vec<f32> = (0..(n_samples * n_traces))
            .map(|i| (i % 97) as f32)
            .collect();
        data.put_values(&values, ..).unwrap();
        let mut elevation = file.add_variable::<f64>("elevation", &["x"]).unwrap();
        elevation
            .put_values(
                &(0..n_traces)
                    .map(|i| 100.0 + i as f64 * 0.1)
                    .collect::<Vec<f64>>(),
                ..,
            )
            .unwrap();
        let mut depth = file.add_variable::<f64>("depth", &["y"]).unwrap();
        depth
            .put_values(
                &(0..n_samples)
                    .map(|i| i as f64 * 0.05)
                    .collect::<Vec<f64>>(),
                ..,
            )
            .unwrap();
        file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
            .unwrap();
        file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
            .unwrap();
        file.add_attribute("ridal_radargram_id", "topo-line")
            .unwrap();
    }

    let hash = users::hash_password(password()).unwrap();
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();

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
    let erik = sign_in(&app, "erik").await;

    // With no window configured the view resolves fine.
    let ok = get(
        &app,
        "/api/v1/datasets/topo-line/views/topo/geometry",
        Some(&erik),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);

    // A floor above the highest surface leaves no rows to draw.
    let saved = put(
        &app,
        "/api/v1/datasets/topo-line/properties",
        &json!({"unlisted": false, "elevation_min": 9000.0}),
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.text);

    for path in [
        "/api/v1/datasets/topo-line/views/topo/geometry",
        "/api/v1/datasets/topo-line/views/topo/overview",
        "/api/v1/datasets/topo-line/views/topo/chunks/default/0/0",
    ] {
        let refused = get(&app, path, Some(&erik)).await;
        assert_eq!(
            refused.status,
            StatusCode::BAD_REQUEST,
            "{path}: {}",
            refused.text
        );
        assert_eq!(
            refused.body["error"]["code"], "topo_window_invalid",
            "{path} must blame the window, not the file"
        );
    }

    // And the standard view is unaffected by a window it does not use.
    let standard = get(
        &app,
        "/api/v1/datasets/topo-line/views/standard/overview",
        Some(&erik),
    )
    .await;
    assert_eq!(standard.status, StatusCode::OK, "{}", standard.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_picker_cannot_edit_properties() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![
            activated("student", Role::Picker, DownloadScope::All, &hash),
            activated("erik", Role::Operator, DownloadScope::All, &hash),
        ],
        ..UserSet::default()
    });
    let student = sign_in(&app, "student").await;

    let response = put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"display_name": "Mine now", "unlisted": false}),
        Some(&student),
    )
    .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN, "{}", response.text);
    // Reading is gated too: what a project says over its files is an
    // operator's working notes, not something a picker needs.
    assert_eq!(
        get(&app, "/api/v1/datasets/line-01/properties", Some(&student))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn moving_a_radargram_into_a_named_group_takes_effect_at_once() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
        ..UserSet::default()
    });
    let erik = sign_in(&app, "erik").await;

    // Named without being spelled: the slug is derived exactly as
    // processing derives it, so an operator never has to know about slugs.
    let saved = put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"grouping": "group", "group_name": "Drønbreen 2022", "unlisted": false}),
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.text);

    let properties = get(&app, "/api/v1/datasets/line-01/properties", Some(&erik)).await;
    assert_eq!(properties.body["effective"]["group_name"], "Drønbreen 2022");
    assert_eq!(properties.body["effective"]["group_id"], "dronbreen-2022");
    // The dialog still reports what the field would be without the
    // override, which is what makes the revert offer meaningful rather
    // than a leap of faith. Here that is *ungrouped*: the file declares no
    // group, and the only directory above it is the project's own
    // `radargrams/`, which is where a project keeps its files rather than
    // a group anyone chose. This assertion used to read "radargrams" and
    // was writing the fallback bug down as though it were the design.
    assert!(properties.body["from_file"]["group_id"].is_null());
    assert_eq!(properties.body["overridden"]["group"], true);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_unlisted_radargram_is_off_the_group_map_too() {
    // A track on the group map is a listing by another means. A group with
    // one listed member and one unlisted one still renders for a `picker`,
    // so without filtering the map endpoint their track would be drawn --
    // the one thing unlisting does claim to prevent.
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![
            activated("student", Role::Picker, DownloadScope::All, &hash),
            activated("erik", Role::Operator, DownloadScope::All, &hash),
        ],
        ..UserSet::default()
    });
    let _ = dir;
    let erik = sign_in(&app, "erik").await;

    // Put both in one group, so the group survives the filter.
    for id in ["line-01", "line-02"] {
        let response = put(
            &app,
            &format!("/api/v1/datasets/{id}/properties"),
            &json!({
                "grouping": "group",
                "group_name": "Shared",
                "unlisted": id == "line-02",
            }),
            Some(&erik),
        )
        .await;
        assert_eq!(response.status, StatusCode::NO_CONTENT, "{}", response.text);
    }

    let named = |body: &Value| -> Vec<String> {
        body.as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };

    let student = sign_in(&app, "student").await;
    let seen = get(&app, "/api/v1/groups/shared/tracks", Some(&student)).await;
    assert_eq!(
        named(&seen.body),
        vec!["line-01"],
        "the unlisted member must not be drawn: {}",
        seen.text
    );

    // The operator, who can change it, still sees it.
    let seen = get(&app, "/api/v1/groups/shared/tracks", Some(&erik)).await;
    let mut ids = named(&seen.body);
    ids.sort();
    assert_eq!(ids, vec!["line-01", "line-02"]);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn unlisting_takes_a_radargram_out_of_a_pickers_listing_at_once() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![
            activated("student", Role::Picker, DownloadScope::All, &hash),
            activated("erik", Role::Operator, DownloadScope::All, &hash),
        ],
        ..UserSet::default()
    });
    let student = sign_in(&app, "student").await;
    let erik = sign_in(&app, "erik").await;

    let count = |body: &Value| body["entries"].as_array().unwrap().len();
    assert_eq!(
        count(&get(&app, "/api/v1/datasets", Some(&student)).await.body),
        1
    );

    put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"unlisted": true}),
        Some(&erik),
    )
    .await;

    assert_eq!(
        count(&get(&app, "/api/v1/datasets", Some(&student)).await.body),
        0,
        "the picker's listing empties"
    );
    assert_eq!(
        count(&get(&app, "/api/v1/datasets", Some(&erik)).await.body),
        2,
        "the operator still sees both"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_operator_can_rename_a_group_in_one_place() {
    // The point of keeping a group's name with the group: one save, and
    // every member reports the new name. Renaming it through a member would
    // work too, but it reads as though the name belonged to that radargram.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![
            activated("student", Role::Picker, DownloadScope::All, &hash),
            activated("erik", Role::Operator, DownloadScope::All, &hash),
        ],
        ..UserSet::default()
    });
    let erik = sign_in(&app, "erik").await;

    for id in ["line-01", "line-02"] {
        put(
            &app,
            &format!("/api/v1/datasets/{id}/properties"),
            &json!({"grouping": "group", "group_name": "Kroppbreen", "unlisted": false}),
            Some(&erik),
        )
        .await;
    }

    let before = get(&app, "/api/v1/groups/kroppbreen/properties", Some(&erik)).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    assert_eq!(before.body["name"], "Kroppbreen");
    assert_eq!(before.body["member_count"], 2);
    assert_eq!(before.body["overridden"], true);

    let saved = put(
        &app,
        "/api/v1/groups/kroppbreen/properties",
        &json!({"display_name": "Kroppbreen, spring 2022"}),
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.text);

    // Both members, from one save, without a restart.
    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    for entry in listing.body["entries"].as_array().unwrap() {
        assert_eq!(
            entry["group_name"], "Kroppbreen, spring 2022",
            "{}",
            entry["radargram_id"]
        );
    }

    // And reverting puts back what the member files say -- which here is
    // the directory they sit in, since neither carries a group name.
    let reverted = put(
        &app,
        "/api/v1/groups/kroppbreen/properties",
        &json!({"display_name": null}),
        Some(&erik),
    )
    .await;
    assert_eq!(reverted.status, StatusCode::NO_CONTENT, "{}", reverted.text);
    let after = get(&app, "/api/v1/groups/kroppbreen/properties", Some(&erik)).await;
    assert_eq!(after.body["overridden"], false);
    assert_ne!(after.body["name"], "Kroppbreen, spring 2022");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn ungrouped_is_not_a_group_to_rename() {
    // `_none` is the absence of a group, not one that lost its name, and
    // saying so is more use than "not found".
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
        ..UserSet::default()
    });
    let erik = sign_in(&app, "erik").await;

    let response = put(
        &app,
        "/api/v1/groups/_none/properties",
        &json!({"display_name": "Everything else"}),
        Some(&erik),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert_eq!(response.body["error"]["code"], "not_a_group");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_picker_cannot_rename_a_group() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![
            activated("student", Role::Picker, DownloadScope::All, &hash),
            activated("erik", Role::Operator, DownloadScope::All, &hash),
        ],
        ..UserSet::default()
    });
    let erik = sign_in(&app, "erik").await;
    put(
        &app,
        "/api/v1/datasets/line-01/properties",
        &json!({"grouping": "group", "group_name": "Kroppbreen", "unlisted": false}),
        Some(&erik),
    )
    .await;

    let student = sign_in(&app, "student").await;
    let response = put(
        &app,
        "/api/v1/groups/kroppbreen/properties",
        &json!({"display_name": "Mine now"}),
        Some(&student),
    )
    .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN, "{}", response.text);
    assert_eq!(
        get(&app, "/api/v1/groups/kroppbreen/properties", Some(&student))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn merged_downloads_leave_unlisted_members_out_and_say_how_many() {
    // "Everything in this project" quietly including a radargram somebody
    // unlisted is a surprise, and a merged file that silently omits members
    // looks complete. Counted rather than named in the header: the omission
    // is the point, and listing the ids would undo it for whoever
    // downloaded the file.
    let hash = users::hash_password(password()).unwrap();
    let (dir, app) = app_with_an_unlisted_radargram(UserSet {
        users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
        ..UserSet::default()
    });
    let erik = sign_in(&app, "erik").await;

    let tracks = get(&app, "/api/v1/catalog/track.geojson", Some(&erik)).await;
    assert_eq!(tracks.status, StatusCode::OK, "{}", tracks.text);
    assert!(
        tracks.text.contains("line-01") && !tracks.text.contains("line-02"),
        "the unlisted member must not be in the file"
    );

    // Interpret the listed one so the level 2 download has something to
    // produce, then check the same rule holds there.
    let doc = serde_json::json!({
        "key": "line-01",
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[2.0, 2.0], [30.0, 3.0]]},
            "properties": {"id": "f-1", "label": "bed"}
        }]
    });
    let stored = data(dir.path()).join("interpretations/line-01");
    std::fs::create_dir_all(&stored).unwrap();
    std::fs::write(
        stored.join("erik.gprinterp.json"),
        serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();

    let points = get(
        &app,
        "/api/v1/catalog/level2?format=csv&spacing=10&user=erik",
        Some(&erik),
    )
    .await;
    assert_eq!(points.status, StatusCode::OK, "{}", points.text);
    assert!(!points.text.contains("line-02"), "unlisted member omitted");
    assert!(
        points.text.contains("line-01"),
        "the listed one is still there"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_warning_naming_an_unlisted_radargram_is_not_shown_to_a_picker() {
    // The warnings quote ids and paths, so showing one about a radargram
    // the listing just dropped would put it straight back.
    use crate::project::overrides::{self, RadargramOverride};

    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    // Two radargrams, plus a third carrying the same id as the second --
    // which is what makes the catalog warn about that id by name. Written
    // before the state is built, since discovery happens once.
    for (file, id) in [
        ("line-01.nc", "line-01"),
        ("line-02.nc", "line-02"),
        ("duplicate.nc", "line-02"),
    ] {
        super::interp_routes_tests::write_test_nc_with_axes(
            &radargrams(dir.path()).join(file),
            id,
            None,
        );
    }
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![
                activated("student", Role::Picker, DownloadScope::All, &hash),
                activated("erik", Role::Operator, DownloadScope::All, &hash),
            ],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
    overrides::update(project.documents(), |o| {
        o.radargrams.insert(
            crate::identity::RadargramId::new("line-02").unwrap(),
            RadargramOverride {
                unlisted: true,
                ..Default::default()
            },
        );
        Ok(())
    })
    .unwrap();
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

    let erik = sign_in(&app, "erik").await;
    let seen = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let warnings = seen.body["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("line-02")),
        "the operator is told: {warnings:?}"
    );

    let student = sign_in(&app, "student").await;
    let seen = get(&app, "/api/v1/datasets", Some(&student)).await;
    let warnings = seen.body["warnings"].as_array().unwrap();
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("line-02")),
        "the picker must not learn it exists: {warnings:?}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_radargram_from_an_external_root_says_it_is_not_in_the_project() {
    // What the difference means to a user: one in the project can be
    // removed, one in an archive can only be ignored, because Ridal never
    // writes outside the project. The UI has to offer different words for
    // those, and this is what it asks.
    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let archive = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    super::interp_routes_tests::write_test_nc_with_axes(
        &radargrams(dir.path()).join("ours.nc"),
        "ours",
        None,
    );
    super::interp_routes_tests::write_test_nc_with_axes(
        &archive.path().join("theirs.nc"),
        "theirs",
        None,
    );
    std::fs::write(
        dir.path().join("ridal.toml"),
        format!(
            "[project]\nname = \"test\"\n\n[radargrams]\nroots = [\"radargrams\", \"{}\"]\n",
            archive.path().display()
        ),
    )
    .unwrap();

    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
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
    let erik = sign_in(&app, "erik").await;

    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let entries = listing.body["entries"].as_array().unwrap();
    let by_id = |id: &str| {
        entries
            .iter()
            .find(|e| e["radargram_id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from {entries:?}"))
            .clone()
    };
    assert_eq!(
        by_id("ours")["in_project"],
        true,
        "the project's own is writable"
    );
    assert_eq!(
        by_id("theirs")["in_project"],
        false,
        "the archive's is served and never written to"
    );
}

/// A writable project with one radargram in it and one in an external
/// archive, plus the given accounts. Returns both directories so a test can
/// check what actually happened on disk.
fn lifecycle_app(users: Vec<User>) -> (tempfile::TempDir, tempfile::TempDir, Router) {
    lifecycle_app_with_cap(users, None)
}

/// `max_bytes` has to be in `ridal.toml` before the project is opened: the
/// config is read once and cached, so a test that edits the file afterwards
/// is testing the default.
fn lifecycle_app_with_cap(
    users: Vec<User>,
    max_bytes: Option<u64>,
) -> (tempfile::TempDir, tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let archive = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    super::interp_routes_tests::write_test_nc_with_axes(
        &radargrams(dir.path()).join("ours.nc"),
        "ours",
        None,
    );
    super::interp_routes_tests::write_test_nc_with_axes(
        &archive.path().join("theirs.nc"),
        "theirs",
        None,
    );
    std::fs::write(
        dir.path().join("ridal.toml"),
        format!(
            "[project]\nname = \"test\"\nformat_version = 1\n\n[radargrams]\n\
             roots = [\"{}/{}\", \"{}\"]\n{}",
            crate::project::DEFAULT_DATA_DIR,
            crate::project::DEFAULT_RADARGRAM_DIR,
            archive.path().display(),
            max_bytes
                .map(|n| format!("max_bytes = {n}\n"))
                .unwrap_or_default(),
        ),
    )
    .unwrap();

    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users,
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
    let state = Arc::new(
        AppState::build_with_project(
            dir.path(),
            &RenderServiceConfig::default(),
            Some(project),
            AccessOptions::default(),
        )
        .unwrap(),
    );
    (dir, archive, build_router(state))
}

async fn post_bytes(app: &Router, uri: &str, body: Vec<u8>, session: Option<&str>) -> Response {
    send(
        app,
        request("POST", uri, session)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_uploaded_radargram_appears_without_a_restart() {
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    // A real processed radargram, built the way every other fixture is.
    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("new.nc");
    super::interp_routes_tests::write_test_nc_with_axes(&source, "arrived", None);
    let bytes = std::fs::read(&source).unwrap();

    let response = post_bytes(&app, "/api/v1/datasets?filename=new.nc", bytes, Some(&erik)).await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text);
    assert_eq!(response.body["radargram_id"], "arrived");

    // In the catalog immediately, and on disk under its id rather than the
    // name the client happened to use.
    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let ids: Vec<&str> = listing.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["radargram_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"arrived"), "{ids:?}");
    assert!(radargrams(dir.path()).join("arrived.nc").exists());
    assert!(!radargrams(dir.path()).join("new.nc").exists());

    // And it is recorded.
    let log: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data(dir.path()).join("audit.json")).unwrap(),
    )
    .unwrap();
    let entries = log["entries"].as_array().unwrap();
    assert_eq!(entries.last().unwrap()["action"], "added");
    assert_eq!(entries.last().unwrap()["user"], "erik");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_upload_that_is_not_a_ridal_radargram_leaves_nothing_behind() {
    // Every refusal happens after the file exists, so the one thing that
    // must always hold is that nothing is left in the radargram directory.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let response = post_bytes(
        &app,
        "/api/v1/datasets?filename=notes.txt",
        b"this is not a NetCDF file".to_vec(),
        Some(&erik),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert_eq!(response.body["error"]["code"], "not_a_radargram");

    let left: Vec<String> = std::fs::read_dir(radargrams(dir.path()))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(left, vec!["ours.nc"], "no temporary file survived");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_upload_of_a_legacy_ridal_file_is_refused_with_a_reprocess_message() {
    // Distinct from the generic "not a ridal radargram" case (#167): an old
    // ridal file is recognizable as ridal output, so the refusal should say
    // so and point at reprocessing, not just "no radargram id".
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("old.nc");
    super::interp_routes_tests::write_legacy_test_nc(&source, "ridal version 0.5.1 by test");
    let bytes = std::fs::read(&source).unwrap();

    let response = post_bytes(&app, "/api/v1/datasets?filename=old.nc", bytes, Some(&erik)).await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert_eq!(response.body["error"]["code"], "ridal_file_too_old");
    let message = response.body["error"]["message"].as_str().unwrap();
    assert!(message.contains("old.nc"), "{message}");
    assert!(message.contains("0.5.1"), "{message}");
    assert!(message.contains("ridal process"), "{message}");

    let left: Vec<String> = std::fs::read_dir(radargrams(dir.path()))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(left, vec!["ours.nc"], "no temporary file survived");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_upload_of_a_valid_but_unrelated_netcdf_is_refused_as_not_ridal() {
    // Distinct from `an_upload_that_is_not_a_ridal_radargram_leaves_nothing_behind`
    // (unparsable bytes, refused before inspection even succeeds) and from
    // the legacy case above: this file parses fine as NetCDF and still has
    // none of ridal's attributes at all, so it should land on the plain
    // "not one Ridal processed" answer (#167).
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("unrelated.nc");
    super::interp_routes_tests::write_unrelated_test_nc(&source);
    let bytes = std::fs::read(&source).unwrap();

    let response = post_bytes(
        &app,
        "/api/v1/datasets?filename=unrelated.nc",
        bytes,
        Some(&erik),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert_eq!(response.body["error"]["code"], "not_a_ridal_radargram");

    let left: Vec<String> = std::fs::read_dir(radargrams(dir.path()))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(left, vec!["ours.nc"], "no temporary file survived");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_upload_colliding_with_an_existing_id_is_refused() {
    // Refused while the operator is standing there and can rename it,
    // rather than left to become a duplicate-id warning later.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("again.nc");
    super::interp_routes_tests::write_test_nc_with_axes(&source, "ours", None);
    let bytes = std::fs::read(&source).unwrap();

    let response = post_bytes(&app, "/api/v1/datasets", bytes, Some(&erik)).await;
    assert_eq!(response.status, StatusCode::CONFLICT, "{}", response.text);
    assert_eq!(response.body["error"]["code"], "radargram_exists");

    let left: Vec<String> = std::fs::read_dir(radargrams(dir.path()))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(left, vec!["ours.nc"], "the existing one is untouched");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn removing_a_project_radargram_deletes_the_file_and_keeps_the_picks() {
    // The hazard is not wasted space: it is a different file arriving later
    // under the same id and orphaned picks reattaching to data they were
    // never drawn on.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let picks = data(dir.path()).join("interpretations/ours");
    std::fs::create_dir_all(&picks).unwrap();
    std::fs::write(
        picks.join("erik.gprinterp.json"),
        r#"{"schema":"gprinterp","key":"ours","features":[]}"#,
    )
    .unwrap();

    let response = delete(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    assert_eq!(response.body["outcome"], "removed");
    assert_eq!(response.body["archived"], 1);

    assert!(!radargrams(dir.path()).join("ours.nc").exists());
    assert!(
        !picks.join("erik.gprinterp.json").exists(),
        "nothing is left to reattach"
    );
    let archived = data(dir.path()).join("interpretations/_archived/ours");
    assert!(archived.exists(), "and nothing authored was destroyed");

    // Gone from the catalog without a restart.
    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let ids: Vec<&str> = listing.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["radargram_id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"ours"), "{ids:?}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn removing_an_external_radargram_ignores_it_and_leaves_the_file_alone() {
    // Ridal never writes outside the project. The most "remove" can mean
    // there is "stop serving it", and the response says so rather than
    // implying the file is gone.
    let hash = users::hash_password(password()).unwrap();
    let (dir, archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let response = delete(&app, "/api/v1/datasets/theirs", Some(&erik)).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    assert_eq!(response.body["outcome"], "ignored");
    assert!(
        archive.path().join("theirs.nc").exists(),
        "the archive is not Ridal's to change"
    );

    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let ids: Vec<&str> = listing.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["radargram_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["ours"]);

    // Listed as ignored rather than simply absent, and restorable.
    let ignored = get(&app, "/api/v1/catalog/ignored", Some(&erik)).await;
    assert_eq!(ignored.body["ignored"][0]["radargram_id"], "theirs");
    assert!(ignored.body["vestigial"].as_array().unwrap().is_empty());

    let restored = post(
        &app,
        "/api/v1/datasets/theirs/restore",
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT, "{}", restored.text);
    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    assert_eq!(listing.body["entries"].as_array().unwrap().len(), 2);

    let _ = dir;
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_upload_past_the_project_cap_is_refused_before_it_is_written() {
    let hash = users::hash_password(password()).unwrap();
    // A cap the project has already passed, so there is no room at all.
    let (dir, _archive, app) = lifecycle_app_with_cap(
        vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
        Some(1024),
    );
    let erik = sign_in(&app, "erik").await;

    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("big.nc");
    super::interp_routes_tests::write_test_nc_with_axes(&source, "big", None);
    let bytes = std::fs::read(&source).unwrap();

    let response = post_bytes(&app, "/api/v1/datasets", bytes, Some(&erik)).await;
    assert_eq!(
        response.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        response.text
    );
    assert_eq!(response.body["error"]["code"], "project_full");
    assert!(
        response.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("max_bytes"),
        "the message says how to fix it: {}",
        response.body["error"]["message"]
    );
    assert!(!radargrams(dir.path()).join("big.nc").exists());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_picker_can_neither_add_nor_remove() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, _archive, app) = lifecycle_app(vec![activated(
        "student",
        Role::Picker,
        DownloadScope::All,
        &hash,
    )]);
    let student = sign_in(&app, "student").await;

    assert_eq!(
        post_bytes(
            &app,
            "/api/v1/datasets",
            b"anything".to_vec(),
            Some(&student)
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        delete(&app, "/api/v1/datasets/ours", Some(&student))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(
            &app,
            "/api/v1/datasets/ours/restore",
            &json!({}),
            Some(&student)
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn serving_one_file_inside_a_project_still_knows_it_is_the_project() {
    // `Project::discover` searches upwards, so `ridal gui project/radargrams`
    // is a supported way to start. Deciding writability from the path the
    // CLI was given then marked the project's *own* directory read-only,
    // and every radargram in it reported `in_project: false` -- which is
    // the flag the UI uses to choose between deleting a file and merely
    // not serving it.
    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    super::interp_routes_tests::write_test_nc_with_axes(
        &radargrams(dir.path()).join("ours.nc"),
        "ours",
        None,
    );
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();

    // A second radargram, and the server pointed at the *first file*
    // rather than a directory -- which `Project::discover` supports, since
    // it searches upwards for the marker.
    super::interp_routes_tests::write_test_nc_with_axes(
        &radargrams(dir.path()).join("theirs.nc"),
        "theirs",
        None,
    );
    let inside = radargrams(dir.path()).join("ours.nc");
    let project = Project::discover(&inside).unwrap().unwrap();
    let state = Arc::new(
        AppState::build_with_project(
            &inside,
            &RenderServiceConfig::default(),
            Some(project),
            AccessOptions::default(),
        )
        .unwrap(),
    );
    let app = build_router(state);
    let erik = sign_in(&app, "erik").await;

    let listing = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let entries = listing.body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "both are found: {entries:?}");
    for entry in entries {
        assert_eq!(
            entry["in_project"], true,
            "{} is in the project: {entry:?}",
            entry["radargram_id"]
        );
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_upload_of_an_ignored_id_is_refused_rather_than_installed_and_hidden() {
    // `find_entry` only sees what is served, so an ignored id looked free.
    // The upload was accepted, installed, and then hidden again by the very
    // ignore that made the id look available: 201 Created for a radargram
    // that never appeared.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    // Ignore the external one, then try to add a project file under its id.
    let removed = delete(&app, "/api/v1/datasets/theirs", Some(&erik)).await;
    assert_eq!(removed.body["outcome"], "ignored");

    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("again.nc");
    super::interp_routes_tests::write_test_nc_with_axes(&source, "theirs", None);
    let bytes = std::fs::read(&source).unwrap();

    let response = post_bytes(&app, "/api/v1/datasets", bytes, Some(&erik)).await;
    assert_eq!(response.status, StatusCode::CONFLICT, "{}", response.text);
    assert_eq!(response.body["error"]["code"], "radargram_ignored");
    assert!(
        !radargrams(dir.path()).join("theirs.nc").exists(),
        "nothing was installed"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_interrupted_upload_leaves_no_temporary_file_behind() {
    // Every refusal happens once the file exists, and so does a failure
    // *during* the transfer. The guard used to be built from the stream's
    // result, so an oversized body returned through `?` while the temporary
    // was still on disk -- and with a `.nc` suffix it would then have been
    // discovered as a radargram.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app_with_cap(
        vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
        Some(1024),
    );
    let erik = sign_in(&app, "erik").await;

    let response = post_bytes(&app, "/api/v1/datasets", vec![0u8; 64 * 1024], Some(&erik)).await;
    assert_eq!(
        response.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        response.text
    );

    let left: Vec<String> = std::fs::read_dir(radargrams(dir.path()))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(left, vec!["ours.nc"], "no .tmp.nc survived: {left:?}");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_upload_will_not_follow_a_symlinked_radargram_directory_out_of_the_project() {
    // Ridal does not write outside the project. Without a containment
    // check, `create_dir_all` and `rename` follow a replaced `radargrams`
    // and the upload lands in someone else's archive.
    #[cfg(unix)]
    {
        let hash = users::hash_password(password()).unwrap();
        let (dir, _archive, app) = lifecycle_app(vec![activated(
            "erik",
            Role::Operator,
            DownloadScope::All,
            &hash,
        )]);
        let erik = sign_in(&app, "erik").await;

        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::remove_dir_all(radargrams(dir.path())).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), radargrams(dir.path())).unwrap();

        let staging = tempfile::tempdir().unwrap();
        let source = staging.path().join("new.nc");
        super::interp_routes_tests::write_test_nc_with_axes(&source, "arrived", None);
        let bytes = std::fs::read(&source).unwrap();

        let response = post_bytes(&app, "/api/v1/datasets", bytes, Some(&erik)).await;
        assert_eq!(response.status, StatusCode::CONFLICT, "{}", response.text);
        assert_eq!(
            response.body["error"]["code"],
            "destination_outside_project"
        );
        assert_eq!(
            std::fs::read_dir(elsewhere.path()).unwrap().count(),
            0,
            "nothing was written outside the project"
        );
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn removing_a_radargram_keeps_its_axes_so_the_picks_stay_carryable() {
    // The file goes and the mapping stays. That is what lets a document
    // drawn on it be carried onto a later revision of the same id -- and
    // the reason the snapshot is taken unconditionally rather than only
    // when something references the revision: someone with the viewer open
    // has not saved yet, and their PUT arrives after the file is gone.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let before = get(&app, "/api/v1/datasets/ours/revisions", Some(&erik)).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text);
    let revisions = before.body["revisions"].as_array().unwrap();
    // Discovery registers the current revision, so there is a record --
    // but nothing has superseded anything, and there are no axes kept yet.
    assert_eq!(revisions.len(), 1, "{revisions:?}");
    assert_eq!(revisions[0]["current"], true);
    assert!(revisions[0]["superseded_at"].is_null());
    assert_eq!(revisions[0]["has_axes"], false);

    let removed = delete(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
    assert!(!radargrams(dir.path()).join("ours.nc").exists());

    let after = get(&app, "/api/v1/datasets/ours/revisions", Some(&erik)).await;
    let revisions = after.body["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 1, "{revisions:?}");
    let record = &revisions[0];
    assert_eq!(record["has_axes"], true, "the mapping outlived the file");
    assert_eq!(record["current"], false, "nothing is current now");
    assert!(record["superseded_at"].is_string());
    assert!(
        record["superseded_by"].is_null(),
        "a removal is a supersession with nothing on the other side"
    );
    assert_eq!(record["y_anchor"], "twtt");
    assert_eq!(record["n_traces"], 40);
    assert_eq!(record["n_samples"], 8);

    // And the snapshot is on disk, tiny, next to the ledger.
    let axes = data(dir.path()).join("revisions/ours");
    let kept: Vec<_> = std::fs::read_dir(&axes)
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    assert_eq!(kept.len(), 1);
    assert!(
        kept[0].metadata().unwrap().len() < 2048,
        "a few hundred bytes standing in for the file"
    );
    assert!(data(dir.path()).join("revisions.json").exists());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_viewer_can_read_the_history_but_not_change_it() {
    // Which revisions a radargram has had is provenance about data people
    // are already being shown, not an operator's working notes.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, _archive, app) = lifecycle_app(vec![
        activated("reader", Role::Viewer, DownloadScope::None, &hash),
        activated("erik", Role::Operator, DownloadScope::All, &hash),
    ]);
    let reader = sign_in(&app, "reader").await;

    assert_eq!(
        get(&app, "/api/v1/datasets/ours/revisions", Some(&reader))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        delete(&app, "/api/v1/datasets/ours", Some(&reader))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_radargram_that_has_never_been_removed_still_has_a_current_revision() {
    // The ledger used to learn about a radargram only when one was
    // removed, so `/revisions` had nothing to say about a radargram that
    // had simply always been there -- and a later supersession had no
    // earlier record to compare its checksum against.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let listed = get(&app, "/api/v1/datasets/ours/revisions", Some(&erik)).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    let revisions = listed.body["revisions"].as_array().unwrap();
    assert_eq!(
        revisions.len(),
        1,
        "discovery registered it: {}",
        listed.text
    );
    assert_eq!(revisions[0]["current"], true);
    assert!(revisions[0]["superseded_at"].is_null());
    assert_eq!(
        revisions[0]["has_axes"], false,
        "no snapshot yet: nothing has superseded it"
    );
    assert!(data(dir.path()).join("revisions.json").exists());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn restoring_an_ignored_radargram_stops_its_history_saying_it_is_gone() {
    // An ignore supersedes the revision. Lifting it makes that revision
    // current again, and a record still marked superseded would have the
    // history contradict the catalog that is serving it.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let removed = delete(&app, "/api/v1/datasets/theirs", Some(&erik)).await;
    assert_eq!(removed.body["outcome"], "ignored");

    let gone = get(&app, "/api/v1/datasets/theirs/revisions", Some(&erik)).await;
    let while_ignored = gone.body["revisions"].as_array().unwrap();
    assert_eq!(while_ignored.len(), 1, "{}", gone.text);
    assert_eq!(while_ignored[0]["current"], false, "{}", gone.text);
    assert!(while_ignored[0]["superseded_at"].is_string());

    let restored = post(
        &app,
        "/api/v1/datasets/theirs/restore",
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT, "{}", restored.text);

    let back = get(&app, "/api/v1/datasets/theirs/revisions", Some(&erik)).await;
    let after = back.body["revisions"].as_array().unwrap();
    assert_eq!(after.len(), 1, "{}", back.text);
    assert_eq!(after[0]["current"], true, "current again: {}", back.text);
    assert!(
        after[0]["superseded_at"].is_null(),
        "and no longer says it is gone: {}",
        back.text
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_removal_is_refused_when_the_axes_it_declares_cannot_be_kept() {
    // The snapshot is not a *record* of the removal, the way the audit log
    // is. It is the only copy of the mapping once the file is gone, and
    // without it no document drawn on this revision can be carried onto a
    // later one. A full disk or an unwritable `revisions/` therefore has to
    // stop the deletion rather than warn past it.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    // Standing in for the write failing. A file where the radargram's
    // snapshot directory belongs makes creating it fail with `ENOTDIR`,
    // which -- unlike a permission bit -- also holds when the tests run as
    // root, as they do in the container.
    let revisions = data(dir.path()).join("revisions");
    std::fs::create_dir_all(&revisions).unwrap();
    std::fs::write(revisions.join("ours"), b"not a directory").unwrap();

    let refused = delete(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    assert_eq!(
        refused.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        refused.text
    );
    assert_eq!(refused.body["error"]["code"], "snapshot_failed");
    assert!(
        radargrams(dir.path()).join("ours.nc").exists(),
        "the file is still there, which is the whole point"
    );

    // And once it can be written, the same removal goes through.
    std::fs::remove_file(revisions.join("ours")).unwrap();
    let removed = delete(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text);
    assert!(!radargrams(dir.path()).join("ours.nc").exists());
}

/// The bytes of a radargram processed with the given id, for an upload.
///
/// A later processing datetime than the fixtures `lifecycle_app` installs,
/// so this really is a *new revision* of that radargram: the revision id is
/// `hash(radargram_id + processing_datetime)`, and reusing the datetime
/// would make it the same revision wearing different bytes.
fn staged_bytes(id: &str) -> Vec<u8> {
    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("new.nc");
    super::interp_routes_tests::write_test_nc_with_axes_at(
        &source,
        id,
        None,
        "2026-06-01T00:00:00Z",
    );
    std::fs::read(&source).unwrap()
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_staged_replacement_is_not_served_as_a_radargram() {
    // A staged file is a `.nc` carrying the *same* radargram id as the one
    // it would replace, sitting inside the project. Discovery walks the
    // whole project tree, so without the dot-prefixed staging directory it
    // would be catalogued immediately as a duplicate of its own target --
    // and the operator would be told their id already exists, by the file
    // they just uploaded.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let response = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&erik),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    assert!(response.body["token"].as_str().is_some());

    // On disk, under the project's data directory rather than loose in the
    // project root (#187), and invisible to the catalog.
    assert!(data(dir.path()).join(".staging").is_dir());
    assert!(!dir.path().join(".staging").exists());
    assert_eq!(staged_count(dir.path()), 1, "the upload is staged");
    let listed = get(&app, "/api/v1/datasets", Some(&erik)).await;
    let ids: Vec<&str> = listed.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["radargram_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["ours", "theirs"], "no duplicate: {}", listed.text);
    assert!(
        listed.body["warnings"].as_array().unwrap().is_empty(),
        "and no duplicate-id warning either: {}",
        listed.text
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn replacing_a_radargram_leaves_every_pick_exactly_as_drawn() {
    // #148's central rule. Re-anchoring is approximate, so writing it back
    // would launder an approximation into ground truth; it compounds on the
    // next replace; and it cannot be undone. The file changes and the
    // documents do not.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let before = get(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    let from_revision = before.body["revision_id"].as_str().unwrap().to_string();

    let document = json!({
        "key": "ours",
        "source": {"radargram_id": "ours", "revision_id": from_revision},
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [30.0, 3.0]]},
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    });
    let saved = put(
        &app,
        "/api/v1/datasets/ours/interpretations/erik",
        &document,
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.text);

    let staged = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&erik),
    )
    .await;
    assert_eq!(staged.status, StatusCode::OK, "{}", staged.text);
    let token = staged.body["token"].as_str().unwrap().to_string();

    let done = post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);

    // The document on disk is untouched, down to the coordinates and the
    // revision it declares.
    let stored: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data(dir.path()).join("interpretations/ours/erik.gprinterp.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored["source"]["revision_id"], done.body["from_revision"]);
    assert_eq!(
        stored["features"][0]["geometry"]["coordinates"],
        json!([[5.0, 2.0], [30.0, 3.0]]),
        "not rewritten"
    );

    // And the history says what happened, with the old mapping kept so the
    // picks can still be placed.
    let history = get(&app, "/api/v1/datasets/ours/revisions", Some(&erik)).await;
    let revisions = history.body["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 2, "{}", history.text);
    let old = revisions
        .iter()
        .find(|r| r["revision_id"] == done.body["from_revision"])
        .unwrap();
    assert_eq!(old["superseded_by"], done.body["to_revision"]);
    assert_eq!(old["has_axes"], true, "the mapping outlived the file");
    assert_eq!(old["current"], false);

    // Nothing is left staged.
    assert_eq!(staged_count(dir.path()), 0);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_replacement_for_a_different_radargram_is_refused() {
    // Without this, replacing `ours` with a file whose id is something else
    // installs it at `ours.nc`, and the catalog then disagrees with the
    // file about what it is.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let response = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("somewhere-else"),
        Some(&erik),
    )
    .await;
    assert_eq!(response.status, StatusCode::CONFLICT, "{}", response.text);
    assert_eq!(response.body["error"]["code"], "wrong_radargram");
    assert_eq!(
        staged_count(dir.path()),
        0,
        "a refused upload leaves nothing behind"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_replacement_that_is_a_legacy_ridal_file_is_refused_with_a_reprocess_message() {
    // Same distinction as the upload route (#167): an old ridal file gets a
    // specific "reprocess it" answer, not the generic not-ridal message.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("old.nc");
    super::interp_routes_tests::write_legacy_test_nc(&source, "ridal version 0.5.1 by test");
    let bytes = std::fs::read(&source).unwrap();

    let response = post_bytes(&app, "/api/v1/datasets/ours/replace", bytes, Some(&erik)).await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert_eq!(response.body["error"]["code"], "ridal_file_too_old");
    let message = response.body["error"]["message"].as_str().unwrap();
    assert!(message.contains("0.5.1"), "{message}");
    assert!(message.contains("ridal process"), "{message}");
    assert_eq!(
        staged_count(dir.path()),
        0,
        "a refused upload leaves nothing behind"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_replacement_that_is_a_valid_but_unrelated_netcdf_is_refused_as_not_ridal() {
    // Same distinction as the upload route's equivalent test (#167): a file
    // that parses fine as NetCDF but has none of ridal's attributes at all
    // should land on the plain "not one Ridal processed" answer.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("unrelated.nc");
    super::interp_routes_tests::write_unrelated_test_nc(&source);
    let bytes = std::fs::read(&source).unwrap();

    let response = post_bytes(&app, "/api/v1/datasets/ours/replace", bytes, Some(&erik)).await;
    assert_eq!(
        response.status,
        StatusCode::BAD_REQUEST,
        "{}",
        response.text
    );
    assert_eq!(response.body["error"]["code"], "not_a_ridal_radargram");
    assert_eq!(
        staged_count(dir.path()),
        0,
        "a refused upload leaves nothing behind"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_radargram_outside_the_project_cannot_be_replaced() {
    // Ridal never writes outside the project, so the honest answer is a
    // refusal rather than a replace that lands somewhere else.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let response = post_bytes(
        &app,
        "/api/v1/datasets/theirs/replace",
        staged_bytes("theirs"),
        Some(&erik),
    )
    .await;
    assert_eq!(response.status, StatusCode::CONFLICT, "{}", response.text);
    assert_eq!(response.body["error"]["code"], "not_in_project");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_discarded_replacement_is_given_back() {
    // An abandoned dialog should not leave a radargram-sized file in the
    // project. The sweep catches it eventually; this is the immediate path.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let staged = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&erik),
    )
    .await;
    let token = staged.body["token"].as_str().unwrap().to_string();
    assert_eq!(staged_count(dir.path()), 1);

    let discarded = delete(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        Some(&erik),
    )
    .await;
    assert_eq!(discarded.status, StatusCode::NO_CONTENT);
    assert_eq!(staged_count(dir.path()), 0);

    // And the radargram is still the one it was.
    let listed = get(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn only_an_operator_may_replace_a_radargram() {
    let hash = users::hash_password(password()).unwrap();
    let (_dir, _archive, app) = lifecycle_app(vec![
        activated("student", Role::Picker, DownloadScope::All, &hash),
        activated("erik", Role::Operator, DownloadScope::All, &hash),
    ]);
    let student = sign_in(&app, "student").await;

    let response = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&student),
    )
    .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN, "{}", response.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_replacement_that_cannot_be_told_apart_is_refused_by_the_server() {
    // `RevisionId` is hash(radargram_id + processing_datetime) and says
    // nothing about contents, so a file edited in another tool can carry
    // the same datetime and land on the same id. Every cache and staleness
    // check would then believe nothing changed.
    //
    // The dialog disables its button for this, but a disabled button is a
    // courtesy and committing deletes the old file. Refused here too.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    // The same processing datetime as the installed fixture.
    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("same-date.nc");
    super::interp_routes_tests::write_test_nc_with_axes_at(
        &source,
        "ours",
        None,
        "2020-01-01T00:00:00Z",
    );
    let bytes = std::fs::read(&source).unwrap();

    // Staging says so rather than refusing: the operator is entitled to see
    // the report, and this is the row that explains the refusal.
    let staged = post_bytes(&app, "/api/v1/datasets/ours/replace", bytes, Some(&erik)).await;
    assert_eq!(staged.status, StatusCode::OK, "{}", staged.text);
    assert_eq!(staged.body["report"]["revision_id_collision"], true);
    let token = staged.body["token"].as_str().unwrap().to_string();

    // Committing does not.
    let refused = post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text);
    assert_eq!(refused.body["error"]["code"], "revision_id_collision");

    // And the radargram is untouched: same revision, same file.
    let still = get(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    assert_eq!(still.status, StatusCode::OK);
    assert!(radargrams(dir.path()).join("ours.nc").exists());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_uncorrected_revision_can_be_replaced() {
    // Reported: going from a zero-corrected revision to an uncorrected one
    // worked, and going back did not. Replacing the uncorrected one was
    // refused outright -- "its mapping cannot be kept, every pick drawn on
    // it would be impossible to place on anything ever again" -- because
    // the snapshot only ever looked for a travel-time axis, and an
    // uncorrected revision has none.
    //
    // It has a mapping: the original recording's clock. The snapshot keeps
    // whichever anchor the revision offers, and names it, because a
    // corrected and an uncorrected revision can carry numerically similar
    // values meaning entirely different things.
    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    // The radargram being replaced has never had a zero correction.
    super::interp_routes_tests::write_test_nc_uncorrected(
        &radargrams(dir.path()).join("ours.nc"),
        "ours",
        "2026-01-01T00:00:00Z",
    );
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(
        project.documents(),
        &UserSet {
            users: vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
            ..UserSet::default()
        },
        &Expectation::Any,
    )
    .unwrap();
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
    let erik = sign_in(&app, "erik").await;

    // A corrected revision replacing it: the two share no travel-time
    // axis, and relate through the recording clock.
    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("corrected.nc");
    super::interp_routes_tests::write_test_nc_with_axes_at(
        &source,
        "ours",
        None,
        "2026-06-01T00:00:00Z",
    );

    let staged = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        std::fs::read(&source).unwrap(),
        Some(&erik),
    )
    .await;
    assert_eq!(staged.status, StatusCode::OK, "{}", staged.text);
    assert_eq!(
        staged.body["report"]["outgoing_axes_kept"], true,
        "the recording clock is a mapping: {}",
        staged.text
    );

    let token = staged.body["token"].as_str().unwrap().to_string();
    let done = post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.text);

    // And the outgoing revision's mapping really was kept, under the name
    // that says what it is.
    let history = get(&app, "/api/v1/datasets/ours/revisions", Some(&erik)).await;
    let old = history.body["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["revision_id"] == done.body["from_revision"])
        .expect("the superseded revision");
    assert_eq!(old["has_axes"], true, "{}", history.text);
    assert_eq!(
        old["y_anchor"], "recording_time",
        "named, so it cannot be mistaken for a travel time: {}",
        history.text
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn carried_picks_can_be_adopted_onto_the_current_revision() {
    // "I have looked at these on the current version, they are in the right
    // place, and they are now my picks on it." #148 argues against
    // *migrating* interpretations and every word holds -- but that argument
    // is about something happening automatically to everybody's picks. This
    // is one person, one document, having looked.
    //
    // The three objections are answered rather than ignored: the document
    // records that it was carried, records what from, and the version as
    // drawn is archived first.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let before = get(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    let from_revision = before.body["revision_id"].as_str().unwrap().to_string();

    // The axes the viewer hands the picker, taken from the page rather
    // than rebuilt here: without them a document cannot be carried at all,
    // which is the whole subject of this test.
    let page = get(&app, "/view/ours", Some(&erik)).await;
    let line = super::interp_routes_tests::axes_line(&page.text).expect("the page offers axes");
    let axes: serde_json::Value =
        serde_json::from_str(line.trim_start_matches("axes: ").trim_end_matches(','))
            .expect("valid JSON");

    let document = json!({
        "key": "ours",
        "source": {"radargram_id": "ours", "revision_id": from_revision},
        "coordinates": {"space": "index", "axes": axes},
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [30.0, 3.0]]},
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    });
    let saved = put(
        &app,
        "/api/v1/datasets/ours/interpretations/erik",
        &document,
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.text);

    // Replace it, so the picks are now on a superseded revision.
    let staged = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&erik),
    )
    .await;
    let token = staged.body["token"].as_str().unwrap().to_string();
    let replaced = post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.text);
    let to_revision = replaced.body["to_revision"].as_str().unwrap().to_string();

    // What the page is showing: the carry, unedited.
    let carried = get(
        &app,
        "/api/v1/datasets/ours/interpretations/erik/carried",
        Some(&erik),
    )
    .await;
    let shown = carried.body["document"].clone();
    let adopted = post(
        &app,
        &format!("/api/v1/datasets/ours/interpretations/erik/promote?onto={to_revision}"),
        &shown,
        Some(&erik),
    )
    .await;
    assert_eq!(adopted.status, StatusCode::OK, "{}", adopted.text);

    // The stored document now belongs to this revision, and says how.
    let stored: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data(dir.path()).join("interpretations/ours/erik.gprinterp.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored["source"]["revision_id"], to_revision);
    let carried = &stored["meta"]["ridal_carried_from"];
    assert_eq!(
        carried["from_revision"], replaced.body["from_revision"],
        "the artefact says what it was carried from, so a chain is visible: {stored}"
    );
    assert!(
        carried["note"].as_str().unwrap().contains("never exact"),
        "and that the coordinates were derived rather than drawn"
    );

    // And the version as drawn is archived, which is what makes this
    // reversible and therefore defensible at all.
    let archived = data(dir.path()).join("interpretations/_archived/ours");
    let mut found = Vec::new();
    for removal in std::fs::read_dir(&archived).unwrap() {
        for file in std::fs::read_dir(removal.unwrap().path()).unwrap() {
            found.push(file.unwrap().path());
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    let original: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&found[0]).unwrap()).unwrap();
    assert_eq!(
        original["source"]["revision_id"], replaced.body["from_revision"],
        "the archived copy is the one as drawn"
    );
    assert_eq!(
        original["features"][0]["geometry"]["coordinates"],
        json!([[5.0, 2.0], [30.0, 3.0]])
    );

    assert_eq!(
        carried_meta_edited(&stored),
        false,
        "nothing was adjusted on top: {stored}"
    );

    // Adopting twice is refused: there is nothing left to adopt.
    let again = post(
        &app,
        &format!("/api/v1/datasets/ours/interpretations/erik/promote?onto={to_revision}"),
        &shown,
        Some(&erik),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.text);
    assert_eq!(again.body["error"]["code"], "already_current");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn nobody_may_adopt_someone_elses_picks() {
    // Interpretations belong to whoever drew them, and adopting writes to
    // one. Not even an admin, which is the same rule `writing_as` enforces
    // for saving -- a property of the data model rather than a permission.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, _archive, app) = lifecycle_app(vec![
        activated("erik", Role::Admin, DownloadScope::All, &hash),
        activated("student", Role::Picker, DownloadScope::All, &hash),
    ]);
    let erik = sign_in(&app, "erik").await;

    let response = post(
        &app,
        "/api/v1/datasets/ours/interpretations/student/promote?onto=whatever",
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN, "{}", response.text);
    assert_eq!(response.body["error"]["code"], "not_your_interpretation");
}

/// Whether an adopted document records that it was adjusted after carrying.
fn carried_meta_edited(stored: &serde_json::Value) -> bool {
    stored["meta"]["ridal_carried_from"]["edited_after_carry"]
        .as_bool()
        .unwrap_or(false)
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_edit_made_over_a_carried_view_survives_adopting() {
    // Reported: carry the picks, drag a vertex, press Adopt, and the edit
    // is gone. Adopting used to recompute the carry server-side and ignore
    // what the page sent, which threw away exactly the adjustment somebody
    // had just made.
    //
    // An edit made over a carried view is made by dragging a vertex across
    // *this* revision, so it is already in this revision's index space and
    // there is nothing to carry about it.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let before = get(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    let from_revision = before.body["revision_id"].as_str().unwrap().to_string();
    let page = get(&app, "/view/ours", Some(&erik)).await;
    let line = super::interp_routes_tests::axes_line(&page.text).expect("axes");
    let axes: serde_json::Value =
        serde_json::from_str(line.trim_start_matches("axes: ").trim_end_matches(',')).unwrap();

    let document = json!({
        "key": "ours",
        "source": {"radargram_id": "ours", "revision_id": from_revision},
        "coordinates": {"space": "index", "axes": axes},
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [30.0, 3.0]]},
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    });
    let saved = put(
        &app,
        "/api/v1/datasets/ours/interpretations/erik",
        &document,
        Some(&erik),
    )
    .await;
    assert_eq!(saved.status, StatusCode::CREATED, "{}", saved.text);

    let staged = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&erik),
    )
    .await;
    let token = staged.body["token"].as_str().unwrap().to_string();
    let replaced = post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        &json!({}),
        Some(&erik),
    )
    .await;
    let to_revision = replaced.body["to_revision"].as_str().unwrap().to_string();

    // What the page shows, then a vertex dragged: the second point moves.
    let carried = get(
        &app,
        "/api/v1/datasets/ours/interpretations/erik/carried",
        Some(&erik),
    )
    .await;
    let mut edited = carried.body["document"].clone();
    edited["features"][0]["geometry"]["coordinates"][1] = json!([31.0, 6.0]);

    let adopted = post(
        &app,
        &format!("/api/v1/datasets/ours/interpretations/erik/promote?onto={to_revision}"),
        &edited,
        Some(&erik),
    )
    .await;
    assert_eq!(adopted.status, StatusCode::OK, "{}", adopted.text);

    let stored: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data(dir.path()).join("interpretations/ours/erik.gprinterp.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        stored["features"][0]["geometry"]["coordinates"][1],
        json!([31.0, 6.0]),
        "the edit survived: {stored}"
    );
    assert_eq!(stored["source"]["revision_id"], to_revision);
    assert!(
        carried_meta_edited(&stored),
        "and the provenance says it was adjusted rather than carried straight: {stored}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn adopting_from_a_page_left_open_across_a_replace_is_refused() {
    // The one thing the page cannot be trusted about is which revision it
    // was looking at. A tab open across a replace would otherwise adopt
    // coordinates validated against a file that is no longer there, and
    // the provenance would record it as deliberate.
    let hash = users::hash_password(password()).unwrap();
    let (_dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let before = get(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    let stale = before.body["revision_id"].as_str().unwrap().to_string();

    let document = json!({
        "key": "ours",
        "source": {"radargram_id": "ours", "revision_id": stale},
        "features": [{
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [30.0, 3.0]]},
            "properties": {"id": "f-0001", "label": "bed"}
        }]
    });
    put(
        &app,
        "/api/v1/datasets/ours/interpretations/erik",
        &document,
        Some(&erik),
    )
    .await;

    let staged = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&erik),
    )
    .await;
    let token = staged.body["token"].as_str().unwrap().to_string();
    post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        &json!({}),
        Some(&erik),
    )
    .await;

    // The page still believes it is looking at the revision it loaded.
    let refused = post(
        &app,
        &format!("/api/v1/datasets/ours/interpretations/erik/promote?onto={stale}"),
        &document,
        Some(&erik),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text);
    assert_eq!(refused.body["error"]["code"], "stale_revision");
}

/// How many replacements are staged, ignoring the note beside each one.
fn staged_count(dir: &StdPath) -> usize {
    std::fs::read_dir(data(dir).join(".staging"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "nc"))
                .count()
        })
        .unwrap_or(0)
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_report_measured_against_a_superseded_revision_is_not_committed() {
    // A consequence report describes a transition: from the revision on
    // disk when it was made, to the one in the staged file. Committing
    // without checking the first half lets another replace landing in
    // between turn an approved A -> C into an unapproved B -> C, and the
    // operator read what would happen to picks drawn on A.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    // Stage one replacement and read its report, but do not commit.
    let first = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&erik),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    let stale_token = first.body["token"].as_str().unwrap().to_string();

    // Somebody else replaces the radargram in the meantime.
    let staging = tempfile::tempdir().unwrap();
    let source = staging.path().join("other.nc");
    super::interp_routes_tests::write_test_nc_with_axes_at(
        &source,
        "ours",
        None,
        "2026-07-01T00:00:00Z",
    );
    let second = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        std::fs::read(&source).unwrap(),
        Some(&erik),
    )
    .await;
    let token = second.body["token"].as_str().unwrap().to_string();
    let landed = post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(landed.status, StatusCode::OK, "{}", landed.text);

    // The first report now describes a change that is no longer the one
    // that would happen.
    let refused = post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{stale_token}"),
        &json!({}),
        Some(&erik),
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text);
    assert_eq!(refused.body["error"]["code"], "report_is_stale");

    // And what is served is still what the second replace installed.
    let now = get(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    assert_eq!(now.body["revision_id"], landed.body["to_revision"]);
    assert!(radargrams(dir.path()).join("ours.nc").exists());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn adopting_from_a_page_that_missed_a_save_is_refused() {
    // Adopting writes over a document. From a page that loaded before
    // another tab saved, it used to overwrite the newer version --
    // archived, but replaced without anybody being told, where an ordinary
    // save in the same position is refused with 412.
    let hash = users::hash_password(password()).unwrap();
    let (dir, _archive, app) = lifecycle_app(vec![activated(
        "erik",
        Role::Operator,
        DownloadScope::All,
        &hash,
    )]);
    let erik = sign_in(&app, "erik").await;

    let before = get(&app, "/api/v1/datasets/ours", Some(&erik)).await;
    let from_revision = before.body["revision_id"].as_str().unwrap().to_string();
    let page = get(&app, "/view/ours", Some(&erik)).await;
    let line = super::interp_routes_tests::axes_line(&page.text).expect("axes");
    let axes: serde_json::Value =
        serde_json::from_str(line.trim_start_matches("axes: ").trim_end_matches(',')).unwrap();
    let document = |points: serde_json::Value| {
        json!({
            "key": "ours",
            "source": {"radargram_id": "ours", "revision_id": from_revision},
            "coordinates": {"space": "index", "axes": axes},
            "features": [{
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": points},
                "properties": {"id": "f-0001", "label": "bed"}
            }]
        })
    };

    let saved = put(
        &app,
        "/api/v1/datasets/ours/interpretations/erik",
        &document(json!([[5.0, 2.0], [30.0, 3.0]])),
        Some(&erik),
    )
    .await;
    // The ETag this page would have been holding.
    let stale_etag = saved.etag.expect("an etag");

    // Another tab saves in the meantime.
    put(
        &app,
        "/api/v1/datasets/ours/interpretations/erik",
        &document(json!([[6.0, 2.0], [31.0, 3.0]])),
        Some(&erik),
    )
    .await;

    let staged = post_bytes(
        &app,
        "/api/v1/datasets/ours/replace",
        staged_bytes("ours"),
        Some(&erik),
    )
    .await;
    let token = staged.body["token"].as_str().unwrap().to_string();
    let replaced = post(
        &app,
        &format!("/api/v1/datasets/ours/replace/{token}"),
        &json!({}),
        Some(&erik),
    )
    .await;
    let to_revision = replaced.body["to_revision"].as_str().unwrap().to_string();

    let refused = post_with_if_match(
        &app,
        &format!("/api/v1/datasets/ours/interpretations/erik/promote?onto={to_revision}"),
        &document(json!([[5.0, 2.0], [30.0, 3.0]])),
        &stale_etag,
        Some(&erik),
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        refused.text
    );

    // And nothing was archived, because nothing was replaced.
    assert!(
        !data(dir.path())
            .join("interpretations/_archived/ours")
            .exists(),
        "a refused adoption leaves no stray archived copy"
    );
}
