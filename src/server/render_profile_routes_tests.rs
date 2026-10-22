//! HTTP-level tests for a project's own render profiles (#183): the store
//! behind them is tested in `project::render_profiles`; these are about what
//! the rest of the server makes of them.
//!
//! Every test builds an `AppState`, which opens a NetCDF, so they carry the
//! `#[serial_test::serial(netcdf)]` guard the other route tests do.

use std::path::Path as StdPath;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::app::{build_router, AccessOptions, AppState};
use crate::project::Project;
use crate::render::profile::RenderProfile;
use crate::server::render_service::RenderServiceConfig;

const PROFILES: &str = "/api/v1/render-profiles";
const SURVEY: &str = "/api/v1/render-profiles/survey";
const SETTINGS: &str = "/api/v1/settings";

fn write_test_nc(path: &StdPath, radargram_id: &str) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", 8).unwrap();
    file.add_dimension("x", 40).unwrap();
    let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    let data: Vec<f32> = (0..(8 * 40)).map(|i| (i % 97) as f32 - 48.0).collect();
    var.put_values(&data, ..).unwrap();
    file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
        .unwrap();
    file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
        .unwrap();
    file.add_attribute("ridal_radargram_id", radargram_id)
        .unwrap();
}

fn app_for(dir: &StdPath) -> Router {
    let project = Project::discover(dir).unwrap().unwrap();
    let state = Arc::new(
        AppState::build_with_project(
            dir,
            &RenderServiceConfig::default(),
            Some(project),
            AccessOptions::default(),
        )
        .unwrap(),
    );
    build_router(state)
}

fn project_app() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    let radargrams = dir
        .path()
        .join(crate::project::DEFAULT_DATA_DIR)
        .join(crate::project::DEFAULT_RADARGRAM_DIR);
    write_test_nc(&radargrams.join("line-01.nc"), "line-01");
    let app = app_for(dir.path());
    (dir, app)
}

/// A colourful profile, so that it is visibly not a built-in.
fn survey() -> Value {
    serde_json::to_value(RenderProfile {
        name: "survey".to_string(),
        contrast: 1.5,
        nodata_color: Some([255, 0, 255]),
        ..RenderProfile::seismic_profile()
    })
    .unwrap()
}

struct Answer {
    status: StatusCode,
    etag: Option<String>,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    if_match: Option<&str>,
) -> Answer {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(value) = if_match {
        builder = builder.header(header::IF_MATCH, value);
    }
    let body = body.map_or(Body::empty(), |b| Body::from(b.to_string()));
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let header_text = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v: &axum::http::HeaderValue| v.to_str().ok())
            .map(str::to_string)
    };
    let (status, etag, content_type) = (
        response.status(),
        header_text(header::ETAG),
        header_text(header::CONTENT_TYPE),
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Answer {
        status,
        etag,
        content_type,
        bytes,
    }
}

async fn get(app: &Router, uri: &str) -> Answer {
    send(app, "GET", uri, None, None).await
}

fn chunk(profile: &str) -> String {
    format!("/api/v1/datasets/line-01/views/standard/chunks/{profile}/0/0")
}

fn names(list: &Value) -> Vec<String> {
    list["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_project_profile_is_offered_and_renders_like_a_built_in() {
    let (_dir, app) = project_app();
    assert_eq!(
        get(&app, &chunk("survey")).await.status,
        StatusCode::BAD_REQUEST
    );

    let created = send(&app, "PUT", SURVEY, Some(&survey()), None).await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text());
    assert_eq!(created.json()["scope"], "project");
    assert!(created.etag.is_some());

    // Listed after the built-ins, in full, and by name where the pages
    // and the older endpoint list them.
    let list = get(&app, PROFILES).await.json();
    let listed = names(&list);
    assert_eq!(listed.last().map(String::as_str), Some("survey"));
    assert_eq!(listed[0], "default");
    assert_eq!(list["default"], "default");
    let offered: Vec<String> =
        serde_json::from_slice(&get(&app, "/api/v1/profiles").await.bytes).unwrap();
    assert!(offered.contains(&"survey".to_string()));

    // A tile renders with it, in colour.
    let tile = get(&app, &chunk("survey")).await;
    assert_eq!(tile.status, StatusCode::OK);
    assert_eq!(tile.content_type.as_deref(), Some("image/jpeg"));

    // The viewer offers it, marked as a colour scale.
    let page = get(&app, "/view/line-01?profile=survey").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page
        .text()
        .contains(r#"<option value="survey" selected data-colormap>"#));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_profile_comes_back_as_a_file_the_command_line_reads() {
    let (dir, app) = project_app();
    send(&app, "PUT", SURVEY, Some(&survey()), None).await;
    let file = get(&app, &format!("{SURVEY}?format=toml")).await;
    assert_eq!(file.status, StatusCode::OK);
    assert_eq!(file.content_type.as_deref(), Some("application/toml"));
    let path = dir.path().join("survey.toml");
    std::fs::write(&path, &file.bytes).unwrap();
    let read = RenderProfile::resolve(path.to_str().unwrap()).unwrap();
    assert_eq!(serde_json::to_value(read).unwrap(), survey());

    // A built-in too, which is the way to start a profile file from one.
    let built_in = get(&app, &format!("{PROFILES}/seismic?format=toml")).await;
    let parsed: RenderProfile = toml::from_str(&built_in.text()).unwrap();
    assert_eq!(parsed, RenderProfile::seismic_profile());
    assert_eq!(
        get(&app, &format!("{SURVEY}?format=yaml")).await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn edits_are_conditional_and_take_effect_at_once() {
    let (_dir, app) = project_app();
    let created = send(&app, "PUT", SURVEY, Some(&survey()), None).await;
    let first = created.etag.unwrap();
    let before = get(&app, &chunk("survey")).await.bytes;

    let mut edited = survey();
    edited["colormap"] = Value::Null;
    edited["symmetric_limits"] = json!(false);
    let saved = send(&app, "PUT", SURVEY, Some(&edited), Some(&first)).await;
    assert_eq!(saved.status, StatusCode::OK);
    assert_ne!(saved.etag.as_deref(), Some(first.as_str()));

    // The stale version is refused.
    let stale = send(&app, "PUT", SURVEY, Some(&survey()), Some(&first)).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(stale.json()["error"]["code"], "version_conflict");

    // The next tile is the edited profile's, not a cached one of the old.
    let after = get(&app, &chunk("survey")).await;
    assert_eq!(after.status, StatusCode::OK);
    assert_ne!(after.bytes, before);
    assert_eq!(get(&app, SURVEY).await.json()["colormap"], Value::Null);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn names_and_bodies_are_checked() {
    let (_dir, app) = project_app();
    let put_as = |name: &str, mut body: Value| {
        body["name"] = json!(name);
        let uri = format!("{PROFILES}/{name}");
        let app = app.clone();
        async move { send(&app, "PUT", &uri, Some(&body), None).await }
    };
    for (name, status, code) in [
        ("seismic", StatusCode::CONFLICT, "built_in_render_profile"),
        (
            "personal-mine",
            StatusCode::BAD_REQUEST,
            "reserved_render_profile_name",
        ),
        (
            "Mine",
            StatusCode::BAD_REQUEST,
            "invalid_render_profile_name",
        ),
    ] {
        let answer = put_as(name, survey()).await;
        assert_eq!(answer.status, status, "{name}: {}", answer.text());
        assert_eq!(answer.json()["error"]["code"], code, "{name}");
    }

    // The body names the profile it is, and must agree with the path.
    let mut other = survey();
    other["name"] = json!("other");
    let mismatch = send(&app, "PUT", SURVEY, Some(&other), None).await;
    assert_eq!(
        mismatch.json()["error"]["code"],
        "render_profile_name_mismatch"
    );

    // Values the profile cannot mean are refused, not clamped.
    let mut wild = survey();
    wild["contrast"] = json!(0.0);
    let refused = send(&app, "PUT", SURVEY, Some(&wild), None).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(refused.json()["error"]["code"], "invalid_render_profile");
    assert!(refused.text().contains("contrast"), "{}", refused.text());

    // Nothing above was stored.
    assert_eq!(get(&app, SURVEY).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_default_and_the_built_ins_cannot_be_deleted() {
    let (_dir, app) = project_app();
    send(&app, "PUT", SURVEY, Some(&survey()), None).await;
    let made_default = send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"default_profile": "survey"})),
        None,
    )
    .await;
    assert_eq!(
        made_default.status,
        StatusCode::OK,
        "{}",
        made_default.text()
    );
    assert_eq!(get(&app, PROFILES).await.json()["default"], "survey");

    let refused = send(&app, "DELETE", SURVEY, None, None).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.json()["error"]["code"], "render_profile_is_default");
    let built_in = send(&app, "DELETE", &format!("{PROFILES}/seismic"), None, None).await;
    assert_eq!(built_in.status, StatusCode::CONFLICT);
    assert_eq!(built_in.json()["error"]["code"], "built_in_render_profile");

    send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"default_profile": null})),
        None,
    )
    .await;
    assert_eq!(
        send(&app, "DELETE", SURVEY, None, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&app, "DELETE", SURVEY, None, None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_deleted_profile_falls_back_wherever_it_was_named() {
    let (_dir, app) = project_app();
    send(&app, "PUT", SURVEY, Some(&survey()), None).await;
    send(&app, "DELETE", SURVEY, None, None).await;

    // A tile names its profile in its path, so asking for one is a 400, as
    // for any unknown name; a page falls back to the next choice instead.
    assert_eq!(
        get(&app, &chunk("survey")).await.status,
        StatusCode::BAD_REQUEST
    );
    for page in ["/?profile=survey", "/view/line-01?profile=survey"] {
        let answer = get(&app, page).await;
        assert_eq!(answer.status, StatusCode::OK, "{page}");
        assert!(
            answer.text().contains(r#"value="default" selected"#),
            "{page} did not fall back"
        );
    }
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_hidden_built_in_is_not_offered_but_still_renders() {
    let (_dir, app) = project_app();
    let hidden = send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"hidden_profiles": ["abslog", "abslog"]})),
        None,
    )
    .await;
    assert_eq!(hidden.status, StatusCode::OK, "{}", hidden.text());
    assert_eq!(hidden.json()["hidden_profiles"], json!(["abslog"]));

    let offered: Vec<String> =
        serde_json::from_slice(&get(&app, "/api/v1/profiles").await.bytes).unwrap();
    assert!(!offered.contains(&"abslog".to_string()));
    let list = get(&app, PROFILES).await.json();
    let abslog = list["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "abslog")
        .unwrap();
    assert_eq!(abslog["hidden"], true);

    // Links that name it keep working, and a page open on it still shows it.
    assert_eq!(get(&app, &chunk("abslog")).await.status, StatusCode::OK);
    let page = get(&app, "/?profile=abslog").await.text();
    assert!(page.contains(r#"value="abslog" selected"#));
    assert!(!get(&app, "/").await.text().contains(r#"value="abslog""#));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn only_a_built_in_that_is_not_the_default_can_be_hidden() {
    let (_dir, app) = project_app();
    send(&app, "PUT", SURVEY, Some(&survey()), None).await;
    for (body, code) in [
        // `default` is the default while the project names none.
        (
            json!({"hidden_profiles": ["default"]}),
            "hidden_default_profile",
        ),
        (
            json!({"default_profile": "abslog", "hidden_profiles": ["abslog"]}),
            "hidden_default_profile",
        ),
        // A project profile is deleted rather than hidden.
        (json!({"hidden_profiles": ["survey"]}), "unknown_profile"),
    ] {
        let answer = send(&app, "PUT", SETTINGS, Some(&body), None).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(answer.json()["error"]["code"], code, "{body}");
    }
    // With another default, `default` can go.
    let answer = send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"default_profile": "survey", "hidden_profiles": ["default"]})),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.text());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_hand_edited_profile_is_read_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project::init(dir.path(), Some("test")).unwrap();
    let radargrams = dir
        .path()
        .join(crate::project::DEFAULT_DATA_DIR)
        .join(crate::project::DEFAULT_RADARGRAM_DIR);
    write_test_nc(&radargrams.join("line-01.nc"), "line-01");
    let store_dir = project.data_dir().join("render/profiles");
    std::fs::create_dir_all(&store_dir).unwrap();
    std::fs::write(store_dir.join("survey.json"), survey().to_string()).unwrap();
    // A broken one beside it costs only itself.
    std::fs::write(store_dir.join("broken.json"), "{").unwrap();

    let app = app_for(dir.path());
    let listed = names(&get(&app, PROFILES).await.json());
    assert!(listed.contains(&"survey".to_string()));
    assert!(!listed.contains(&"broken".to_string()));
    assert_eq!(get(&app, &chunk("survey")).await.status, StatusCode::OK);
}

const MINE: &str = "/api/v1/preferences/render-profiles/personal-mine";

/// A personal profile, drawn in the viewer, with seismic thumbnails.
fn mine(thumbnail: Option<&str>) -> Value {
    let mut body = serde_json::to_value(RenderProfile {
        name: "personal-mine".to_string(),
        contrast: 1.8,
        ..RenderProfile::high_contrast_profile()
    })
    .unwrap();
    body["thumbnail_profile"] = json!(thumbnail);
    body
}

/// The overview files the project keeps on disk.
fn overview_files(dir: &StdPath) -> Vec<String> {
    let root = dir
        .join(crate::project::DEFAULT_DATA_DIR)
        .join("cache/overviews");
    let mut files = Vec::new();
    for revision in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(revision.path()).unwrap().flatten() {
            files.push(file.file_name().into_string().unwrap());
        }
    }
    files
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_personal_profile_draws_tiles_and_leaves_thumbnails_to_a_shared_one() {
    let (dir, app) = project_app();
    let saved = send(&app, "PUT", MINE, Some(&mine(Some("seismic"))), None).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());
    assert_eq!(saved.json()["scope"], "personal");
    assert_eq!(saved.json()["thumbnail_profile"], "seismic");
    assert!(saved.etag.is_some());

    // Offered to its owner wherever a profile is chosen, and listed last.
    let listed = names(&get(&app, PROFILES).await.json());
    assert_eq!(listed.last().map(String::as_str), Some("personal-mine"));
    let mine_list = get(&app, "/api/v1/preferences/render-profiles")
        .await
        .json();
    assert_eq!(mine_list["allowed"], true);
    assert_eq!(names(&mine_list), vec!["personal-mine".to_string()]);
    assert_eq!(
        get(&app, &chunk("personal-mine")).await.status,
        StatusCode::OK
    );
    let page = get(&app, "/view/line-01?profile=personal-mine")
        .await
        .text();
    assert!(page.contains(r#"value="personal-mine" selected"#));

    // A thumbnail asked for in it is the seismic one, and only that is kept.
    let overview = |profile: &str| {
        format!("/api/v1/datasets/line-01/views/standard/overview?profile={profile}")
    };
    let personal = get(&app, &overview("personal-mine")).await;
    assert_eq!(personal.status, StatusCode::OK);
    assert_eq!(personal.bytes, get(&app, &overview("seismic")).await.bytes);
    let seismic_tag = super::overview_cache::profile_tag(&RenderProfile::seismic_profile());
    let files = overview_files(dir.path());
    assert!(!files.is_empty());
    assert!(
        files.iter().all(|f| f.starts_with(&seismic_tag)),
        "{files:?}"
    );

    // Nor does a download drawn in it go to disk.
    let image = get(
        &app,
        "/api/v1/datasets/line-01/views/standard/image?profile=personal-mine&width=20",
    )
    .await;
    assert_eq!(image.status, StatusCode::OK, "{}", image.text());
    assert_eq!(overview_files(dir.path()), files);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_personal_thumbnail_falls_back_to_the_project_default() {
    let (_dir, app) = project_app();
    send(&app, "PUT", MINE, Some(&mine(None)), None).await;
    send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"default_profile": "abslog"})),
        None,
    )
    .await;
    let overview = |profile: &str| {
        format!("/api/v1/datasets/line-01/views/standard/overview?profile={profile}")
    };
    assert_eq!(
        get(&app, &overview("personal-mine")).await.bytes,
        get(&app, &overview("abslog")).await.bytes
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_personal_profile_is_checked_like_a_project_one() {
    let (_dir, app) = project_app();
    for (uri, body, code) in [
        (
            "/api/v1/preferences/render-profiles/mine",
            {
                let mut b = mine(None);
                b["name"] = json!("mine");
                b
            },
            "not_a_personal_profile_name",
        ),
        (MINE, mine(Some("nonexistent")), "unknown_thumbnail_profile"),
        (
            MINE,
            mine(Some("personal-mine")),
            "unknown_thumbnail_profile",
        ),
        (
            MINE,
            {
                let mut b = mine(None);
                b["contrast"] = json!(-1.0);
                b
            },
            "invalid_render_profile",
        ),
    ] {
        let answer = send(&app, "PUT", uri, Some(&body), None).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{uri} {body}");
        assert_eq!(answer.json()["error"]["code"], code, "{uri} {body}");
    }
    // A personal profile can be someone's choice but not the project's.
    send(&app, "PUT", MINE, Some(&mine(None)), None).await;
    let preference = send(
        &app,
        "PUT",
        "/api/v1/preferences",
        Some(&json!({"render_profile": "personal-mine"})),
        None,
    )
    .await;
    assert_eq!(preference.status, StatusCode::OK, "{}", preference.text());
    assert!(get(&app, "/")
        .await
        .text()
        .contains(r#"value="personal-mine" selected"#));
    let default = send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"default_profile": "personal-mine"})),
        None,
    )
    .await;
    assert_eq!(default.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn turning_personal_profiles_off_keeps_them_unused() {
    let (dir, app) = project_app();
    send(&app, "PUT", MINE, Some(&mine(None)), None).await;
    let off = send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"personal_profiles": false})),
        None,
    )
    .await;
    assert_eq!(off.status, StatusCode::OK, "{}", off.text());
    assert_eq!(off.json()["personal_profiles"], false);
    let toml = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    assert!(toml.contains("personal_profiles = false"), "{toml}");

    // Not rendered, not offered, not added to; but kept, and listed to
    // their owner as kept.
    assert_eq!(
        get(&app, &chunk("personal-mine")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert!(!names(&get(&app, PROFILES).await.json()).contains(&"personal-mine".to_string()));
    let page = get(&app, "/view/line-01?profile=personal-mine").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains(r#"value="default" selected"#));
    let refused = send(&app, "PUT", MINE, Some(&mine(None)), None).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.json()["error"]["code"], "personal_profiles_off");
    let kept = get(&app, "/api/v1/preferences/render-profiles")
        .await
        .json();
    assert_eq!(kept["allowed"], false);
    assert_eq!(names(&kept), vec!["personal-mine".to_string()]);

    // Back on, back as they were; and allowed is the default, stored as
    // nothing.
    send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"personal_profiles": true})),
        None,
    )
    .await;
    assert_eq!(
        get(&app, &chunk("personal-mine")).await.status,
        StatusCode::OK
    );
    let toml = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    assert!(!toml.contains("personal_profiles"), "{toml}");

    // What someone keeps is theirs to delete, whatever the switch says.
    send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"personal_profiles": false})),
        None,
    )
    .await;
    assert_eq!(
        send(&app, "DELETE", MINE, None, None).await.status,
        StatusCode::NO_CONTENT
    );
}
