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

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::IntoResponse;
use axum::Json;

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::{expectation_from, layer_error, parse_radargram, readable_project};
use super::needs::{Needs, Who};
use super::routes::{lookup_dataset, ApiError, ErrorBody};
use crate::identity::UserId;
use crate::interp::derive::{self, GridPosition, ReducedPicks};
use crate::interp::derived_points::{self, DerivedPointsExport};
use crate::interp::level2::{self, RadargramGeometry};
use crate::interp::{source, writer};
use crate::project::derived::{self, Audience, DerivedError, DerivedItem, DerivedSet, Scope};
use crate::project::roles::{DownloadScope, Role};
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
        | DerivedError::Malformed { .. }
        | DerivedError::ReservedProperty { .. }
        | DerivedError::PropertyCollision { .. }
        | DerivedError::BrokenDependency { .. } => {
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

/// [`evaluable_users`] as a figure's [`Readable`] (#142), so a figure's
/// derived layers are evaluated by this module's rule rather than a copy.
pub(super) fn readable_for(
    caller: &Caller,
    audience: Audience,
) -> crate::render::figure::overlays::Readable {
    use crate::render::figure::overlays::Readable;
    match evaluable_users(caller, audience) {
        EvaluableUsers::All => Readable::All,
        EvaluableUsers::Only(user) => Readable::Only(user),
    }
}

/// Whether `caller` may see other contributors' raw picks: the gate on the
/// contributor overlay, and on another user's picks in a figure (#142).
pub(super) fn may_see_others_picks(caller: &Caller) -> bool {
    may_see_cross_user(caller) && caller.may_download(DownloadScope::Picks)
}

/// The picks that feed an evaluation, as it uses them.
struct Visible {
    /// Each user's document, carried onto the revision being served.
    documents: Vec<(String, gprinterp::Document)>,
    /// Users whose picks could not be carried there and are used as
    /// stored (#434), sorted.
    not_carried: Vec<String>,
}

/// Read the picks that feed an item with this `audience`, for this caller,
/// carried onto the revision being served (#434): what the viewer draws.
fn visible_documents(
    state: &AppState,
    caller: &Caller,
    radargram: &crate::identity::RadargramId,
    audience: Audience,
) -> Result<Visible, ApiError> {
    let project = readable_project(state)?;
    // Read once: every document is carried onto the same revision.
    let target = {
        let catalog = state.catalog();
        let entry = lookup_dataset(&catalog, radargram.as_str())?;
        let path = state
            .absolute_path(entry)
            .map_err(|e| ApiError::internal("path_resolve_failed", e))?;
        let axes = super::interp_routes::current_axes(project, &path, entry);
        (entry.revision_id.clone(), axes)
    };
    // Skip and report (#213): a filename that is not a valid `UserId` used to
    // fail the whole request, so one bad name hid every good one.
    let (users, unreadable) =
        interpretations::list_users_checked(project.documents(), radargram)
            .map_err(|e| ApiError::internal("interpretation_read_failed", e.to_string()))?;
    for stem in &unreadable {
        tracing::warn!(
            radargram = radargram.as_str(),
            stem = stem.as_str(),
            "skipping an interpretation whose filename is not a valid user id"
        );
    }

    let mut documents = Vec::new();
    let mut not_carried = Vec::new();
    for parsed in users {
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
            let (document, refused) = crate::interp::carry::for_evaluation(
                project,
                radargram,
                &target.1,
                target.0.as_str(),
                stored.document,
            );
            if refused {
                not_carried.push(parsed.as_str().to_string());
            }
            documents.push((parsed.as_str().to_string(), document));
        }
    }
    Ok(Visible {
        documents,
        not_carried,
    })
}

/// Picks reduced for an evaluation, and whose went in as stored.
struct Reduced {
    picks: ReducedPicks,
    not_carried: Vec<String>,
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
    geometry: &RadargramGeometry,
    audience: Audience,
) -> Result<Reduced, ApiError> {
    // The viewer draws derived items per native trace, so its routes stay on
    // the per-trace grid. The export evaluates on the radargram's arc grid
    // instead, so a derived point and a picked point coincide.
    let grid: Vec<f64> = (0..geometry.n_traces()).map(|t| t as f64).collect();
    reduce_on(state, caller, radargram, geometry, audience, &grid)
}

/// Build the reduced picks on an explicit grid of fractional traces.
fn reduce_on(
    state: &AppState,
    caller: &Caller,
    radargram: &crate::identity::RadargramId,
    geometry: &RadargramGeometry,
    audience: Audience,
    grid: &[f64],
) -> Result<Reduced, ApiError> {
    let project = readable_project(state)?;
    let (layer_set, _) = layers::read(project.documents()).map_err(layer_error)?;
    let visible = visible_documents(state, caller, radargram, audience)?;
    let positions: Vec<GridPosition> = grid
        .iter()
        .map(|trace| GridPosition {
            trace: *trace,
            distance_m: level2::interpolate_index(&geometry.distance, *trace),
        })
        .collect();
    let picks = derive::reduce_picks(&visible.documents, &layer_set, geometry, &positions, false);
    Ok(Reduced {
        picks,
        not_carried: visible.not_carried,
    })
}

/// Log the items an export or download leaves out because they failed.
fn log_failures(radargram: &crate::identity::RadargramId, evaluation: &derived::Evaluation) {
    for (id, error) in &evaluation.failures {
        tracing::warn!(
            radargram = radargram.as_str(),
            item = id.as_str(),
            "skipping a derived item that cannot be evaluated: {error}"
        );
    }
}

/// Log whose picks a download evaluated as stored (#434). A file has nowhere
/// to say it, so the server's log does.
fn log_not_carried(radargram: &crate::identity::RadargramId, users: &[String]) {
    if !users.is_empty() {
        tracing::warn!(
            radargram = radargram.as_str(),
            users = users.join(", "),
            "derived results use picks that could not be carried onto this revision, as stored"
        );
    }
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

/// The derived items the caller may see, with an `ETag`.
#[utoipa::path(
    get,
    path = "/api/v1/derived",
    tag = "Derived items",
    responses(
        (status = 200, description = "The items.", body = DerivedItems),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
pub async fn get_derived(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let (set, version) = load_set(&state)?;
    let project = readable_project(&state)?;
    let (layer_set, _) = layers::read(project.documents()).map_err(layer_error)?;

    let user = caller.display_name().to_string();
    // How many other items reference each item. Computed over the *whole*
    // stored set, not just the caller's visible partition: an invisible item
    // can depend on a visible one, and deleting it would then be refused, so
    // the counter has to say so or the refusal looks arbitrary. Only a count
    // is exposed, never who.
    let mut used_by: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for item in &set.items {
        for dependency in set.dependencies(&item.id) {
            *used_by.entry(dependency).or_insert(0) += 1;
        }
    }

    // Inferred once for the whole set, each item with its own outcome, so a
    // broken item reads as an error without taking the others with it (#271).
    let kinds = set.inferred_kinds(&layer_set);
    let items: Vec<DerivedItemView> = set
        .visible_to(&user)
        .into_iter()
        .map(|item| {
            let kind = match &kinds {
                Ok(kinds) => match kinds.get(&item.id) {
                    Some(Ok(kind)) => kind.to_string(),
                    Some(Err(e)) => format!("error: {e}"),
                    None => "error: not evaluated".to_string(),
                },
                Err(e) => format!("error: {e}"),
            };
            DerivedItemView {
                id: item.id.clone(),
                name: item.name.clone(),
                expression: item.expression.clone(),
                unit: item.unit,
                kind,
                color: item.color.clone(),
                show: item.show,
                listed: item.listed,
                used_by: used_by.get(&item.id).copied().unwrap_or(0),
                fill_to: item.fill_to.clone(),
                scope: item.scope.clone(),
                audience: item.audience,
            }
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
        Json(DerivedItems {
            items,
            layers_unusable_in_expressions: unusable,
            // The editor asks the server what it may do rather than guessing
            // from a role string, the same rule as the contributor toggle.
            can_author: caller.may(Role::Operator),
            can_release: caller.may(Role::Admin),
        }),
    ))
}

/// `GET /api/v1/derived`: the derived items the caller may see.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct DerivedItems {
    items: Vec<DerivedItemView>,
    /// Layers whose ids an expression cannot name.
    layers_unusable_in_expressions: Vec<String>,
    /// Whether the caller may author project-wide items (`operator`).
    can_author: bool,
    /// Whether the caller may release a cross-user result (`admin`).
    can_release: bool,
}

/// One derived item, with what it was inferred to be.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct DerivedItemView {
    id: String,
    name: String,
    expression: String,
    unit: derive::Unit,
    /// `layer` or `attribute`, or why it cannot be evaluated, as
    /// `error: …`.
    kind: String,
    #[schema(required = true)]
    color: Option<String>,
    show: bool,
    listed: bool,
    /// How many other items use it.
    used_by: usize,
    #[schema(required = true)]
    fill_to: Option<derived::FillTo>,
    scope: Scope,
    audience: Audience,
}

/// `PUT /api/v1/derived`: the set as saved.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct SavedDerived {
    /// The new version, as the `ETag` gives it.
    version: String,
}

/// Replace the derived items. Project-wide items need `operator`; a private
/// item can only be saved by its owner, and releasing a result computed from
/// other people's picks to everyone needs `admin`. Takes `If-Match`.
#[utoipa::path(
    put,
    path = "/api/v1/derived",
    tag = "Derived items",
    request_body = DerivedSet,
    responses(
        (status = 200, description = "Saved.", body = SavedDerived),
        (status = 400, description = "An item that cannot be saved, such as a cycle between items.", body = ErrorBody),
        (status = 412, description = "`version_conflict`: the `If-Match` is not the current version.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn)))),
)]
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

    // Merge rather than replace. `GET /api/v1/derived` returns only the items
    // the caller may see, and the editor saves back what it loaded, so writing
    // the body wholesale deletes every other user's private item -- and the
    // panel is the ordinary way to author an expression, so that is the normal
    // path, not an edge case.
    //
    // Items the caller cannot see are preserved verbatim and appended: order
    // decides draw order, but nobody ever sees both partitions at once, so
    // where they sit relative to each other cannot matter.
    let (stored, _) = derived::read(project.documents()).map_err(derived_error)?;
    let viewer = caller.display_name();
    let preserved: Vec<DerivedItem> = stored
        .items
        .iter()
        .filter(|item| !item.visible_to(viewer))
        .cloned()
        .collect();
    for item in &set.items {
        if let Some(clash) = preserved.iter().find(|p| p.id == item.id) {
            // Refused rather than merged: the caller cannot see what they
            // would be overwriting, so there is no way for them to have meant
            // it. Naming the owner would leak who has what, so it does not.
            let _ = clash;
            return Err(ApiError::conflict(
                "derived_id_taken",
                format!(
                    "The derived id '{}' is already used by an item you cannot see. \
                     Choose another id.",
                    item.id
                ),
            ));
        }
    }
    // Refuse to delete an item another item depends on. A save that omits a
    // visible item is a delete, and the client cannot check this itself: the
    // dependent may be a private item it cannot see, or the dependency may
    // have been added by hand. The stored set is the only place both
    // partitions and the whole graph are visible, so the check belongs here.
    //
    // Detected against the *stored* graph, where the item being deleted still
    // exists -- `dependencies` only counts references to items that are
    // present, so after the delete it would report nothing.
    let incoming: Vec<&str> = set.items.iter().map(|item| item.id.as_str()).collect();
    let deleted: Vec<&str> = stored
        .items
        .iter()
        .filter(|item| item.visible_to(viewer))
        .map(|item| item.id.as_str())
        .filter(|id| !incoming.contains(id))
        .collect();
    if !deleted.is_empty() {
        for item in set.items.iter().chain(preserved.iter()) {
            for dependency in stored.dependencies(&item.id) {
                if deleted.contains(&dependency.as_str()) {
                    return Err(ApiError::conflict(
                        "derived_item_in_use",
                        format!(
                            "Cannot delete '{}': the derived item '{}' depends on it. \
                             Delete or change '{}' first.",
                            dependency, item.id, item.id
                        ),
                    ));
                }
            }
        }
    }

    let mut merged = set;
    merged.items.extend(preserved);

    // Refuse an expression that cannot be evaluated (#271), but only one this
    // save adds or changes. An item that was valid when saved can break later
    // -- its layer is deleted (#276) -- and refusing it here would stop every
    // other edit until someone fixed it, including deleting it.
    let (layer_set, _) = layers::read(project.documents()).map_err(layer_error)?;
    let kinds = merged.inferred_kinds(&layer_set).map_err(derived_error)?;
    for item in &merged.items {
        let unchanged = stored
            .get(&item.id)
            .is_some_and(|before| before.expression == item.expression && before.unit == item.unit);
        if unchanged {
            continue;
        }
        if let Some(Err(error)) = kinds.get(&item.id) {
            return Err(ApiError::bad_request(
                "invalid_derived_items",
                format!("'{}': {error}", item.id),
            ));
        }
    }

    let version = derived::write(project.documents(), &merged, &expected).map_err(derived_error)?;
    let mut response = HeaderMap::new();
    if let Ok(value) = format!("\"{version}\"").parse() {
        response.insert(header::ETAG, value);
    }
    Ok((
        response,
        Json(SavedDerived {
            version: version.as_str().to_string(),
        }),
    ))
}

/// The interpretations the caller may see, for the layer panel.
#[utoipa::path(
    get,
    path = "/api/v1/datasets/{radargram_id}/contributors",
    tag = "Derived items",
    responses(
        (status = 200, description = "The interpretations.", body = Contributors),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
// the caller may see, for the layer panel's contributor overlay (#209).
//
// Each entry has the stored `document`, the `view` to draw on the revision
// being served (`null` when it cannot be carried), and the `carry` report
// that `GET .../carried` gives for the caller's own (#434).
//
// The obvious route, `GET .../interpretations/{user}`, takes no `Caller` at
// all (#212) and therefore cannot decide what a caller may see, so the panel
// must not use it. This one returns the caller's own document always, and
// everyone's only to a caller who may see cross-user results. `can_see_others`
// says which, so the panel shows or hides its "show all contributors" toggle
// from the server's answer rather than guessing from a role string.
pub async fn get_contributors(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let radargram = parse_radargram(&radargram_id)?;
    // Another contributor's picks are raw picks, whatever route they leave by.
    // `get_interpretation_raw` serves the same bytes behind `Picks`, and two
    // routes disclosing identical data under different gates is exactly how
    // #212 happened. A project that set a scope below `Picks` said those
    // documents should not leave the server; it did not say "unless the
    // request came from the layer panel".
    //
    // Role is still the first gate -- this only narrows what an operator may
    // have. The caller's own picks are never gated: they already have them,
    // and the viewer has always drawn them.
    let can_see_others = may_see_others_picks(&caller);
    let project = readable_project(&state)?;
    let (users, unreadable) = interpretations::list_users_checked(project.documents(), &radargram)
        .map_err(|e| ApiError::internal("interpretation_read_failed", e.to_string()))?;
    for stem in &unreadable {
        tracing::warn!(
            radargram = radargram.as_str(),
            stem = stem.as_str(),
            "skipping an interpretation whose filename is not a valid user id"
        );
    }

    // What every document is carried onto: the revision this catalog
    // serves. Read once, since it is the same for every contributor.
    // `None` when the radargram is not catalogued, where there is nothing
    // to carry onto and the documents go back as they are.
    let target = {
        let catalog = state.catalog();
        lookup_dataset(&catalog, radargram.as_str())
            .ok()
            .and_then(|entry| {
                let path = state.absolute_path(entry).ok()?;
                let axes = super::interp_routes::current_axes(project, &path, entry);
                let declared = source::read_axis_declarations(&path).ok();
                Some((entry.revision_id.clone(), axes, declared))
            })
    };

    let mut documents = Vec::new();
    for parsed in users {
        let user = parsed.as_str().to_string();
        let is_own = caller.user.as_ref() == Some(&parsed);
        if !can_see_others && !is_own {
            continue;
        }
        if let Some(stored) = interpretations::read(project.documents(), &radargram, &parsed)
            .map_err(|e| ApiError::internal("interpretation_read_failed", e.to_string()))?
        {
            let document = serde_json::to_value(&stored.document)
                .map_err(|e| ApiError::internal("interpretation_read_failed", e.to_string()))?;
            // Carried exactly as `GET .../carried` carries the caller's own
            // (#434). Drawing a stored document at its stored indices is
            // only right on the revision it was drawn on; anywhere else the
            // panel draws `view`, and when there is none it has the report
            // to say why, rather than drawing an earlier revision's indices
            // as if they were this one's (gprinterp SPEC §8.1).
            let (view, carry) = match &target {
                Some((revision, axes, declared)) => {
                    let source =
                        crate::interp::carry::with_kept_axes(project, &radargram, &stored.document);
                    let mut carried = crate::interp::carry::carry(&source, axes, revision.as_str());
                    super::interp_routes::add_processing_warnings(
                        &mut carried,
                        project,
                        &radargram,
                        &stored.document,
                        declared.as_ref(),
                    );
                    (
                        serde_json::to_value(&carried.document).unwrap_or_default(),
                        serde_json::to_value(&carried.report).unwrap_or_default(),
                    )
                }
                None => (document.clone(), serde_json::Value::Null),
            };
            documents.push(Contributor {
                user,
                own: is_own,
                document,
                view,
                carry,
            });
        }
    }

    Ok(Json(Contributors {
        can_see_others,
        documents,
        // Reported rather than swallowed, so the viewer can say a contributor
        // was dropped instead of quietly showing fewer than exist (#213).
        unreadable,
    }))
}

/// `GET /api/v1/datasets/{radargram_id}/contributors`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct Contributors {
    /// Whether the caller sees everyone's picks, or only their own.
    can_see_others: bool,
    documents: Vec<Contributor>,
    /// Interpretations left out because their file name is not a valid user
    /// id.
    unreadable: Vec<String>,
}

/// One person's interpretation of the radargram, for the layer panel.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct Contributor {
    user: String,
    /// Whether it is the caller's.
    own: bool,
    /// As stored.
    #[schema(value_type = gprinterp::Document)]
    document: serde_json::Value,
    /// As drawn on the current revision: carried there, if it was picked on
    /// an earlier one.
    #[schema(value_type = gprinterp::Document)]
    view: serde_json::Value,
    /// What carrying it changed, or `null` when the radargram is not
    /// served.
    #[schema(required = true, value_type = Option<crate::interp::carry::CarryReport>)]
    carry: serde_json::Value,
}

// Deliberately carries **no audience**. `Audience::Released` grants the
// cross-user pick set unconditionally, which is only sound because on a
// stored item it can only have been put there by an admin. Accepting it
// from a request body would turn that admin decision into a self-service
// one -- a picker asks for `released` and gets everyone's data. A preview
// is always evaluated as `OwnPicks`, which for an operator already means
// everyone (rule 3), so nothing is lost by refusing to take it.

/// The body of `POST /api/v1/datasets/{radargram_id}/derived/preview`: an
/// expression, evaluated over the caller's picks as a saved item would be.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct PreviewBody {
    pub expression: String,
    /// The unit it is written in. Default: metres.
    #[serde(default)]
    pub unit: Option<crate::interp::derive::Unit>,
}

/// Evaluate an unsaved expression over the caller's picks, without storing
/// anything.
#[utoipa::path(
    post,
    path = "/api/v1/datasets/{radargram_id}/derived/preview",
    tag = "Derived items",
    request_body = PreviewBody,
    responses(
        (status = 200, description = "Its values.", body = Preview),
        (status = 400, description = "`invalid_expression`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::download(DownloadScope::Results)))),
)]
// expression over the caller's picks.
//
// The editor's live preview is the best guard against a sign or unit
// mistake -- `bed - cts` the wrong way round is obvious as a line and
// invisible as a number -- so it evaluates without writing anything. The
// expression is added to a copy of the set so it can reference the project's
// derived items, and only its own result is returned.
pub async fn preview_derived(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
    Json(body): Json<PreviewBody>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_download(DownloadScope::Results, "derived preview")?;
    let radargram = parse_radargram(&radargram_id)?;
    let (set, _) = load_set(&state)?;
    let geometry = geometry_for(&state, &radargram)?;
    // Never from the body -- see `PreviewBody`.
    let Reduced {
        picks: reduced,
        not_carried,
    } = reduce_for(&state, &caller, &radargram, &geometry, Audience::OwnPicks)?;

    let mut preview = set.clone();
    preview.items.retain(|item| item.id != "preview");
    preview.items.push(DerivedItem {
        id: "preview".to_string(),
        name: "preview".to_string(),
        expression: body.expression.clone(),
        unit: body.unit.unwrap_or(crate::interp::derive::Unit::Meters),
        color: None,
        show: false,
        listed: true,
        fill_to: None,
        scope: Scope::Project,
        audience: Audience::OwnPicks,
        extra: Default::default(),
    });

    let evaluation = preview
        .evaluate(&reduced, &geometry)
        .map_err(|e| ApiError::bad_request("invalid_expression", e.to_string()))?;
    if let Some(error) = evaluation.failures.get("preview") {
        return Err(ApiError::bad_request(
            "invalid_expression",
            error.to_string(),
        ));
    }
    let result = &evaluation.results["preview"];
    let values: Vec<Option<f64>> = result
        .values
        .iter()
        .map(|v| (!v.is_nan()).then_some(*v))
        .collect();

    Ok(Json(Preview {
        kind: result.kind,
        unit: result.unit,
        values,
        // How many contributors fed the evaluation (#241). The editor shows
        // it next to the kind so `median(bed)` reading "over 7 contributors"
        // is visibly a different quantity from a per-contributor line.
        contributors: reduced.users.len(),
        unknown_users: unknown_users(&caller, &body.expression, &reduced.users),
        // Whose picks could not be carried onto this revision and went in
        // as stored (#434), so the editor can say the result leans on them.
        not_carried,
    }))
}

/// `POST /api/v1/datasets/{radargram_id}/derived/preview`: an unsaved
/// expression's values.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct Preview {
    kind: derive::Kind,
    unit: derive::Unit,
    /// One value per position, `null` where there is none.
    values: Vec<Option<f64>>,
    /// How many people's picks went into it.
    contributors: usize,
    /// Users the expression names whose picks it did not have.
    unknown_users: Vec<String>,
    /// Users whose picks could not be carried onto the current revision and
    /// were used as stored.
    not_carried: Vec<String>,
}

/// The usernames an expression names that have no picks on this radargram.
///
/// `only` and `without` match nothing for a name nobody has, without an
/// error (#326), so a typo would otherwise pass unnoticed. Only a caller who
/// sees everyone's picks is told: anyone else sees only their own, so every
/// other name would be listed, which says nothing about whether it is a
/// typo.
fn unknown_users(caller: &Caller, expression: &str, users: &[String]) -> Vec<String> {
    if !may_see_cross_user(caller) {
        return Vec::new();
    }
    let engine = crate::interp::derive::build_engine();
    let Ok(ast) = crate::interp::derive::compile(&engine, expression) else {
        return Vec::new();
    };
    crate::interp::derive::named_users(&ast)
        .into_iter()
        .filter(|name| !users.contains(name))
        .collect()
}

/// One derived item's values along the radargram.
#[utoipa::path(
    get,
    path = "/api/v1/datasets/{radargram_id}/derived/{item}",
    tag = "Derived items",
    responses(
        (status = 200, description = "Its values.", body = DerivedValues),
        (status = 400, description = "`derived_failed`: it cannot be evaluated here.", body = ErrorBody),
        (status = 403, description = "`not_your_item`: someone else's private item.", body = ErrorBody),
        (status = 404, description = "`derived_item_not_found`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::download(DownloadScope::Results)))),
)]
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
    let Reduced {
        picks: reduced,
        not_carried,
    } = reduce_for(&state, &caller, &radargram, &geometry, item.audience)?;
    let evaluation = set
        .evaluate(&reduced, &geometry)
        .map_err(|e| ApiError::bad_request("derived_failed", e.to_string()))?;
    if let Some(error) = evaluation.failures.get(&item_id) {
        return Err(ApiError::bad_request("derived_failed", error.to_string()));
    }
    let result = &evaluation.results[&item_id];

    let values: Vec<Option<f64>> = result
        .values
        .iter()
        .map(|v| (!v.is_nan()).then_some(*v))
        .collect();

    Ok(Json(DerivedValues {
        radargram_id: radargram.as_str().to_string(),
        id: item.id.clone(),
        kind: result.kind,
        unit: result.unit,
        trace: (0..geometry.n_traces()).collect(),
        values,
        not_carried,
    }))
}

/// `GET /api/v1/datasets/{radargram_id}/derived/{item}`: one item's values
/// along the radargram.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct DerivedValues {
    radargram_id: String,
    id: String,
    kind: derive::Kind,
    unit: derive::Unit,
    /// The trace each value is at.
    trace: Vec<usize>,
    /// One value per trace, `null` where there is none.
    values: Vec<Option<f64>>,
    /// Users whose picks could not be carried onto the current revision and
    /// were used as stored.
    not_carried: Vec<String>,
}

/// Every derived item the caller may see, as a long-format CSV: one row per
/// item per position.
#[utoipa::path(
    get,
    path = "/api/v1/datasets/{radargram_id}/derived",
    tag = "Derived items",
    responses(
        (status = 200, description = "The CSV, as an attachment.", content_type = "text/csv"),
    ),
    extensions(("x-ridal-needs" = json!(Needs::download(DownloadScope::Results)))),
)]
// Long format: one row per item per grid position. Gaps are empty, which is
// how a NaN reads in a CSV.
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
        let reduced = reduce_for(&state, &caller, &radargram, &geometry, audience)?;
        log_not_carried(&radargram, &reduced.not_carried);
        let reduced = reduced.picks;
        let evaluation = set
            .evaluate(&reduced, &geometry)
            .map_err(|e| ApiError::bad_request("derived_failed", e.to_string()))?;
        log_failures(&radargram, &evaluation);
        per_audience.insert(audience, evaluation.results);
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
    set_header(&mut headers, header::CACHE_CONTROL, "no-store".to_string());
    set_header(
        &mut headers,
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\""),
    );
    Ok((headers, body))
}

/// A derived layer points export: the picked-points options plus the listing
/// choice.
#[derive(serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DerivedPointsQuery {
    /// `auto`, `per-trace`, `vertices`, or a distance in metres.
    #[serde(default)]
    pub spacing: Option<String>,
    /// `geojson` (default) or `csv`.
    #[serde(default)]
    pub format: Option<String>,
    /// Coordinates for the GeoJSON geometry: absent for WGS84, `native` for
    /// the radargram's own projected CRS, or any CRS PROJ accepts.
    #[serde(default)]
    pub crs: Option<String>,
    /// Include derived layers marked "not listed". Off by default, matching
    /// the viewer panel: unlisted means "keep it out of the way", so an
    /// export takes the same set unless asked otherwise.
    #[serde(default)]
    pub include_unlisted: bool,
}

/// Export one radargram's derived items as a wide point product.
///
/// Shared with the merged (group/catalog) export so both produce exactly the
/// same points for the same radargram; only the assembly differs.
pub(crate) fn export_derived_points(
    state: &AppState,
    caller: &Caller,
    radargram: &crate::identity::RadargramId,
    geometry: &RadargramGeometry,
    spacing: level2::Spacing,
    include_unlisted: bool,
) -> Result<DerivedPointsExport, ApiError> {
    // `vertices` describes a drawn polyline; a derived item is one value per
    // position, so its only sensible reading is per-trace. Everything else
    // evaluates on the radargram's shared arc grid, exactly as the picked
    // export samples, so a derived point and a picked point coincide.
    let spacing = match spacing {
        level2::Spacing::Vertices => level2::Spacing::PerTrace,
        other => other,
    };
    let grid = level2::grid(geometry, spacing)
        .map_err(|e| ApiError::bad_request("level2_failed", e.to_string()))?
        .unwrap_or_default();
    let spacing_m = match spacing {
        level2::Spacing::ArcLength(step) => Some(step),
        level2::Spacing::Auto => Some(level2::auto_step(&geometry.distance)),
        level2::Spacing::PerTrace | level2::Spacing::Vertices => None,
    };
    let (set, _) = load_set(state)?;
    let reduced = reduce_on(
        state,
        caller,
        radargram,
        geometry,
        Audience::OwnPicks,
        &grid,
    )?;
    log_not_carried(radargram, &reduced.not_carried);
    let reduced = reduced.picks;
    let evaluation = set
        .evaluate(&reduced, geometry)
        .map_err(|e| ApiError::bad_request("derived_failed", e.to_string()))?;
    // A broken item is left out rather than failing the export (#276): the
    // layers pane is what defines which layers exist, and an item naming one
    // that has been deleted has nothing to export.
    log_failures(radargram, &evaluation);
    let viewer = caller.display_name().to_string();
    derived_points::build(
        &set,
        &evaluation.results,
        geometry,
        &grid,
        spacing_m,
        &viewer,
        include_unlisted,
    )
    .map_err(|e| ApiError::bad_request("derived_property_collision", e.to_string()))
}

/// Derived layers as points.
#[utoipa::path(
    get,
    path = "/api/v1/datasets/{radargram_id}/derived/level2",
    tag = "Level 2 points",
    params(DerivedPointsQuery),
    responses(
        (status = 200, description = "The points, as an attachment.", content(("application/geo+json"), ("text/csv"))),
    ),
    extensions(("x-ridal-needs" = json!(Needs::download(DownloadScope::Results)))),
)]
// points, in the same formats and spacings as the picked-layer export.
pub async fn derived_level2(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
    Query(query): Query<DerivedPointsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_download(DownloadScope::Results, "derived layer points")?;
    let radargram = parse_radargram(&radargram_id)?;
    let geometry = geometry_for(&state, &radargram)?;
    let spacing = crate::cli::parse_spacing(query.spacing.as_deref().unwrap_or("auto"))
        .map_err(|e| ApiError::bad_request("invalid_spacing", e))?;
    let export = export_derived_points(
        &state,
        &caller,
        &radargram,
        &geometry,
        spacing,
        query.include_unlisted,
    )?;
    // A single radargram's derived product is already grid-capped, but the
    // same total budget applies so the limit is one thing everywhere (#135).
    level2::check_export_size(export.points.len(), spacing, export.spacing_m)
        .map_err(|e| ApiError::bad_request("level2_failed", e.to_string()))?;

    let exports = [export];
    let csv = matches!(query.format.as_deref(), Some("csv"));
    let (body, content_type, extension) = if csv {
        (
            writer::to_csv_derived(&exports),
            "text/csv; charset=utf-8",
            "csv",
        )
    } else {
        let output_crs = match query.crs.as_deref() {
            None | Some("") => writer::OutputCrs::Wgs84,
            Some(name) => writer::OutputCrs::Named(name.to_string()),
        };
        (
            writer::to_geojson_derived(&exports, &output_crs)
                .map_err(|e| ApiError::bad_request("invalid_crs", e))?,
            "application/geo+json",
            "geojson",
        )
    };

    let filename = format!("{}-derived-layer-points.{extension}", radargram.as_str());
    let mut headers = HeaderMap::new();
    let set_header = |headers: &mut HeaderMap, name: header::HeaderName, value: String| {
        if let Ok(value) = value.parse() {
            headers.insert(name, value);
        }
    };
    set_header(&mut headers, header::CONTENT_TYPE, content_type.to_string());
    set_header(&mut headers, header::CACHE_CONTROL, "no-store".to_string());
    set_header(
        &mut headers,
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\""),
    );
    Ok((headers, body))
}
