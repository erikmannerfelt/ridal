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
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&dir.path().join("radargrams").join("line-01.nc"), RADARGRAM);
    let project = Project::discover(dir.path()).unwrap().unwrap();
    users::write(project.documents(), &set, &Expectation::Any).unwrap();

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

/// The parts of a response these tests assert on.
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
        !dir.path().join("preferences/student.json").exists(),
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
    write_test_nc(&dir.path().join("radargrams").join("line-01.nc"), RADARGRAM);

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
    write_test_nc(&dir.path().join("radargrams").join("line-01.nc"), RADARGRAM);
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
    assert!(!dir.path().join("users.json").exists());
    // And no session key either: a project that never authenticates never
    // grows one.
    assert!(!dir.path().join("session.key").exists());

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
async fn the_first_account_takes_effect_without_a_restart() {
    // What lets `ridal project user add` say there is nothing to restart.
    // The account file is read per request, so a server already serving an
    // open project becomes an authenticated one the moment the file
    // appears beneath it.
    let hash = users::hash_password(password()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    write_test_nc(&dir.path().join("radargrams").join("line-01.nc"), RADARGRAM);
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
    write_test_nc(&dir.path().join("radargrams").join("line-01.nc"), RADARGRAM);
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

    std::fs::write(dir.path().join("users.json"), "{ not json at all").unwrap();

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
    write_test_nc(&dir.path().join("radargrams").join("line-01.nc"), RADARGRAM);
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
    write_test_nc(&dir.path().join("radargrams").join("line-01.nc"), RADARGRAM);
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
    write_test_nc(&dir.path().join("radargrams").join("line-01.nc"), RADARGRAM);
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
            &dir.path().join("radargrams").join(format!("{id}.nc")),
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
    // The file put it in a group of its own -- here from its directory --
    // and the dialog still reports that, which is what makes the revert
    // offer meaningful rather than a leap of faith.
    assert_eq!(properties.body["from_file"]["group_id"], "radargrams");
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
    let stored = dir.path().join("interpretations/line-01");
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
            &dir.path().join("radargrams").join(file),
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
