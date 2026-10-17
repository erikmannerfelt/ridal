//! The OpenAPI description of the HTTP API (#330), committed as
//! `docs/reference/openapi.json`.
//!
//! Its paths come from the routers: every route registered through
//! `site::site_api` or `app::project_api` is described by its handler's
//! `#[utoipa::path]`, and could not be served otherwise. A project's routes
//! are written as a site serves them, under `/api/v1/projects/{key}`.
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
        description = "The Ridal server's HTTP API. `x-ridal-needs` on an \
                       operation is the role or kind of caller it needs, and \
                       the download scope; the HTTP API reference page says \
                       what each means."
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

/// A processed `.nc` file sent as the whole request body.
///
/// A schema of its own, since `Vec<u8>` would describe the bytes as an
/// array of integers.
pub(super) struct NetcdfFile;

impl PartialSchema for NetcdfFile {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
                utoipa::openapi::KnownFormat::Binary,
            )))
            .description(Some("A processed `.nc` file, as Ridal writes them."))
            .into()
    }
}

impl utoipa::ToSchema for NetcdfFile {}

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
        .map(|(path, item)| (under_project(&path), item))
        .collect();
    spec.merge(project);
    for (path, item) in spec.paths.paths.iter_mut() {
        add_path_parameters(path, item);
        for operation in operations(item) {
            add_refusals(operation);
        }
    }
    spec
}

/// What each path parameter is, wherever it appears. Declared once per
/// path, from here, so the same name means the same thing everywhere.
const PATH_PARAMETERS: &[(&str, &str)] = &[
    (
        "key",
        "The project's key. `ridal gui` serves its one project as `default`.",
    ),
    ("radargram_id", "A radargram's id."),
    ("group", "A group's id."),
    (
        "view",
        "`standard`, or `topo` for the topographically corrected view.",
    ),
    ("trace", "A trace's index, from 0."),
    // A render profile in the one image path that has one, which says so
    // itself (`chunk_image`).
    ("profile", "A processing profile's id."),
    ("x", "The tile's column, from 0."),
    ("y", "The tile's row, from 0."),
    ("token", "The token a staged replacement was given."),
    (
        "blake3",
        "A stored raw file's blake3 hash, as its upload answered.",
    ),
    ("dem", "A DEM's id."),
    ("batch", "A batch's id."),
    ("job", "A processing job's number, as a draft gives it."),
    ("name", "An account name."),
    ("id", "An API token's id."),
    ("user", "An account name: whose interpretation it is."),
    (
        "removal",
        "An archive entry: the removal or archiving that put the interpretation there, as a summary row's `removal` gives it.",
    ),
    ("item", "A derived item's id."),
];

/// The `{name}`s in a path, in order.
pub(super) fn path_parameter_names(path: &str) -> Vec<&str> {
    path.split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .collect()
}

/// Declare each of the path's parameters that [`PATH_PARAMETERS`] knows,
/// unless its operations declare it themselves.
fn add_path_parameters(path: &str, item: &mut PathItem) {
    for name in path_parameter_names(path) {
        let declared_by_all = operations(item).all(|operation| {
            operation.parameters.iter().flatten().any(|p| match p {
                utoipa::openapi::RefOr::T(p) => p.name == name,
                utoipa::openapi::RefOr::Ref(_) => false,
            })
        });
        if declared_by_all {
            continue;
        }
        let Some((_, description)) = PATH_PARAMETERS.iter().find(|(known, _)| *known == name)
        else {
            continue;
        };
        let parameter = ParameterBuilder::new()
            .name(name)
            .parameter_in(ParameterIn::Path)
            .required(Required::True)
            .description(Some(*description))
            .schema(Some(String::schema()))
            .build();
        item.parameters
            .get_or_insert_with(Vec::new)
            .push(parameter.into());
    }
}

/// `/api/v1/datasets` as a site serves it: `/api/v1/projects/{key}/datasets`.
fn under_project(path: &str) -> String {
    let rest = path
        .strip_prefix("/api/v1")
        .expect("a project route is under /api/v1");
    format!("{PROJECT_PREFIX}{rest}")
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
    let mut refusals: Vec<(&str, &str)> = match needs.who {
        Who::Nobody => vec![],
        Who::Anyone => vec![(
            "401",
            "`authentication_required`: the project requires a login to read.",
        )],
        Who::SignedIn => vec![("401", "`authentication_required`: sign in first.")],
        Who::ServerAdmin => vec![
            ("401", "`authentication_required`: sign in first."),
            (
                "403",
                "The caller is not a server administrator, or the server is \
                 read-only (`read_only`), or an API token was used \
                 (`token_not_allowed`).",
            ),
        ],
        Who::Role(_) => vec![
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
    if needs.download.is_some() {
        // A role's 401 and 403 already cover a download refusal too.
        if !matches!(needs.who, Who::Role(_)) {
            refusals = vec![
                (
                    "401",
                    "`authentication_required`: sign in, or the project requires a \
                     login to read.",
                ),
                (
                    "403",
                    "The caller's download scope is too low \
                     (`download_not_permitted`), or an API token's grant is below \
                     it (`token_limit`).",
                ),
            ];
        }
    }
    for (status, description) in refusals {
        operation
            .responses
            .responses
            .entry(status.to_string())
            .or_insert_with(|| {
                ResponseBuilder::new()
                    .description(description)
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

    /// Stored documents that leave out what they do not set, and are sent
    /// as stored: the layer vocabulary's layers and groups, a derived item's
    /// fill, and a line of the site's history. A missing field there is its
    /// default, which the document's own description gives.
    const DOCUMENTS: [&str; 4] = ["Layer", "ExclusivityGroup", "FillTo", "AuditEntry"];

    /// The schema names a `$ref` anywhere in `value` points at.
    fn refs(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(target) = map.get("$ref").and_then(|r| r.as_str()) {
                    out.push(target.rsplit('/').next().unwrap().to_string());
                }
                map.values().for_each(|v| refs(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| refs(v, out)),
            _ => {}
        }
    }

    /// Every schema reachable from `roots` through the components.
    fn reachable(spec: &serde_json::Value, roots: Vec<String>) -> Vec<String> {
        let components = &spec["components"]["schemas"];
        let mut seen: Vec<String> = Vec::new();
        let mut queue = roots;
        while let Some(name) = queue.pop() {
            if seen.contains(&name) {
                continue;
            }
            refs(&components[&name], &mut queue);
            seen.push(name);
        }
        seen
    }

    /// The schemas only requests use: every route's request body and what
    /// it contains, less anything a response uses too. Their optional fields
    /// really are optional: a client may leave them out and get the default.
    fn request_only(spec: &serde_json::Value) -> Vec<String> {
        let mut requests: Vec<String> = Vec::new();
        let mut responses = Vec::new();
        for item in spec["paths"].as_object().unwrap().values() {
            for operation in item.as_object().unwrap().values() {
                refs(&operation["requestBody"], &mut requests);
                refs(&operation["responses"], &mut responses);
            }
        }
        let responses = reachable(spec, responses);
        reachable(spec, requests)
            .into_iter()
            .filter(|name| !responses.contains(name))
            .collect()
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
        let requests = request_only(&spec);
        let mut optional = Vec::new();
        for (name, schema) in spec["components"]["schemas"].as_object().unwrap() {
            if requests.contains(name)
                || gprinterp.contains_key(name)
                || DOCUMENTS.contains(&name.as_str())
            {
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
    /// (its description there), an `operationId` no other has, and a
    /// description of every path parameter.
    #[test]
    fn every_operation_is_described() {
        let mut spec = super::spec();
        let mut ids = std::collections::BTreeSet::new();
        let mut faults = Vec::new();
        for (path, item) in spec.paths.paths.iter_mut() {
            let names = |parameters: &Option<
                Vec<utoipa::openapi::RefOr<utoipa::openapi::path::Parameter>>,
            >|
             -> Vec<String> {
                parameters
                    .iter()
                    .flatten()
                    .filter_map(|p| match p {
                        utoipa::openapi::RefOr::T(p) => Some(p.name.clone()),
                        utoipa::openapi::RefOr::Ref(_) => None,
                    })
                    .collect()
            };
            let on_path = names(&item.parameters);
            let on_operations: Vec<Vec<String>> = super::operations(item)
                .map(|operation| names(&operation.parameters))
                .collect();
            for name in super::path_parameter_names(path) {
                let declared = on_path.iter().any(|d| d == name)
                    || on_operations
                        .iter()
                        .all(|own| own.iter().any(|d| d == name));
                if !declared {
                    faults.push(format!("{path}: {{{name}}} is not in PATH_PARAMETERS"));
                }
            }
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
