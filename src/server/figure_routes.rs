//! `POST /api/v1/datasets/{id}/figure` (#142): an annotated figure of a
//! radargram from a figure spec, the same figure `ridal render --figure`
//! makes of the file.
//!
//! The spec is the request body, as JSON, and `?format=` picks the output:
//! `png` (the default), `svg`, or `json` for the sidecar of a figure without
//! axes. Everything else about the figure, including its render profile, is
//! in the spec, so a figure is repeatable from the spec alone.
//!
//! What may be drawn follows the routes that already serve the same data:
//!
//! - The figure itself is a rendered image of the radargram, a download at
//!   [`DownloadScope::Derived`], as `GET .../image` is.
//! - Picks: the caller's own always; anyone else's only to a caller who may
//!   see them through the contributor overlay (`GET .../contributors`). A
//!   spec naming another user's picks without that is refused rather than
//!   drawn without them.
//! - Derived layers need [`DownloadScope::Results`], as `GET .../derived/
//!   {item}` does, and each is evaluated over exactly the picks its audience
//!   permits this caller, by the rule in `derived_routes`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::app::AppState;
use super::auth::Caller;
use super::interp_routes::readable_project;
use super::needs::Needs;
use super::routes::{attachment, entry_path, lookup_dataset, ApiError, ErrorBody};
use crate::project::derived::Audience;
use crate::project::roles::DownloadScope;
use crate::render::figure::overlays::{LayerAccess, Readable};
use crate::render::figure::spec::FigureSpec;
use crate::render::figure::Sidecar;
use crate::render::figure::{FigureError, FigureFormat};

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FigureQuery {
    /// `png` (the default), `svg`, or `json` for the sidecar of a figure
    /// without axes.
    pub format: Option<String>,
}

/// An annotated figure, from a figure spec sent as the JSON body. Picks
/// other than the caller's own need what the contributor overlay needs, and
/// derived layers download `results`; a spec asking for more is refused.
/// Picks that could not be drawn as stored are listed in the
/// `X-Ridal-Figure-Warnings` header, as a JSON list.
#[utoipa::path(
    post,
    path = "/api/v1/datasets/{radargram_id}/figure",
    tag = "Images",
    params(FigureQuery),
    request_body = FigureSpec,
    responses(
        (status = 200, description = "The figure as an attachment, or the sidecar of one without axes.", content(("image/png"), ("image/svg+xml"), (Sidecar = "application/json"))),
        (status = 400, description = "`invalid_figure_spec`.", body = ErrorBody),
        (status = 409, description = "`revision_mismatch`: the spec names another revision.", body = ErrorBody),
        (status = 404, description = "No such radargram.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::download(DownloadScope::Derived)))),
)]
pub async fn render_figure(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
    Query(query): Query<FigureQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    caller.require_download(DownloadScope::Derived, "a figure")?;
    let text = std::str::from_utf8(&body).map_err(|_| {
        ApiError::bad_request("invalid_figure_spec", "The figure spec is not UTF-8.")
    })?;
    let spec =
        FigureSpec::from_json(text).map_err(|e| ApiError::bad_request("invalid_figure_spec", e))?;
    let format = match query.format.as_deref() {
        None | Some("") | Some("png") => FigureFormat::Png,
        Some("svg") => FigureFormat::Svg,
        Some("json") => FigureFormat::Sidecar,
        Some(other) => {
            return Err(ApiError::bad_request(
                "invalid_format",
                format!("Unknown figure format '{other}'. Use 'png', 'svg' or 'json'."),
            ))
        }
    };
    // By name only: on the command line a profile may also be a TOML file,
    // which a request must never be able to make the server read.
    let profile = super::routes::lookup_profile(&state, &caller, &spec.profile)?;
    let access = layer_access(&caller, &spec)?;

    let catalog = state.catalog();
    let entry = lookup_dataset(&catalog, &radargram_id)?;
    let radargram_key = entry.radargram_id.clone();
    let revision = entry.revision_id.clone();
    let range = entry.elevation_range();
    let path = entry_path(&state, entry)?;
    let radargram = catalog
        .radargram(entry.radargram_id.as_str())
        .ok_or_else(|| {
            ApiError::internal(
                "dataset_unavailable",
                "Dataset is cataloged but its render service failed to initialize.",
            )
        })?;
    drop(catalog);

    let wants_overlays = spec.has_overlays() && format != FigureFormat::Sidecar;
    if wants_overlays {
        // Fail early and with the right reason: a bare directory has no
        // picks to draw.
        readable_project(&state)?;
    }

    // A figure permit before a render permit, so an export waiting behind
    // another holds no render permit while it waits (#473).
    let figure_permit = acquire_figure_permit(&state).await?;
    let permit = super::routes::acquire_render_permit(&state).await?;
    let activity = state.render_config().activity.start(
        super::activity::RenderKind::Figure,
        &state.key,
        radargram_key.to_string(),
    );
    // Fires when this handler's future is dropped, which is when the
    // client goes away: nobody else wants this figure, so it stops at the
    // next band rather than finishing into the void.
    let (interrupt, _cancel_on_drop) = crate::render::interrupt::Interrupt::on_drop();
    let task_state = Arc::clone(&state);
    let figure = tokio::task::spawn_blocking(move || {
        let _permits = (figure_permit, permit);
        // Gathered before the render service is locked, so reading picks
        // and evaluating derived layers does not hold up the viewer's chunks
        // of this radargram.
        let overlays = if wants_overlays {
            let project = readable_project(&task_state)?;
            Some(
                crate::render::figure::overlays::gather(
                    project,
                    &radargram_key,
                    &path,
                    &revision,
                    &spec,
                    &access,
                )
                .map_err(figure_error)?,
            )
        } else {
            None
        };
        let figure = super::routes::with_locked_service(
            &task_state.render_config().open_readers,
            &radargram,
            |service| {
                activity.rendering();
                // From now: the wait for the lock is not the render's.
                let interrupt = interrupt.with_deadline(super::routes::BANDED_RENDER_DEADLINE);
                service
                    .render_figure(
                        &spec,
                        &profile,
                        format,
                        overlays.as_ref(),
                        range,
                        interrupt.clone(),
                    )
                    .map_err(|e| {
                        super::routes::interrupted_error(&interrupt)
                            .unwrap_or_else(|| figure_error(e))
                    })
            },
        )?;
        Ok::<_, ApiError>((figure, overlays.map(|o| o.warnings).unwrap_or_default()))
    })
    .await
    .map_err(|e| ApiError::internal("render_task_failed", format!("Render task failed: {e}")))??;
    let (figure, warnings) = figure;

    let filename = format!("{radargram_id}-figure.{}", format.extension());
    let mut response = (
        [
            (header::CONTENT_TYPE, format.content_type().to_string()),
            attachment(&filename),
        ],
        figure.bytes,
    )
        .into_response();
    if !warnings.is_empty() {
        if let Ok(value) = header::HeaderValue::from_str(&ascii_json(&warnings)) {
            response
                .headers_mut()
                .insert(header::HeaderName::from_static(WARNINGS_HEADER), value);
        }
    }
    Ok(response)
}

/// How long a figure export waits for another to finish before a 503.
const FIGURE_PERMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Take one of the server's figure permits (#473), before the render
/// permit: an export waiting for another export then holds nothing the
/// viewer needs.
pub(super) async fn acquire_figure_permit(
    state: &AppState,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    tokio::time::timeout(
        FIGURE_PERMIT_TIMEOUT,
        state.render_config().figure_permits.acquire_owned(),
    )
    .await
    .map_err(|_| {
        ApiError::service_unavailable(
            "figure_busy",
            "The server is exporting as many figures as it allows at once. Retry shortly.",
        )
        .with_header(header::RETRY_AFTER, "5".to_string())
    })?
    .map_err(|_| ApiError::internal("figure_permits_closed", "Figure permits were closed"))
}

/// The header that carries what could not be drawn as stored (picks that
/// could not be carried to this revision), as a JSON list of strings.
pub const WARNINGS_HEADER: &str = "x-ridal-figure-warnings";

/// `values` as JSON with every non-ASCII character escaped, so it is a
/// valid header value.
fn ascii_json(values: &[String]) -> String {
    let json = serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string());
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if c.is_ascii() && !c.is_ascii_control() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

fn figure_error(error: FigureError) -> ApiError {
    match error {
        FigureError::Spec(message) => ApiError::bad_request("invalid_figure_spec", message),
        error @ FigureError::RevisionMismatch { .. } => {
            ApiError::conflict("revision_mismatch", error.to_string())
        }
        FigureError::Render(message) => ApiError::internal("figure_failed", message),
    }
}

/// What `caller` may have drawn in a figure, or the refusal for a spec that
/// asks for more.
fn layer_access(caller: &Caller, spec: &FigureSpec) -> Result<LayerAccess, ApiError> {
    let own = caller.user.clone();
    let others = super::derived_routes::may_see_others_picks(caller);
    if !spec.picks.items.is_empty() && !others {
        let foreign = spec
            .picks
            .items
            .iter()
            .filter_map(|selector| selector.user.as_deref())
            .find(|user| own.as_ref().is_none_or(|own| own.as_str() != *user));
        if let Some(user) = foreign {
            return Err(ApiError::forbidden(
                "not_your_picks",
                format!(
                    "The figure asks for {user}'s picks, and {} may only draw their own.",
                    caller.display_name()
                ),
            ));
        }
    }
    if !spec.derived.items.is_empty() {
        caller.require_download(DownloadScope::Results, "derived results")?;
    }
    Ok(LayerAccess {
        picks: if others {
            Readable::All
        } else {
            Readable::Only(own)
        },
        own_picks_items: super::derived_routes::readable_for(caller, Audience::OwnPicks),
        released_items: super::derived_routes::readable_for(caller, Audience::Released),
        viewer: Some(caller.display_name().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_are_a_valid_header_value() {
        let text = ascii_json(&["Drønbreen — carried".to_string()]);
        assert!(text.is_ascii());
        let back: Vec<String> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, ["Drønbreen — carried"]);
        assert!(header::HeaderValue::from_str(&text).is_ok());
    }
}
