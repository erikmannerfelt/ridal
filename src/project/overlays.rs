//! Vector overlays the GUI's maps can draw: `[[overlays]]` in `ridal.toml`
//! (#177).
//!
//! ```toml
//! [[overlays]]
//! id = "stakes"
//! name = "Mass balance stakes"
//! url = "https://static.example.org/shapes/mass_balance_stakes_2024.geojson"
//! name_field = "Stake"
//! description_field = "notes"
//! color = "#e63"
//! ```
//!
//! The generalisation of a thing that was written once by hand: PFA_website's
//! `overview_map.js` fetches one GeoJSON, binds a popup reading
//! `properties.Stake`, and registers it in the layer control as "Mass
//! balance stakes". Here the URL, the property holding each feature's name,
//! the property holding its description, and the name in the control are all
//! settings, so a project can add its own without touching any code.
//!
//! # Why off by default
//!
//! An overlay is context, not the subject: the maps exist to show where the
//! radargrams are. Every overlay therefore starts unchecked and is not even
//! fetched until someone asks for it -- a project with five of them costs a
//! page nothing until one is switched on.
//!
//! # Why the browser fetches it
//!
//! The alternative is proxying through Ridal, which would need an HTTP
//! client in the binary and would turn every overlay into a request the
//! *server* makes to an address a project member typed. The browser
//! fetching it directly keeps both out. The cost is CORS: a host that does
//! not send `Access-Control-Allow-Origin` cannot be read from a page, which
//! is a real limit and is reported as one rather than as a blank layer.
//!
//! # WGS84 only, for now
//!
//! RFC 7946 GeoJSON *is* WGS84, and Leaflet assumes it. A file carrying a
//! projected CRS is refused in the browser with a message naming the CRS and
//! how to convert it, rather than drawn somewhere in the Atlantic. See
//! `app.js`.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "overlays are drawn and edited in the browser; a CLI-only \
                  build still needs the types to read a project's config"
    )
)]

use serde::{Deserialize, Serialize};

/// Drawn colour when an overlay does not choose one. The track orange is
/// taken; this is a blue that stays legible on satellite imagery.
pub const DEFAULT_COLOR: &str = "#3aa3e3";

/// One vector overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    /// Stable slug. Not shown anywhere -- it exists so an overlay can be
    /// referred to without depending on its display name.
    pub id: String,
    /// What the layer control calls it: "Mass balance stakes".
    pub name: String,
    /// Where the GeoJSON is fetched from, by the browser.
    pub url: String,
    /// Property holding each feature's name, used as the popup's heading:
    /// `"Stake"` reads `properties.Stake`. Optional -- an overlay of
    /// unlabelled shapes is a legitimate thing to draw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_field: Option<String>,
    /// Property holding each feature's description, which is treated as
    /// HTML and becomes the body of the popup.
    ///
    /// HTML because that is what the setting is for -- a description with a
    /// link or a table in it. It is scrubbed in the browser before it is
    /// inserted (see `app.js`), which removes the obvious ways a hostile
    /// file could run something, but the real protection is that the URL is
    /// one an operator chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_field: Option<String>,
    /// Hex colour the features draw in. Unset means [`DEFAULT_COLOR`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Anything a future version adds, preserved on rewrite.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, PartialEq)]
pub enum OverlayError {
    InvalidId { id: String, reason: String },
    DuplicateId(String),
    InvalidUrl { id: String, reason: String },
    InvalidColor { id: String, color: String },
    InvalidField { id: String, field: String },
}

impl std::fmt::Display for OverlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverlayError::InvalidId { id, reason } => {
                write!(f, "the overlay id '{id}' is not usable: {reason}")
            }
            OverlayError::DuplicateId(id) => {
                write!(f, "the overlay id '{id}' is defined more than once")
            }
            OverlayError::InvalidUrl { id, reason } => {
                write!(f, "the URL for overlay '{id}' is not usable: {reason}")
            }
            OverlayError::InvalidColor { id, color } => write!(
                f,
                "overlay '{id}' has colour '{color}'; use a hex colour such as \
                 '#3aa3e3'. Anything else would be handed to the map as a \
                 style, which accepts more than colours."
            ),
            OverlayError::InvalidField { id, field } => write!(
                f,
                "overlay '{id}' names an empty {field}. Leave it out to have no \
                 {field} rather than naming a property with no name."
            ),
        }
    }
}

impl std::error::Error for OverlayError {}

impl Overlay {
    /// The colour to draw in, resolved.
    pub fn color(&self) -> &str {
        self.color.as_deref().unwrap_or(DEFAULT_COLOR)
    }

    /// Reject an overlay that cannot be drawn, or that would draw something
    /// other than a shape.
    ///
    /// Checked when the settings page saves one *and* when the config is
    /// read, because `ridal.toml` is meant to be hand-edited and a
    /// hand-written `javascript:` URL would otherwise reach the browser
    /// exactly as a saved one would.
    pub fn validate(&self) -> Result<(), OverlayError> {
        if self.id.is_empty() {
            return Err(OverlayError::InvalidId {
                id: self.id.clone(),
                reason: "an overlay id must not be empty".to_string(),
            });
        }
        // The same charset as the identity slugs, the layer ids and the
        // basemaps: an id is compared as a string and stored in files.
        if let Some(bad) = self
            .id
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
        {
            return Err(OverlayError::InvalidId {
                id: self.id.clone(),
                reason: format!(
                    "'{bad}' is not allowed; use lowercase ASCII letters, digits, \
                     '-' and '_'. The name in the layer control is free text -- \
                     put the punctuation there instead."
                ),
            });
        }
        if self.name.trim().is_empty() {
            return Err(OverlayError::InvalidId {
                id: self.id.clone(),
                reason: "an overlay needs a name, since that is what the layer \
                         control on each map shows"
                    .to_string(),
            });
        }
        validate_geojson_url(&self.id, &self.url)?;
        for (value, what) in [
            (&self.name_field, "name field"),
            (&self.description_field, "description field"),
        ] {
            if value.as_deref().map(str::trim) == Some("") {
                return Err(OverlayError::InvalidField {
                    id: self.id.clone(),
                    field: what.to_string(),
                });
            }
        }
        if let Some(color) = &self.color {
            if !is_hex_color(color) {
                return Err(OverlayError::InvalidColor {
                    id: self.id.clone(),
                    color: color.clone(),
                });
            }
        }
        Ok(())
    }

    /// This overlay as the browser needs it, with the colour resolved.
    ///
    /// Keyed as the TOML is, for the same reason the basemaps are: the
    /// settings page edits the same records over the same API, and one
    /// spelling means nothing has to map between them.
    pub fn to_browser_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "url": self.url,
            "name_field": self.name_field,
            "description_field": self.description_field,
            "color": self.color(),
        })
    }
}

/// Is this `#rgb`, `#rrggbb` or `#rrggbbaa`?
///
/// The same check the layer vocabulary makes, and for the same reason: the
/// value reaches the browser as a style, where anything else would be
/// interpreted far more generously than "a colour".
fn is_hex_color(value: &str) -> bool {
    let Some(digits) = value.strip_prefix('#') else {
        return false;
    };
    matches!(digits.len(), 3 | 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit())
}

/// Is this an address a browser will fetch a document from?
///
/// `http`/`https`, or a path on Ridal's own origin for a project that serves
/// its shapes from the same place. A `file:` or `javascript:` URL is refused
/// here rather than in the browser, where the failure would be silent.
fn validate_geojson_url(id: &str, url: &str) -> Result<(), OverlayError> {
    let invalid = |reason: &str| OverlayError::InvalidUrl {
        id: id.to_string(),
        reason: reason.to_string(),
    };
    if url.is_empty() {
        return Err(invalid("an overlay needs a URL to fetch its shapes from"));
    }
    let absolute = url.starts_with("http://") || url.starts_with("https://");
    // A single leading slash: a path on this server. `//host/path` is a
    // protocol-relative URL to somewhere else, which is not the same thing
    // and is not what someone typing a path means.
    let same_origin = url.starts_with('/') && !url.starts_with("//");
    if !absolute && !same_origin {
        return Err(invalid(
            "it must start with 'https://', 'http://', or '/' for a path on \
             this server. Only those are fetched; anything else would be a \
             way to run something in the browser rather than to load a file.",
        ));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("it must not contain spaces or control characters"));
    }
    Ok(())
}

/// Reject a set that cannot be used unambiguously.
///
/// Used where a bad set should be refused outright -- the settings page
/// saving one. Reading uses [`usable`] instead, which drops what it cannot
/// use rather than failing every page over one bad entry.
pub fn validate_set(entries: &[Overlay]) -> Result<(), OverlayError> {
    let mut seen: Vec<&str> = Vec::new();
    for entry in entries {
        entry.validate()?;
        if seen.contains(&entry.id.as_str()) {
            return Err(OverlayError::DuplicateId(entry.id.clone()));
        }
        seen.push(&entry.id);
    }
    Ok(())
}

/// The overlays to offer, in the order the file lists them.
///
/// Lenient by design, exactly as the basemaps are: an entry that does not
/// validate is dropped rather than taken as a reason to draw no map. The
/// difference from a basemap is that an absent overlay costs nothing --
/// there is no fallback to reach for, because nothing was being covered up.
pub fn usable(entries: &[Overlay]) -> Vec<Overlay> {
    let mut usable: Vec<Overlay> = Vec::new();
    for entry in entries {
        if entry.validate().is_err() {
            continue;
        }
        if usable.iter().any(|kept| kept.id == entry.id) {
            continue;
        }
        usable.push(entry.clone());
    }
    usable
}

/// What [`usable`] silently dropped, as sentences for the settings page.
pub fn problems(entries: &[Overlay]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for entry in entries {
        if let Err(error) = entry.validate() {
            problems.push(error.to_string());
            continue;
        }
        if seen.contains(&entry.id.as_str()) {
            problems.push(OverlayError::DuplicateId(entry.id.clone()).to_string());
            continue;
        }
        seen.push(&entry.id);
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay(id: &str, url: &str) -> Overlay {
        Overlay {
            id: id.to_string(),
            name: format!("Overlay {id}"),
            url: url.to_string(),
            name_field: None,
            description_field: None,
            color: None,
            extra: serde_json::Map::new(),
        }
    }

    const GOOD_URL: &str = "https://static.example.org/shapes/stakes.geojson";

    #[test]
    fn a_url_must_be_fetchable_by_a_browser() {
        for (url, expected) in [
            ("javascript:alert(1)", "https://"),
            ("file:///etc/passwd", "https://"),
            ("", "needs a URL"),
        ] {
            let error = overlay("m", url).validate().unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} to be explained, got: {error}"
            );
        }
        overlay("m", GOOD_URL).validate().unwrap();
        // A path on this server, for a project serving its own shapes.
        overlay("m", "/static/shapes/stakes.geojson")
            .validate()
            .unwrap();
        // But not a protocol-relative URL, which points somewhere else
        // entirely and is not what a typed path means.
        assert!(overlay("m", "//elsewhere.example/x.geojson")
            .validate()
            .is_err());
    }

    #[test]
    fn a_colour_must_be_a_colour() {
        // It reaches the browser as a style, where `url(...)` would make
        // every reader fetch whatever the author chose.
        let mut map = overlay("m", GOOD_URL);
        map.color = Some("url(http://tracker.example/x.png)".to_string());
        assert!(matches!(
            map.validate(),
            Err(OverlayError::InvalidColor { .. })
        ));
        map.color = Some("#3aa3e3".to_string());
        map.validate().unwrap();
        assert_eq!(map.color(), "#3aa3e3");
        map.color = None;
        assert_eq!(map.color(), DEFAULT_COLOR);
    }

    #[test]
    fn an_empty_field_name_is_refused_rather_than_stored() {
        // Naming a property with no name would read every feature's `""`,
        // find nothing, and look like a popup that does not work.
        let mut map = overlay("m", GOOD_URL);
        map.name_field = Some("  ".to_string());
        assert!(matches!(
            map.validate(),
            Err(OverlayError::InvalidField { .. })
        ));
        map.name_field = Some("Stake".to_string());
        map.validate().unwrap();
        // Absent is fine: an overlay of unlabelled shapes is a real thing.
        map.name_field = None;
        map.validate().unwrap();
    }

    #[test]
    fn a_set_refuses_duplicates_and_reading_drops_what_it_cannot_use() {
        let good = overlay("stakes", GOOD_URL);
        assert_eq!(
            validate_set(&[good.clone(), good.clone()]),
            Err(OverlayError::DuplicateId("stakes".to_string()))
        );

        let entries = vec![good.clone(), overlay("bad", "javascript:alert(1)")];
        let ids: Vec<String> = usable(&entries).iter().map(|o| o.id.clone()).collect();
        assert_eq!(ids, vec!["stakes".to_string()]);

        let problems = problems(&entries);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("'bad'"), "{problems:?}");
    }

    #[test]
    fn the_browser_sees_the_four_dials_and_a_resolved_colour() {
        // The four settings #177 asks for, as the browser reads them: where
        // to fetch, what names a feature, what describes it, and what the
        // layer control calls the whole thing.
        let mut map = overlay("stakes", GOOD_URL);
        map.name = "Mass balance stakes".to_string();
        map.name_field = Some("Stake".to_string());
        map.description_field = Some("notes".to_string());
        let json = map.to_browser_json();
        assert_eq!(json["url"], GOOD_URL);
        assert_eq!(json["name_field"], "Stake");
        assert_eq!(json["description_field"], "notes");
        assert_eq!(json["name"], "Mass balance stakes");
        assert_eq!(json["color"], DEFAULT_COLOR);
    }

    #[test]
    fn an_unknown_key_survives_a_read_and_rewrite() {
        let text = r#"
            id = "stakes"
            name = "Mass balance stakes"
            url = "https://static.example.org/shapes/stakes.geojson"
            future_key = "kept"
        "#;
        let overlay: Overlay = toml::from_str(text).unwrap();
        assert_eq!(overlay.extra["future_key"], "kept");
        assert_eq!(
            serde_json::to_value(&overlay).unwrap()["future_key"],
            "kept"
        );
    }
}
