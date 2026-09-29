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

fn patch_json(uri: &str, body: &Value, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("PATCH")
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
    let projects = response.body["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2);
    // The listing carries how many people belong, for the card's text.
    assert_eq!(projects[0]["member_count"], 0);
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
async fn a_server_admin_gets_the_project_controls_on_the_landing() {
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
    // Creating accounts is an identity act and lives on the site settings
    // page now, not on the landing.
    assert!(
        !page.text.contains(r#"id="accounts-section""#),
        "accounts moved to /settings: {}",
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
    // The controls are one Edit menu, the way a radargram's are, and the
    // card says how many members the project has.
    assert!(page.text.contains(">Edit<"), "{}", page.text);
    assert!(
        page.text.contains(r#"class="rename-project""#),
        "{}",
        page.text
    );
    assert!(
        page.text.contains(r#"class="danger delete-project""#),
        "{}",
        page.text
    );
    assert!(
        page.text.contains(r#"class="project-members">0 members"#),
        "{}",
        page.text
    );

    let settings = send(&app, get("/settings", Some(&cookie))).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.text);
    assert!(
        settings.text.contains(r#"id="accounts-section""#),
        "{}",
        settings.text
    );
    assert!(
        settings.text.contains(r#"id="add-account""#),
        "{}",
        settings.text
    );
    assert!(
        settings.text.contains(r#"id="bulk-account-form""#),
        "{}",
        settings.text
    );
    // The project/role assignment the create form offers: the project key is
    // a select option, so the account can be created with a membership.
    assert!(
        settings.text.contains(r#"value="glac""#),
        "{}",
        settings.text
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
    // The site theme is offered here too, and the project form no longer
    // carries a theme of its own.
    assert!(
        settings.text.contains(r#"id="my-site-settings-section""#),
        "{}",
        settings.text
    );
    assert!(settings.text.contains(r#"id="site-theme""#));
    assert!(
        !settings.text.contains(r#"id="my-theme""#),
        "the site's theme replaced the project's: {}",
        settings.text
    );
    assert!(settings.text.contains("My project settings"));
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

#[tokio::test]
async fn creating_an_account_with_a_project_grants_it_on_redemption() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &["glac"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let created = send(
        &app,
        post_json(
            "/api/v1/accounts",
            &json!({
                "name": "bo",
                "server_admin": false,
                "project": "glac",
                "role": "picker",
                "download": "picks",
            }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let token = created.body["invite_path"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    let redeemed = send(
        &app,
        post_json(
            "/api/v1/auth/invite",
            &json!({ "token": token, "password": password() }),
            None,
        ),
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.text);

    let project = Site::open(_dir.path())
        .unwrap()
        .project(&key("glac"))
        .unwrap();
    let (members, _) = members::read(project.documents()).unwrap().unwrap();
    assert_eq!(members.get(&id("bo")).unwrap().role, Role::Picker);
}

#[tokio::test]
async fn bulk_invites_create_accounts_and_grant_the_project() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &["glac"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let created = send(
        &app,
        post_json(
            "/api/v1/accounts/bulk/invites",
            &json!({
                "prefix": "student",
                "count": 3,
                "role": "picker",
                "download": "picks",
                "project": "glac",
            }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let users = created.body["users"].as_array().unwrap();
    assert_eq!(users.len(), 3);
    assert_eq!(users[0]["name"], "student-01");

    let token = users[0]["invite_path"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    let redeemed = send(
        &app,
        post_json(
            "/api/v1/auth/invite",
            &json!({ "token": token, "password": password() }),
            None,
        ),
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.text);

    let project = Site::open(_dir.path())
        .unwrap()
        .project(&key("glac"))
        .unwrap();
    let (members, _) = members::read(project.documents()).unwrap().unwrap();
    let member = members.get(&id("student-01")).expect("a membership");
    assert_eq!(member.role, Role::Picker);
    assert_eq!(member.download, DownloadScope::Picks);
}

#[tokio::test]
async fn bulk_passwords_need_acknowledgement_and_refuse_admins() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &["glac"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;

    let unacknowledged = send(
        &app,
        post_json(
            "/api/v1/accounts/bulk/passwords",
            &json!({ "prefix": "student", "count": 2, "role": "picker" }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(unacknowledged.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        unacknowledged.body["error"]["code"],
        "risk_acknowledgement_required"
    );

    let admin = send(
        &app,
        post_json(
            "/api/v1/accounts/bulk/passwords",
            &json!({
                "prefix": "boss",
                "count": 1,
                "role": "admin",
                "acknowledge_risk": true,
            }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(admin.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        admin.body["error"]["code"],
        "admin_bulk_passwords_forbidden"
    );

    let ok = send(
        &app,
        post_json(
            "/api/v1/accounts/bulk/passwords",
            &json!({
                "prefix": "student",
                "count": 2,
                "role": "picker",
                "project": "glac",
                "acknowledge_risk": true,
            }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    assert_eq!(ok.body["users"].as_array().unwrap().len(), 2);
    assert!(
        ok.body["advisory"].is_string(),
        "the risk advisory is returned: {}",
        ok.text
    );

    // There is no invite to redeem, so the membership is present at once.
    let project = Site::open(_dir.path())
        .unwrap()
        .project(&key("glac"))
        .unwrap();
    let (members, _) = members::read(project.documents()).unwrap().unwrap();
    assert_eq!(members.get(&id("student-01")).unwrap().role, Role::Picker);
}

#[tokio::test]
async fn a_bulk_batch_requires_a_server_admin() {
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
            "/api/v1/accounts/bulk/invites",
            &json!({ "prefix": "student", "count": 1, "role": "picker" }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_site_project_links_back_to_the_site_root() {
    let (_dir, app) = site_with(Vec::new(), &["glac"], AccessOptions::default());
    let page = send(&app, get("/p/glac/", None)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(
        page.text.contains(r#"href="/""#),
        "the project menu needs a way back to the site: {}",
        page.text
    );
    assert!(page.text.contains(">Projects<"), "{}", page.text);
}

#[tokio::test]
async fn a_project_admin_creates_an_account_for_their_project() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![
            activated("anna", true, &hash),
            activated("bo", false, &hash),
        ],
        &["glac"],
        AccessOptions::default(),
    );
    // Bo administers glac but not the site.
    let site = Site::open(_dir.path()).unwrap();
    let project = site.project(&key("glac")).unwrap();
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
    let created = send(
        &app,
        post_json(
            "/api/v1/projects/glac/members/invite",
            &json!({ "name": "cara", "role": "picker", "download": "picks" }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(created.body["name"], "cara");
    let token = created.body["invite_path"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    // The account is created without server administration and with the
    // membership the invite carries -- and nothing beyond this project.
    let site = Site::open(_dir.path()).unwrap();
    let (set, _) = accounts::read(site.store()).unwrap().unwrap();
    assert!(
        !set.get(&id("cara"))
            .expect("the account exists")
            .server_admin
    );
    let project = site.project(&key("glac")).unwrap();
    let (members, _) = members::read(project.documents()).unwrap().unwrap();
    assert_eq!(members.get(&id("cara")).unwrap().role, Role::Picker);

    // Redeeming the link sets the password and leaves the membership alone.
    let redeemed = send(
        &app,
        post_json(
            "/api/v1/auth/invite",
            &json!({ "token": token, "password": password() }),
            None,
        ),
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.text);
    let site = Site::open(_dir.path()).unwrap();
    let (set, _) = accounts::read(site.store()).unwrap().unwrap();
    assert!(set.get(&id("cara")).unwrap().is_activated());

    // A name that already has an account is refused; the project admin is
    // pointed at add-member rather than silently resetting anything.
    let again = send(
        &app,
        post_json(
            "/api/v1/projects/glac/members/invite",
            &json!({ "name": "cara", "role": "viewer", "download": "none" }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn only_a_project_admin_may_invite_a_member() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![
            activated("anna", true, &hash),
            activated("bo", false, &hash),
        ],
        &["glac"],
        AccessOptions::default(),
    );
    let site = Site::open(_dir.path()).unwrap();
    let project = site.project(&key("glac")).unwrap();
    members::update(project.documents(), |set| {
        set.members.push(members::Member::new(
            id("bo"),
            Role::Viewer,
            DownloadScope::None,
        ));
        Ok(())
    })
    .unwrap();

    let cookie = sign_in(&app, "bo").await;
    let response = send(
        &app,
        post_json(
            "/api/v1/projects/glac/members/invite",
            &json!({ "name": "cara", "role": "picker" }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_project_admin_creates_a_bulk_batch_for_their_project() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![
            activated("anna", true, &hash),
            activated("bo", false, &hash),
        ],
        &["glac"],
        AccessOptions::default(),
    );
    let site = Site::open(_dir.path()).unwrap();
    let project = site.project(&key("glac")).unwrap();
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
    let created = send(
        &app,
        post_json(
            "/api/v1/projects/glac/members/bulk/invites",
            &json!({
                "prefix": "student",
                "count": 2,
                "role": "picker",
                "download": "picks",
            }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let users = created.body["users"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["name"], "student-01");

    // The batch created accounts without server administration; the
    // membership arrives with the invitation's redemption.
    let site = Site::open(_dir.path()).unwrap();
    let (accounts, _) = accounts::read(site.store()).unwrap().unwrap();
    assert!(!accounts.get(&id("student-01")).unwrap().server_admin);

    let token = users[0]["invite_path"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    let redeemed = send(
        &app,
        post_json(
            "/api/v1/auth/invite",
            &json!({ "token": token, "password": password() }),
            None,
        ),
    )
    .await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.text);

    let site = Site::open(_dir.path()).unwrap();
    let project = site.project(&key("glac")).unwrap();
    let (members, _) = members::read(project.documents()).unwrap().unwrap();
    let member = members.get(&id("student-01")).expect("a membership");
    assert_eq!(member.role, Role::Picker);
    assert_eq!(member.download, DownloadScope::Picks);
}

#[tokio::test]
async fn a_project_admin_bulk_batch_still_refuses_admin_passwords() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![
            activated("anna", true, &hash),
            activated("bo", false, &hash),
        ],
        &["glac"],
        AccessOptions::default(),
    );
    let site = Site::open(_dir.path()).unwrap();
    let project = site.project(&key("glac")).unwrap();
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
    let admin = send(
        &app,
        post_json(
            "/api/v1/projects/glac/members/bulk/passwords",
            &json!({
                "prefix": "boss",
                "count": 1,
                "role": "admin",
                "acknowledge_risk": true,
            }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(admin.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        admin.body["error"]["code"],
        "admin_bulk_passwords_forbidden"
    );

    // A plain member cannot create a batch at all.
    let outsider = send(
        &app,
        post_json(
            "/api/v1/projects/glac/members/bulk/invites",
            &json!({ "prefix": "x", "count": 1, "role": "picker" }),
            None,
        ),
    )
    .await;
    assert_eq!(outsider.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_site_audit_records_account_and_project_changes() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", true, &hash)],
        &["glac"],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;

    let created = send(
        &app,
        post_json(
            "/api/v1/accounts",
            &json!({
                "name": "bo",
                "server_admin": false,
                "project": "glac",
                "role": "picker",
                "download": "picks",
            }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let renamed = send(
        &app,
        patch_json(
            "/api/v1/projects/glac",
            &json!({ "name": "Glaciology" }),
            Some(&cookie),
        ),
    )
    .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.text);
    let archived = send(
        &app,
        post_json("/api/v1/projects/glac/archive", &json!({}), Some(&cookie)),
    )
    .await;
    assert_eq!(archived.status, StatusCode::OK, "{}", archived.text);

    let page = send(&app, get("/api/v1/site/audit", Some(&cookie))).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let entries = page.body["entries"].as_array().unwrap();
    let actions: Vec<&str> = entries
        .iter()
        .map(|entry| entry["action"].as_str().unwrap())
        .collect();
    // Most recent first.
    assert_eq!(actions[0], "project_archived");
    assert!(actions.contains(&"project_renamed"), "{actions:?}");
    assert!(actions.contains(&"account_created"), "{actions:?}");

    let account = entries
        .iter()
        .find(|entry| entry["action"] == "account_created")
        .expect("the account creation is recorded");
    assert_eq!(account["actor"], "anna");
    assert_eq!(account["subject"], "bo");
    assert_eq!(account["project"], "glac");
    assert_eq!(account["role"], "picker");
}

#[tokio::test]
async fn a_project_admin_sees_only_their_projects_history() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![
            activated("anna", true, &hash),
            activated("bo", false, &hash),
            activated("dan", false, &hash),
        ],
        &["glac", "share"],
        AccessOptions::default(),
    );
    let site = Site::open(_dir.path()).unwrap();
    let project = site.project(&key("glac")).unwrap();
    members::update(project.documents(), |set| {
        set.members.push(members::Member::new(
            id("bo"),
            Role::Admin,
            DownloadScope::All,
        ));
        set.members.push(members::Member::new(
            id("dan"),
            Role::Viewer,
            DownloadScope::None,
        ));
        Ok(())
    })
    .unwrap();

    let bo = sign_in(&app, "bo").await;
    let created = send(
        &app,
        post_json(
            "/api/v1/projects/glac/members/invite",
            &json!({ "name": "cara", "role": "picker" }),
            Some(&bo),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);

    // A change in another project, by the server administrator.
    let anna = sign_in(&app, "anna").await;
    let renamed = send(
        &app,
        patch_json(
            "/api/v1/projects/share",
            &json!({ "name": "Shared" }),
            Some(&anna),
        ),
    )
    .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.text);

    let page = send(&app, get("/api/v1/projects/glac/audit", Some(&bo))).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let entries = page.body["entries"].as_array().unwrap();
    assert!(!entries.is_empty(), "{}", page.text);
    assert!(
        entries.iter().all(|entry| entry["project"] == "glac"),
        "a project admin sees only their project: {}",
        page.text
    );

    // A plain member may not read it.
    let dan = sign_in(&app, "dan").await;
    let refused = send(&app, get("/api/v1/projects/glac/audit", Some(&dan))).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_site_audit_requires_a_server_admin() {
    let hash = accounts::hash_password(password()).unwrap();
    let (_dir, app) = site_with(
        vec![activated("anna", false, &hash)],
        &[],
        AccessOptions::default(),
    );
    let cookie = sign_in(&app, "anna").await;
    let response = send(&app, get("/api/v1/site/audit", Some(&cookie))).await;
    assert_eq!(response.status, StatusCode::FORBIDDEN);
}
