//! A person's own settings in a project (#131): render profile, scale,
//! basemap and the like, as `GET`/`PUT /api/v1/preferences`.
//!
//! What they resolve through -- request, then person, then project, then
//! built-in -- is `routes::cascade`; this module only reads and writes the
//! person's own document.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;

use super::app::AppState;
use super::auth::Caller;
use super::routes::ApiError;
use crate::identity::UserId;
use crate::project::preferences;
use crate::project::store::Expectation;
use crate::project::Project;

/// The project, or a 409 explaining that this catalog is not one.
fn project(state: &AppState) -> Result<&Project, ApiError> {
    state.project.as_ref().ok_or_else(|| {
        ApiError::conflict(
            "not_a_project",
            "This catalog is not a Ridal project, so there is nowhere to keep \
             settings. Run `ridal project init` in the directory you are \
             serving, then restart.",
        )
    })
}

/// `GET /api/v1/preferences` -- the caller's own.
pub async fn get_preferences(
    State(state): State<Arc<AppState>>,
    caller: Caller,
) -> Result<impl IntoResponse, ApiError> {
    let (project, user) = my_preferences_target(&state, &caller)?;
    let preferences = preferences::read(project.documents(), user)
        .map_err(|e| ApiError::internal("preferences_read_failed", e.to_string()))?;
    Ok(Json(serde_json::json!({
        "user": user.as_str(),
        "render_profile": preferences.render_profile,
        "x_scale": preferences.x_scale,
        "theme": preferences.theme,
        "show_picks": preferences.show_picks,
        "level2_spacing": preferences.level2_spacing,
        "level2_format": preferences.level2_format,
        "basemap": preferences.basemap,
        "show_group_radargrams": preferences.show_group_radargrams,
        "open_zoomed_out": preferences.open_zoomed_out,
    })))
}

/// One person's settings, as the page submits them.
///
/// Every field distinguishes three things: absent leaves the stored value
/// alone, `null` clears it -- which is what "Project default" in the
/// dropdown means -- and a value sets it.
///
/// Absent used to mean "clear it", on the grounds that the page always
/// sends everything. That made the endpoint a trap for anything else:
/// `PUT {"theme":"dark"}` silently wiped this person's render profile and
/// scale. With six settings and more coming, "sends everything" is a
/// property of one caller rather than of the API, so it is no longer
/// assumed.
///
/// This narrows, but does not close, last-writer-wins between two settings
/// tabs: both send every field, so the later save still overwrites what the
/// earlier one changed. Deliberately left there. A preference is cheap to
/// re-choose and belongs to one person, so the conditional-write machinery
/// the picks and the layer vocabulary use -- ETag out, `If-Match` back --
/// would cost a round trip and a 412 dialog to protect a dropdown from its
/// owner's other tab.
#[derive(serde::Deserialize)]
pub struct PreferencesBody {
    #[serde(default, deserialize_with = "present")]
    render_profile: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    x_scale: Option<Option<f64>>,
    /// `light`, `dark`, or `null` to follow the device (#141).
    #[serde(default, deserialize_with = "present")]
    theme: Option<Option<String>>,
    /// Whether the viewer opens with the interpretations drawn (#143).
    #[serde(default, deserialize_with = "present")]
    show_picks: Option<Option<bool>>,
    /// Which basemap the maps draw on, by id (#177).
    #[serde(default, deserialize_with = "present")]
    basemap: Option<Option<String>>,
    /// What the layer-point download dialogs open on (#166).
    #[serde(default, deserialize_with = "present")]
    level2_spacing: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    level2_format: Option<Option<String>>,
    /// Whether the catalog opens with each group's radargrams listed
    /// (#314). `null` defers to the project.
    #[serde(default, deserialize_with = "present")]
    show_group_radargrams: Option<Option<bool>>,
    /// Whether the viewer opens on the whole radargram (#315).
    #[serde(default, deserialize_with = "present")]
    open_zoomed_out: Option<Option<bool>>,
}

/// Tell an absent key from one sent as `null`.
///
/// `Option<Option<T>>`: the outer layer is "was it mentioned", the inner is
/// "was it set". Serde has no built-in for the distinction, and without it
/// a partial update is indistinguishable from a request to clear whatever
/// it left out.
fn present<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// `PUT /api/v1/preferences`
///
/// A `viewer` may do this. Setting how you like to look at something is not
/// a permission an administrator grants -- it is the floor of what having an
/// account means.
pub async fn put_preferences(
    State(state): State<Arc<AppState>>,
    caller: Caller,
    Json(body): Json<PreferencesBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (project, user) = my_preferences_target(&state, &caller)?;

    // Read first, then apply what was sent: a field the request did not
    // mention keeps the value it had. Leniently, so that saving over a
    // hand-broken document repairs it rather than failing forever -- the
    // settings page reads strictly elsewhere, which is where a fault is
    // visible and fixable.
    let mut stored = preferences::read_lenient(project.documents(), user);

    // Validated here rather than in the store, for the same reason the
    // project's defaults are: which profiles exist and which scales the
    // viewer offers are server concepts, and storing one that nothing
    // renders would leave every page failing with no obvious cause.
    //
    // Every one of these keeps what it is sent, neutral values included
    // (#176): each dropdown offers "Project default" as its own entry, so
    // absence means deferring rather than agreeing, and collapsing a value
    // to absence because it matches the fallback would make that value
    // unsayable.
    if let Some(sent) = body.render_profile {
        stored.render_profile = match sent.as_deref() {
            None | Some("") => None,
            Some(name) => {
                if crate::render::profile::RenderProfile::by_name(name).is_none() {
                    return Err(ApiError::bad_request(
                        "unknown_profile",
                        format!("There is no render profile called '{name}'."),
                    ));
                }
                Some(name.to_string())
            }
        };
    }
    if let Some(sent) = body.x_scale {
        stored.x_scale = match sent {
            None => None,
            Some(scale) => {
                if !super::routes::is_offered_x_scale(scale) {
                    return Err(ApiError::bad_request(
                        "unknown_xscale",
                        format!("The viewer does not offer a horizontal scale of {scale}."),
                    ));
                }
                Some(scale)
            }
        };
    }
    if let Some(sent) = body.theme {
        stored.theme = match sent.as_deref() {
            None | Some("") => None,
            Some(name) => {
                if !super::routes::is_offered_theme(name) {
                    return Err(ApiError::bad_request(
                        "unknown_theme",
                        format!(
                            "'{name}' is not a theme. Use 'light', 'dark', or \
                             nothing at all to follow the device."
                        ),
                    ));
                }
                Some(name.to_string())
            }
        };
    }
    if let Some(sent) = body.level2_spacing {
        stored.level2_spacing = match sent.as_deref() {
            None | Some("") => None,
            Some(value) => {
                if !super::routes::is_offered_spacing(value) {
                    return Err(ApiError::bad_request(
                        "unknown_spacing",
                        format!("The download dialogs do not offer a spacing of '{value}'."),
                    ));
                }
                Some(value.to_string())
            }
        };
    }
    if let Some(sent) = body.level2_format {
        stored.level2_format = match sent.as_deref() {
            None | Some("") => None,
            Some(value) => {
                if !super::routes::is_offered_format(value) {
                    return Err(ApiError::bad_request(
                        "unknown_format",
                        format!("The download dialogs do not offer a format of '{value}'."),
                    ));
                }
                Some(value.to_string())
            }
        };
    }
    if let Some(sent) = body.show_picks {
        // The one that *is* collapsed, and the one exception the rule above
        // names: it is a checkbox with no "project default" state to offer,
        // so shown -- the built-in answer -- is stored as absence.
        stored.show_picks = sent.filter(|shown| !*shown);
    }
    if let Some(sent) = body.open_zoomed_out {
        // A checkbox too, with no project layer under it: zoomed in is the
        // built-in answer and is stored as absence.
        stored.open_zoomed_out = sent.filter(|zoomed_out| *zoomed_out);
    }
    if let Some(sent) = body.show_group_radargrams {
        // A dropdown with "Project default", so kept as sent, shown
        // included: in a project that hides them, "show them to me" has to
        // be sayable.
        stored.show_group_radargrams = sent;
    }

    if let Some(sent) = body.basemap {
        // Same rule again: a basemap id the project does not offer would
        // leave this person with maps drawing nothing, and no control that
        // could put it back -- the layer control only lists what is offered.
        stored.basemap = match sent.as_deref() {
            None | Some("") => None,
            Some(id) => {
                if !super::routes::offers_basemap(&state, id) {
                    return Err(ApiError::bad_request(
                        "unknown_basemap",
                        format!("This project does not offer a basemap called '{id}'."),
                    ));
                }
                Some(id.to_string())
            }
        };
    }

    preferences::write(project.documents(), user, &stored, &Expectation::Any)
        .map_err(|e| ApiError::internal("preferences_write_failed", e.to_string()))?;

    Ok(Json(serde_json::json!({
        "user": user.as_str(),
        "render_profile": stored.render_profile,
        "x_scale": stored.x_scale,
        "theme": stored.theme,
        "show_picks": stored.show_picks,
        "level2_spacing": stored.level2_spacing,
        "level2_format": stored.level2_format,
        "basemap": stored.basemap,
        "show_group_radargrams": stored.show_group_radargrams,
        "open_zoomed_out": stored.open_zoomed_out,
    })))
}

/// Where the caller's own preferences live.
///
/// Anonymous readers have nowhere to keep any, which is a 401 rather than a
/// silent no-op: the page would otherwise show a saved setting that was
/// never saved.
fn my_preferences_target<'a, 'b>(
    state: &'a AppState,
    caller: &'b Caller,
) -> Result<(&'a Project, &'b UserId), ApiError> {
    let project = project(state)?;
    let user = caller.user.as_ref().ok_or_else(|| {
        ApiError::unauthorized(
            "authentication_required",
            "Sign in to keep your own settings. Without an account they have \
             nowhere to live, so the project's defaults apply and `?profile=` \
             overrides them for one page.",
        )
    })?;
    Ok((project, user))
}
