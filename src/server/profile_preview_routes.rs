//! `POST /api/v1/datasets/{id}/render-profile-preview` (#183): one image of
//! part of a radargram, drawn with a render profile that need not be saved.
//!
//! This is what the profile editor shows while a profile is tuned. Tiles
//! would be the wrong tool for that: every change would be a new set of
//! tiles at every zoom level, rendered and cached for a profile that is
//! about to change again. A preview is one image of the extent on screen,
//! drawn by the figure renderer without axes, and never cached; only saving
//! the profile makes the viewer render tiles with it.
//!
//! An unsaved profile is arbitrary input to the renderer, so a preview is
//! bounded on every side:
//!
//! - its size, at most [`MAX_PREVIEW_WIDTH`] x [`MAX_PREVIEW_HEIGHT`];
//! - one in flight per person: a new preview stops the one before it, which
//!   is what someone moving a slider wants anyway;
//! - the server's figure permits, so previews queue behind figure exports
//!   rather than taking the render permits the viewer's tiles need;
//! - who may ask: anyone who may keep personal profiles, and operators, who
//!   edit the project's. The project's switch for personal profiles is then
//!   also the switch for this route, for everyone else.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::app::AppState;
use super::auth::Caller;
use super::needs::{Needs, Who};
use super::routes::{lookup_dataset, ApiError, ErrorBody};
use crate::identity::UserId;
use crate::project::roles::Role;
use crate::render::figure::spec::{Extent, FigureSpec, Size, SizeUnit, View};
use crate::render::figure::{FigureError, FigureFormat};
use crate::render::interrupt::{CancelHandle, Interrupt};
use crate::render::profile::RenderProfile;

/// The widest preview, in pixels: about a large editor pane on a 2x screen.
pub const MAX_PREVIEW_WIDTH: u32 = 2000;

/// The tallest preview, in pixels. An extent taller than the width allows
/// gets a narrower image rather than a refusal, so a client can always ask
/// for its pane's width.
pub const MAX_PREVIEW_HEIGHT: u32 = 1500;

/// The narrowest preview the figure renderer draws without axes.
const MIN_PREVIEW_WIDTH: u32 = 10;

/// What to preview, and where.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfilePreviewRequest {
    /// The profile to draw with, saved or not. Its `name` is not looked up.
    pub profile: RenderProfile,
    /// The standard or the topographically corrected view.
    #[serde(default)]
    pub view: View,
    /// `[first, last]` trace position, as in a figure spec's extent.
    #[schema(min_items = 2, max_items = 2)]
    pub traces: [f64; 2],
    /// `[top, bottom]` row position, as in a figure spec's extent.
    #[schema(min_items = 2, max_items = 2)]
    pub rows: [f64; 2],
    /// Horizontal stretch, as the viewer's horizontal scale. Unset is 1.
    #[serde(default = "one")]
    #[schema(default = 1.0)]
    pub x_scale: f64,
    /// The image's width in pixels, at most 2000. Its height follows from
    /// the extent and `x_scale`; past 1500 px the width is reduced to keep
    /// the height there.
    pub width: u32,
}

fn one() -> f64 {
    1.0
}

/// The previews in flight, one per person: the handle that stops each.
#[derive(Debug, Default)]
pub struct Previews(Mutex<HashMap<UserId, CancelHandle>>);

/// A preview's place in [`Previews`], given up when it is dropped.
struct PreviewSlot<'a> {
    previews: &'a Previews,
    user: UserId,
    handle: CancelHandle,
}

impl Previews {
    /// Make `handle` `user`'s preview in flight, stopping the one before.
    fn claim(&self, user: &UserId, handle: CancelHandle) -> PreviewSlot<'_> {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = map.insert(user.clone(), handle.clone()) {
            previous.cancel();
        }
        PreviewSlot {
            previews: self,
            user: user.clone(),
            handle,
        }
    }
}

impl Drop for PreviewSlot<'_> {
    fn drop(&mut self) {
        let mut map = self.previews.0.lock().unwrap_or_else(|e| e.into_inner());
        // Only its own: a newer preview may already have taken the place.
        if map.get(&self.user).is_some_and(|h| h.same_as(&self.handle)) {
            map.remove(&self.user);
        }
    }
}

/// The preview's size in pixels: `width`, narrowed if the height that
/// follows from the extent would be above [`MAX_PREVIEW_HEIGHT`].
fn preview_width(request: &ProfilePreviewRequest) -> Result<u32, ApiError> {
    let invalid = |message: String| ApiError::bad_request("invalid_preview", message);
    let span = |[a, b]: [f64; 2], what: &str| {
        if a.is_finite() && b.is_finite() && a < b {
            Ok(b - a)
        } else {
            Err(invalid(format!(
                "{what} must be two finite positions, the first below the second, not [{a}, {b}]."
            )))
        }
    };
    let traces = span(request.traces, "traces")?;
    let rows = span(request.rows, "rows")?;
    if !(request.x_scale.is_finite() && request.x_scale > 0.0) {
        return Err(invalid(format!(
            "x_scale must be positive, not {}.",
            request.x_scale
        )));
    }
    if !(MIN_PREVIEW_WIDTH..=MAX_PREVIEW_WIDTH).contains(&request.width) {
        return Err(invalid(format!(
            "width must be {MIN_PREVIEW_WIDTH} to {MAX_PREVIEW_WIDTH} px, not {}.",
            request.width
        )));
    }
    // As the figure renderer lays out a figure without axes.
    let height_per_px = rows / (traces * request.x_scale);
    let width = f64::from(request.width).min(f64::from(MAX_PREVIEW_HEIGHT) / height_per_px);
    Ok((width.floor() as u32).max(MIN_PREVIEW_WIDTH))
}

/// Who may preview: anyone who may keep personal profiles, and operators.
fn require_preview(state: &AppState, caller: &Caller) -> Result<UserId, ApiError> {
    let user = caller.user.clone().ok_or_else(|| {
        ApiError::unauthorized(
            "authentication_required",
            "Sign in to tune a render profile: previews are for profiles of your own.",
        )
    })?;
    if state.personal_project().is_none() {
        caller.require(Role::Operator, "preview a render profile")?;
    }
    Ok(user)
}

/// A PNG of part of a radargram drawn with `profile`, which need not be
/// saved: what the profile editor shows. At most 2000 x 1500 px, never
/// cached, and one in flight per person, so a new preview stops the
/// previous one (which then answers `503 render_cancelled`). Needs a
/// signed-in caller in a project that allows personal profiles, or an
/// operator.
#[utoipa::path(
    post,
    path = "/api/v1/datasets/{radargram_id}/render-profile-preview",
    tag = "Render profiles",
    request_body = ProfilePreviewRequest,
    responses(
        (status = 200, description = "The preview.", content_type = "image/png"),
        (status = 400, description = "`invalid_preview` or `invalid_render_profile`.", body = ErrorBody),
        (status = 404, description = "No such radargram.", body = ErrorBody),
        (status = 503, description = "`render_cancelled`: a newer preview replaced it; `figure_busy`, `render_busy` or `render_timed_out`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn)))),
)]
pub async fn preview_render_profile(
    State(state): State<Arc<AppState>>,
    Path(radargram_id): Path<String>,
    caller: Caller,
    Json(request): Json<ProfilePreviewRequest>,
) -> Result<Response, ApiError> {
    let user = require_preview(&state, &caller)?;
    request
        .profile
        .validate()
        .map_err(|e| ApiError::bad_request("invalid_render_profile", e))?;
    let width = preview_width(&request)?;
    let spec = FigureSpec {
        view: request.view,
        profile: request.profile.name.clone(),
        x_scale: request.x_scale,
        extent: Extent {
            traces: Some(request.traces),
            rows: Some(request.rows),
            elevation: None,
        },
        size: Size {
            width: f64::from(width),
            unit: SizeUnit::Px,
            dpi: 96.0,
        },
        axes: false,
        ..FigureSpec::default()
    };
    spec.validate()
        .map_err(|e| ApiError::bad_request("invalid_preview", e))?;
    let profile = request.profile;

    let catalog = state.catalog();
    let entry = lookup_dataset(&catalog, &radargram_id)?;
    let radargram_key = entry.radargram_id.clone();
    let range = entry.elevation_range();
    let radargram = catalog
        .radargram(entry.radargram_id.as_str())
        .ok_or_else(|| {
            ApiError::internal(
                "dataset_unavailable",
                "Dataset is cataloged but its render service failed to initialize.",
            )
        })?;
    drop(catalog);

    // Claimed before waiting for permits, so a preview queued behind its
    // own predecessor stops that one rather than waiting for it.
    // The guard also fires when the client goes away, as for a figure.
    let (interrupt, cancel_on_drop) = Interrupt::on_drop();
    let _slot = state.previews.claim(&user, cancel_on_drop.handle());
    let figure_permit = super::figure_routes::acquire_figure_permit(&state).await?;
    let permit = super::routes::acquire_render_permit(&state).await?;
    if let Some(error) = super::routes::interrupted_error(&interrupt) {
        return Err(error);
    }
    let activity = state.render_config().activity.start(
        super::activity::RenderKind::Figure,
        &state.key,
        radargram_key.to_string(),
    );
    let task_state = Arc::clone(&state);
    let figure = tokio::task::spawn_blocking(move || {
        let _permits = (figure_permit, permit);
        super::routes::with_locked_service(
            &task_state.render_config().open_readers,
            &radargram,
            |service| {
                activity.rendering();
                let interrupt = interrupt.with_deadline(super::routes::BANDED_RENDER_DEADLINE);
                service
                    .render_figure(
                        &spec,
                        &profile,
                        FigureFormat::Png,
                        None,
                        range,
                        interrupt.clone(),
                    )
                    .map_err(|e| {
                        super::routes::interrupted_error(&interrupt).unwrap_or_else(|| match e {
                            FigureError::Spec(message) => {
                                ApiError::bad_request("invalid_preview", message)
                            }
                            other => ApiError::internal("preview_failed", other.to_string()),
                        })
                    })
            },
        )
    })
    .await
    .map_err(|e| ApiError::internal("render_task_failed", format!("Render task failed: {e}")))??;

    Ok((
        [
            (header::CONTENT_TYPE, "image/png".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        figure.bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        traces: [f64; 2],
        rows: [f64; 2],
        x_scale: f64,
        width: u32,
    ) -> ProfilePreviewRequest {
        ProfilePreviewRequest {
            profile: RenderProfile::default_profile(),
            view: View::Standard,
            traces,
            rows,
            x_scale,
            width,
        }
    }

    #[test]
    fn a_wide_extent_keeps_its_width() {
        let width = preview_width(&request([0.0, 1000.0], [0.0, 500.0], 1.0, 1200))
            .ok()
            .unwrap();
        assert_eq!(width, 1200);
    }

    #[test]
    fn a_tall_extent_is_narrowed_to_the_height_cap() {
        // 1 trace : 4 rows at x_scale 1 is four times as tall as wide.
        let width = preview_width(&request([0.0, 100.0], [0.0, 400.0], 1.0, 2000))
            .ok()
            .unwrap();
        assert_eq!(width, MAX_PREVIEW_HEIGHT / 4);
        // A larger x_scale stretches it wide again.
        let width = preview_width(&request([0.0, 100.0], [0.0, 400.0], 4.0, 2000))
            .ok()
            .unwrap();
        assert_eq!(width, MAX_PREVIEW_HEIGHT);
    }

    #[test]
    fn bad_extents_and_sizes_are_refused() {
        assert!(preview_width(&request([0.0, 10.0], [0.0, 10.0], 1.0, 2001)).is_err());
        assert!(preview_width(&request([0.0, 10.0], [0.0, 10.0], 1.0, 5)).is_err());
        assert!(preview_width(&request([10.0, 0.0], [0.0, 10.0], 1.0, 500)).is_err());
        assert!(preview_width(&request([0.0, f64::NAN], [0.0, 10.0], 1.0, 500)).is_err());
        assert!(preview_width(&request([0.0, 10.0], [0.0, 10.0], 0.0, 500)).is_err());
    }

    #[test]
    fn a_new_preview_stops_the_previous_one() {
        let previews = Previews::default();
        let user = UserId::new("alice").unwrap();
        let (first, first_guard) = Interrupt::on_drop();
        let first_slot = previews.claim(&user, first_guard.handle());
        let (second, second_guard) = Interrupt::on_drop();
        let second_slot = previews.claim(&user, second_guard.handle());
        assert!(first.interruption().is_some());
        assert!(second.interruption().is_none());
        // The first finishing must not free the second's place.
        drop(first_slot);
        assert!(previews.0.lock().unwrap().contains_key(&user));
        drop(second_slot);
        assert!(previews.0.lock().unwrap().is_empty());
        // Someone else's preview is left alone.
        let (other, other_guard) = Interrupt::on_drop();
        let _other_slot = previews.claim(&UserId::new("bob").unwrap(), other_guard.handle());
        let _third = previews.claim(&user, Interrupt::on_drop().1.handle());
        assert!(other.interruption().is_none());
    }
}
