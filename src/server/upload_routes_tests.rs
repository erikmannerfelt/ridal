//! Uploads sent in chunks (#396), end to end through the router.

use std::path::Path as StdPath;

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::Router;
use serde_json::json;

use super::access_tests::{
    activated, data, delete, distinct_bytes, get, lifecycle_app_with_cap, open_project, post,
    post_bytes, radargram_bytes, request,
};
use super::test_support::{password_hash, send, sign_in, Response};
use crate::project::roles::{DownloadScope, Role};

fn app() -> (tempfile::TempDir, tempfile::TempDir, Router) {
    let hash = password_hash();
    lifecycle_app_with_cap(
        vec![
            activated("erik", Role::Operator, DownloadScope::All, &hash),
            activated("anna", Role::Operator, DownloadScope::All, &hash),
        ],
        None,
    )
}

/// What is left in `ridal_data/uploads/`.
fn upload_files(site: &StdPath) -> usize {
    std::fs::read_dir(data(site).join(crate::project::uploads::UPLOADS_DIR))
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

async fn start(app: &Router, name: &str, size: usize, session: &str) -> String {
    let started = post(
        app,
        "/api/v1/uploads",
        &json!({"name": name, "size": size}),
        Some(session),
    )
    .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.text);
    assert_eq!(started.body["offset"], 0);
    started.body["id"].as_str().unwrap().to_string()
}

fn chunk_request(
    id: &str,
    start: usize,
    length: usize,
    size: usize,
    session: &str,
) -> axum::http::request::Builder {
    request("PUT", &format!("/api/v1/uploads/{id}"), Some(session))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{size}", start + length - 1),
        )
}

async fn put_chunk(
    app: &Router,
    id: &str,
    start: usize,
    bytes: &[u8],
    size: usize,
    session: &str,
) -> Response {
    send(
        app,
        chunk_request(id, start, bytes.len(), size, session)
            .body(Body::from(bytes.to_vec()))
            .unwrap(),
    )
    .await
}

/// Send `bytes` as a new upload, `chunk` bytes at a time, and answer its id.
async fn upload_in_chunks(
    app: &Router,
    name: &str,
    bytes: &[u8],
    chunk: usize,
    session: &str,
) -> String {
    let id = start(app, name, bytes.len(), session).await;
    for (i, piece) in bytes.chunks(chunk).enumerate() {
        let sent = put_chunk(app, &id, i * chunk, piece, bytes.len(), session).await;
        assert_eq!(sent.status, StatusCode::OK, "{}", sent.text);
        assert_eq!(sent.body["offset"], i * chunk + piece.len());
    }
    id
}

async fn offset(app: &Router, id: &str, session: &str) -> u64 {
    let status = get(app, &format!("/api/v1/uploads/{id}"), Some(session)).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.text);
    status.body["offset"].as_u64().unwrap()
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_file_sent_in_chunks_is_stored_as_the_same_file_sent_whole() {
    let (dir, _archive, app) = app();
    let erik = sign_in(&app, "erik").await;
    let bytes = distinct_bytes(100_000, 1);

    let id = upload_in_chunks(&app, "DAT_0001_A1.rd3", &bytes, 30_000, &erik).await;
    let stored = post_bytes(
        &app,
        &format!("/api/v1/raw?upload={id}"),
        Vec::new(),
        Some(&erik),
    )
    .await;
    assert_eq!(stored.status, StatusCode::CREATED, "{}", stored.text);
    assert_eq!(stored.body["name"], "DAT_0001_A1.rd3");
    assert_eq!(stored.body["bytes"], 100_000);

    let whole = post_bytes(&app, "/api/v1/raw?filename=whole.rd3", bytes, Some(&erik)).await;
    assert_eq!(whole.status, StatusCode::CREATED, "{}", whole.text);
    assert_eq!(stored.body["blake3"], whole.body["blake3"]);

    // Used, the upload is gone, and the file is in the store once.
    assert_eq!(upload_files(dir.path()), 0);
    let gone = get(&app, &format!("/api/v1/uploads/{id}"), Some(&erik)).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{}", gone.text);
    assert_eq!(gone.body["error"]["code"], "no_such_upload");
    let again = post_bytes(
        &app,
        &format!("/api/v1/raw?upload={id}"),
        Vec::new(),
        Some(&erik),
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND, "{}", again.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_chunk_must_start_where_the_upload_has_got_to() {
    let (_dir, _archive, app) = app();
    let erik = sign_in(&app, "erik").await;
    let bytes = distinct_bytes(20, 2);
    let id = start(&app, "a.rd3", 20, &erik).await;
    assert_eq!(
        put_chunk(&app, &id, 0, &bytes[..10], 20, &erik)
            .await
            .status,
        StatusCode::OK
    );

    for wrong in [0, 5, 15] {
        let refused = put_chunk(&app, &id, wrong, &bytes[wrong..wrong + 5], 20, &erik).await;
        assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.text);
        assert_eq!(refused.body["error"]["code"], "offset_mismatch");
    }
    assert_eq!(offset(&app, &id, &erik).await, 10);

    // Not fitting the declared size, or a range that is not one.
    let past = put_chunk(&app, &id, 10, &distinct_bytes(11, 3), 20, &erik).await;
    assert_eq!(past.body["error"]["code"], "size_mismatch", "{}", past.text);
    let other_size = put_chunk(&app, &id, 10, &bytes[10..], 21, &erik).await;
    assert_eq!(
        other_size.body["error"]["code"], "size_mismatch",
        "{}",
        other_size.text
    );
    let no_range = send(
        &app,
        request("PUT", &format!("/api/v1/uploads/{id}"), Some(&erik))
            .body(Body::from(bytes[10..].to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(no_range.status, StatusCode::BAD_REQUEST);
    assert_eq!(no_range.body["error"]["code"], "invalid_content_range");
    assert_eq!(offset(&app, &id, &erik).await, 10);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_chunk_that_breaks_off_keeps_what_arrived_and_the_upload_resumes_from_there() {
    let (_dir, _archive, app) = app();
    let erik = sign_in(&app, "erik").await;
    let bytes = distinct_bytes(1000, 4);
    let id = start(&app, "a.rd3", 1000, &erik).await;

    let broken = futures_util::stream::iter(vec![
        Ok(bytes[..300].to_vec()),
        Err(std::io::Error::other("the laptop went to sleep")),
    ]);
    let interrupted = send(
        &app,
        chunk_request(&id, 0, 1000, 1000, &erik)
            .body(Body::from_stream(broken))
            .unwrap(),
    )
    .await;
    assert_eq!(
        interrupted.status,
        StatusCode::BAD_REQUEST,
        "{}",
        interrupted.text
    );
    assert_eq!(interrupted.body["error"]["code"], "upload_interrupted");
    assert_eq!(offset(&app, &id, &erik).await, 300);

    // A body shorter than its range keeps what came, too.
    let short = send(
        &app,
        chunk_request(&id, 300, 400, 1000, &erik)
            .body(Body::from(bytes[300..500].to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(
        short.body["error"]["code"], "range_mismatch",
        "{}",
        short.text
    );
    assert_eq!(offset(&app, &id, &erik).await, 500);

    // A longer one keeps only what its range says.
    let long = send(
        &app,
        chunk_request(&id, 500, 100, 1000, &erik)
            .body(Body::from(bytes[500..800].to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(
        long.body["error"]["code"], "range_mismatch",
        "{}",
        long.text
    );
    assert_eq!(offset(&app, &id, &erik).await, 600);

    let rest = put_chunk(&app, &id, 600, &bytes[600..], 1000, &erik).await;
    assert_eq!(rest.status, StatusCode::OK, "{}", rest.text);
    let stored = post_bytes(
        &app,
        &format!("/api/v1/raw?upload={id}"),
        Vec::new(),
        Some(&erik),
    )
    .await;
    assert_eq!(stored.status, StatusCode::CREATED, "{}", stored.text);
    assert_eq!(
        stored.body["blake3"],
        blake3::hash(&bytes).to_hex().to_string()
    );
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_upload_is_only_its_senders() {
    let (_dir, _archive, app) = app();
    let erik = sign_in(&app, "erik").await;
    let anna = sign_in(&app, "anna").await;
    let bytes = distinct_bytes(10, 5);
    let id = upload_in_chunks(&app, "a.rd3", &bytes, 10, &erik).await;

    let uri = format!("/api/v1/uploads/{id}");
    for refused in [
        get(&app, &uri, Some(&anna)).await,
        put_chunk(&app, &id, 10, &bytes, 10, &anna).await,
        post_bytes(
            &app,
            &format!("/api/v1/raw?upload={id}"),
            Vec::new(),
            Some(&anna),
        )
        .await,
        post_bytes(
            &app,
            &format!("/api/v1/datasets?upload={id}"),
            Vec::new(),
            Some(&anna),
        )
        .await,
        delete(&app, &uri, Some(&anna)).await,
    ] {
        assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.text);
        assert_eq!(refused.body["error"]["code"], "no_such_upload");
    }
    assert_eq!(offset(&app, &id, &erik).await, 10);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn an_unfinished_upload_cannot_be_used_and_a_cancelled_one_is_gone() {
    let (dir, _archive, app) = app();
    let erik = sign_in(&app, "erik").await;
    let bytes = distinct_bytes(20, 6);
    let id = start(&app, "a.rd3", 20, &erik).await;
    put_chunk(&app, &id, 0, &bytes[..10], 20, &erik).await;

    let early = post_bytes(
        &app,
        &format!("/api/v1/raw?upload={id}"),
        Vec::new(),
        Some(&erik),
    )
    .await;
    assert_eq!(early.status, StatusCode::CONFLICT, "{}", early.text);
    assert_eq!(early.body["error"]["code"], "upload_incomplete");
    assert_eq!(offset(&app, &id, &erik).await, 10);

    let cancelled = delete(&app, &format!("/api/v1/uploads/{id}"), Some(&erik)).await;
    assert_eq!(
        cancelled.status,
        StatusCode::NO_CONTENT,
        "{}",
        cancelled.text
    );
    assert_eq!(upload_files(dir.path()), 0);
    let late = put_chunk(&app, &id, 10, &bytes[10..], 20, &erik).await;
    assert_eq!(late.status, StatusCode::NOT_FOUND, "{}", late.text);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn the_size_limit_is_checked_up_front_and_for_each_chunk() {
    let hash = password_hash();
    let cap = 1024 * 1024;
    let (dir, _archive, app) = lifecycle_app_with_cap(
        vec![activated("erik", Role::Operator, DownloadScope::All, &hash)],
        Some(cap),
    );
    let erik = sign_in(&app, "erik").await;
    let room = (cap - open_project(dir.path()).size_bytes()) as usize;

    let too_big = post(
        &app,
        "/api/v1/uploads",
        &json!({"name": "a.tif", "size": room + 1}),
        Some(&erik),
    )
    .await;
    assert_eq!(
        too_big.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        too_big.text
    );
    assert_eq!(too_big.body["error"]["code"], "project_full");

    // Two that each fit alone: the second's chunks stop fitting once the
    // first has taken the room.
    let size = room * 6 / 10;
    let first = start(&app, "first.tif", size, &erik).await;
    let second = start(&app, "second.tif", size, &erik).await;
    let ok = put_chunk(
        &app,
        &first,
        0,
        &distinct_bytes(size as u64, 7),
        size,
        &erik,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    let full = put_chunk(
        &app,
        &second,
        0,
        &distinct_bytes(size as u64, 8),
        size,
        &erik,
    )
    .await;
    assert_eq!(full.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", full.text);
    assert_eq!(full.body["error"]["code"], "project_full");
    assert_eq!(offset(&app, &second, &erik).await, 0);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_chunk_over_the_limit_is_refused_before_it_is_read() {
    let (_dir, _archive, app) = app();
    let erik = sign_in(&app, "erik").await;
    let huge = super::upload_routes::MAX_CHUNK_BYTES as usize + 1;
    let id = start(&app, "huge.tif", huge, &erik).await;
    let refused = send(
        &app,
        chunk_request(&id, 0, huge, huge, &erik)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        refused.text
    );
    assert_eq!(refused.body["error"]["code"], "chunk_too_large");
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn two_requests_cannot_send_to_one_upload_at_once() {
    let (_dir, _archive, app) = app();
    let erik = sign_in(&app, "erik").await;
    let bytes = distinct_bytes(2000, 9);
    let id = start(&app, "a.rd3", 2000, &erik).await;

    let (sender, receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), receiver))
    });
    let held = chunk_request(&id, 0, 1000, 2000, &erik)
        .body(Body::from_stream(stream))
        .unwrap();
    let slow = {
        let app = app.clone();
        tokio::spawn(async move { send(&app, held).await })
    };
    sender.send(bytes[..500].to_vec()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let busy = put_chunk(&app, &id, 0, &bytes[..1000], 2000, &erik).await;
    assert_eq!(busy.status, StatusCode::CONFLICT, "{}", busy.text);
    assert_eq!(busy.body["error"]["code"], "upload_busy");
    let used = post_bytes(
        &app,
        &format!("/api/v1/raw?upload={id}"),
        Vec::new(),
        Some(&erik),
    )
    .await;
    assert_eq!(used.body["error"]["code"], "upload_busy", "{}", used.text);

    sender.send(bytes[500..1000].to_vec()).await.unwrap();
    drop(sender);
    let slow = slow.await.unwrap();
    assert_eq!(slow.status, StatusCode::OK, "{}", slow.text);
    assert_eq!(offset(&app, &id, &erik).await, 1000);
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn a_radargram_sent_in_chunks_is_added_and_replaces() {
    let (dir, _archive, app) = app();
    let erik = sign_in(&app, "erik").await;

    let new = radargram_bytes("new");
    let id = upload_in_chunks(&app, "new.nc", &new, new.len() / 3 + 1, &erik).await;
    let added = post_bytes(
        &app,
        &format!("/api/v1/datasets?upload={id}"),
        Vec::new(),
        Some(&erik),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.text);
    assert_eq!(added.body["radargram_id"], "new");
    assert_eq!(added.body["bytes"], new.len());

    let ours = radargram_bytes("ours");
    let id = upload_in_chunks(&app, "ours.nc", &ours, ours.len() / 2 + 1, &erik).await;
    let staged = post_bytes(
        &app,
        &format!("/api/v1/datasets/ours/replace?upload={id}"),
        Vec::new(),
        Some(&erik),
    )
    .await;
    assert_eq!(staged.status, StatusCode::OK, "{}", staged.text);
    assert_eq!(staged.body["bytes"], ours.len());

    // Refused, it is gone all the same, as a body would be.
    let id = upload_in_chunks(&app, "again.nc", &new, new.len(), &erik).await;
    let taken = post_bytes(
        &app,
        &format!("/api/v1/datasets?upload={id}"),
        Vec::new(),
        Some(&erik),
    )
    .await;
    assert_eq!(taken.status, StatusCode::CONFLICT, "{}", taken.text);
    assert_eq!(taken.body["error"]["code"], "radargram_exists");
    assert_eq!(upload_files(dir.path()), 0);
}
