//! HTTP-level tests for moving picks between radargrams (#500).
//!
//! The unconfigured project here makes every request the local `default`
//! user, an operator: the `ridal gui` case, where every picks file is the
//! caller's own. Who may move someone else's picks is in `access_tests`.
//!
//! Every test builds an `AppState`, which opens a NetCDF, so they carry the
//! same `#[serial_test::serial(netcdf)]` guard as the other route tests.

use std::path::Path as StdPath;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};

use super::app::AccessOptions;
use super::interp_routes_tests::{write_test_nc_uncorrected, write_test_nc_with_axes};
use super::test_support::{self, project_data, project_router, Response};
use crate::project::Project;

fn radargram_dir(root: &StdPath) -> std::path::PathBuf {
    project_data(root).join(crate::project::DEFAULT_RADARGRAM_DIR)
}

/// A project with `line-01` and `new-line`, whose grids match unless
/// `different_grid`.
fn project_app(different_grid: bool) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    Project::init(dir.path(), Some("test")).unwrap();
    let radargrams = radargram_dir(dir.path());
    write_test_nc_with_axes(&radargrams.join("line-01.nc"), "line-01", None);
    if different_grid {
        write_test_nc_uncorrected(
            &radargrams.join("new-line.nc"),
            "new-line",
            "2021-01-01T00:00:00Z",
        );
    } else {
        write_test_nc_with_axes(&radargrams.join("new-line.nc"), "new-line", None);
    }
    let app = project_router(dir.path(), AccessOptions::default());
    (dir, app)
}

async fn get(app: &Router, uri: &str) -> Response {
    test_support::send(
        app,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

async fn send_json(app: &Router, method: &str, uri: &str, body: &Value) -> Response {
    test_support::send(app, test_support::json_request(method, uri, body, None)).await
}

async fn revision_of(app: &Router, radargram: &str) -> String {
    let detail = get(app, &format!("/api/v1/datasets/{radargram}")).await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.text);
    detail.body["revision_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no revision_id: {}", detail.text))
        .to_string()
}

/// One `bed` line, drawn on `revision`.
fn document(key: &str, revision: &str) -> Value {
    json!({
        "key": key,
        "date_modified": chrono::Utc::now().to_rfc3339(),
        "source": {"id": key, "revision_id": revision},
        "features": [
            {"type": "Feature",
             "geometry": {"type": "LineString", "coordinates": [[5.0, 2.0], [14.0, 3.0]]},
             "properties": {"id": "f-0001", "label": "bed"}}
        ]
    })
}

fn interpretation_uri(radargram: &str) -> String {
    format!("/api/v1/datasets/{radargram}/interpretations/default")
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_change_of_id_moves_the_picks_and_a_late_save_is_told_where() {
    let (dir, app) = project_app(false);
    let old_revision = revision_of(&app, "line-01").await;
    let new_revision = revision_of(&app, "new-line").await;
    let saved = send_json(
        &app,
        "PUT",
        &interpretation_uri("line-01"),
        &document("line-01", &old_revision),
    )
    .await;
    assert!(saved.status.is_success(), "{}", saved.text);

    let moved = send_json(
        &app,
        "POST",
        "/api/v1/datasets/line-01/transfer",
        &json!({"to": "new-line", "remove_source": true}),
    )
    .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.text);
    assert_eq!(moved.body["same_grid"], true, "{}", moved.text);
    assert_eq!(moved.body["moved"][0]["user"], "default");
    assert_eq!(moved.body["moved"][0]["current"], true);
    assert_eq!(moved.body["removed"]["outcome"], "removed");
    // The originals go to the archive with the removal, as always.
    assert_eq!(moved.body["removed"]["archived"], 1);

    let there = get(&app, &interpretation_uri("new-line")).await;
    assert_eq!(there.status, StatusCode::OK, "{}", there.text);
    assert_eq!(there.body["key"], "new-line");
    assert_eq!(there.body["source"]["id"], "new-line");
    assert_eq!(there.body["source"]["revision_id"], new_revision.as_str());
    assert_eq!(
        there.body["meta"]["ridal_transferred_from"]["radargram_id"],
        "line-01"
    );
    assert!(!radargram_dir(dir.path()).join("line-01.nc").exists());

    // A picker still open on the old id saves into nothing, and is told
    // where the picks went rather than only that the radargram is gone.
    let late = send_json(
        &app,
        "PUT",
        &interpretation_uri("line-01"),
        &document("line-01", &old_revision),
    )
    .await;
    assert_eq!(late.status, StatusCode::NOT_FOUND, "{}", late.text);
    assert_eq!(late.body["error"]["code"], "dataset_moved");
    assert!(late.text.contains("new-line"), "{}", late.text);

    let project = Project::discover(dir.path()).unwrap().unwrap();
    let entries = crate::project::audit::read(project.documents()).unwrap();
    let transfer = entries
        .iter()
        .find(|e| e.action == crate::project::audit::Action::Transferred)
        .unwrap();
    assert_eq!(transfer.radargram_id, "line-01");
    assert_eq!(transfer.to_radargram_id.as_deref(), Some("new-line"));
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn picks_on_a_different_grid_are_carried_onto_the_new_radargram() {
    // The version-bump case: the new radargram was processed differently,
    // so the picks keep the revision they were drawn on and are carried,
    // as after a reprocess, through axes copied under the new id.
    let (dir, app) = project_app(true);
    let old_revision = revision_of(&app, "line-01").await;
    let saved = send_json(
        &app,
        "PUT",
        &interpretation_uri("line-01"),
        &document("line-01", &old_revision),
    )
    .await;
    assert!(saved.status.is_success(), "{}", saved.text);

    let moved = send_json(
        &app,
        "POST",
        "/api/v1/datasets/line-01/transfer",
        &json!({"to": "new-line", "remove_source": true}),
    )
    .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.text);
    assert_eq!(moved.body["same_grid"], false, "{}", moved.text);
    assert_eq!(moved.body["moved"][0]["current"], false);
    assert_eq!(moved.body["moved"][0]["revision_id"], old_revision.as_str());

    let project = Project::discover(dir.path()).unwrap().unwrap();
    let new_line = crate::identity::RadargramId::new("new-line").unwrap();
    assert!(crate::project::revisions::has(
        project.documents(),
        &new_line,
        &old_revision
    ));

    let carried = get(&app, &format!("{}/carried", interpretation_uri("new-line"))).await;
    assert_eq!(carried.status, StatusCode::OK, "{}", carried.text);
    assert_ne!(
        carried.body["report"]["severity"], "refused",
        "{}",
        carried.text
    );
    assert_eq!(
        carried.body["report"]["from_revision"],
        old_revision.as_str()
    );

    // And its owner adopts them there as after a reprocess.
    let new_revision = revision_of(&app, "new-line").await;
    let stored = get(&app, &interpretation_uri("new-line")).await;
    let request = Request::builder()
        .method("POST")
        .uri(format!(
            "{}/promote?onto={new_revision}",
            interpretation_uri("new-line")
        ))
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::IF_MATCH, stored.etag.unwrap())
        .body(Body::from(carried.body["document"].to_string()))
        .unwrap();
    let adopted = test_support::send(&app, request).await;
    assert_eq!(adopted.status, StatusCode::OK, "{}", adopted.text);
    assert_eq!(adopted.body["to_revision"], new_revision.as_str());
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_transfer_never_replaces_picks_and_can_be_retried() {
    let (_dir, app) = project_app(false);
    let old_revision = revision_of(&app, "line-01").await;
    let new_revision = revision_of(&app, "new-line").await;
    for (radargram, revision) in [("line-01", &old_revision), ("new-line", &new_revision)] {
        let saved = send_json(
            &app,
            "PUT",
            &interpretation_uri(radargram),
            &document(radargram, revision),
        )
        .await;
        assert!(saved.status.is_success(), "{}", saved.text);
    }

    let moved = send_json(
        &app,
        "POST",
        "/api/v1/datasets/line-01/transfer",
        &json!({"to": "new-line"}),
    )
    .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.text);
    assert_eq!(moved.body["moved"], json!([]));
    assert_eq!(moved.body["occupied"], json!(["default"]));
    assert_eq!(moved.body["removed"], Value::Null);
    let still = get(&app, &interpretation_uri("line-01")).await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.text);

    for (body, code) in [
        (json!({"to": "line-01"}), "same_radargram"),
        (json!({"to": "Not An Id"}), "invalid_radargram_id"),
    ] {
        let refused = send_json(&app, "POST", "/api/v1/datasets/line-01/transfer", &body).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.text);
        assert_eq!(refused.body["error"]["code"], code);
    }
    let missing = send_json(
        &app,
        "POST",
        "/api/v1/datasets/line-01/transfer",
        &json!({"to": "no-such-line"}),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn activity_says_who_saved_or_opened_the_viewer() {
    let (_dir, app) = project_app(false);
    let quiet = get(&app, "/api/v1/datasets/line-01/activity").await;
    assert_eq!(quiet.status, StatusCode::OK, "{}", quiet.text);
    assert_eq!(quiet.body["level"], "none");
    assert_eq!(quiet.body["you"]["opened_at"], Value::Null);
    assert_eq!(quiet.body["you"]["saved_at"], Value::Null);

    let page = get(&app, "/view/line-01").await;
    assert_eq!(page.status, StatusCode::OK);
    let revision = revision_of(&app, "line-01").await;
    let saved = send_json(
        &app,
        "PUT",
        &interpretation_uri("line-01"),
        &document("line-01", &revision),
    )
    .await;
    assert!(saved.status.is_success(), "{}", saved.text);

    // The caller's own work is listed apart, and is no reason for the
    // stronger confirmations: one person on a project is the common case.
    let mine = get(&app, "/api/v1/datasets/line-01/activity").await;
    assert_eq!(mine.body["level"], "none", "{}", mine.text);
    assert!(mine.body["you"]["opened_at"].is_string(), "{}", mine.text);
    assert!(mine.body["you"]["saved_at"].is_string(), "{}", mine.text);
    assert_eq!(mine.body["others"], json!([]));
    assert_eq!(mine.body["may_transfer"], true);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_copy_under_a_new_id_takes_the_picks_as_current() {
    // The whole change of id, as the catalog's dialog makes it: copy, then
    // transfer with removal. A copy's grid is the original's, so the picks
    // arrive current, with nothing to adopt.
    let (dir, app) = project_app(false);
    let old_revision = revision_of(&app, "line-01").await;
    let named = send_json(
        &app,
        "PUT",
        "/api/v1/datasets/line-01/properties",
        &json!({"display_name": "Line one"}),
    )
    .await;
    assert!(named.status.is_success(), "{}", named.text);
    let saved = send_json(
        &app,
        "PUT",
        &interpretation_uri("line-01"),
        &document("line-01", &old_revision),
    )
    .await;
    assert!(saved.status.is_success(), "{}", saved.text);

    let copied = send_json(
        &app,
        "POST",
        "/api/v1/datasets/line-01/copy",
        &json!({"to": "line-one"}),
    )
    .await;
    assert_eq!(copied.status, StatusCode::CREATED, "{}", copied.text);
    assert_eq!(copied.body["from"], "line-01");
    assert_eq!(copied.body["copied"], json!(["catalog_settings"]));
    assert_eq!(copied.body["warnings"], json!([]));
    let new_revision = revision_of(&app, "line-one").await;
    assert_eq!(copied.body["revision_id"], new_revision.as_str());
    assert_ne!(new_revision, old_revision);
    let detail = get(&app, "/api/v1/datasets/line-one").await;
    assert_eq!(detail.body["display_name"], "Line one", "{}", detail.text);
    // The id is only ever in the file's attributes, and the copy says so.
    let crate::io::RidalNetcdfKind::Supported(meta) =
        crate::io::inspect_ridal_netcdf(&radargram_dir(dir.path()).join("line-one.nc")).unwrap()
    else {
        panic!("the copy is not a Ridal radargram");
    };
    assert_eq!(meta.radargram_id.as_str(), "line-one");
    // No temporary file is left where discovery would look.
    let leftovers: Vec<_> = std::fs::read_dir(radargram_dir(dir.path()))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let moved = send_json(
        &app,
        "POST",
        "/api/v1/datasets/line-01/transfer",
        &json!({"to": "line-one", "remove_source": true}),
    )
    .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.text);
    assert_eq!(moved.body["same_grid"], true, "{}", moved.text);
    assert_eq!(moved.body["moved"][0]["current"], true);

    let catalog = get(&app, "/api/v1/datasets").await;
    let ids: Vec<&str> = catalog.body["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("{}", catalog.text))
        .iter()
        .filter_map(|d| d["radargram_id"].as_str())
        .collect();
    assert!(
        ids.contains(&"line-one") && !ids.contains(&"line-01"),
        "{ids:?}"
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_copy_never_takes_an_id_in_use() {
    let (_dir, app) = project_app(false);
    let taken = send_json(
        &app,
        "POST",
        "/api/v1/datasets/line-01/copy",
        &json!({"to": "new-line"}),
    )
    .await;
    assert_eq!(taken.status, StatusCode::CONFLICT, "{}", taken.text);
    assert_eq!(taken.body["error"]["code"], "radargram_exists");
    let invalid = send_json(
        &app,
        "POST",
        "/api/v1/datasets/line-01/copy",
        &json!({"to": "Line 1"}),
    )
    .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST, "{}", invalid.text);
    let missing = send_json(
        &app,
        "POST",
        "/api/v1/datasets/no-such-line/copy",
        &json!({"to": "line-02"}),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.text);
}
