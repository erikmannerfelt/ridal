//! HTTP routes for derived items (#205).
//!
//! Two ideas carry the whole permission model:
//!
//! - **Expressions are evaluated over the picks the caller may see.** A
//!   project-wide definition gives an operator the full consensus and an
//!   ordinary picker a result from their own picks only. This is the core
//!   property, and it is a property of the *evaluation*, not of the response.
//! - **Cross-user results are an explicit decision.** Anyone may see a result
//!   computed from their own picks; a result computed across contributors
//!   needs an admin to set `Audience::Released` on the item. An operator sees
//!   the cross-user result either way, so a consensus can be defined and
//!   watched while picking is still open.
//!
//! Note what is *not* true: `DownloadScope` is an ordered ladder
//! (`None < Results < Picks < Derived < All`), so a scope that permits raw
//! picks necessarily permits results too. Releasing a consensus without
//! releasing picks is expressed by giving a user `DownloadScope::Results`,
//! not by any per-item setting.
//!
//! Defining a project-wide item needs the operator role; a private item
//! belongs to one user and any signed-in user may keep one.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::IntoResponse;
use axum::Json;

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::{expectation_from, layer_error, parse_radargram, readable_project};
use super::routes::{lookup_dataset, ApiError};
use crate::identity::UserId;
use crate::interp::derive::{self, GridPosition, ReducedPicks};
use crate::interp::source;
use crate::project::derived::{self, Audience, DerivedError, DerivedSet, Scope};
use crate::project::users::{DownloadScope, Role};
use crate::project::{interpretations, layers};

fn derived_error(error: DerivedError) -> ApiError {
    match error {
        DerivedError::Store(e) => match e {
            crate::project::store::StoreError::Conflict { .. } => {
                ApiError::precondition_failed("version_conflict", e.to_string())
            }
            _ => ApiError::internal("store_failed", e.to_string()),
        },
        // A cycle or an invalid id is the author's mistake, not a server
        // fault: 400, so the editor can show the message.
        DerivedError::Cycle { .. }
        | DerivedError::InvalidId { .. }
        | DerivedError::DuplicateId(_)
        | DerivedError::InvalidColor { .. }
        | DerivedError::Malformed { .. } => {
            ApiError::bad_request("invalid_derived_items", error.to_string())
        }
        DerivedError::Derive(e) => ApiError::bad_request("invalid_derived_items", e.to_string()),
        DerivedError::UnitMismatch { .. } => {
            ApiError::bad_request("invalid_derived_items", error.to_string())
        }
    }
}

/// Whether `caller` may see results computed across everyone's picks,
/// regardless of what any single item says.
///
/// Operators and above can already read every interpretation, so an aggregate
/// of them reveals nothing new. A project with no authentication configured
/// behaves as it did before #131.
fn may_see_cross_user(caller: &Caller) -> bool {
    caller.may(Role::Operator) || !caller.authentication_configured
}

/// The pick set one item is evaluated over, for this caller.
///
/// Three rules, and they are the whole permission model for derived results:
///
/// 1. Anyone may see a result computed from **their own** picks. It is a
///    function of data they already have, so it needs no decision from anyone.
/// 2. A result computed **across users** is an admin decision, recorded as
///    [`Audience::Released`] on the item.
/// 3. An operator sees the cross-user result either way, so a consensus can be
///    defined and watched while picking is still open without the pickers
///    seeing each other's work.
fn evaluable_users(caller: &Caller, audience: Audience) -> EvaluableUsers {
    if may_see_cross_user(caller) || audience == Audience::Released {
        EvaluableUsers::All
    } else {
        EvaluableUsers::Only(caller.user.clone())
    }
}

enum EvaluableUsers {
    All,
    Only(Option<UserId>),
}

/// Read the picks that feed an item with this `audience`, for this caller.
fn visible_documents(
    state: &AppState,
    caller: &Caller,
    radargram: &crate::identity::RadargramId,
    audience: Audience,
) -> Result<Vec<(String, gprinterp::Document)>, ApiError> {
    let project = readable_project(state)?;
    let users = interpretations::list_users(project.documents(), radargram)
        .map_err(|e| ApiError::internal("interpretation_read_failed", e.to_string()))?;

    let mut documents = Vec::new();
    for user in users {
        let parsed =
            UserId::new(&user).map_err(|e| ApiError::internal("invalid_stored_user", e))?;
        let permitted = match evaluable_users(caller, audience) {
            EvaluableUsers::All => true,
            EvaluableUsers::Only(Some(own)) => own == parsed,
            EvaluableUsers::Only(None) => false,
        };
        if !permitted {
            continue;
        }
        if let Some(stored) = interpretations::read(project.documents(), radargram, &parsed)
            .map_err(|e| ApiError::internal("interpretation_read_failed", e.to_string()))?
        {
            documents.push((user, stored.document));
        }
    }
    Ok(documents)
}

/// Build the reduced picks for one radargram at per-trace spacing.
///
/// `audience` decides whose picks go in, so an item is always evaluated over
/// exactly the set its audience permits -- never over a wider set that is
/// filtered afterwards, which is how a cross-user value leaks.
fn reduce_for(
    state: &AppState,
    caller: &Caller,
    radargram: &crate::identity::RadargramId,
    geometry: &crate::interp::level2::RadargramGeometry,
    audience: Audience,
) -> Result<(ReducedPicks, crate::project::layers::LayerSet), ApiError> {
    let project = readable_project(state)?;
    let (layer_set, _) = layers::read(project.documents()).map_err(layer_error)?;
    let documents = visible_documents(state, caller, radargram, audience)?;
    let grid: Vec<GridPosition> = (0..geometry.n_traces())
        .map(|trace| GridPosition {
            trace: trace as f64,
            distance_m: geometry.distance[trace],
        })
        .collect();
    let reduced = derive::reduce_picks(&documents, &layer_set, geometry, &grid, false);
    Ok((reduced, layer_set))
}

fn geometry_for(
    state: &AppState,
    radargram: &crate::identity::RadargramId,
) -> Result<crate::interp::level2::RadargramGeometry, ApiError> {
    let catalog = state.catalog();
    let entry = lookup_dataset(&catalog, radargram.as_str())?;
    let path = state
        .absolute_path(entry)
        .map_err(|e| ApiError::internal("path_resolve_failed", e))?;
    source::read_geometry(&path).map_err(|e| ApiError::internal("radargram_read_failed", e))
}

fn load_set(
    state: &AppState,
) -> Result<(DerivedSet, Option<crate::project::store::Version>), ApiError> {
    let project = readable_project(state)?;
    derived::read(project.documents()).map_err(derived_error)
}

/// `GET /api/v1/derived` -- the derived items the caller may see.
pub async fn get_derived(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let (set, version) = load_set(&state)?;
    let project = readable_project(&state)?;
    let (layer_set, _) = layers::read(project.documents()).map_err(layer_error)?;

    let user = caller.display_name().to_string();
    let items: Vec<serde_json::Value> = set
        .visible_to(&user)
        .into_iter()
        .map(|item| {
            let kind = set
                .inferred_kind(&item.id, &layer_set)
                .map(|kind| kind.to_string())
                .unwrap_or_else(|e| format!("error: {e}"));
            serde_json::json!({
                "id": item.id,
                "name": item.name,
                "expression": item.expression,
                "unit": item.unit,
                "kind": kind,
                "color": item.color,
                "show": item.show,
                "fill_to": item.fill_to,
                "scope": item.scope,
                "audience": item.audience,
            })
        })
        .collect();

    let mut headers = HeaderMap::new();
    if let Some(version) = version {
        if let Ok(value) = format!("\"{version}\"").parse() {
            headers.insert(header::ETAG, value);
        }
    }
    // Layers whose id predates #206 cannot appear in an expression at all --
    // a hyphen is a minus sign to the parser. The editor needs to say so next
    // to its layer-name autocomplete, because the alternative is a user typing
    // a name they can see in the layer manager and getting a parse error with
    // no hint that the *id* is the problem.
    let unusable = layer_set.expression_unsafe_ids();

    Ok((
        headers,
        Json(serde_json::json!({
            "items": items,
            "layers_unusable_in_expressions": unusable,
        })),
    ))
}

/// `PUT /api/v1/derived` -- replace the derived set.
pub async fn put_derived(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    headers: HeaderMap,
    Json(set): Json<DerivedSet>,
) -> Result<impl IntoResponse, ApiError> {
    // Project-wide items need the operator role. Private items must belong to
    // the caller, and need a signed-in caller to belong to.
    for item in &set.items {
        // Releasing is a bigger decision than authoring: it publishes other
        // contributors' work in aggregate, to everyone who can see the item.
        // Authoring stays with the operator who maintains the vocabulary.
        if item.audience == Audience::Released {
            caller.require(Role::Admin, "release a cross-user derived result")?;
        }
        match &item.scope {
            Scope::Project => caller.require(Role::Operator, "edit project-wide derived items")?,
            Scope::Private { user } => {
                let own = caller.user.as_ref().ok_or_else(|| {
                    ApiError::unauthorized(
                        "authentication_required",
                        "Sign in to save a private derived item.",
                    )
                })?;
                if own.as_str() != user {
                    return Err(ApiError::forbidden(
                        "not_your_item",
                        format!(
                            "The private derived item '{}' belongs to '{user}'.",
                            item.id
                        ),
                    ));
                }
            }
        }
    }
    let project = readable_project(&state)?;
    let expected = expectation_from(&headers);
    let version = derived::write(project.documents(), &set, &expected).map_err(derived_error)?;
    let mut response = HeaderMap::new();
    if let Ok(value) = format!("\"{version}\"").parse() {
        response.insert(header::ETAG, value);
    }
    Ok((
        response,
        Json(serde_json::json!({ "version": version.as_str() })),
    ))
}

/// `GET /api/v1/datasets/{id}/derived/{item}` -- one item's values.
pub async fn get_derived_item(
    State(state): State<Arc<AppState>>,
    Path((radargram_id, item_id)): Path<(String, String)>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_download(DownloadScope::Results, "derived results")?;
    let radargram = parse_radargram(&radargram_id)?;
    let (set, _) = load_set(&state)?;
    let Some(item) = set.get(&item_id) else {
        return Err(ApiError::not_found(
            "derived_item_not_found",
            format!("No derived item with id '{item_id}'."),
        ));
    };
    if !item.visible_to(caller.display_name()) {
        return Err(ApiError::forbidden(
            "not_your_item",
            "That private derived item belongs to someone else.",
        ));
    }
    let geometry = geometry_for(&state, &radargram)?;
    let (reduced, _) = reduce_for(&state, &caller, &radargram, &geometry, item.audience)?;
    let results = set
        .evaluate(&reduced, &geometry)
        .map_err(|e| ApiError::bad_request("derived_failed", e.to_string()))?;
    let result = &results[&item_id];

    let values: Vec<serde_json::Value> = result
        .values
        .iter()
        .map(|v| {
            if v.is_nan() {
                serde_json::Value::Null
            } else {
                serde_json::json!(v)
            }
        })
        .collect();

    Ok(Json(serde_json::json!({
        "radargram_id": radargram.as_str(),
        "id": item.id,
        "kind": result.kind,
        "unit": result.unit,
        "trace": (0..geometry.n_traces()).collect::<Vec<usize>>(),
        "values": values,
    })))
}

/// `GET /api/v1/datasets/{id}/derived` -- every visible item, long CSV.
///
/// Long format: one row per item per grid position. Gaps are empty, which is
/// how a NaN reads in a CSV.
pub async fn download_derived(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_download(DownloadScope::Results, "derived results")?;
    let radargram = parse_radargram(&radargram_id)?;
    let (set, _) = load_set(&state)?;
    let geometry = geometry_for(&state, &radargram)?;

    // Items in one download may not share an audience, and an item must never
    // be served from an evaluation over a wider pick set than its own audience
    // allows. So evaluate once per distinct audience present -- at most twice,
    // and exactly once whenever the caller may see everything anyway.
    let mut per_audience: std::collections::BTreeMap<
        Audience,
        std::collections::BTreeMap<String, crate::interp::derive::EvaluatedItem>,
    > = std::collections::BTreeMap::new();
    for audience in [Audience::OwnPicks, Audience::Released] {
        if !set
            .visible_to(caller.display_name())
            .iter()
            .any(|item| item.audience == audience)
        {
            continue;
        }
        let (reduced, _) = reduce_for(&state, &caller, &radargram, &geometry, audience)?;
        let results = set
            .evaluate(&reduced, &geometry)
            .map_err(|e| ApiError::bad_request("derived_failed", e.to_string()))?;
        per_audience.insert(audience, results);
    }

    let mut body = String::from("radargram_id,item,kind,unit,trace,value\n");
    for item in set.visible_to(caller.display_name()) {
        let Some(result) = per_audience
            .get(&item.audience)
            .and_then(|results| results.get(&item.id))
        else {
            continue;
        };
        for (trace, value) in result.values.iter().enumerate() {
            let value = if value.is_nan() {
                String::new()
            } else {
                format!("{value:.6}")
            };
            body.push_str(&format!(
                "{},{},{},{},{trace},{value}\n",
                radargram.as_str(),
                item.id,
                result.kind,
                result.unit,
            ));
        }
    }

    let filename = format!("{}-derived.csv", radargram.as_str());
    let mut headers = HeaderMap::new();
    let set_header = |headers: &mut HeaderMap, name: header::HeaderName, value: String| {
        if let Ok(value) = value.parse() {
            headers.insert(name, value);
        }
    };
    set_header(
        &mut headers,
        header::CONTENT_TYPE,
        "text/csv; charset=utf-8".to_string(),
    );
    set_header(
        &mut headers,
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\""),
    );
    Ok((headers, body))
}
