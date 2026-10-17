//! Every operation in the OpenAPI description needs what its `x-ridal-needs`
//! says (#330), no more and no less.
//!
//! The *Needs* column of the HTTP reference is rendered from that
//! declaration, and the handler checks something else entirely
//! (`Caller::require`, `Caller::require_download`, or nothing at all). This
//! ties the two: each operation is sent the same request by callers just
//! below what it declares, who must be refused, and by one exactly at it,
//! who must succeed. An operation stricter than declared fails the second;
//! a laxer one, the first. A request that succeeds at the declared level is
//! also one whose refusal below it is about the caller, not about the
//! request.
//!
//! Each operation needs a [`Case`]: what to put in its query and body, and
//! anything to set up first. One missing fails the test, so a route cannot
//! be added to the spec without one.

use std::path::Path as StdPath;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::access_tests::{
    activated, id, new_site, password, password_hash, project_key, radargrams, site_router,
    write_people, UserSet, PROJECT,
};
use super::app::AccessOptions;
use super::auth::{self, SessionKey, SESSION_COOKIE};
use super::needs::{Needs, Who};
use crate::project::roles::{DownloadScope, Role};
use crate::site::accounts::{self, invite};
use crate::site::Site;

/// The radargram every site holds, in [`GROUP`].
const RADARGRAM: &str = "line-01";
const GROUP: &str = "grp";
const N_TRACES: usize = 50;
const N_SAMPLES: usize = 5;

/// The account that holds a pending invite, for the operation that
/// redeems one.
const INVITEE: &str = "invitee";

/// The value of each path parameter, unless a case's setup answers with a
/// field of the same name.
const PARAMETERS: &[(&str, &str)] = &[
    ("key", PROJECT),
    ("radargram_id", RADARGRAM),
    ("group", GROUP),
    ("view", "standard"),
    ("trace", "0"),
    ("profile", "default"),
    ("x", "0"),
    ("y", "0"),
];

/// What a [`Case`] can build its request from.
struct Fixture {
    /// The token of [`INVITEE`]'s invite.
    invite_token: String,
    /// A processed file of another radargram, to upload.
    upload: Vec<u8>,
    /// A later revision of [`RADARGRAM`], to replace it with.
    revision: Vec<u8>,
}

/// A request body.
enum Payload {
    None,
    Json(Value),
    Netcdf(Vec<u8>),
}

/// A request a case sends as an `admin` before the probe, such as staging
/// the replacement it then commits: method, path, body.
type Setup = (&'static str, &'static str, fn(&Fixture) -> Payload);

/// What to send to one operation, by `operationId`.
struct Case {
    operation: &'static str,
    /// Appended to the path, without the `?`.
    query: &'static str,
    body: fn(&Fixture) -> Payload,
    /// Changes to the site's files before the server is built over it.
    prepare: Option<fn(&StdPath)>,
    /// Its JSON answer supplies path parameters by name.
    setup: Option<Setup>,
}

impl Case {
    const fn new(operation: &'static str) -> Self {
        Self {
            operation,
            query: "",
            body: |_| Payload::None,
            prepare: None,
            setup: None,
        }
    }

    const fn body(self, body: fn(&Fixture) -> Payload) -> Self {
        Self { body, ..self }
    }

    const fn query(self, query: &'static str) -> Self {
        Self { query, ..self }
    }

    const fn prepare(self, prepare: fn(&StdPath)) -> Self {
        Self {
            prepare: Some(prepare),
            ..self
        }
    }

    const fn setup(self, setup: Setup) -> Self {
        Self {
            setup: Some(setup),
            ..self
        }
    }
}

/// A later revision of [`RADARGRAM`], as an upload.
fn revision(fixture: &Fixture) -> Payload {
    Payload::Netcdf(fixture.revision.clone())
}

/// Staging a replacement, for the cases that commit or discard one.
const STAGE: Setup = (
    "POST",
    "/api/v1/projects/{key}/datasets/{radargram_id}/replace",
    revision,
);

/// One per operation in the spec.
const CASES: &[Case] = &[
    // Server.
    Case::new("health"),
    Case::new("openapi_json"),
    Case::new("list_profiles"),
    // Signing in.
    Case::new("me"),
    Case::new("login")
        .body(|_| Payload::Json(json!({"name": "viewer-none", "password": password()}))),
    Case::new("logout"),
    Case::new("redeem_invite").body(|fixture| {
        Payload::Json(json!({"token": fixture.invite_token, "password": "a long new passphrase"}))
    }),
    // Preferences and project settings.
    Case::new("get_preferences"),
    Case::new("put_preferences").body(|_| Payload::Json(json!({"theme": "dark"}))),
    Case::new("get_settings"),
    Case::new("put_settings").body(|_| Payload::Json(json!({"default_profile": null}))),
    // Radargrams.
    Case::new("list_datasets"),
    Case::new("upload_dataset")
        .query("filename=line-02.nc")
        .body(|fixture| Payload::Netcdf(fixture.upload.clone())),
    Case::new("dataset_detail"),
    Case::new("remove_dataset"),
    Case::new("dataset_attributes"),
    Case::new("dataset_axes"),
    Case::new("dataset_gprinterp_axes"),
    Case::new("dataset_trace"),
    Case::new("dataset_track"),
    Case::new("dataset_topo_geometry"),
    Case::new("list_revisions"),
    Case::new("get_properties"),
    Case::new("put_properties").body(|_| Payload::Json(json!({"unlisted": false}))),
    Case::new("preflight_replacement").body(|_| {
        Payload::Json(json!({
            "radargram_id": RADARGRAM,
            "processing_datetime": "2021-01-01T00:00:00Z",
            "recipe_hash": null,
            "time": (0..N_TRACES).map(|i| i as f64 * 0.1).collect::<Vec<_>>(),
            "twtt_anchor": null,
            "twtt_crop": [],
            "twtt_time_zero": [],
            "dt_ns": 0.0,
            "n_samples": N_SAMPLES,
        }))
    }),
    Case::new("stage_replacement")
        .query("filename=line-01.nc")
        .body(revision),
    Case::new("commit_replacement").setup(STAGE),
    Case::new("discard_replacement").setup(STAGE),
    Case::new("restore_dataset").prepare(ignore_radargram),
    Case::new("list_ignored"),
    // Groups.
    Case::new("group_tracks"),
    Case::new("get_group_properties"),
    Case::new("put_group_properties")
        .body(|_| Payload::Json(json!({"display_name": "The glacier"}))),
    // Images.
    Case::new("overview_image"),
    Case::new("chunk_image"),
    Case::new("dataset_image"),
    Case::new("render_figure").body(|_| Payload::Json(json!({}))),
    // Downloads.
    Case::new("dataset_download"),
    Case::new("dataset_track_geojson"),
    Case::new("group_track_geojson"),
    Case::new("catalog_track_geojson"),
];

/// Who sends a probe.
#[derive(Debug, Clone, Copy)]
enum Caller {
    Anonymous,
    /// A member of the project with this role and download scope, signed
    /// in.
    Member(Role, DownloadScope),
}

/// One request to send, and what must come back.
#[derive(Debug)]
struct Probe {
    caller: Caller,
    /// Whether the project requires a login to read.
    private: bool,
    /// `None` for success, or the refusal's status.
    refused: Option<StatusCode>,
}

/// The one just below `value` in `ladder`, if there is one.
fn below<T: Copy + PartialOrd>(ladder: &[T], value: T) -> Option<T> {
    ladder.iter().copied().take_while(|v| *v < value).last()
}

/// The probes a declaration implies: one at it, and one just below it on
/// each axis it names.
fn probes(needs: &Needs) -> Vec<Probe> {
    let at = |caller, private| Probe {
        caller,
        private,
        refused: None,
    };
    let refused = |caller, private, status| Probe {
        caller,
        private,
        refused: Some(status),
    };
    let download = needs.download.unwrap_or(DownloadScope::None);
    let role = match needs.who {
        Who::Role(role) => role,
        _ => Role::Viewer,
    };
    let mut probes = match needs.who {
        // Even in a project that requires a login, nobody is refused.
        Who::Nobody => vec![at(Caller::Anonymous, true)],
        Who::Anyone => vec![
            refused(Caller::Anonymous, true, StatusCode::UNAUTHORIZED),
            at(Caller::Member(role, download), true),
        ],
        // A public project, so that being able to read it is not the reason.
        Who::SignedIn => vec![
            refused(Caller::Anonymous, false, StatusCode::UNAUTHORIZED),
            at(Caller::Member(role, download), false),
        ],
        Who::Role(_) => {
            let lower = below(&Role::ALL, role)
                .expect("an endpoint anyone who may read may call says `anyone`");
            vec![
                refused(
                    Caller::Member(lower, download),
                    false,
                    StatusCode::FORBIDDEN,
                ),
                at(Caller::Member(role, download), false),
            ]
        }
    };
    if let Some(lower) = below(&DownloadScope::ALL, download) {
        probes.push(refused(
            Caller::Member(role, lower),
            false,
            StatusCode::FORBIDDEN,
        ));
    }
    probes
}

/// The account of the member with `role` and `download`.
fn member_name(role: Role, download: DownloadScope) -> String {
    format!("{}-{}", role.as_str(), download.as_str())
}

/// A processed radargram in [`GROUP`], with a track, an elevation and a
/// depth axis, so that every view of it can be drawn.
fn write_radargram(path: &StdPath, radargram_id: &str, processed: &str) {
    let mut file = netcdf::create(path).unwrap();
    file.add_dimension("y", N_SAMPLES).unwrap();
    file.add_dimension("x", N_TRACES).unwrap();
    let mut data = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
    let values: Vec<f32> = (0..N_SAMPLES * N_TRACES).map(|i| (i % 7) as f32).collect();
    data.put_values(&values, ..).unwrap();
    let per_trace: [(&str, Vec<f64>); 4] = [
        (
            "easting",
            (0..N_TRACES).map(|i| 500_000.0 + i as f64).collect(),
        ),
        ("northing", vec![8_000_000.0; N_TRACES]),
        ("time", (0..N_TRACES).map(|i| i as f64 * 0.1).collect()),
        (
            "elevation",
            (0..N_TRACES)
                .map(|i| 100.0 + (i as f64 * 0.1).sin())
                .collect(),
        ),
    ];
    for (name, values) in per_trace {
        let mut variable = file.add_variable::<f64>(name, &["x"]).unwrap();
        variable.put_values(&values, ..).unwrap();
    }
    let mut depth = file.add_variable::<f32>("depth", &["y"]).unwrap();
    let values: Vec<f32> = (0..N_SAMPLES).map(|i| i as f32 * 0.1).collect();
    depth.put_values(&values, ..).unwrap();
    for (name, value) in [
        ("crs", "EPSG:32633"),
        ("ridal_processing_datetime", processed),
        ("ridal_version", "ridal version 0.0.0 by test"),
        ("ridal_radargram_id", radargram_id),
        ("ridal_group_name", GROUP),
        ("ridal_group_id", GROUP),
    ] {
        file.add_attribute(name, value).unwrap();
    }
}

/// The bytes of a radargram written as [`write_radargram`] does.
fn radargram_bytes(dir: &StdPath, name: &str, radargram_id: &str, processed: &str) -> Vec<u8> {
    let path = dir.join(name);
    write_radargram(&path, radargram_id, processed);
    std::fs::read(path).unwrap()
}

/// Stop serving [`RADARGRAM`], for restoring.
fn ignore_radargram(root: &StdPath) {
    let project = Site::open(root).unwrap().project(&project_key()).unwrap();
    crate::project::overrides::update(project.documents(), |stored| {
        stored.ignored.insert(
            crate::identity::RadargramId::new(RADARGRAM).unwrap(),
            Default::default(),
        );
        Ok(())
    })
    .unwrap();
}

/// A site holding [`RADARGRAM`], one member per role and download scope,
/// the [`INVITEE`] with a pending invite, and the project's read policy.
fn site(private: bool) -> (tempfile::TempDir, Fixture) {
    let dir = new_site();
    write_radargram(
        &radargrams(dir.path()).join(format!("{RADARGRAM}.nc")),
        RADARGRAM,
        "2020-01-01T00:00:00Z",
    );
    let hash = password_hash();
    let users = Role::ALL
        .into_iter()
        .flat_map(|role| {
            DownloadScope::ALL
                .into_iter()
                .map(move |download| activated(&member_name(role, download), role, download, hash))
        })
        .collect();
    write_people(
        dir.path(),
        &UserSet {
            require_auth_to_read: private,
            anonymous_download: DownloadScope::None,
            users,
        },
    );
    let (invite_token, pending) = invite::mint_for_project(
        auth::now(),
        project_key(),
        Role::Viewer,
        DownloadScope::None,
    )
    .unwrap();
    let site = Site::open(dir.path()).unwrap();
    accounts::update(site.store(), |set| {
        let mut account = accounts::Account::new(id(INVITEE), false);
        account.invite = Some(pending.clone());
        set.users.push(account);
        Ok(())
    })
    .unwrap();
    let scratch = dir.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let fixture = Fixture {
        invite_token,
        upload: radargram_bytes(&scratch, "upload.nc", "line-02", "2020-01-01T00:00:00Z"),
        revision: radargram_bytes(&scratch, "revision.nc", RADARGRAM, "2021-01-01T00:00:00Z"),
    };
    (dir, fixture)
}

/// A session cookie for a member, minted rather than signed in for, so the
/// probes do not pay for a password hash each.
fn session(root: &StdPath, role: Role, download: DownloadScope) -> String {
    let key = SessionKey::load_or_create(Site::open(root).unwrap().store()).unwrap();
    let value = key.mint(&id(&member_name(role, download)), 1, auth::now() + 3600);
    format!("{SESSION_COOKIE}={value}")
}

/// `path` with every `{name}` filled in, from `answer` first.
fn fill(path: &str, answer: &Value) -> String {
    let mut filled = path.to_string();
    for name in super::openapi::path_parameter_names(path) {
        let value = answer[name].as_str().map(str::to_string).or_else(|| {
            PARAMETERS
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, value)| value.to_string())
        });
        let value = value.unwrap_or_else(|| panic!("no value for {{{name}}} in {path}"));
        filled = filled.replace(&format!("{{{name}}}"), &value);
    }
    filled
}

fn request(method: &str, uri: &str, cookie: Option<String>, payload: Payload) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    match payload {
        Payload::None => request.body(Body::empty()),
        Payload::Json(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        Payload::Netcdf(bytes) => request
            .header(header::CONTENT_TYPE, "application/x-netcdf")
            .body(Body::from(bytes)),
    }
    .unwrap()
}

/// Every operation in the spec: `(operationId, method, path, needs)`.
fn operations() -> Vec<(String, String, String, Needs)> {
    let spec = serde_json::to_value(super::openapi::spec()).unwrap();
    let mut operations = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            if method == "parameters" {
                continue;
            }
            let needs: Needs =
                serde_json::from_value(operation[super::needs::EXTENSION].clone()).unwrap();
            operations.push((
                operation["operationId"].as_str().unwrap().to_string(),
                method.to_uppercase(),
                path.clone(),
                needs,
            ));
        }
    }
    operations
}

/// Send `case`'s request as `probe` says, to a fresh site, and say what is
/// wrong with the answer, if anything.
async fn run(
    case: &Case,
    method: &str,
    path: &str,
    needs: &Needs,
    probe: &Probe,
) -> Option<String> {
    let (dir, fixture) = site(probe.private);
    if let Some(prepare) = case.prepare {
        prepare(dir.path());
    }
    let app = site_router(dir.path(), AccessOptions::default());
    let mut answer = Value::Null;
    if let Some((setup_method, setup_path, body)) = case.setup {
        let cookie = session(dir.path(), Role::Admin, DownloadScope::All);
        let response = app
            .clone()
            .oneshot(request(
                setup_method,
                &fill(setup_path, &Value::Null),
                Some(cookie),
                body(&fixture),
            ))
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        if !status.is_success() {
            return Some(format!(
                "{} ({}): its setup answered {status}: {}",
                path,
                case.operation,
                String::from_utf8_lossy(&bytes)
            ));
        }
        answer = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    }
    let mut uri = fill(path, &answer);
    if !case.query.is_empty() {
        uri = format!("{uri}?{}", case.query);
    }
    let cookie = match probe.caller {
        Caller::Anonymous => None,
        Caller::Member(role, download) => Some(session(dir.path(), role, download)),
    };
    let response = app
        .oneshot(request(method, &uri, cookie, (case.body)(&fixture)))
        .await
        .unwrap();
    let status = response.status();
    let ok = match probe.refused {
        None => status.is_success(),
        Some(refused) => status == refused,
    };
    if ok {
        return None;
    }
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    Some(format!(
        "{method} {path} ({}) declares {needs:?}; as {:?} in a {} project it \
         answered {status}, expected {}: {}",
        case.operation,
        probe.caller,
        if probe.private { "private" } else { "public" },
        probe
            .refused
            .map_or("success".to_string(), |s| s.to_string()),
        String::from_utf8_lossy(&bytes),
    ))
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn every_operation_needs_what_it_declares() {
    let mut faults = Vec::new();
    for (operation, method, path, needs) in operations() {
        let Some(case) = CASES.iter().find(|case| case.operation == operation) else {
            faults.push(format!("{operation}: no Case in needs_tests::CASES"));
            continue;
        };
        for probe in probes(&needs) {
            if let Some(fault) = run(case, &method, &path, &needs, &probe).await {
                faults.push(fault);
            }
        }
    }
    assert!(faults.is_empty(), "{faults:#?}");
}

/// A [`Case`] for an operation the spec no longer has is stale.
#[test]
fn every_case_names_an_operation() {
    let operations: Vec<String> = operations().into_iter().map(|(id, ..)| id).collect();
    let stale: Vec<&str> = CASES
        .iter()
        .map(|case| case.operation)
        .filter(|name| !operations.iter().any(|id| id == name))
        .collect();
    assert!(stale.is_empty(), "cases for no operation: {stale:?}");
}
