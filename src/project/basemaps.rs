//! The tiled imagery the GUI's maps draw on: `[[basemaps]]` in `ridal.toml`
//! (#177).
//!
//! ```toml
//! [[basemaps]]
//! id = "osm"
//! name = "OpenStreetMap"
//! url = "https://tile.openstreetmap.org/{z}/{x}/{y}.png"
//! attribution = "© OpenStreetMap contributors"
//! attribution_url = "https://www.openstreetmap.org/copyright"
//! max_zoom = 19
//!
//! [map]
//! default_basemap = "osm"
//! ```
//!
//! # Why the config file rather than a document store
//!
//! A basemap is a setting, not authored data: it says nothing about this
//! project's radargrams, it is the kind of thing an administrator pastes in
//! from a tile provider's documentation, and a project served read-only
//! still wants one. That puts it beside `[render]` in the hand-editable
//! marker file rather than beside the layer vocabulary in the store.
//!
//! The settings page rewrites the whole `[[basemaps]]` block when it saves,
//! so comments *inside* it are not preserved. Everything else in the file
//! is -- see [`crate::project::Project::set_basemaps`].
//!
//! # Why the built-in stays unless it is switched off
//!
//! Every project today shows ESRI World Imagery, because that is all there
//! was. If defining a basemap of your own replaced it, adding OpenStreetMap
//! would silently take the imagery away -- a surprising answer to "also
//! offer me this one". So the built-in is offered alongside whatever the
//! project defines, and a project that must not reach ESRI says so
//! explicitly with `built_in_basemap = false`.
//!
//! # Why attribution is two plain fields
//!
//! Leaflet writes the attribution control's content with `innerHTML`, so an
//! attribution carrying markup would be a scripting primitive handed to
//! whoever may edit the project settings -- which is `operator`, a role
//! below `admin`. `attribution` is therefore plain text that the browser
//! escapes, and a link -- which OpenStreetMap's terms require -- is a
//! separate, scheme-checked `attribution_url`.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "basemaps are drawn and edited in the browser; a CLI-only \
                  build still needs the types to read a project's config"
    )
)]

use serde::{Deserialize, Serialize};

/// The id of the basemap Ridal ships with.
///
/// Reserved: a project entry may not reuse it, or "the built-in" would mean
/// two different things depending on which was found first.
pub const BUILT_IN_ID: &str = "esri-world-imagery";

/// Tile edge in pixels when a basemap does not say. What almost every XYZ
/// service serves; 512 is the common alternative.
pub const DEFAULT_TILE_SIZE: u32 = 256;

/// Deepest zoom when a basemap does not say. The built-in's own limit.
pub const DEFAULT_MAX_ZOOM: u8 = 18;

const MIN_TILE_SIZE: u32 = 32;
const MAX_TILE_SIZE: u32 = 2048;
const MAX_MAX_ZOOM: u8 = 24;
/// Both directions. Beyond a level or two this is a mistake rather than a
/// tiling scheme, and a large offset asks the provider for zooms it has no
/// tiles at.
const MAX_ZOOM_OFFSET: i8 = 4;

/// Placeholders Leaflet's `TileLayer` fills in.
///
/// Checked rather than assumed: `L.Util.template` *throws* on a placeholder
/// it has no value for, which takes out the whole map rather than showing
/// grey tiles. A URL copied from a provider that uses `{apikey}` would do
/// exactly that, and the error appears only in the browser console.
const KNOWN_PLACEHOLDERS: &[&str] = &["z", "x", "y", "-y", "s", "r"];

/// Ridal's built-in basemap: ESRI World Imagery, what every project saw
/// before this was configurable.
pub fn built_in() -> Basemap {
    Basemap {
        id: BUILT_IN_ID.to_string(),
        name: "ESRI World Imagery".to_string(),
        url: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/\
              MapServer/tile/{z}/{y}/{x}"
            .to_string(),
        attribution: Some("Esri".to_string()),
        attribution_url: None,
        tile_size: None,
        max_zoom: None,
        zoom_offset: None,
        subdomains: None,
        extra: serde_json::Map::new(),
    }
}

/// One tiled basemap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Basemap {
    /// Stable slug. What `[map] default_basemap` and a person's saved
    /// preference refer to, so renaming one is not free the way renaming a
    /// layer is -- a preference pointing at a gone id falls back rather
    /// than failing, but it is still a preference lost.
    pub id: String,
    /// Display name, shown in the layer control on each map. Free text.
    pub name: String,
    /// XYZ tile template, with `{z}`, `{x}` and `{y}` (or `{-y}`).
    pub url: String,
    /// Credit line, as plain text. Escaped by the browser before it reaches
    /// Leaflet's attribution control, which writes HTML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// Where the credit line links to, when the provider's terms ask for a
    /// link. `http`/`https` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_url: Option<String>,
    /// Tile edge in pixels. Unset means [`DEFAULT_TILE_SIZE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile_size: Option<u32>,
    /// Deepest zoom the provider serves. Unset means [`DEFAULT_MAX_ZOOM`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_zoom: Option<u8>,
    /// What to add to the map's zoom before asking for a tile. Unset means
    /// 0.
    ///
    /// Separate from `tile_size`, rather than derived from it, because the
    /// two answers a 512 px basemap can want are both real: a service
    /// serving double-resolution tiles of the *standard* XYZ scheme (the
    /// `@2x` endpoints) wants `-1`, while one whose own tiling is 512 px
    /// wants `0`. Guessing from the tile size would silently halve or
    /// double the scale of the one it guessed wrong about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom_offset: Option<i8>,
    /// Hosts `{s}` cycles through, as one letter each: `"abc"`. Required
    /// when the URL uses `{s}`, meaningless otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdomains: Option<String>,
    /// Anything a future version adds, preserved on rewrite.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The `[map]` table: which basemap applies, and whether the built-in is
/// offered at all.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapSection {
    /// The basemap someone who has not chosen one sees. Unset means the
    /// first offered, which is the built-in unless it has been switched
    /// off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_basemap: Option<String>,
    /// Whether to offer Ridal's built-in ESRI World Imagery. Unset means
    /// yes -- see the module note on why adding a basemap does not replace
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_in_basemap: Option<bool>,
}

impl MapSection {
    pub fn offers_built_in(&self) -> bool {
        self.built_in_basemap.unwrap_or(true)
    }
}

#[derive(Debug, PartialEq)]
pub enum BasemapError {
    InvalidId { id: String, reason: String },
    DuplicateId(String),
    ReservedId(String),
    InvalidUrl { id: String, reason: String },
    InvalidTileSize { id: String, tile_size: u32 },
    InvalidMaxZoom { id: String, max_zoom: u8 },
    InvalidZoomOffset { id: String, zoom_offset: i8 },
}

impl std::fmt::Display for BasemapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BasemapError::InvalidId { id, reason } => {
                write!(f, "the basemap id '{id}' is not usable: {reason}")
            }
            BasemapError::DuplicateId(id) => {
                write!(f, "the basemap id '{id}' is defined more than once")
            }
            BasemapError::ReservedId(id) => write!(
                f,
                "'{id}' is the id of Ridal's built-in basemap, so a project \
                 cannot define another with it. Switch the built-in off if you \
                 want to replace it."
            ),
            BasemapError::InvalidUrl { id, reason } => {
                write!(f, "the URL for basemap '{id}' is not usable: {reason}")
            }
            BasemapError::InvalidTileSize { id, tile_size } => write!(
                f,
                "basemap '{id}' has a tile size of {tile_size} px; it must be \
                 between {MIN_TILE_SIZE} and {MAX_TILE_SIZE}. Most services \
                 serve {DEFAULT_TILE_SIZE}."
            ),
            BasemapError::InvalidMaxZoom { id, max_zoom } => write!(
                f,
                "basemap '{id}' has a maximum zoom of {max_zoom}; it must be at \
                 most {MAX_MAX_ZOOM}."
            ),
            BasemapError::InvalidZoomOffset { id, zoom_offset } => write!(
                f,
                "basemap '{id}' has a zoom offset of {zoom_offset}; it must be \
                 between -{MAX_ZOOM_OFFSET} and {MAX_ZOOM_OFFSET}. Use -1 for a \
                 service serving 512 px tiles of the standard XYZ scheme, and 0 \
                 otherwise."
            ),
        }
    }
}

impl std::error::Error for BasemapError {}

impl Basemap {
    /// Tile edge in pixels, resolved.
    pub fn tile_size(&self) -> u32 {
        self.tile_size.unwrap_or(DEFAULT_TILE_SIZE)
    }

    /// Deepest zoom, resolved.
    pub fn max_zoom(&self) -> u8 {
        self.max_zoom.unwrap_or(DEFAULT_MAX_ZOOM)
    }

    /// Zoom offset, resolved.
    pub fn zoom_offset(&self) -> i8 {
        self.zoom_offset.unwrap_or(0)
    }

    /// Reject a basemap that cannot be drawn, or that would draw something
    /// other than tiles.
    ///
    /// Everything here is checked at both boundaries -- when the settings
    /// page saves one, and when the config is read -- because `ridal.toml`
    /// is meant to be hand-edited, and a hand-written `javascript:` URL
    /// would otherwise reach the browser exactly as a saved one would.
    pub fn validate(&self) -> Result<(), BasemapError> {
        if self.id.is_empty() {
            return Err(BasemapError::InvalidId {
                id: self.id.clone(),
                reason: "a basemap id must not be empty".to_string(),
            });
        }
        // The same charset as the identity slugs and the layer ids: this one
        // is stored in a person's preferences and compared as a string, so
        // case and punctuation would be a trap rather than a freedom.
        if let Some(bad) = self
            .id
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
        {
            return Err(BasemapError::InvalidId {
                id: self.id.clone(),
                reason: format!(
                    "'{bad}' is not allowed; use lowercase ASCII letters, digits, \
                     '-' and '_'. The display name is free text -- put the \
                     punctuation there instead."
                ),
            });
        }
        if self.name.trim().is_empty() {
            return Err(BasemapError::InvalidId {
                id: self.id.clone(),
                reason: "a basemap needs a name, since that is what the layer \
                         control on each map shows"
                    .to_string(),
            });
        }
        validate_tile_url(&self.id, &self.url, self.subdomains.as_deref())?;
        if let Some(url) = &self.attribution_url {
            validate_http_url(&self.id, url)?;
        }
        let tile_size = self.tile_size();
        if !(MIN_TILE_SIZE..=MAX_TILE_SIZE).contains(&tile_size) {
            return Err(BasemapError::InvalidTileSize {
                id: self.id.clone(),
                tile_size,
            });
        }
        if self.max_zoom() > MAX_MAX_ZOOM {
            return Err(BasemapError::InvalidMaxZoom {
                id: self.id.clone(),
                max_zoom: self.max_zoom(),
            });
        }
        if !(-MAX_ZOOM_OFFSET..=MAX_ZOOM_OFFSET).contains(&self.zoom_offset()) {
            return Err(BasemapError::InvalidZoomOffset {
                id: self.id.clone(),
                zoom_offset: self.zoom_offset(),
            });
        }
        Ok(())
    }

    /// This basemap as the browser needs it: every optional value resolved,
    /// so the page script has no defaults of its own to keep in step.
    ///
    /// Keyed exactly as the TOML is, rather than camel-cased on the way out:
    /// the settings page edits the *unresolved* form of the same records
    /// over the same API, and one spelling for both means nothing has to map
    /// between them.
    pub fn to_browser_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "url": self.url,
            "attribution": self.attribution,
            "attribution_url": self.attribution_url,
            "tile_size": self.tile_size(),
            "max_zoom": self.max_zoom(),
            "zoom_offset": self.zoom_offset(),
            "subdomains": self.subdomains,
        })
    }
}

/// Is this an `http`/`https` URL with nothing in it that would end the
/// attribute or the string it is written into?
fn validate_http_url(id: &str, url: &str) -> Result<(), BasemapError> {
    let invalid = |reason: &str| BasemapError::InvalidUrl {
        id: id.to_string(),
        reason: reason.to_string(),
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(invalid(
            "it must start with 'https://' or 'http://'. Only those two \
             schemes are fetched; anything else would be a way to run \
             something in the browser rather than to load a tile.",
        ));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("it must not contain spaces or control characters"));
    }
    Ok(())
}

/// Is this a tile template Leaflet can actually fill in?
fn validate_tile_url(id: &str, url: &str, subdomains: Option<&str>) -> Result<(), BasemapError> {
    validate_http_url(id, url)?;
    let invalid = |reason: String| BasemapError::InvalidUrl {
        id: id.to_string(),
        reason,
    };

    let placeholders = placeholders_in(url);
    if let Some(unknown) = placeholders
        .iter()
        .find(|name| !KNOWN_PLACEHOLDERS.contains(&name.as_str()))
    {
        return Err(invalid(format!(
            "'{{{unknown}}}' is not something Leaflet fills in, and a tile \
             template it cannot complete leaves the map blank rather than \
             grey. Substitute the value yourself -- an API key, for instance \
             -- and paste the finished URL."
        )));
    }
    for required in ["z", "x"] {
        if !placeholders.iter().any(|name| name == required) {
            return Err(invalid(format!(
                "it has no '{{{required}}}', so every tile would be the same \
                 image. An XYZ template looks like \
                 'https://example.org/tiles/{{z}}/{{x}}/{{y}}.png'."
            )));
        }
    }
    if !placeholders.iter().any(|name| name == "y" || name == "-y") {
        return Err(invalid(
            "it has no '{y}' (or '{-y}' for services numbered from the \
             south), so every tile would be the same image."
                .to_string(),
        ));
    }
    if placeholders.iter().any(|name| name == "s") && subdomains.map(str::is_empty).unwrap_or(true)
    {
        return Err(invalid(
            "it uses '{s}', so it needs a `subdomains` value naming the hosts \
             to cycle through, such as \"abc\"."
                .to_string(),
        ));
    }
    Ok(())
}

/// The `{...}` names in `template`, in order of appearance.
fn placeholders_in(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        rest = &rest[start + 1..];
        match rest.find('}') {
            Some(end) => {
                names.push(rest[..end].to_string());
                rest = &rest[end + 1..];
            }
            // An unclosed brace is not a placeholder; Leaflet leaves it be.
            None => break,
        }
    }
    names
}

/// Reject a set that cannot be used unambiguously.
///
/// Used where a bad set should be refused outright -- the settings page
/// saving one. Reading uses [`offered`] instead, which drops what it cannot
/// use rather than failing every page over one bad entry.
pub fn validate_set(entries: &[Basemap]) -> Result<(), BasemapError> {
    let mut seen: Vec<&str> = Vec::new();
    for entry in entries {
        entry.validate()?;
        if entry.id == BUILT_IN_ID {
            return Err(BasemapError::ReservedId(entry.id.clone()));
        }
        if seen.contains(&entry.id.as_str()) {
            return Err(BasemapError::DuplicateId(entry.id.clone()));
        }
        seen.push(&entry.id);
    }
    Ok(())
}

/// The basemaps to offer, built-in first.
///
/// Lenient by design: an entry that does not validate is dropped rather
/// than taken as a reason to serve no map at all. A hand-edited typo should
/// cost that one basemap, and [`problems`] is what makes it visible on the
/// settings page rather than only in the shape of a missing entry.
///
/// Never empty: with the built-in switched off and nothing usable defined,
/// the built-in comes back, because a page with no basemap at all is a
/// worse answer than one ignoring a setting.
pub fn offered(entries: &[Basemap], built_in_offered: bool) -> Vec<Basemap> {
    let mut offered: Vec<Basemap> = Vec::new();
    if built_in_offered {
        offered.push(built_in());
    }
    for entry in entries {
        if entry.validate().is_err() {
            continue;
        }
        if offered.iter().any(|kept| kept.id == entry.id) {
            continue;
        }
        offered.push(entry.clone());
    }
    if offered.is_empty() {
        offered.push(built_in());
    }
    offered
}

/// What [`offered`] silently dropped, as sentences for the settings page.
pub fn problems(entries: &[Basemap]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for entry in entries {
        if let Err(error) = entry.validate() {
            problems.push(error.to_string());
            continue;
        }
        if entry.id == BUILT_IN_ID {
            problems.push(BasemapError::ReservedId(entry.id.clone()).to_string());
            continue;
        }
        if seen.contains(&entry.id.as_str()) {
            problems.push(BasemapError::DuplicateId(entry.id.clone()).to_string());
            continue;
        }
        seen.push(&entry.id);
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basemap(id: &str, url: &str) -> Basemap {
        Basemap {
            id: id.to_string(),
            name: id.to_string(),
            url: url.to_string(),
            attribution: None,
            attribution_url: None,
            tile_size: None,
            max_zoom: None,
            zoom_offset: None,
            subdomains: None,
            extra: serde_json::Map::new(),
        }
    }

    const GOOD_URL: &str = "https://tile.example.org/{z}/{x}/{y}.png";

    #[test]
    fn the_built_in_validates() {
        // It is the one basemap nobody can fix by editing a file, so a
        // mistake in it would be a mistake in every project at once.
        built_in().validate().unwrap();
        assert_eq!(built_in().tile_size(), DEFAULT_TILE_SIZE);
        assert_eq!(built_in().max_zoom(), DEFAULT_MAX_ZOOM);
    }

    #[test]
    fn a_tile_url_must_be_http_and_must_vary_per_tile() {
        for (url, expected) in [
            ("javascript:alert(1)", "https://"),
            ("file:///etc/passwd", "https://"),
            ("https://tile.example.org/{z}/{x}/static.png", "{y}"),
            ("https://tile.example.org/tiles/{x}/{y}.png", "{z}"),
        ] {
            let error = basemap("m", url).validate().unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains(expected),
                "expected {expected:?} to be explained, got: {message}"
            );
        }
        basemap("m", "https://tile.example.org/{z}/{x}/{-y}.png")
            .validate()
            .unwrap();
    }

    #[test]
    fn a_placeholder_leaflet_cannot_fill_is_refused() {
        // L.Util.template throws on an unknown placeholder, which takes the
        // whole map out rather than showing grey tiles -- and only says so
        // in the browser console.
        let error = basemap("m", "https://tile.example.org/{z}/{x}/{y}?key={apikey}")
            .validate()
            .unwrap_err();
        assert!(error.to_string().contains("{apikey}"), "{error}");
    }

    #[test]
    fn subdomains_are_required_by_the_url_that_uses_them() {
        let mut map = basemap("m", "https://{s}.tile.example.org/{z}/{x}/{y}.png");
        assert!(map
            .validate()
            .unwrap_err()
            .to_string()
            .contains("subdomains"));
        map.subdomains = Some("abc".to_string());
        map.validate().unwrap();
    }

    #[test]
    fn tile_size_and_zoom_are_bounded() {
        let mut map = basemap("m", GOOD_URL);
        map.tile_size = Some(0);
        assert!(matches!(
            map.validate(),
            Err(BasemapError::InvalidTileSize { .. })
        ));
        map.tile_size = Some(512);
        map.max_zoom = Some(30);
        assert!(matches!(
            map.validate(),
            Err(BasemapError::InvalidMaxZoom { .. })
        ));
        map.max_zoom = Some(22);
        map.validate().unwrap();
        assert_eq!(map.tile_size(), 512);
    }

    #[test]
    fn an_attribution_url_is_scheme_checked_too() {
        // It becomes an href in the attribution control, where a
        // `javascript:` URL is one click from running.
        let mut map = basemap("m", GOOD_URL);
        map.attribution_url = Some("javascript:alert(1)".to_string());
        assert!(map.validate().is_err());
        map.attribution_url = Some("https://example.org/terms".to_string());
        map.validate().unwrap();
    }

    #[test]
    fn a_set_refuses_duplicates_and_the_built_in_id() {
        let a = basemap("osm", GOOD_URL);
        assert_eq!(
            validate_set(&[a.clone(), a.clone()]),
            Err(BasemapError::DuplicateId("osm".to_string()))
        );
        assert_eq!(
            validate_set(&[basemap(BUILT_IN_ID, GOOD_URL)]),
            Err(BasemapError::ReservedId(BUILT_IN_ID.to_string()))
        );
        validate_set(&[a, basemap("other", GOOD_URL)]).unwrap();
    }

    #[test]
    fn reading_drops_what_it_cannot_use_rather_than_failing() {
        let entries = vec![
            basemap("good", GOOD_URL),
            basemap("bad", "javascript:alert(1)"),
        ];
        let offered = offered(&entries, true);
        let ids: Vec<&str> = offered.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec![BUILT_IN_ID, "good"]);

        // And the fault is reportable rather than only visible as an absence.
        let problems = problems(&entries);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("'bad'"), "{problems:?}");
    }

    #[test]
    fn the_built_in_can_be_switched_off_but_never_leaves_a_project_mapless() {
        let entries = vec![basemap("osm", GOOD_URL)];
        let ids: Vec<String> = offered(&entries, false)
            .iter()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(ids, vec!["osm".to_string()]);

        // Switched off with nothing usable to replace it, it comes back: a
        // map with no tiles at all is a worse answer than one ignoring a
        // setting.
        assert_eq!(offered(&[], false), vec![built_in()]);
    }

    #[test]
    fn the_browser_sees_resolved_values() {
        let mut map = basemap("osm", GOOD_URL);
        map.attribution = Some("© OpenStreetMap contributors".to_string());
        let json = map.to_browser_json();
        assert_eq!(json["tile_size"], serde_json::json!(DEFAULT_TILE_SIZE));
        assert_eq!(json["max_zoom"], serde_json::json!(DEFAULT_MAX_ZOOM));
        assert_eq!(json["attribution"], "© OpenStreetMap contributors");
        assert_eq!(json["attribution_url"], serde_json::Value::Null);
    }

    #[test]
    fn an_unknown_key_survives_a_read_and_rewrite() {
        // The same rule the layer vocabulary follows: a key a future version
        // adds must not be destroyed by an older one saving the file.
        let text = r#"
            id = "osm"
            name = "OpenStreetMap"
            url = "https://tile.example.org/{z}/{x}/{y}.png"
            future_key = "kept"
        "#;
        let map: Basemap = toml::from_str(text).unwrap();
        assert_eq!(map.extra["future_key"], "kept");
        let round_tripped = serde_json::to_value(&map).unwrap();
        assert_eq!(round_tripped["future_key"], "kept");
    }
}
