//! HTTP-level tests for the multi-project site router (#214).
//!
//! Driven through the real Axum router with `ServiceExt::oneshot`, so the
//! delegation to each project's own router, the site session cookie and the
//! 404-for-non-members rule are exercised exactly as a browser would meet
//! them.
//!
//! None of these projects contains a radargram, so nothing here calls
//! netcdf-c and the `serial(netcdf)` guard is unnecessary: the routes under
//! test are about identity, routing and the membership file.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::app::AccessOptions;
use super::site::{build_site_router, SiteState};
use crate::identity::{ProjectKey, UserId};
use crate::project::members;
use crate::project::users::{DownloadScope, Role};
use crate::server::render_service::RenderServiceConfig;
use crate::site::accounts::{self, invite, Account};
use crate::site::Site;

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

fn key(name: &str) -> ProjectKey {
    ProjectKey::new(name).unwrap()
}

/// An activated account. The hash is passed in so a test with several
/// accounts pays for Argon2id once rather than once per person.
fn activated(name: &str, server_admin: bool, hash: &str) -> Account {
    let mut account = Account::new(id(name), server_admin);
    account.password_hash = Some(hash.to_string());
    account
}

/// A site with the given accounts and project keys, and its router.
///
/// Setup happens on a `Site` opened from the same directory the router will
/// use, so a test can keep a second handle for assertions on disk.
fn site_with(
    accounts_in: Vec<Account>,
    projects: &[&str],
    access: AccessOptions,
) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let site = Site::init(dir.path(), Some("Test site")).unwrap();
    if !accounts_in.is_empty() {
        accounts::update(site.store(), |set| {
            set.users = accounts_in.clone();
            Ok(())
        })
        .unwrap();
    }
    for name in projects {
        site.create_project(&key(name), Some(name)).unwrap();
    }
    let state = SiteState::new(site, access, RenderServiceConfig::default());
    (dir, build_site_router(state))
}

struct Response {
    status: StatusCode,
    cookie: Option<String>,
    location: Option<String>,
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
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    Response {
        status,
        cookie,
        location,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text,
    }
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::empty()).unwrap()
}

fn post_json(uri: &str, body: &Value, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn put_json(uri: &str, body: &Value, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

async fn sign_in(app: &Router, name: &str) -> String {
    let response = send(
        app,
        post_json(
            "/api/v1/auth/login",
            &json!({ "name": name, "password": password() }),
            None,
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    cookie_pair(&response.cookie.expect("a session cookie"))
}

#[tokio::test]
async fn the_landing_redirects_an_anonymous_visitor_to_login() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &[],
        AccessOptions::default(),
    );
    let response = send(&app, get("/", None)).await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    assert_eq!(response.location.as_deref(), Some("/login"));
}

#[tokio::test]
async fn signing_in_returns_a_session_and_me_reports_it() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &[],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let me = send(&app, get("/api/v1/auth/me", Some(&cookie))).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["user"], "anna");
    assert_eq!(me.body["server_admin"], true);
}

#[tokio::test]
async fn a_wrong_password_is_refused() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &[],
        AccessOptions::default(),
    );
    let response = send(
        &app,
        post_json(
            "/api/v1/auth/login",
            &json!({ "name": "anna", "password": "not the password" }),
            None,
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert!(response.cookie.is_none());
}

#[tokio::test]
async fn a_project_page_is_served_under_its_key() {
    // No accounts: the site is the public read-only arrangement, and the
    // project has no members file, so an anonymous reader may see it.
    let (_dir, app) = site_with(Vec::new(), &["glac"], AccessOptions::default());
    let response = send(&app, get("/p/glac/", None)).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    assert!(response.text.contains("Ridal"), "{}", response.text);
}

#[tokio::test]
async fn the_old_project_account_routes_are_refused_through_the_prefix() {
    let (_dir, app) = site_with(Vec::new(), &["glac"], AccessOptions::default());
    for (method, uri) in [
        ("GET", "/p/glac/login"),
        ("GET", "/p/glac/invite/abc"),
        ("POST", "/api/v1/projects/glac/users"),
        ("POST", "/api/v1/projects/glac/auth/invite"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let response = send(&app, request).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
}

#[tokio::test]
async fn a_server_admin_sees_every_project() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &["glac", "share"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let response = send(&app, get("/api/v1/projects", Some(&cookie))).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["projects"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_non_member_neither_lists_nor_reaches_a_private_project() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", false, &hash)],
        &["glac"],
        AccessOptions::default(),
    );
    // Make the project require a login, and give nobody a membership.
    let site = Site::open(_dir.path()).unwrap();
    let project = site.project(&key("glac")).unwrap();
    members::update(project.documents(), |set| {
        set.require_auth_to_read = true;
        Ok(())
    })
    .unwrap();

    let cookie = sign_in(&app, "anna").await;
    let list = send(&app, get("/api/v1/projects", Some(&cookie))).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(list.body["projects"].as_array().unwrap().is_empty());

    let page = send(&app, get("/p/glac/", Some(&cookie))).await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_invite_into_a_project_adds_the_membership_on_redemption() {
    let dir = tempfile::tempdir().unwrap();
    let site = Site::init(dir.path(), Some("Test site")).unwrap();
    site.create_project(&key("glac"), Some("glac")).unwrap();
    let (token, pending) = invite::mint_for_project(
        chrono::Utc::now().timestamp(),
        key("glac"),
        Role::Picker,
        DownloadScope::Picks,
    )
    .unwrap();
    let mut account = Account::new(id("anna"), false);
    account.invite = Some(pending);
    accounts::update(site.store(), |set| {
        set.users.push(account.clone());
        Ok(())
    })
    .unwrap();
    let state = SiteState::new(
        site,
        AccessOptions::default(),
        RenderServiceConfig::default(),
    );
    let app = build_site_router(state);

    let response = send(
        &app,
        post_json(
            "/api/v1/auth/invite",
            &json!({ "token": token, "password": password() }),
            None,
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    assert!(response.cookie.is_some());

    let site = Site::open(dir.path()).unwrap();
    let project = site.project(&key("glac")).unwrap();
    let (members, _) = members::read(project.documents()).unwrap().unwrap();
    let member = members.get(&id("anna")).expect("a membership was added");
    assert_eq!(member.role, Role::Picker);
    assert_eq!(member.download, DownloadScope::Picks);
}

#[tokio::test]
async fn creating_an_account_requires_a_server_admin() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", false, &hash)],
        &[],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let response = send(
        &app,
        post_json(
            "/api/v1/accounts",
            &json!({ "name": "bo", "server_admin": false }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_server_admin_creates_an_account_and_its_invite() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &[],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let response = send(
        &app,
        post_json(
            "/api/v1/accounts",
            &json!({ "name": "bo", "server_admin": false }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.text);
    assert_eq!(response.body["account"]["name"], "bo");
    assert!(response.body["invite_path"]
        .as_str()
        .unwrap()
        .starts_with("/invite/"));
}

#[tokio::test]
async fn a_server_admin_gets_the_project_and_account_controls_on_the_landing() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &["glac"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let page = send(&app, get("/", Some(&cookie))).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(page.text.contains(r#"id="new-project""#), "{}", page.text);
    assert!(
        page.text.contains(r#"id="accounts-section""#),
        "{}",
        page.text
    );
    // The project card carries its management controls, keyed by the
    // directory name rather than the display name.
    assert!(
        page.text.contains(r#"class="archive-project""#),
        "{}",
        page.text
    );
    assert!(
        page.text.contains(r#"data-project-key="glac""#),
        "{}",
        page.text
    );
}

#[tokio::test]
async fn a_plain_member_gets_members_but_not_site_admin_controls() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![
            activated("anna", true, &hash),
            activated("bo", false, &hash),
        ],
        &["glac"],
        AccessOptions::default(),
    );
    // Bo is a project administrator but not a server administrator.
    let project = Site::open(_dir.path())
        .unwrap()
        .project(&key("glac"))
        .unwrap();
    members::update(project.documents(), |set| {
        set.members.push(members::Member::new(
            id("bo"),
            Role::Admin,
            DownloadScope::All,
        ));
        Ok(())
    })
    .unwrap();

    let cookie = sign_in(&app, "bo").await;

    let landing = send(&app, get("/", Some(&cookie))).await;
    assert_eq!(landing.status, StatusCode::OK, "{}", landing.text);
    assert!(
        !landing.text.contains(r#"id="accounts-section""#),
        "a project admin is not a site admin: {}",
        landing.text
    );
    assert!(!landing.text.contains(r#"id="new-project""#));

    let settings = send(&app, get("/p/glac/settings", Some(&cookie))).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.text);
    assert!(
        settings.text.contains(r#"id="members-section""#),
        "{}",
        settings.text
    );
    assert!(settings.text.contains(r#"id="members-table""#));
    assert!(settings.text.contains(r#"id="access-form""#));
}

#[tokio::test]
async fn a_project_admin_adds_and_lists_a_member() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![
            activated("anna", true, &hash),
            activated("bo", false, &hash),
        ],
        &["glac"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;

    let added = send(
        &app,
        post_json(
            "/api/v1/projects/glac/members",
            &json!({ "name": "bo", "role": "picker", "download": "picks" }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.text);

    let listed = send(&app, get("/api/v1/projects/glac/members", Some(&cookie))).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.body["require_auth_to_read"], false);
    let found = listed.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["name"] == "bo")
        .expect("the new member is listed");
    assert_eq!(found["role"], "picker");
    assert_eq!(found["download"], "picks");
}

#[tokio::test]
async fn site_settings_require_a_login() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &[],
        AccessOptions::default(),
    );
    let response = send(&app, get("/settings", None)).await;
    assert_eq!(response.status, StatusCode::SEE_OTHER);
    assert_eq!(response.location.as_deref(), Some("/login"));
}

#[tokio::test]
async fn a_site_theme_is_personal_and_reaches_every_project() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &["glac"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;

    let saved = send(
        &app,
        put_json(
            "/api/v1/site/preferences",
            &json!({ "theme": "dark" }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    assert_eq!(saved.body["theme"], "dark");

    // Site page: the landing renders the chosen theme on the root element.
    let landing = send(&app, get("/", Some(&cookie))).await;
    assert!(
        landing.text.contains(r#"data-theme="dark""#),
        "{}",
        landing.text
    );

    // And it follows the person into a project, which is what makes it a
    // site setting rather than one saved per project.
    let project = send(&app, get("/p/glac/", Some(&cookie))).await;
    assert_eq!(project.status, StatusCode::OK, "{}", project.text);
    assert!(
        project.text.contains(r#"data-theme="dark""#),
        "{}",
        project.text
    );

    // Clearing it goes back to following the device.
    let cleared = send(
        &app,
        put_json(
            "/api/v1/site/preferences",
            &json!({ "theme": null }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.text);
    assert_eq!(cleared.body["theme"], Value::Null);
    let landing = send(&app, get("/", Some(&cookie))).await;
    assert!(!landing.text.contains(r#"data-theme="dark""#));
}

#[tokio::test]
async fn the_site_menu_offers_site_links_not_project_ones() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &["glac"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let landing = send(&app, get("/", Some(&cookie))).await;
    assert_eq!(landing.status, StatusCode::OK, "{}", landing.text);
    assert!(
        landing.text.contains(r#"href="/settings""#),
        "{}",
        landing.text
    );
    assert!(
        !landing.text.contains(r#"href="/layers"#),
        "the landing has no project to layer: {}",
        landing.text
    );
    assert!(
        !landing.text.contains("Radargram catalog"),
        "{}",
        landing.text
    );
}
