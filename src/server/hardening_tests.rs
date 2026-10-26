//! HTTP-level tests for refusing cross-site writes and other hosts (#514),
//! throttling sign-ins (#515) and the security headers (#516), through the
//! real site router.

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::Router;
use serde_json::json;

use super::access_tests::{activated, gui_router, new_site, site_router, write_people, UserSet};
use super::app::AccessOptions;
use super::test_support::{self, new_project, password, password_hash, send, Response};
use crate::project::roles::{DownloadScope, Role};
use crate::site::accounts;

/// `method` on `uri`, with the given extra headers.
async fn call(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)]) -> Response {
    let mut request = test_support::request(method, uri, None);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    send(app, request.body(Body::empty()).unwrap()).await
}

fn error_code(response: &Response) -> &str {
    response.body["error"]["code"].as_str().unwrap_or("")
}

/// A site with one project and no accounts: enough for the routes that
/// need nobody signed in.
fn empty_site(access: AccessOptions) -> (tempfile::TempDir, Router) {
    let dir = new_site();
    let app = site_router(dir.path(), access);
    (dir, app)
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_browser_write_from_another_site_is_refused() {
    let (_dir, app) = empty_site(AccessOptions::default());
    for site in ["cross-site", "same-site"] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let refused = call(
                &app,
                method,
                "/api/v1/auth/logout",
                &[("sec-fetch-site", site)],
            )
            .await;
            assert_eq!(refused.status, StatusCode::FORBIDDEN, "{method} {site}");
            assert_eq!(
                error_code(&refused),
                "cross_site_request",
                "{method} {site}"
            );
        }
    }
    // Ridal's own pages, a typed-in address, and anything that is not a
    // browser.
    for headers in [
        &[("sec-fetch-site", "same-origin")][..],
        &[("sec-fetch-site", "none")][..],
        &[][..],
    ] {
        let allowed = call(&app, "POST", "/api/v1/auth/logout", headers).await;
        assert_eq!(
            allowed.status,
            StatusCode::OK,
            "{headers:?}: {}",
            allowed.text
        );
    }
    // Reading is never refused: a link from another site is a GET.
    let read = call(
        &app,
        "GET",
        "/api/v1/health",
        &[("sec-fetch-site", "cross-site")],
    )
    .await;
    assert_eq!(read.status, StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn gui_answers_only_for_loopback_names() {
    let dir = new_project();
    let app = gui_router(dir.path(), AccessOptions::default());

    let refused = call(&app, "GET", "/api/v1/health", &[("host", "evil.example")]).await;
    assert_eq!(refused.status, StatusCode::MISDIRECTED_REQUEST);
    assert_eq!(error_code(&refused), "unexpected_host");
    // A rebinding page's write is refused on the host alone, whatever
    // `Sec-Fetch-Site` says (to the browser it is same-origin).
    let write = call(
        &app,
        "POST",
        "/api/v1/auth/logout",
        &[
            ("host", "evil.example:8000"),
            ("sec-fetch-site", "same-origin"),
        ],
    )
    .await;
    assert_eq!(write.status, StatusCode::MISDIRECTED_REQUEST);

    for host in [
        "localhost:8000",
        "127.0.0.1:8000",
        "[::1]:8000",
        "LOCALHOST",
    ] {
        let allowed = call(&app, "GET", "/api/v1/health", &[("host", host)]).await;
        assert_eq!(allowed.status, StatusCode::OK, "{host}");
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_site_leaves_host_names_to_its_proxy() {
    let (_dir, app) = empty_site(AccessOptions::default());
    let response = call(
        &app,
        "GET",
        "/api/v1/health",
        &[("host", "ridal.example.org")],
    )
    .await;
    assert_eq!(response.status, StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_flag_turns_both_checks_off() {
    let access = AccessOptions {
        allow_cross_site_requests: true,
        ..AccessOptions::default()
    };
    let dir = new_project();
    let gui = gui_router(dir.path(), access);
    let (_site_dir, site) = empty_site(access);

    let host = call(&gui, "GET", "/api/v1/health", &[("host", "evil.example")]).await;
    assert_eq!(host.status, StatusCode::OK);
    for app in [&gui, &site] {
        let write = call(
            app,
            "POST",
            "/api/v1/auth/logout",
            &[("sec-fetch-site", "cross-site")],
        )
        .await;
        assert_eq!(write.status, StatusCode::OK, "{}", write.text);
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn every_kind_of_response_carries_the_security_headers() {
    let (_dir, app) = empty_site(AccessOptions::default());
    let page = call(&app, "GET", "/login", &[]).await;
    let json = call(&app, "GET", "/api/v1/health", &[]).await;
    let asset = call(&app, "GET", "/static/app.js", &[]).await;
    let refusal = call(
        &app,
        "POST",
        "/api/v1/auth/logout",
        &[("sec-fetch-site", "cross-site")],
    )
    .await;
    let missing = call(&app, "GET", "/no/such/page", &[]).await;
    for (what, response) in [
        ("page", &page),
        ("JSON", &json),
        ("asset", &asset),
        ("refusal", &refusal),
        ("404", &missing),
    ] {
        let policy = response
            .header(header::CONTENT_SECURITY_POLICY)
            .unwrap_or_default();
        assert!(
            policy.contains("script-src 'self';") && policy.contains("frame-ancestors 'self'"),
            "{what}: {policy}"
        );
        for (name, value) in [
            (header::X_FRAME_OPTIONS, "SAMEORIGIN"),
            (header::REFERRER_POLICY, "same-origin"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ] {
            assert_eq!(
                response.header(&name).as_deref(),
                Some(value),
                "{what} ({}): {name}",
                response.status
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Sign-in throttling (#515)
// ---------------------------------------------------------------------------

/// A site where `erik` and `anna` can sign in with [`password`].
fn site_with_accounts() -> (tempfile::TempDir, Router) {
    let dir = new_site();
    let hash = password_hash();
    write_people(
        dir.path(),
        &UserSet {
            users: vec![
                activated("erik", Role::Admin, DownloadScope::All, hash),
                activated("anna", Role::Picker, DownloadScope::All, hash),
            ],
            ..UserSet::default()
        },
    );
    let app = site_router(dir.path(), AccessOptions::default());
    (dir, app)
}

async fn login(app: &Router, name: &str, password: &str) -> Response {
    send(
        app,
        test_support::json_request(
            "POST",
            "/api/v1/auth/login",
            &json!({"name": name, "password": password}),
            None,
        ),
    )
    .await
}

fn verifications() -> usize {
    accounts::VERIFICATIONS.with(|n| n.get())
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_failed_sign_in_delays_the_next_one_to_that_name() {
    let (_dir, app) = site_with_accounts();
    let wrong = format!("{}x", password());

    let before = verifications();
    let failed = login(&app, "erik", &wrong).await;
    assert_eq!(failed.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        verifications() - before,
        1,
        "the failed sign-in was not counted as a verification on this thread, \
         so the count of zero below would prove nothing"
    );

    // Even the right password waits, and no hash is spent on it.
    let before = verifications();
    let throttled = login(&app, "erik", password()).await;
    assert_eq!(
        throttled.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        throttled.text
    );
    assert_eq!(error_code(&throttled), "too_many_attempts");
    assert_eq!(throttled.header(header::RETRY_AFTER).as_deref(), Some("1"));
    assert!(throttled.cookie.is_none());
    assert_eq!(
        verifications() - before,
        0,
        "a throttled sign-in verified a hash"
    );

    // Someone else is not held up.
    let other = login(&app, "anna", password()).await;
    assert_eq!(other.status, StatusCode::OK, "{}", other.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_name_without_an_account_is_throttled_the_same_way() {
    let (_dir, app) = site_with_accounts();
    let failed = login(&app, "nobody", password()).await;
    assert_eq!(failed.status, StatusCode::UNAUTHORIZED);

    let before = verifications();
    let throttled = login(&app, "nobody", password()).await;
    assert_eq!(throttled.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(&throttled), "too_many_attempts");
    assert_eq!(verifications() - before, 0);

    // A name that is not even a valid account name, likewise.
    let failed = login(&app, "Not A Name!", password()).await;
    assert_eq!(failed.status, StatusCode::UNAUTHORIZED);
    let throttled = login(&app, "Not A Name!", password()).await;
    assert_eq!(throttled.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_successful_sign_in_leaves_no_delay_behind() {
    let (_dir, app) = site_with_accounts();
    for _ in 0..3 {
        let response = login(&app, "erik", password()).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    }
}

// ---------------------------------------------------------------------------
// Content Security Policy (#517)
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn every_project_page_carries_a_policy_without_inline_script() {
    let dir = new_project();
    test_support::write_test_nc(
        &test_support::project_radargrams(dir.path()).join("line-01.nc"),
        "line-01",
    );
    let app = gui_router(dir.path(), AccessOptions::default());
    for page in [
        "/p/default/",
        "/p/default/view/line-01",
        "/p/default/layers",
        "/p/default/processing",
        "/p/default/ingest",
        "/p/default/settings",
    ] {
        let response = call(&app, "GET", page, &[]).await;
        assert_eq!(response.status, StatusCode::OK, "{page}");
        let policy = response
            .header(header::CONTENT_SECURITY_POLICY)
            .unwrap_or_default();
        assert!(policy.contains("script-src 'self';"), "{page}: {policy}");
        assert!(!policy.contains("unsafe-eval"), "{page}: {policy}");
        assert!(
            policy.contains("https://server.arcgisonline.com"),
            "{page}: the built-in basemap is offered: {policy}"
        );
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_projects_map_hosts_are_in_its_policy() {
    let dir = new_project();
    let config = dir.path().join(crate::project::MARKER);
    let mut toml = std::fs::read_to_string(&config).unwrap();
    toml.push_str(
        r#"
[[basemaps]]
id = "osm"
name = "OpenStreetMap"
url = "https://{s}.tile.example.org/{z}/{x}/{y}.png"
subdomains = "abc"

[[overlays]]
id = "stakes"
name = "Stakes"
url = "https://static.example.org/stakes.geojson"

[[overlays]]
id = "local"
name = "Local"
url = "/static/local.geojson"
"#,
    );
    std::fs::write(&config, toml).unwrap();
    let app = gui_router(dir.path(), AccessOptions::default());

    let response = call(&app, "GET", "/p/default/", &[]).await;
    let policy = response
        .header(header::CONTENT_SECURITY_POLICY)
        .unwrap_or_default();
    assert!(
        policy.contains("img-src 'self' data: blob: https://*.tile.example.org https://server.arcgisonline.com;"),
        "{policy}"
    );
    assert!(
        policy.contains("connect-src 'self' https://static.example.org;"),
        "{policy}"
    );
    // The site's own pages and the API keep the default.
    let health = call(&app, "GET", "/api/v1/health", &[]).await;
    assert!(!health
        .header(header::CONTENT_SECURITY_POLICY)
        .unwrap_or_default()
        .contains("example.org"));
}
