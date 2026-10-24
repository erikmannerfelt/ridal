//! HTTP-level tests for a project's own render profiles (#183): the store
//! behind them is tested in `project::render_profiles`; these are about what
//! the rest of the server makes of them.
//!
//! Every test builds an `AppState`, which opens a NetCDF, so they carry the
//! `#[serial_test::serial(netcdf)]` guard the other route tests do.

use std::path::Path as StdPath;

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::Router;
use serde_json::{json, Value};

use super::app::AccessOptions;
use super::test_support::{
    self, mark_as_ridal, new_project, project_radargrams, project_router, request, Response,
};
use crate::project::Project;
use crate::render::profile::RenderProfile;

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
    mark_as_ridal(&mut file, radargram_id);
}

fn app_for(dir: &StdPath) -> Router {
    project_router(dir, AccessOptions::default())
}

fn project_app() -> (tempfile::TempDir, Router) {
    let dir = new_project();
    write_test_nc(
        &project_radargrams(dir.path()).join("line-01.nc"),
        "line-01",
    );
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

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    if_match: Option<&str>,
) -> Response {
    let mut builder = request(method, uri, None);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(value) = if_match {
        builder = builder.header(header::IF_MATCH, value);
    }
    let body = body.map_or(Body::empty(), |b| Body::from(b.to_string()));
    test_support::send(app, builder.body(body).unwrap()).await
}

async fn get(app: &Router, uri: &str) -> Response {
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
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);
    assert_eq!(created.body["scope"], "project");
    assert!(created.etag.is_some());

    // Listed after the built-ins, in full, and by name where the pages
    // and the older endpoint list them.
    let list = get(&app, PROFILES).await.body;
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
        .text
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
    let parsed: RenderProfile = toml::from_str(&built_in.text).unwrap();
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
    assert_eq!(stale.body["error"]["code"], "version_conflict");

    // The next tile is the edited profile's, not a cached one of the old.
    let after = get(&app, &chunk("survey")).await;
    assert_eq!(after.status, StatusCode::OK);
    assert_ne!(after.bytes, before);
    assert_eq!(get(&app, SURVEY).await.body["colormap"], Value::Null);
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
        assert_eq!(answer.status, status, "{name}: {}", answer.text);
        assert_eq!(answer.body["error"]["code"], code, "{name}");
    }

    // The body names the profile it is, and must agree with the path.
    let mut other = survey();
    other["name"] = json!("other");
    let mismatch = send(&app, "PUT", SURVEY, Some(&other), None).await;
    assert_eq!(
        mismatch.body["error"]["code"],
        "render_profile_name_mismatch"
    );

    // Values the profile cannot mean are refused, not clamped.
    let mut wild = survey();
    wild["contrast"] = json!(0.0);
    let refused = send(&app, "PUT", SURVEY, Some(&wild), None).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(refused.body["error"]["code"], "invalid_render_profile");
    assert!(refused.text.contains("contrast"), "{}", refused.text);

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
    assert_eq!(made_default.status, StatusCode::OK, "{}", made_default.text);
    assert_eq!(get(&app, PROFILES).await.body["default"], "survey");

    let refused = send(&app, "DELETE", SURVEY, None, None).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.body["error"]["code"], "render_profile_is_default");
    let built_in = send(&app, "DELETE", &format!("{PROFILES}/seismic"), None, None).await;
    assert_eq!(built_in.status, StatusCode::CONFLICT);
    assert_eq!(built_in.body["error"]["code"], "built_in_render_profile");

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
            answer.text.contains(r#"value="default" selected"#),
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
    assert_eq!(hidden.status, StatusCode::OK, "{}", hidden.text);
    assert_eq!(hidden.body["hidden_profiles"], json!(["abslog"]));

    let offered: Vec<String> =
        serde_json::from_slice(&get(&app, "/api/v1/profiles").await.bytes).unwrap();
    assert!(!offered.contains(&"abslog".to_string()));
    let list = get(&app, PROFILES).await.body;
    let abslog = list["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "abslog")
        .unwrap();
    assert_eq!(abslog["hidden"], true);

    // Links that name it keep working, and a page open on it still shows it.
    assert_eq!(get(&app, &chunk("abslog")).await.status, StatusCode::OK);
    let page = get(&app, "/?profile=abslog").await.text;
    assert!(page.contains(r#"value="abslog" selected"#));
    assert!(!get(&app, "/").await.text.contains(r#"value="abslog""#));
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
        assert_eq!(answer.body["error"]["code"], code, "{body}");
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
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.text);
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
    let listed = names(&get(&app, PROFILES).await.body);
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
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    assert_eq!(saved.body["scope"], "personal");
    assert_eq!(saved.body["thumbnail_profile"], "seismic");
    assert!(saved.etag.is_some());

    // Offered to its owner wherever a profile is chosen, and listed last.
    let listed = names(&get(&app, PROFILES).await.body);
    assert_eq!(listed.last().map(String::as_str), Some("personal-mine"));
    let mine_list = get(&app, "/api/v1/preferences/render-profiles").await.body;
    assert_eq!(mine_list["allowed"], true);
    assert_eq!(names(&mine_list), vec!["personal-mine".to_string()]);
    assert_eq!(
        get(&app, &chunk("personal-mine")).await.status,
        StatusCode::OK
    );
    let page = get(&app, "/view/line-01?profile=personal-mine").await.text;
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
    assert_eq!(image.status, StatusCode::OK, "{}", image.text);
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
        assert_eq!(answer.body["error"]["code"], code, "{uri} {body}");
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
    assert_eq!(preference.status, StatusCode::OK, "{}", preference.text);
    assert!(get(&app, "/")
        .await
        .text
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
    assert_eq!(off.status, StatusCode::OK, "{}", off.text);
    assert_eq!(off.body["personal_profiles"], false);
    let toml = std::fs::read_to_string(dir.path().join("ridal.toml")).unwrap();
    assert!(toml.contains("personal_profiles = false"), "{toml}");

    // Not rendered, not offered, not added to; but kept, and listed to
    // their owner as kept.
    assert_eq!(
        get(&app, &chunk("personal-mine")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert!(!names(&get(&app, PROFILES).await.body).contains(&"personal-mine".to_string()));
    let page = get(&app, "/view/line-01?profile=personal-mine").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text.contains(r#"value="default" selected"#));
    let refused = send(&app, "PUT", MINE, Some(&mine(None)), None).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.body["error"]["code"], "personal_profiles_off");
    let kept = get(&app, "/api/v1/preferences/render-profiles").await.body;
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

const PREVIEW: &str = "/api/v1/datasets/line-01/render-profile-preview";

fn preview_body(profile: Value) -> Value {
    json!({"profile": profile, "traces": [0, 40], "rows": [0, 8], "x_scale": 0.5, "width": 400})
}

fn png_size(bytes: &[u8]) -> (u32, u32) {
    let image = image::load_from_memory(bytes).unwrap();
    (image.width(), image.height())
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_preview_draws_an_unsaved_profile_and_keeps_nothing() {
    let (dir, app) = project_app();
    let grey = send(
        &app,
        "POST",
        PREVIEW,
        Some(&preview_body(
            serde_json::to_value(RenderProfile::default_profile()).unwrap(),
        )),
        None,
    )
    .await;
    assert_eq!(grey.status, StatusCode::OK, "{}", grey.text);
    assert_eq!(grey.content_type.as_deref(), Some("image/png"));
    // 40 traces at x_scale 0.5 for 8 rows: 400 px wide is 160 px tall.
    assert_eq!(png_size(&grey.bytes), (400, 160));

    // A profile nobody saved, under a name nobody has: drawn as sent.
    let draft = send(&app, "POST", PREVIEW, Some(&preview_body(survey())), None).await;
    assert_eq!(draft.status, StatusCode::OK, "{}", draft.text);
    assert_ne!(draft.bytes, grey.bytes);
    assert!(overview_files(dir.path()).is_empty());
    assert!(!names(&get(&app, PROFILES).await.body).contains(&"survey".to_string()));

    // Checked as a saved profile would be, and bounded in size.
    let mut bad = survey();
    bad["contrast"] = json!(-1.0);
    let refused = send(&app, "POST", PREVIEW, Some(&preview_body(bad)), None).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(refused.body["error"]["code"], "invalid_render_profile");
    let mut huge = preview_body(survey());
    huge["width"] = json!(5000);
    let refused = send(&app, "POST", PREVIEW, Some(&huge), None).await;
    assert_eq!(refused.body["error"]["code"], "invalid_preview");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn previews_follow_the_personal_profile_switch_below_operator() {
    let (dir, app) = project_app();
    let read_only = || {
        project_router(
            dir.path(),
            AccessOptions {
                read_only: true,
                ..AccessOptions::default()
            },
        )
    };
    let body = preview_body(survey());
    // A viewer may tune a profile of their own...
    let viewer = send(&read_only(), "POST", PREVIEW, Some(&body), None).await;
    assert_eq!(viewer.status, StatusCode::OK, "{}", viewer.text);

    send(
        &app,
        "PUT",
        SETTINGS,
        Some(&json!({"personal_profiles": false})),
        None,
    )
    .await;
    // ...while the project lets them keep one; an operator still may.
    let viewer = send(&read_only(), "POST", PREVIEW, Some(&body), None).await;
    assert_eq!(viewer.status, StatusCode::FORBIDDEN, "{}", viewer.text);
    let operator = send(&app, "POST", PREVIEW, Some(&body), None).await;
    assert_eq!(operator.status, StatusCode::OK, "{}", operator.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_pages_group_profiles_and_offer_the_editor() {
    let (_dir, app) = project_app();
    send(&app, "PUT", SURVEY, Some(&survey()), None).await;
    send(&app, "PUT", MINE, Some(&mine(None)), None).await;

    let viewer = get(&app, "/view/line-01?profile=personal-mine").await;
    assert_eq!(viewer.status, StatusCode::OK, "{}", viewer.text);
    let html = viewer.text;
    for group in ["Built-in", "Project", "Mine"] {
        assert!(
            html.contains(&format!(r#"<optgroup label="{group}">"#)),
            "{group}"
        );
    }
    // Listed under "Mine" without its prefix, and the colormapped project
    // profile marked for the display adjustment.
    assert!(html.contains(r#"<option value="personal-mine" selected>mine</option>"#));
    assert!(html.contains(r#"<option value="survey" data-colormap>survey</option>"#));
    assert!(html.contains(r#"id="profile-dialog""#));
    assert!(html.contains(r#"data-edit-personal="true""#));
    assert!(html.contains(r#"data-offset-default="-1.7""#));
    assert!(html.contains("<summary>Display: mine</summary>"));

    // A personal name a shared profile also has is told apart.
    let mut seismic = mine(None);
    seismic["name"] = json!("personal-seismic");
    let saved = send(
        &app,
        "PUT",
        "/api/v1/preferences/render-profiles/personal-seismic",
        Some(&seismic),
        None,
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text);
    let html = get(&app, "/view/line-01?profile=personal-seismic")
        .await
        .text;
    assert!(html.contains("<summary>Display: Personal: seismic</summary>"));
    assert!(html.contains("/static/profile_editor.js"));

    // The catalog's thumbnails are never personal, so neither are its
    // choices, but for the one the page is on.
    let catalog = get(&app, "/").await.text;
    assert!(catalog.contains(r#"value="survey""#));
    assert!(!catalog.contains("personal-mine"));
    let catalog = get(&app, "/?profile=personal-mine").await.text;
    assert!(catalog.contains(r#"value="personal-mine""#));

    let settings = get(&app, "/settings").await;
    assert_eq!(settings.status, StatusCode::OK);
    assert!(settings.text.contains(r#"id="render-profiles-section""#));
    assert!(settings.text.contains(r#"id="my-render-profiles""#));
}
