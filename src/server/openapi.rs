//! The OpenAPI description of the HTTP API (#330), committed as
//! `docs/reference/openapi.json`.
//!
//! So far it holds schemas only, for the bodies the Python client (#328)
//! reads and sends; `paths` is empty until the routes are annotated. Each schema is
//! derived from the type the handler serializes, so the file cannot describe
//! a field the server does not send. The picks document's schemas (`Document`
//! and the types in it) come from gprinterp's `utoipa` feature (#346).
//!
//! A server also serves it at `GET /api/v1/openapi.json`, to anyone: it
//! describes the code, which is public, and nothing about the site.
//!
//! Regenerate it with
//! `UPDATE_OPENAPI=1 cargo test --no-default-features -F cli,server --bin ridal openapi_json`.

use axum::http::header;
use axum::response::IntoResponse;
use utoipa::OpenApi;

use super::{interp_routes, lifecycle_routes, replace_routes, routes, site};
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
        description = "Schemas of the JSON bodies the Ridal server returns. \
                       Routes are not described here yet; see the HTTP API \
                       reference page for them."
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
    ))
)]
struct ApiDoc;

/// `GET /api/v1/openapi.json`: the committed file, byte for byte.
pub async fn openapi_json() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], spec_json())
}

/// The committed file's contents: pretty-printed, with a trailing newline.
fn spec_json() -> String {
    let mut json = ApiDoc::openapi()
        .to_pretty_json()
        .expect("the spec is plain data and always serializes");
    json.push('\n');
    json
}

#[cfg(test)]
mod tests {
    use utoipa::{OpenApi, PartialSchema, ToSchema};

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
        let spec = serde_json::to_value(super::ApiDoc::openapi()).unwrap();
        let components = &spec["components"]["schemas"];
        for (name, schema) in gprinterp_schemas() {
            assert_eq!(
                components[&name], schema,
                "the {name} schema is not gprinterp's; a Ridal type shares its name"
            );
        }
    }

    /// Request bodies, whose optional fields really are optional: a client
    /// may leave them out and gets the default.
    const REQUEST_BODIES: [&str; 3] = ["LoginBody", "RevisionDeclarations", "CreateTokenBody"];

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
        let spec = serde_json::to_value(super::ApiDoc::openapi()).unwrap();
        let gprinterp = gprinterp_schemas();
        let mut optional = Vec::new();
        for (name, schema) in spec["components"]["schemas"].as_object().unwrap() {
            if REQUEST_BODIES.contains(&name.as_str()) || gprinterp.contains_key(name) {
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
