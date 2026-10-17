//! Every operation in the OpenAPI description needs what its `x-ridal-needs`
//! says (#330), no more and no less.
//!
//! The *Needs* column of the HTTP reference is rendered from that
//! declaration, and the handler checks something else entirely
//! (`Caller::require`, `Caller::require_download`, an owner check, or
//! nothing at all). This ties the two: each operation is sent the same
//! request by callers just below what it declares, who must be refused, and
//! by one exactly at it, who must succeed. An operation stricter than
//! declared fails the second; a laxer one, the first. A request that
//! succeeds at the declared level is also one whose refusal below it is
//! about the caller, not about the request.
//!
//! Each operation needs a [`Case`]: what to put in its query and body, and
//! what to set up first. One missing fails the test, so a route cannot be
//! added to the spec without one.

use std::collections::HashMap;
use std::path::Path as StdPath;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::access_tests::{
    activated, id, new_site, password, password_hash, project_key, radargrams,
    site_router_processing, write_people, UserSet, PROJECT,
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

/// The value of each path parameter, unless a case or its setup gives one.
const PARAMETERS: &[(&str, &str)] = &[
    ("key", PROJECT),
    ("radargram_id", RADARGRAM),
    ("group", GROUP),
    ("view", "standard"),
    ("trace", "0"),
    ("profile", "prof"),
    ("x", "0"),
    ("y", "0"),
    ("dem", "mettebreen"),
];

/// What a [`Case`] can build its requests from.
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
    /// Any other file, such as raw data.
    File(Vec<u8>),
}

/// Builds a body from the fixture and what the setup's requests answered.
type BodyFn = fn(&Fixture, &[Value]) -> Payload;

/// One thing a case's setup does before the probe.
#[derive(Clone, Copy)]
enum Step {
    /// Send a request. Its JSON answer is kept for the bodies that follow,
    /// and each of its top-level fields fills a path parameter of the same
    /// name, or the one `bind` names: `(parameter, field)`.
    Send {
        method: &'static str,
        /// With any query; `{name}` is filled like the probe's path.
        path: &'static str,
        body: BodyFn,
        bind: Option<(&'static str, &'static str)>,
    },
    /// Wait for the draft the last request made to finish.
    AwaitDraft,
}

const fn send(method: &'static str, path: &'static str, body: BodyFn) -> Step {
    Step::Send {
        method,
        path,
        body,
        bind: None,
    }
}

/// What to send to one operation, by `operationId`.
struct Case {
    operation: &'static str,
    /// Path parameters other than [`PARAMETERS`]' values.
    params: &'static [(&'static str, &'static str)],
    /// Appended to the path, without the `?`.
    query: &'static str,
    body: BodyFn,
    /// Changes to the site's files before the server is built over it.
    prepare: Option<fn(&StdPath)>,
    /// Requests made first, as the caller the case is probed at for an
    /// operation on the caller's own things, and as an `admin` otherwise.
    setup: fn() -> Vec<Step>,
    /// Whether it needs a processing job to finish, which only the stand-in
    /// for `ridal process-job` in [`site`] can do.
    needs_a_job: bool,
}

impl Case {
    const fn new(operation: &'static str) -> Self {
        Self {
            operation,
            params: &[],
            query: "",
            body: |_, _| Payload::None,
            prepare: None,
            setup: Vec::new,
            needs_a_job: false,
        }
    }

    const fn body(self, body: BodyFn) -> Self {
        Self { body, ..self }
    }

    const fn query(self, query: &'static str) -> Self {
        Self { query, ..self }
    }

    const fn params(self, params: &'static [(&'static str, &'static str)]) -> Self {
        Self { params, ..self }
    }

    const fn prepare(self, prepare: fn(&StdPath)) -> Self {
        Self {
            prepare: Some(prepare),
            ..self
        }
    }

    const fn setup(self, setup: fn() -> Vec<Step>) -> Self {
        Self { setup, ..self }
    }

    const fn needs_a_job(self) -> Self {
        Self {
            needs_a_job: true,
            ..self
        }
    }
}

/// A later revision of [`RADARGRAM`], as an upload.
fn revision(fixture: &Fixture, _: &[Value]) -> Payload {
    Payload::Netcdf(fixture.revision.clone())
}

fn stage() -> Vec<Step> {
    vec![send(
        "POST",
        "/api/v1/projects/{key}/datasets/{radargram_id}/replace",
        revision,
    )]
}

/// A file from `assets/`, as a raw upload.
fn asset(path: &str) -> Payload {
    Payload::File(std::fs::read(format!("assets/{path}")).unwrap())
}

/// The three files of a Drønbreen recording from `assets/mala/`, uploaded
/// as raw data.
fn upload_recording() -> Vec<Step> {
    vec![
        send(
            "POST",
            "/api/v1/projects/{key}/raw?filename=dronbreen-20250327-DAT_0066_A1.rad",
            |_, _| asset("mala/dronbreen-20250327-DAT_0066_A1.rad"),
        ),
        send(
            "POST",
            "/api/v1/projects/{key}/raw?filename=dronbreen-20250327-DAT_0066_A1.rd3",
            |_, _| asset("mala/dronbreen-20250327-DAT_0066_A1.rd3"),
        ),
        send(
            "POST",
            "/api/v1/projects/{key}/raw?filename=dronbreen-20250327-DAT_0066_A1.cor",
            |_, _| asset("mala/dronbreen-20250327-DAT_0066_A1.cor"),
        ),
    ]
}

/// The files the setup uploaded, as a recording names them.
fn uploaded(answers: &[Value]) -> Vec<Value> {
    answers
        .iter()
        .filter(|answer| answer["blake3"].is_string())
        .map(|answer| json!({"name": answer["name"], "blake3": answer["blake3"]}))
        .collect()
}

fn source_body(_: &Fixture, answers: &[Value]) -> Payload {
    Payload::Json(json!({"recordings": [uploaded(answers)]}))
}

/// That recording as [`RADARGRAM`]'s source.
fn source() -> Vec<Step> {
    let mut steps = upload_recording();
    steps.push(send(
        "PUT",
        "/api/v1/projects/{key}/sources/{radargram_id}",
        source_body,
    ));
    steps
}

fn dem_body(_: &Fixture, answers: &[Value]) -> Payload {
    let file = uploaded(answers).pop().unwrap();
    Payload::Json(json!({"name": "Mettebreen", "file": file}))
}

fn upload_dem() -> Vec<Step> {
    vec![send(
        "POST",
        "/api/v1/projects/{key}/raw?filename=mettebreen.tif",
        |_, _| asset("test_dem_dtm20_mettebreen.tif"),
    )]
}

fn dem() -> Vec<Step> {
    let mut steps = upload_dem();
    steps.push(send("PUT", "/api/v1/projects/{key}/dems/{dem}", dem_body));
    steps
}

/// The steps a processing profile, a draft and a batch run.
const STEPS: &str = "subset(0 100 0 200)";

fn profile_body(_: &Fixture, _: &[Value]) -> Payload {
    Payload::Json(json!({"name": "Profile", "steps": [STEPS], "medium_velocity": 0.168}))
}

fn profile() -> Vec<Step> {
    vec![send(
        "PUT",
        "/api/v1/projects/{key}/processing/profiles/{profile}",
        profile_body,
    )]
}

/// [`RADARGRAM`], with its source, in the profile.
fn assigned() -> Vec<Step> {
    let mut steps = source();
    steps.extend(profile());
    steps.push(send(
        "PUT",
        "/api/v1/projects/{key}/processing/assignments/{radargram_id}",
        |_, _| Payload::Json(json!({"profile": "prof"})),
    ));
    steps
}

fn batch() -> Vec<Step> {
    let mut steps = assigned();
    steps.push(Step::Send {
        method: "POST",
        path: "/api/v1/projects/{key}/processing/profiles/{profile}/reprocess",
        body: |_, _| Payload::Json(json!({})),
        bind: Some(("batch", "id")),
    });
    steps
}

fn draft_body(_: &Fixture, _: &[Value]) -> Payload {
    Payload::Json(json!({"steps": [STEPS]}))
}

/// A draft of [`RADARGRAM`], queued.
fn draft() -> Vec<Step> {
    let mut steps = source();
    steps.push(send(
        "POST",
        "/api/v1/projects/{key}/sources/{radargram_id}/drafts",
        draft_body,
    ));
    steps
}

/// A draft of [`RADARGRAM`], finished.
fn finished_draft() -> Vec<Step> {
    let mut steps = draft();
    steps.push(Step::AwaitDraft);
    steps
}

/// One per operation in the spec.
const CASES: &[Case] = &[
    // Server.
    Case::new("health"),
    Case::new("openapi_json"),
    Case::new("list_profiles"),
    // Signing in.
    Case::new("me"),
    Case::new("login")
        .body(|_, _| Payload::Json(json!({"name": "viewer-none", "password": password()}))),
    Case::new("logout"),
    Case::new("redeem_invite").body(|fixture, _| {
        Payload::Json(json!({"token": fixture.invite_token, "password": "a long new passphrase"}))
    }),
    // Preferences and project settings.
    Case::new("get_preferences"),
    Case::new("put_preferences").body(|_, _| Payload::Json(json!({"theme": "dark"}))),
    Case::new("get_settings"),
    Case::new("put_settings").body(|_, _| Payload::Json(json!({"default_profile": null}))),
    // Radargrams.
    Case::new("list_datasets"),
    Case::new("upload_dataset")
        .query("filename=line-02.nc")
        .body(|fixture, _| Payload::Netcdf(fixture.upload.clone())),
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
    Case::new("put_properties").body(|_, _| Payload::Json(json!({"unlisted": false}))),
    Case::new("preflight_replacement").body(|_, _| {
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
    Case::new("commit_replacement").setup(stage),
    Case::new("discard_replacement").setup(stage),
    Case::new("restore_dataset").prepare(ignore_radargram),
    Case::new("list_ignored"),
    // Groups.
    Case::new("group_tracks"),
    Case::new("get_group_properties"),
    Case::new("put_group_properties")
        .body(|_, _| Payload::Json(json!({"display_name": "The glacier"}))),
    // Images.
    Case::new("overview_image"),
    Case::new("chunk_image").params(&[("profile", "default")]),
    Case::new("dataset_image"),
    Case::new("render_figure").body(|_, _| Payload::Json(json!({}))),
    // Downloads.
    Case::new("dataset_download"),
    Case::new("dataset_track_geojson"),
    Case::new("group_track_geojson"),
    Case::new("catalog_track_geojson"),
    // Raw data.
    Case::new("upload_raw_file")
        .query("filename=DAT_0001_A1.rad")
        .body(|_, _| Payload::File(b"a raw file".to_vec())),
    Case::new("download_raw_file").setup(upload_recording),
    Case::new("list_sources"),
    Case::new("get_source").setup(source),
    Case::new("put_source")
        .setup(upload_recording)
        .body(source_body),
    Case::new("link_dem")
        .setup(source)
        .body(|_, _| Payload::Json(json!({"dem": null}))),
    Case::new("delete_source").setup(source),
    Case::new("plan_ingest")
        .setup(upload_recording)
        .body(|_, answers| Payload::Json(json!({"recordings": [{"files": uploaded(answers)}]}))),
    // DEMs.
    Case::new("list_dems"),
    Case::new("get_dem").setup(dem),
    Case::new("put_dem").setup(upload_dem).body(dem_body),
    Case::new("delete_dem").setup(dem),
    // Processing profiles.
    Case::new("list_processing_profiles"),
    Case::new("get_processing_profile").setup(profile),
    Case::new("put_processing_profile").body(profile_body),
    Case::new("delete_processing_profile").setup(profile),
    Case::new("list_assignments"),
    Case::new("put_assignment")
        .setup(|| {
            let mut steps = source();
            steps.extend(profile());
            steps
        })
        .body(|_, _| Payload::Json(json!({"profile": "prof"}))),
    Case::new("delete_assignment").setup(assigned),
    Case::new("processing_activity"),
    Case::new("processing_status"),
    Case::new("reprocess")
        .setup(assigned)
        .body(|_, _| Payload::Json(json!({}))),
    Case::new("list_batches"),
    Case::new("unseen_batches"),
    Case::new("get_batch").setup(batch),
    Case::new("mark_batch_seen").setup(batch),
    // Drafts.
    Case::new("create_draft").setup(source).body(draft_body),
    Case::new("list_drafts"),
    Case::new("get_draft").setup(draft),
    Case::new("delete_draft").setup(draft),
    Case::new("draft_overview")
        .setup(finished_draft)
        .needs_a_job(),
    Case::new("download_draft")
        .setup(finished_draft)
        .needs_a_job(),
    Case::new("commit_draft")
        .setup(finished_draft)
        .needs_a_job(),
];

/// Who sends a probe.
#[derive(Debug, Clone, Copy)]
enum Caller {
    Anonymous,
    /// A member of the project with this role and download scope, signed
    /// in.
    Member(Role, DownloadScope),
    /// Another member with the same role and download scope.
    Other(Role, DownloadScope),
}

/// One request to send, and what must come back.
#[derive(Debug)]
struct Probe {
    caller: Caller,
    /// Whether the project requires a login to read.
    private: bool,
    /// Empty for success, or the statuses a refusal may have.
    refused: &'static [StatusCode],
}

/// The one just below `value` in `ladder`, if there is one.
fn below<T: Copy + PartialOrd>(ladder: &[T], value: T) -> Option<T> {
    ladder.iter().copied().take_while(|v| *v < value).last()
}

const UNAUTHORIZED: &[StatusCode] = &[StatusCode::UNAUTHORIZED];
const FORBIDDEN: &[StatusCode] = &[StatusCode::FORBIDDEN];
/// Someone else's draft does not exist, as far as they are told; someone
/// else's batch is refused.
const NOT_YOURS: &[StatusCode] = &[StatusCode::FORBIDDEN, StatusCode::NOT_FOUND];

/// The caller a declaration is probed at: the least that should succeed.
fn at_caller(needs: &Needs) -> Caller {
    let download = needs.download.unwrap_or(DownloadScope::None);
    match needs.who {
        Who::Nobody => Caller::Anonymous,
        Who::Anyone | Who::SignedIn => Caller::Member(Role::Viewer, download),
        Who::Role(role) => Caller::Member(role, download),
    }
}

/// The probes a declaration implies: one at it, and one just below it on
/// each axis it names.
fn probes(needs: &Needs) -> Vec<Probe> {
    let probe = |caller, private, refused| Probe {
        caller,
        private,
        refused,
    };
    let at = at_caller(needs);
    let download = needs.download.unwrap_or(DownloadScope::None);
    let role = match needs.who {
        Who::Role(role) => role,
        _ => Role::Viewer,
    };
    let mut probes = match needs.who {
        // Even in a project that requires a login, nobody is refused.
        Who::Nobody => vec![probe(at, true, &[])],
        Who::Anyone => vec![
            probe(Caller::Anonymous, true, UNAUTHORIZED),
            probe(at, true, &[]),
        ],
        // A public project, so that being able to read it is not the reason.
        Who::SignedIn => vec![
            probe(Caller::Anonymous, false, UNAUTHORIZED),
            probe(at, false, &[]),
        ],
        Who::Role(_) => {
            let lower = below(&Role::ALL, role)
                .expect("an endpoint anyone who may read may call says `anyone`");
            vec![
                probe(Caller::Member(lower, download), false, FORBIDDEN),
                probe(at, false, &[]),
            ]
        }
    };
    if let Some(lower) = below(&DownloadScope::ALL, download) {
        probes.push(probe(Caller::Member(role, lower), false, FORBIDDEN));
    }
    if needs.own {
        probes.push(probe(Caller::Other(role, download), false, NOT_YOURS));
    }
    probes
}

/// The account of a member with `role` and `download`.
fn member_name(role: Role, download: DownloadScope) -> String {
    format!("{}-{}", role.as_str(), download.as_str())
}

/// The account of the other member with them.
fn other_name(role: Role, download: DownloadScope) -> String {
    format!("{}-other", member_name(role, download))
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

/// What stands in for `ridal process-job`: the test binary cannot be one.
/// Each job installs the file prepared for its radargram id in
/// `prepared/`, as a real job would write it, and says so.
#[cfg(unix)]
fn processing(dir: &StdPath) -> crate::server::processing::ProcessingConfig {
    use std::os::unix::fs::PermissionsExt;
    let prepared = dir.join("prepared");
    std::fs::create_dir(&prepared).unwrap();
    write_radargram(
        &prepared.join(format!("{RADARGRAM}.nc")),
        RADARGRAM,
        "2022-01-01T00:00:00Z",
    );
    let script = dir.join("job.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             out=$(sed -n 's/.*\"output\":\"\\([^\"]*\\)\".*/\\1/p' \"$2\")\n\
             id=$(sed -n 's/.*\"radargram_id\":\"\\([^\"]*\\)\".*/\\1/p' \"$2\")\n\
             cp {0}/\"$id\".nc \"$out\"\n\
             echo '{{\"event\":\"step\",\"index\":0,\"of\":1,\"step\":\"subset\"}}'\n\
             echo \"{{\\\"event\\\":\\\"done\\\",\\\"output\\\":\\\"$out\\\",\\\"revision_id\\\":\\\"r\\\"}}\"\n",
            prepared.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::server::processing::ProcessingConfig {
        program: script,
        work_dir: dir.join("jobs"),
        ..Default::default()
    }
}

/// Elsewhere every job fails to start, and the cases that need one to
/// finish are not run.
#[cfg(not(unix))]
fn processing(dir: &StdPath) -> crate::server::processing::ProcessingConfig {
    crate::server::processing::ProcessingConfig {
        program: dir.join("no-such-program"),
        work_dir: dir.join("jobs"),
        ..Default::default()
    }
}

/// A site holding [`RADARGRAM`], two members per role and download scope,
/// the [`INVITEE`] with a pending invite, and the project's read policy;
/// and a server over it.
fn site(private: bool, prepare: Option<fn(&StdPath)>) -> (tempfile::TempDir, Fixture, Router) {
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
            DownloadScope::ALL.into_iter().flat_map(move |download| {
                [
                    activated(&member_name(role, download), role, download, hash),
                    activated(&other_name(role, download), role, download, hash),
                ]
            })
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
    if let Some(prepare) = prepare {
        prepare(dir.path());
    }
    let scratch = dir.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let fixture = Fixture {
        invite_token,
        upload: radargram_bytes(&scratch, "upload.nc", "line-02", "2020-01-01T00:00:00Z"),
        revision: radargram_bytes(&scratch, "revision.nc", RADARGRAM, "2021-01-01T00:00:00Z"),
    };
    let app = site_router_processing(dir.path(), AccessOptions::default(), processing(&scratch));
    (dir, fixture, app)
}

/// A session cookie for `caller`, minted rather than signed in for, so the
/// probes do not pay for a password hash each.
fn session(root: &StdPath, caller: Caller) -> Option<String> {
    let name = match caller {
        Caller::Anonymous => return None,
        Caller::Member(role, download) => member_name(role, download),
        Caller::Other(role, download) => other_name(role, download),
    };
    let key = SessionKey::load_or_create(Site::open(root).unwrap().store()).unwrap();
    let value = key.mint(&id(&name), 1, auth::now() + 3600);
    Some(format!("{SESSION_COOKIE}={value}"))
}

/// `path` with every `{name}` filled in: from `bound` first, then
/// `params`, then [`PARAMETERS`].
fn fill(path: &str, bound: &HashMap<String, String>, params: &[(&str, &str)]) -> String {
    let mut filled = path.to_string();
    for name in super::openapi::path_parameter_names(path) {
        let value = bound
            .get(name)
            .cloned()
            .or_else(|| {
                params
                    .iter()
                    .chain(PARAMETERS)
                    .find(|(known, _)| *known == name)
                    .map(|(_, value)| value.to_string())
            })
            .unwrap_or_else(|| panic!("no value for {{{name}}} in {path}"));
        filled = filled.replace(&format!("{{{name}}}"), &value);
    }
    filled
}

fn request(method: &str, uri: &str, cookie: Option<String>, payload: Payload) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let (content_type, body) = match payload {
        Payload::None => return request.body(Body::empty()).unwrap(),
        Payload::Json(body) => ("application/json", Body::from(body.to_string())),
        Payload::Netcdf(bytes) => ("application/x-netcdf", Body::from(bytes)),
        Payload::File(bytes) => ("application/octet-stream", Body::from(bytes)),
    };
    request
        .header(header::CONTENT_TYPE, content_type)
        .body(body)
        .unwrap()
}

/// Send a request, and its status and body.
async fn send_request(app: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
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

/// Run a case's setup as `caller`, and what it bound and answered.
async fn set_up(
    case: &Case,
    app: &Router,
    root: &StdPath,
    fixture: &Fixture,
    caller: Caller,
) -> Result<(HashMap<String, String>, Vec<Value>), String> {
    let mut bound = HashMap::new();
    let mut answers: Vec<Value> = Vec::new();
    for step in (case.setup)() {
        match step {
            Step::Send {
                method,
                path,
                body,
                bind,
            } => {
                let uri = fill(path, &bound, case.params);
                let (status, bytes) = send_request(
                    app,
                    request(method, &uri, session(root, caller), body(fixture, &answers)),
                )
                .await;
                if !status.is_success() {
                    return Err(format!(
                        "setup {method} {uri} answered {status}: {}",
                        String::from_utf8_lossy(&bytes)
                    ));
                }
                let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                for (field, value) in answer.as_object().into_iter().flatten() {
                    let value = match value {
                        Value::String(text) => text.clone(),
                        Value::Number(number) => number.to_string(),
                        _ => continue,
                    };
                    bound.insert(field.clone(), value);
                }
                if let Some((parameter, field)) = bind {
                    let value = bound.get(field).cloned().unwrap_or_default();
                    bound.insert(parameter.to_string(), value);
                }
                answers.push(answer);
            }
            Step::AwaitDraft => {
                let uri = fill("/api/v1/projects/{key}/drafts/{job}", &bound, case.params);
                let mut finished = false;
                for _ in 0..400 {
                    let (_, bytes) = send_request(
                        app,
                        request("GET", &uri, session(root, caller), Payload::None),
                    )
                    .await;
                    let draft: Value = serde_json::from_slice(&bytes).unwrap_or_default();
                    if draft["state"] == "done" {
                        finished = true;
                        break;
                    }
                    if draft["state"] != "queued" && draft["state"] != "running" {
                        return Err(format!("the draft did not finish: {draft}"));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                if !finished {
                    return Err("the draft was still running after 10 s".to_string());
                }
            }
        }
    }
    Ok((bound, answers))
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
    let (dir, fixture, app) = site(probe.private, case.prepare);
    // An operation on the caller's own things is set up by the caller it
    // is probed at, who then owns them; anything else by an `admin`.
    let owner = if needs.own {
        at_caller(needs)
    } else {
        Caller::Member(Role::Admin, DownloadScope::All)
    };
    let (bound, answers) = match set_up(case, &app, dir.path(), &fixture, owner).await {
        Ok(set) => set,
        Err(problem) => return Some(format!("{method} {path} ({}): {problem}", case.operation)),
    };
    let mut uri = fill(path, &bound, case.params);
    if !case.query.is_empty() {
        uri = format!("{uri}?{}", case.query);
    }
    let (status, bytes) = send_request(
        &app,
        request(
            method,
            &uri,
            session(dir.path(), probe.caller),
            (case.body)(&fixture, &answers),
        ),
    )
    .await;
    let ok = if probe.refused.is_empty() {
        status.is_success()
    } else {
        probe.refused.contains(&status)
    };
    if ok {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    Some(format!(
        "{method} {path} ({}) declares {needs:?}; as {:?} in a {} project it \
         answered {status}, expected {}: {}",
        case.operation,
        probe.caller,
        if probe.private { "private" } else { "public" },
        if probe.refused.is_empty() {
            "success".to_string()
        } else {
            format!("{:?}", probe.refused)
        },
        &text[..text.len().min(2000)],
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
        if case.needs_a_job && cfg!(not(unix)) {
            continue;
        }
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
