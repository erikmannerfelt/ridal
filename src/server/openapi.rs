//! The OpenAPI description of the HTTP API (#330), committed as
//! `docs/reference/openapi.json`.
//!
//! Its paths come from the routers: every route registered through
//! `site::site_api` or `app::project_api` is described by its handler's
//! `#[utoipa::path]`, and could not be served otherwise. A project's routes
//! are written as a site serves them, under `/api/v1/projects/{key}`. Routes
//! not yet annotated are still listed by hand in `http-api.md`.
//!
//! Each schema is derived from the type the handler serializes, so the file
//! cannot describe a field the server does not send. The picks document's
//! schemas (`Document` and the types in it) come from gprinterp's `utoipa`
//! feature (#346).
//!
//! Every operation declares what it needs of its caller as `x-ridal-needs`
//! (see [`super::needs`]), and its refusal responses are derived from that
//! here, so they say the same thing everywhere.
//!
//! A server also serves it at `GET /api/v1/openapi.json`, to anyone: it
//! describes the code, which is public, and nothing about the site.
//!
//! Regenerate it with
//! `UPDATE_OPENAPI=1 cargo test --no-default-features -F cli,server --bin ridal openapi_json`.

use axum::http::header;
use axum::response::IntoResponse;
use utoipa::openapi::path::{Operation, ParameterBuilder, ParameterIn, PathItem};
use utoipa::openapi::{ContentBuilder, Ref, Required, ResponseBuilder};
use utoipa::{OpenApi, PartialSchema};

use super::needs::{self, Needs, Who};
use super::{
    app, batch_routes, dem_routes, draft_routes, ingest_routes, interp_routes, lifecycle_routes,
    pick_routes, processing_routes, raw_routes, replace_routes, routes, site,
};
use crate::interp::carry::{CarryReport, Displacement, Dropped, Severity};
use crate::interp::source::RevisionDeclarations;
use crate::project::roles::{DownloadScope, Role};
use crate::site::tokens;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Ridal HTTP API",
        // The API's own version, the `v1` in `/api/v1`, rather than Ridal's:
        // the crate version would make this file stale on every release.
        // `GET /api/v1/health` reports the version a server runs.
        version = "1",
        description = "The Ridal server's HTTP API. Not every route is \
                       described here yet; the HTTP API reference page lists \
                       the rest. `x-ridal-needs` on an operation is the role \
                       or kind of caller it needs."
    ),
    components(schemas(
        routes::ErrorBody,
        routes::ErrorDetail,
        routes::Health,
        routes::DatasetList,
        routes::DatasetSummary,
        routes::DatasetAxes,
        routes::GprinterpAxes,
        interp_routes::InterpretationList,
        interp_routes::Saved,
        interp_routes::Promoted,
        interp_routes::CarriedView,
        // The pick manager (#178).
        pick_routes::InterpretationSummaryList,
        pick_routes::InterpretationSummary,
        pick_routes::LayerSummary,
        pick_routes::InterpretationState,
        pick_routes::CoverageBasis,
        pick_routes::UnreadableInterpretation,
        pick_routes::ArchivedInterpretation,
        pick_routes::Purged,
        pick_routes::RestoredInterpretation,
        pick_routes::BulkDownloadRequest,
        pick_routes::BulkDownloadItem,
        lifecycle_routes::Added,
        RevisionDeclarations,
        replace_routes::Staged,
        replace_routes::Replaced,
        replace_routes::ConsequenceReport,
        replace_routes::DocumentConsequence,
        replace_routes::ShapeChange,
        // The picks document (#346): what `GET`/`PUT
        // …/interpretations/{user}` carry, and the carried view's document.
        gprinterp::Document,
        CarryReport,
        Dropped,
        Displacement,
        Severity,
        site::Me,
        site::LoginBody,
        site::SignedIn,
        site::SignedOut,
        site::ProjectList,
        site::ProjectEntry,
        site::TokenSummary,
        site::TokenInfo,
        site::TokenList,
        site::NewToken,
        site::CreateTokenBody,
        tokens::Grant,
        Role,
        DownloadScope,
        // Processing on the server (#382): what the Python client's
        // processing methods read.
        raw_routes::StoredFile,
        raw_routes::SourceList,
        raw_routes::SourceView,
        dem_routes::DemList,
        dem_routes::DemView,
        draft_routes::DraftList,
        draft_routes::DraftView,
        processing_routes::ProfileList,
        processing_routes::ProfileView,
        processing_routes::StatusList,
        crate::project::processing::Assignments,
        batch_routes::BatchList,
        batch_routes::Batch,
        ingest_routes::IngestPlan,
        // `POST …/figure?format=json` (#142): where a figure without axes is.
        crate::render::figure::Sidecar,
        crate::render::figure::SidecarAxis,
        crate::render::figure::spec::View,
    ))
)]
struct ApiDoc;

/// The OpenAPI description of this API, the same file as the documentation
/// site's `openapi.json`. Never requires a login: it describes the code,
/// which is public, and nothing about the site.
#[utoipa::path(
    get,
    path = "/api/v1/openapi.json",
    tag = "Server",
    responses((status = 200, description = "This description.", content_type = "application/json")),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Nobody)))),
)]
pub async fn openapi_json() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], spec_json())
}

/// Where a site serves a project's API, and so where the spec puts it.
pub(super) const PROJECT_PREFIX: &str = "/api/v1/projects/{key}";

/// The whole description: the schemas above, the site's routes, and each
/// project's routes under [`PROJECT_PREFIX`].
pub(super) fn spec() -> utoipa::openapi::OpenApi {
    let mut spec = ApiDoc::openapi();
    spec.merge(site::site_api().into_openapi());
    let mut project = app::project_api().into_openapi();
    project.paths.paths = std::mem::take(&mut project.paths.paths)
        .into_iter()
        .map(|(path, item)| (under_project(&path), with_key_parameter(item)))
        .collect();
    spec.merge(project);
    for item in spec.paths.paths.values_mut() {
        for operation in operations(item) {
            add_refusals(operation);
        }
    }
    spec
}

/// `/api/v1/datasets` as a site serves it: `/api/v1/projects/{key}/datasets`.
fn under_project(path: &str) -> String {
    let rest = path
        .strip_prefix("/api/v1")
        .expect("a project route is under /api/v1");
    format!("{PROJECT_PREFIX}{rest}")
}

/// Declare `{key}` once per path rather than on every operation.
fn with_key_parameter(mut item: PathItem) -> PathItem {
    let key = ParameterBuilder::new()
        .name("key")
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .description(Some(
            "The project's key. `ridal gui` serves its one project as `default`.",
        ))
        .schema(Some(String::schema()))
        .build();
    item.parameters
        .get_or_insert_with(Vec::new)
        .insert(0, key.into());
    item
}

/// Every operation on one path.
fn operations(item: &mut PathItem) -> impl Iterator<Item = &mut Operation> {
    [
        &mut item.get,
        &mut item.put,
        &mut item.post,
        &mut item.delete,
        &mut item.patch,
    ]
    .into_iter()
    .flatten()
}

/// What an operation's `x-ridal-needs` says, or why it says nothing usable.
pub(super) fn needs_of(operation: &Operation) -> Result<Needs, String> {
    let value = operation
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get(needs::EXTENSION))
        .ok_or_else(|| format!("no {}", needs::EXTENSION))?;
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}

/// The refusals an operation's needs imply, with the shared error body.
///
/// Only those the handler can actually give for its declaration: a route
/// anyone may call is refused only to someone outside a project that
/// requires a login. A status the annotation already describes is left as
/// it is.
fn add_refusals(operation: &mut Operation) {
    let Ok(needs) = needs_of(operation) else {
        return;
    };
    let refusals: &[(&str, &str)] = match needs.who {
        Who::Nobody => &[],
        Who::Anyone => &[(
            "401",
            "`authentication_required`: the project requires a login to read.",
        )],
        Who::SignedIn => &[("401", "`authentication_required`: sign in first.")],
        Who::Role(_) => &[
            ("401", "`authentication_required`: sign in first."),
            (
                "403",
                "The caller's role is too low (`insufficient_role`), or the \
                 server is read-only (`read_only`), or the project is archived \
                 (`archived`), or an API token's grant is below it (`token_limit`).",
            ),
            (
                "409",
                "`not_a_project`: the server is serving files that are not a project.",
            ),
        ],
    };
    for (status, description) in refusals {
        operation
            .responses
            .responses
            .entry(status.to_string())
            .or_insert_with(|| {
                ResponseBuilder::new()
                    .description(*description)
                    .content(
                        "application/json",
                        ContentBuilder::new()
                            .schema(Some(Ref::from_schema_name("ErrorBody")))
                            .build(),
                    )
                    .build()
                    .into()
            });
    }
}

/// The committed file's contents: pretty-printed, with a trailing newline.
fn spec_json() -> String {
    let mut json = spec()
        .to_pretty_json()
        .expect("the spec is plain data and always serializes");
    json.push('\n');
    json
}

#[cfg(test)]
mod tests {
    use utoipa::{PartialSchema, ToSchema};

    /// gprinterp's schemas, by name: the picks document and every type in it.
    fn gprinterp_schemas() -> serde_json::Map<String, serde_json::Value> {
        let mut schemas = vec![(
            gprinterp::Document::name().into_owned(),
            gprinterp::Document::schema(),
        )];
        gprinterp::Document::schemas(&mut schemas);
        schemas
            .into_iter()
            .map(|(name, schema)| (name, serde_json::to_value(schema).unwrap()))
            .collect()
    }

    /// utoipa keys schemas by bare type name, so a Ridal type named like one
    /// of gprinterp's (`Document`, `Axes`, `Source`, …) would silently
    /// replace it in the spec, or be replaced by it.
    #[test]
    fn gprinterp_schemas_are_not_shadowed() {
        let spec = serde_json::to_value(super::spec()).unwrap();
        let components = &spec["components"]["schemas"];
        for (name, schema) in gprinterp_schemas() {
            assert_eq!(
                components[&name], schema,
                "the {name} schema is not gprinterp's; a Ridal type shares its name"
            );
        }
    }

    /// Request bodies, whose optional fields really are optional: a client
    /// may leave them out and gets the default. Those of annotated routes
    /// are read from their `requestBody`; these are the ones whose routes
    /// are not annotated yet.
    const UNROUTED_REQUEST_BODIES: [&str; 3] = [
        "RevisionDeclarations",
        "CreateTokenBody",
        "BulkDownloadItem",
    ];

    /// The schema names `requestBody` refers to, in every path.
    fn request_bodies(spec: &serde_json::Value) -> Vec<String> {
        let mut names: Vec<String> = UNROUTED_REQUEST_BODIES.map(String::from).to_vec();
        for item in spec["paths"].as_object().unwrap().values() {
            for operation in item.as_object().unwrap().values() {
                let schema = &operation["requestBody"]["content"]["application/json"]["schema"];
                if let Some(name) = schema["$ref"].as_str() {
                    names.push(name.rsplit('/').next().unwrap().to_string());
                }
            }
        }
        names
    }

    /// Every field of a response is sent, including one that is `null`, so
    /// all are required.
    ///
    /// utoipa leaves an `Option` field out of `required` unless it is marked
    /// `#[schema(required = true)]`, which would tell a client the key may
    /// be missing. None of Ridal's response types skips a field when
    /// serializing. gprinterp's do: a picks document leaves out what it does
    /// not have, so their optional fields really are optional.
    #[test]
    fn every_response_field_is_required() {
        let spec = serde_json::to_value(super::spec()).unwrap();
        let gprinterp = gprinterp_schemas();
        let requests = request_bodies(&spec);
        let mut optional = Vec::new();
        for (name, schema) in spec["components"]["schemas"].as_object().unwrap() {
            if requests.contains(name) || gprinterp.contains_key(name) {
                continue;
            }
            let required: Vec<&str> = schema["required"]
                .as_array()
                .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let Some(properties) = schema["properties"].as_object() else {
                continue;
            };
            for field in properties.keys() {
                if !required.contains(&field.as_str()) {
                    optional.push(format!("{name}.{field}"));
                }
            }
        }
        assert!(
            optional.is_empty(),
            "not marked required: {optional:?}; add #[schema(required = true)]"
        );
    }

    /// What the reference page and `needs_tests` rely on, for every
    /// operation: what it needs, one tag (the page's section), a summary
    /// (its description there), and an `operationId` no other has.
    #[test]
    fn every_operation_is_described() {
        let mut spec = super::spec();
        let mut ids = std::collections::BTreeSet::new();
        let mut faults = Vec::new();
        for (path, item) in spec.paths.paths.iter_mut() {
            for operation in super::operations(item) {
                let id = operation.operation_id.clone().unwrap_or_default();
                if let Err(e) = super::needs_of(operation) {
                    faults.push(format!("{path} ({id}): x-ridal-needs: {e}"));
                }
                if operation.tags.as_ref().map_or(0, Vec::len) != 1 {
                    faults.push(format!("{path} ({id}): needs exactly one tag"));
                }
                if operation.summary.is_none() {
                    faults.push(format!("{path} ({id}): no doc comment"));
                }
                if !ids.insert(id.clone()) {
                    faults.push(format!("{path}: operationId {id} is not unique"));
                }
            }
        }
        assert!(faults.is_empty(), "{faults:#?}");
    }

    #[test]
    fn openapi_json_is_up_to_date() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/openapi.json");
        let expected = super::spec_json();
        if std::env::var_os("UPDATE_OPENAPI").is_some() {
            // CI must check the committed file, never regenerate it.
            assert!(
                std::env::var_os("CI").is_none(),
                "UPDATE_OPENAPI is set in CI"
            );
            std::fs::write(&path, &expected).unwrap();
        }
        // A Windows checkout may have converted the line endings.
        let actual = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            actual == expected,
            "docs/reference/openapi.json is stale. Regenerate it with\n  \
             UPDATE_OPENAPI=1 cargo test --no-default-features -F cli,server --bin ridal openapi_json"
        );
    }
}
