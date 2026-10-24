//! Fixtures and HTTP helpers shared by the server's test modules.
//!
//! Requests go through the real router with `ServiceExt::oneshot`, so the
//! status codes, headers and bodies are the ones a browser sees.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderName, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::app::{build_router, AccessOptions, AppState};
use crate::project::Project;
use crate::server::render_service::RenderServiceConfig;
use crate::site::accounts;

/// The attributes that make a NetCDF file a current Ridal file, and name
/// the radargram it holds.
pub(crate) fn mark_as_ridal(file: &mut netcdf::FileMut, radargram_id: &str) {
    file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
        .unwrap();
    file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
        .unwrap();
    file.add_attribute("ridal_radargram_id", radargram_id)
        .unwrap();
}

/// The smallest radargram the catalog recognises: 8 samples by 40 traces,
/// with nothing but its data and [`mark_as_ridal`]'s attributes.
pub(super) fn write_test_nc(path: &Path, radargram_id: &str) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", 8).unwrap();
    file.add_dimension("x", 40).unwrap();
    let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    // Varying, not constant: the renderer refuses an array whose amplitude
    // percentiles collapse to a single value, because there is no contrast
    // to stretch. A flat fixture is not a realistic radargram anyway.
    let data: Vec<f32> = (0..(8 * 40)).map(|i| (i % 97) as f32).collect();
    var.put_values(&data, ..).unwrap();
    mark_as_ridal(&mut file, radargram_id);
}

/// Where a project keeps everything Ridal owns, since #187 moved it out of
/// the project root. Tests reach past the API to assert on files on disk --
/// the layout is part of the contract -- so they need the same answer the
/// code has.
pub(super) fn project_data(root: &Path) -> PathBuf {
    root.join(crate::project::DEFAULT_DATA_DIR)
}

/// Where a freshly initialized project keeps its radargrams, and where an
/// upload from the browser lands.
pub(super) fn project_radargrams(root: &Path) -> PathBuf {
    project_data(root).join(crate::project::DEFAULT_RADARGRAM_DIR)
}

/// An empty project in a fresh directory.
pub(super) fn new_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    dir
}

/// The state `ridal gui` builds over the directory at `root`: the project
/// found there, if any, or a bare directory of radargrams.
pub(super) fn project_state(root: &Path, access: AccessOptions) -> AppState {
    let project = Project::discover(root).unwrap();
    AppState::build_with_project(root, &RenderServiceConfig::default(), project, access).unwrap()
}

/// A router over [`project_state`].
pub(super) fn project_router(root: &Path, access: AccessOptions) -> Router {
    build_router(Arc::new(project_state(root, access)))
}

/// The passphrase every test account is activated with.
///
/// Generated once per run rather than written down. Nothing asserts on the
/// value -- only on what signing in with it and without it does -- and the
/// code scanner over this repository cannot tell a fixture from a
/// credential shipped by mistake, so a literal costs a real alert somewhere
/// else its attention.
pub(super) fn password() -> &'static str {
    static PASSWORD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PASSWORD.get_or_init(|| {
        let mut bytes = [0u8; 12];
        getrandom::fill(&mut bytes).expect("system randomness");
        format!("passphrase-{:x?}", bytes)
    })
}

/// The hash of [`password`], computed once per run.
///
/// Every test account shares one passphrase, so hashing it per test --
/// hundreds of times -- bought nothing. Tests that assert on the *cost* of
/// hashing still call [`accounts::hash_password`] directly.
pub(super) fn password_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| accounts::hash_password(password()).expect("hashing the shared password"))
}

/// A request to `uri` as it is, carrying the session cookie if there is one.
pub(super) fn request(
    method: &str,
    uri: &str,
    session: Option<&str>,
) -> axum::http::request::Builder {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(session) = session {
        builder = builder.header(header::COOKIE, session);
    }
    builder
}

/// A request with a JSON body.
pub(super) fn json_request(
    method: &str,
    uri: &str,
    body: &Value,
    session: Option<&str>,
) -> Request<Body> {
    request(method, uri, session)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A response, read to the end.
pub(super) struct Response {
    pub(super) status: StatusCode,
    pub(super) headers: HeaderMap,
    pub(super) cookie: Option<String>,
    /// The version the document is now at, for the routes that offer one.
    pub(super) etag: Option<String>,
    pub(super) location: Option<String>,
    pub(super) disposition: Option<String>,
    pub(super) content_type: Option<String>,
    pub(super) bytes: Vec<u8>,
    /// The body as JSON, or `Null` when it is not JSON.
    pub(super) body: Value,
    pub(super) text: String,
}

impl Response {
    /// A header's value, when it is present and text.
    pub(super) fn header(&self, name: impl axum::http::header::AsHeaderName) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }
}

pub(super) async fn send(app: &Router, request: Request<Body>) -> Response {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    let text_of = |name: HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    Response {
        status,
        cookie: text_of(header::SET_COOKIE),
        etag: text_of(header::ETAG),
        location: text_of(header::LOCATION),
        disposition: text_of(header::CONTENT_DISPOSITION),
        content_type: text_of(header::CONTENT_TYPE),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&bytes).into_owned(),
        headers,
        bytes,
    }
}

/// The `name=value` part of a `Set-Cookie`, ready to send back as `Cookie`.
pub(super) fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

/// Sign in as `name` with [`password`] and return the cookie to send back.
pub(super) async fn sign_in(app: &Router, name: &str) -> String {
    let response = send(
        app,
        json_request(
            "POST",
            "/api/v1/auth/login",
            &json!({"name": name, "password": password()}),
            None,
        ),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    cookie_pair(&response.cookie.expect("a login must set a cookie"))
}
