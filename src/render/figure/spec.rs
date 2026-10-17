//! The figure spec (#142): everything that decides what a figure looks
//! like, as one JSON document.
//!
//! A spec is what makes a figure repeatable. The GUI builds one, the HTTP
//! API and `ridal render --figure` take one, and the same spec rendered
//! from the same revision gives the same figure. Every field has a
//! default, so `{}` is a valid spec: the whole radargram, 16 cm wide, with
//! distance and travel-time axes where the file has them.
//!
//! Unknown fields are refused rather than ignored, so a misspelt option is
//! an error instead of a figure that silently lacks it.

use serde::{Deserialize, Serialize};

/// The version of the spec format this build reads and writes.
pub const SPEC_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields, default)]
pub struct FigureSpec {
    /// Spec format version. Only `1` exists.
    pub version: u32,
    /// The standard or the topographically corrected view.
    pub view: View,
    /// When set, the revision the spec was made for. Rendering it from a
    /// different revision is refused: a reprocessed file can have a
    /// different number of traces, and the same trace numbers would then
    /// show a different stretch of the profile.
    pub revision_id: Option<String>,
    /// A render profile name (built-in, or a file path on the command
    /// line).
    pub profile: String,
    /// Horizontal stretch, as the viewer's horizontal scale: the plot is
    /// `traces * x_scale` units wide for every `rows` units tall.
    pub x_scale: f64,
    pub extent: Extent,
    pub size: Size,
    /// `false` writes the radargram alone, with no axes or margins, plus a
    /// JSON sidecar describing its coordinates (for plotting elsewhere).
    pub axes: bool,
    pub x_axis: AxisSpec<XUnit>,
    pub y_axis: AxisSpec<YUnit>,
    pub style: Style,
    /// `false` leaves the radargram out, so only the picks and derived
    /// layers are drawn, over the background.
    pub radargram: bool,
    /// Picked lines to draw over the radargram.
    pub picks: PickOverlay,
    /// Derived layers to draw over the radargram.
    pub derived: DerivedOverlay,
}

impl Default for FigureSpec {
    fn default() -> Self {
        Self {
            version: SPEC_VERSION,
            view: View::Standard,
            revision_id: None,
            profile: "default".to_string(),
            x_scale: 1.0,
            extent: Extent::default(),
            size: Size::default(),
            axes: true,
            x_axis: AxisSpec::default(),
            y_axis: AxisSpec::default(),
            style: Style::default(),
            radargram: true,
            picks: PickOverlay::default(),
            derived: DerivedOverlay::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureView))]
#[serde(rename_all = "lowercase")]
pub enum View {
    #[default]
    Standard,
    Topo,
}

/// The part of the radargram a figure shows, in raster coordinates.
///
/// Positions are fractional trace and row indices, the same coordinates
/// the viewer uses: trace `t` spans `[t, t + 1)` and row `r` spans
/// `[r, r + 1)`, so `traces: [0, n]` is every trace. In the standard view a
/// row is a sample. In the topographic view it is a row of the corrected
/// raster, which moves when the radargram's elevation window is edited, so
/// a spec meant to outlive that can give `elevation` instead.
///
/// Omitted bounds mean the whole radargram. Bounds past the data are
/// clipped to it, except the rows of the topographic view, which may reach
/// above the highest surface or below the floor to leave empty space there.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureExtent))]
#[serde(deny_unknown_fields, default)]
pub struct Extent {
    /// `[first, last]` trace position.
    pub traces: Option<[f64; 2]>,
    /// `[top, bottom]` row position.
    pub rows: Option<[f64; 2]>,
    /// `[top, bottom]` elevation in metres; topographic view only, and not
    /// together with `rows`.
    pub elevation: Option<[f64; 2]>,
}

/// How big the figure is.
///
/// `width` is the whole figure's, margins included; the height follows
/// from the extent and `x_scale`. With `axes: false` there are no margins
/// and `width` is the radargram's own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureSize))]
#[serde(deny_unknown_fields, default)]
pub struct Size {
    pub width: f64,
    pub unit: SizeUnit,
    /// Pixels per inch. Converts `cm` and `in` to pixels, and point sizes
    /// (fonts, lines) to pixels in every unit.
    pub dpi: f64,
}

impl Default for Size {
    fn default() -> Self {
        Self {
            width: 16.0,
            unit: SizeUnit::Cm,
            dpi: 300.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureSizeUnit))]
#[serde(rename_all = "lowercase")]
pub enum SizeUnit {
    #[default]
    Cm,
    In,
    Px,
}

impl Size {
    /// The width in pixels.
    pub fn width_px(&self) -> f64 {
        match self.unit {
            SizeUnit::Px => self.width,
            SizeUnit::In => self.width * self.dpi,
            SizeUnit::Cm => self.width / 2.54 * self.dpi,
        }
    }

    /// Pixels per typographic point (1/72 inch).
    pub fn px_per_pt(&self) -> f64 {
        self.dpi / 72.0
    }
}

/// One axis's ticks and label.
// Each field gives its schema default itself: the derive would otherwise
// call `AxisSpec::default()`, which it cannot do without the generic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields, default)]
pub struct AxisSpec<U> {
    /// What the axis counts in. `None` picks the default for the view and
    /// what the file has.
    #[cfg_attr(feature = "server", schema(default = json!(null)))]
    pub unit: Option<U>,
    /// Distance between ticks in the axis unit (seconds for time). `None`
    /// picks a round spacing that gives about six ticks.
    #[cfg_attr(feature = "server", schema(default = json!(null)))]
    pub spacing: Option<f64>,
    /// Ticks are at `anchor + k * spacing`. The default, 0, puts them on
    /// multiples of the spacing.
    #[cfg_attr(feature = "server", schema(default = 0.0))]
    pub anchor: f64,
    /// The axis title. `None` uses the unit's own; `""` draws none.
    #[cfg_attr(feature = "server", schema(default = json!(null)))]
    pub label: Option<String>,
}

impl<U> Default for AxisSpec<U> {
    fn default() -> Self {
        Self {
            unit: None,
            spacing: None,
            anchor: 0.0,
            label: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureXUnit))]
#[serde(rename_all = "snake_case")]
pub enum XUnit {
    /// Trace number.
    Trace,
    /// Distance along the profile in metres, from the file's `distance`.
    Distance,
    /// The same distance in kilometres, for a long profile; `spacing` and
    /// `anchor` are in kilometres too.
    DistanceKm,
    /// UTC time of day, from the file's `time`.
    Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureYUnit))]
#[serde(rename_all = "lowercase")]
pub enum YUnit {
    /// Sample number (standard view only).
    Sample,
    /// Two-way travel time, from the file's `twtt` (standard view only).
    Twtt,
    /// Depth, from the file's `depth` (standard view only).
    Depth,
    /// Elevation (topographic view only, where it is the only unit).
    Elevation,
}

impl XUnit {
    pub fn default_label(self) -> &'static str {
        match self {
            XUnit::Trace => "Trace",
            XUnit::Distance => "Distance (m)",
            XUnit::DistanceKm => "Distance (km)",
            XUnit::Time => "Time (UTC)",
        }
    }
}

impl YUnit {
    pub fn default_label(self) -> &'static str {
        match self {
            YUnit::Sample => "Sample",
            YUnit::Twtt => "Two-way travel time (ns)",
            YUnit::Depth => "Depth (m)",
            YUnit::Elevation => "Elevation (m)",
        }
    }
}

/// Which picked lines to draw, and how.
///
/// Every pick whose user and layer match one of `items` is drawn, in its
/// layer's colour. A selector that leaves out `user` matches everyone whose
/// picks the caller may see, and one that leaves out `layer` matches every
/// layer, so `[{}]` is all of them; `"layer": ""` matches unlabelled lines.
/// No items, the default, draws none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigurePicks))]
#[serde(deny_unknown_fields, default)]
pub struct PickOverlay {
    pub items: Vec<PickSelector>,
    /// Line width in points.
    pub width: f64,
    pub opacity: f64,
}

impl Default for PickOverlay {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            width: 1.0,
            opacity: 0.85,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigurePickSelector))]
#[serde(deny_unknown_fields, default)]
pub struct PickSelector {
    pub user: Option<String>,
    pub layer: Option<String>,
}

impl PickSelector {
    /// Whether a pick by `user` on `layer` (`None` when unlabelled) matches.
    /// A selector's `layer` of `""` asks for unlabelled lines, which a
    /// project without a layer vocabulary has nothing but.
    pub fn matches(&self, user: &str, layer: Option<&str>) -> bool {
        self.user.as_deref().is_none_or(|u| u == user)
            && match self.layer.as_deref() {
                None => true,
                Some("") => layer.is_none(),
                Some(l) => Some(l) == layer,
            }
    }
}

/// Which derived layers to draw, and how.
///
/// `items` are derived item ids. Each is drawn in its own colour, and an
/// item whose range fill reaches another drawn item has the fill drawn
/// under them, as in the viewer. Only items of kind `layer` are positions
/// that can be drawn; an attribute is refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureDerived))]
#[serde(deny_unknown_fields, default)]
pub struct DerivedOverlay {
    pub items: Vec<String>,
    /// Line width in points.
    pub width: f64,
    pub opacity: f64,
    /// Whether to draw range fills between drawn items.
    pub fills: bool,
}

impl Default for DerivedOverlay {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            width: 1.5,
            opacity: 0.9,
            fills: true,
        }
    }
}

/// Text, line and background styling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureAppearance))]
#[serde(deny_unknown_fields, default)]
pub struct Style {
    /// Tick label size in points. Axis titles are drawn at the same size.
    pub font_size: f64,
    /// Colour of text, ticks and the frame, as `#rrggbb`.
    pub color: String,
    /// Background colour as `#rrggbb`, or `null` for transparent.
    pub background: Option<String>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font_size: 10.0,
            color: "#000000".to_string(),
            background: Some("#ffffff".to_string()),
        }
    }
}

/// Check `colour` is `#rgb` or `#rrggbb` and return it as `#rrggbb`.
///
/// Only hex: it is the one form every SVG renderer, Office included, reads
/// the same way, and accepting anything else would put arbitrary text into
/// an SVG attribute.
pub fn parse_colour(field: &str, colour: &str) -> Result<String, String> {
    let hex = colour.strip_prefix('#').unwrap_or("");
    let valid = hex.chars().all(|c| c.is_ascii_hexdigit());
    match hex.len() {
        6 if valid => Ok(format!("#{}", hex.to_ascii_lowercase())),
        3 if valid => Ok(format!(
            "#{}",
            hex.chars()
                .flat_map(|c| [c, c])
                .collect::<String>()
                .to_ascii_lowercase()
        )),
        _ => Err(format!(
            "{field} must be a hex colour such as #000000, not '{colour}'."
        )),
    }
}

impl FigureSpec {
    /// Parse a spec from JSON, with a message that names the problem.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let spec: FigureSpec =
            serde_json::from_str(text).map_err(|e| format!("Invalid figure spec: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    /// Whether the spec asks for any picks or derived layers, which need a
    /// project to come from.
    pub fn has_overlays(&self) -> bool {
        !self.picks.items.is_empty() || !self.derived.items.is_empty()
    }

    /// Checks that need no file: the ones serde cannot express.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != SPEC_VERSION {
            return Err(format!(
                "Figure spec version {} is not supported; this Ridal reads version {SPEC_VERSION}.",
                self.version
            ));
        }
        if !(self.x_scale.is_finite() && self.x_scale > 0.0) {
            return Err(format!("x_scale must be positive, not {}.", self.x_scale));
        }
        if !(self.size.width.is_finite() && self.size.width > 0.0) {
            return Err(format!(
                "size.width must be positive, not {}.",
                self.size.width
            ));
        }
        if !(self.size.dpi.is_finite() && self.size.dpi >= 10.0 && self.size.dpi <= 2400.0) {
            return Err(format!(
                "size.dpi must be between 10 and 2400, not {}.",
                self.size.dpi
            ));
        }
        if !(self.style.font_size.is_finite()
            && self.style.font_size > 0.0
            && self.style.font_size <= 200.0)
        {
            return Err(format!(
                "style.font_size must be between 0 and 200 points, not {}.",
                self.style.font_size
            ));
        }
        parse_colour("style.color", &self.style.color)?;
        if let Some(background) = &self.style.background {
            parse_colour("style.background", background)?;
        }
        for (name, bounds) in [
            ("extent.traces", self.extent.traces),
            ("extent.rows", self.extent.rows),
        ] {
            if let Some([a, b]) = bounds {
                if !(a.is_finite() && b.is_finite() && a < b) {
                    return Err(format!(
                        "{name} must be two finite numbers, the first smaller, not [{a}, {b}]."
                    ));
                }
            }
        }
        if let Some([top, bottom]) = self.extent.elevation {
            if !(top.is_finite() && bottom.is_finite() && top > bottom) {
                return Err(format!(
                    "extent.elevation must be [top, bottom] with top above bottom, not \
                     [{top}, {bottom}]."
                ));
            }
            if self.extent.rows.is_some() {
                return Err("Give extent.rows or extent.elevation, not both.".to_string());
            }
            if self.view != View::Topo {
                return Err(
                    "extent.elevation is only for the topographic view; use extent.rows."
                        .to_string(),
                );
            }
        }
        for (name, spacing, anchor) in [
            ("x_axis", self.x_axis.spacing, self.x_axis.anchor),
            ("y_axis", self.y_axis.spacing, self.y_axis.anchor),
        ] {
            if let Some(spacing) = spacing {
                if !(spacing.is_finite() && spacing > 0.0) {
                    return Err(format!("{name}.spacing must be positive, not {spacing}."));
                }
            }
            if !anchor.is_finite() {
                return Err(format!("{name}.anchor must be finite."));
            }
        }
        for (name, width, opacity) in [
            ("picks", self.picks.width, self.picks.opacity),
            ("derived", self.derived.width, self.derived.opacity),
        ] {
            if !(width.is_finite() && width > 0.0 && width <= 20.0) {
                return Err(format!(
                    "{name}.width must be between 0 and 20 points, not {width}."
                ));
            }
            if !(0.0..=1.0).contains(&opacity) {
                return Err(format!(
                    "{name}.opacity must be between 0 and 1, not {opacity}."
                ));
            }
        }
        if !self.radargram && !self.axes && !self.has_overlays() {
            return Err(
                "A figure without the radargram, axes or any picks or derived layers would be \
                 empty."
                    .to_string(),
            );
        }
        match (self.view, self.y_axis.unit) {
            (View::Topo, Some(unit)) if unit != YUnit::Elevation => Err(format!(
                "The topographic view's y axis is elevation; '{}' is for the standard view.",
                serde_json::to_value(unit)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            )),
            (View::Standard, Some(YUnit::Elevation)) => Err(
                "An elevation axis needs the topographic view (\"view\": \"topo\").".to_string(),
            ),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_spec_is_the_default() {
        assert_eq!(FigureSpec::from_json("{}").unwrap(), FigureSpec::default());
    }

    #[test]
    fn a_spec_round_trips_through_json() {
        let spec = FigureSpec {
            view: View::Topo,
            revision_id: Some("abc".into()),
            extent: Extent {
                traces: Some([10.0, 200.5]),
                elevation: Some([800.0, 500.0]),
                rows: None,
            },
            x_axis: AxisSpec {
                unit: Some(XUnit::Time),
                spacing: Some(60.0),
                anchor: 0.0,
                label: Some(String::new()),
            },
            style: Style {
                background: None,
                ..Style::default()
            },
            ..FigureSpec::default()
        };
        let text = serde_json::to_string(&spec).unwrap();
        assert_eq!(FigureSpec::from_json(&text).unwrap(), spec);
    }

    #[test]
    fn a_misspelt_field_is_refused() {
        let err = FigureSpec::from_json(r#"{"x_axis": {"spaceing": 10}}"#).unwrap_err();
        assert!(err.contains("spaceing"), "{err}");
    }

    #[test]
    fn units_must_suit_the_view() {
        assert!(FigureSpec::from_json(r#"{"view": "topo", "y_axis": {"unit": "twtt"}}"#).is_err());
        assert!(FigureSpec::from_json(r#"{"y_axis": {"unit": "elevation"}}"#).is_err());
        assert!(
            FigureSpec::from_json(r#"{"view": "topo", "y_axis": {"unit": "elevation"}}"#).is_ok()
        );
        assert!(FigureSpec::from_json(r#"{"extent": {"elevation": [10, 0]}}"#).is_err());
    }

    #[test]
    fn bad_numbers_and_colours_are_refused() {
        for bad in [
            r#"{"x_scale": 0}"#,
            r#"{"size": {"width": -1}}"#,
            r#"{"extent": {"traces": [5, 5]}}"#,
            r#"{"x_axis": {"spacing": 0}}"#,
            r#"{"style": {"color": "red"}}"#,
            r##"{"style": {"background": "#12345"}}"##,
            r#"{"version": 2}"#,
        ] {
            assert!(FigureSpec::from_json(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn colours_are_normalised() {
        assert_eq!(parse_colour("c", "#ABC").unwrap(), "#aabbcc");
        assert_eq!(parse_colour("c", "#00FF7f").unwrap(), "#00ff7f");
    }

    #[test]
    fn sizes_convert_to_pixels() {
        let size = Size {
            width: 2.54,
            unit: SizeUnit::Cm,
            dpi: 300.0,
        };
        assert!((size.width_px() - 300.0).abs() < 1e-9);
        assert!((size.px_per_pt() - 300.0 / 72.0).abs() < 1e-12);
    }
}
