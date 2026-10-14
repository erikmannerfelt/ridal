//! Annotated figures (#142): an excerpt of a radargram with axes, ticks
//! and labels, as SVG or PNG.
//!
//! # One spec, one drawing
//!
//! A [`spec::FigureSpec`] says everything about a figure: the view, the
//! extent, the size, the axes and the styling. [`render_figure`] turns a
//! spec and a radargram into a figure, and every caller goes through it:
//! `ridal render --figure`, `ridal.render(figure=...)`, and (in later
//! changes) the HTTP API and the GUI's export dialog. A spec saved from one
//! renders the same figure from any other.
//!
//! The figure is composed as an SVG: the radargram as one embedded PNG,
//! with the frame, ticks and labels as vector lines and text. A PNG figure
//! is the same SVG drawn by resvg with the bundled font ([`font`]), except
//! that the radargram raster is composited directly rather than encoded
//! into the SVG and decoded again: it is already pixels, at the plot's
//! exact pixel size, so nothing would be gained and resvg would need its
//! image decoders. The background goes under it and the SVG's lines and
//! text over it, the same stacking the SVG has.
//!
//! # What is drawn
//!
//! The raster is the extent, rendered at the plot's pixel size by
//! [`crate::render::renderer::Renderer::render_window_rgba`]: colour first,
//! then area-averaged, the way the overview is. Contrast comes from the
//! whole radargram ([`crate::render::oneshot::resolve_limits`]), so an
//! excerpt looks as it does in the viewer rather than stretched to its own
//! range. Places with no data (a topographic wedge) are transparent.
//!
//! The plot's aspect follows the viewer: `traces * x_scale` wide for every
//! `rows` tall, so a figure keeps the horizontal scale it was made at.
//! Ticks sit at their coordinate's true position on the raster, not at an
//! even spacing on the page; see [`axis`].

pub mod axis;
pub mod font;
pub mod overlays;
pub mod spec;

use std::path::Path;
use std::sync::{Arc, OnceLock};

use base64::Engine;

use crate::render::grid::SourceWindow;
use crate::render::profile::RenderProfile;
use crate::render::renderer::Renderer;
use crate::render::topo;
use crate::source::{AmplitudeSource, SourceReader};

use axis::{AxisValues, LabelFormat, Tick};
use spec::{FigureSpec, SizeUnit, View, XUnit, YUnit};

/// Most pixels a figure may have. The raster is accumulated in `f32` per
/// pixel and channel before it is encoded, about 16 bytes a pixel, so this
/// bounds a single render at well under a gigabyte. 40 MP is a 53 x 35 cm
/// figure at 300 dpi.
pub const MAX_FIGURE_PIXELS: usize = 40_000_000;

/// Most pixels along either side, which PNG viewers and tiny-skia both
/// handle comfortably.
pub const MAX_FIGURE_SIDE: usize = 32_768;

/// About how many ticks an axis gets when the spec leaves spacing to Ridal.
const AUTO_TICKS: usize = 6;

/// Why a figure could not be made, split by who can fix it.
#[derive(Debug, Clone, PartialEq)]
pub enum FigureError {
    /// The spec is wrong, or asks for something the file cannot give
    /// (a distance axis on a file with no distances).
    Spec(String),
    /// The spec names a different revision than the file is.
    RevisionMismatch { expected: String, actual: String },
    /// Reading or drawing failed.
    Render(String),
}

impl std::fmt::Display for FigureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FigureError::Spec(message) | FigureError::Render(message) => f.write_str(message),
            FigureError::RevisionMismatch { expected, actual } => write!(
                f,
                "The figure spec was made for revision {expected}, but this file is revision \
                 {actual}. Its extent is in trace and row numbers, which can point at a \
                 different part of a reprocessed radargram; check the extent and remove \
                 revision_id from the spec to render it anyway."
            ),
        }
    }
}

impl From<FigureError> for String {
    fn from(e: FigureError) -> Self {
        e.to_string()
    }
}

/// The output encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FigureFormat {
    Svg,
    Png,
    /// Only the [`Sidecar`] of a figure without axes, as JSON, with nothing
    /// read or drawn: how the HTTP API hands it out beside the PNG.
    Sidecar,
}

impl FigureFormat {
    /// The media type of the encoded figure.
    pub fn content_type(self) -> &'static str {
        match self {
            FigureFormat::Svg => "image/svg+xml",
            FigureFormat::Png => "image/png",
            FigureFormat::Sidecar => "application/json",
        }
    }

    /// The file extension for it.
    pub fn extension(self) -> &'static str {
        match self {
            FigureFormat::Svg => "svg",
            FigureFormat::Png => "png",
            FigureFormat::Sidecar => "json",
        }
    }
}

impl FigureFormat {
    /// The format an output path asks for, by extension.
    pub fn for_path(path: &Path) -> Result<Self, String> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("svg") => Ok(FigureFormat::Svg),
            Some("png") => Ok(FigureFormat::Png),
            _ => Err(format!(
                "A figure is written as .svg or .png; cannot tell which from {}.",
                path.display()
            )),
        }
    }
}

/// The coordinate variables of a processed radargram, as far as it has
/// them. Each is `None` when absent or of the wrong length.
#[derive(Debug, Clone, Default)]
pub struct Coordinates {
    /// Per trace, metres along the profile.
    pub distance: Option<Vec<f64>>,
    /// Per trace, seconds since 1970-01-01 UTC.
    pub time: Option<Vec<f64>>,
    /// Per trace, metres; what the topographic view is built from.
    pub elevation: Option<Vec<f64>>,
    /// Per sample, nanoseconds.
    pub twtt: Option<Vec<f64>>,
    /// Per sample, metres.
    pub depth: Option<Vec<f64>>,
}

impl Coordinates {
    /// Read whichever coordinate variables `reader`'s file has. A variable
    /// of the wrong length for the `data` array is treated as absent.
    pub fn read(reader: &SourceReader) -> Self {
        let (n_samples, n_traces) = reader.shape();
        let read = |name: &str, len: usize| {
            reader
                .read_axis_f64(name)
                .ok()
                .filter(|values| values.len() == len)
        };
        Self {
            distance: read("distance", n_traces),
            time: read("time", n_traces),
            elevation: read("elevation", n_traces),
            twtt: read("twtt", n_samples),
            depth: read("depth", n_samples),
        }
    }
}

/// A radargram to make a figure of.
pub struct FigureSource<'a, S: AmplitudeSource> {
    /// The standard view's amplitudes.
    pub source: &'a S,
    pub coordinates: &'a Coordinates,
    /// The file's revision, checked against the spec's when both are known.
    pub revision_id: Option<String>,
    /// The radargram's configured elevation window, for the topographic
    /// view. [`topo::ElevationRange::NONE`] outside a project.
    pub elevation_range: topo::ElevationRange,
    /// The picks and derived layers the spec asks for, gathered by
    /// [`overlays::gather`]. `None` when there is no project to read them
    /// from, which is an error only if the spec asks for any.
    pub overlays: Option<&'a overlays::Overlays>,
    /// The profile pinned for this radargram and its amplitude limits, when
    /// the caller has them already: the server's render service keeps them
    /// per revision and profile. `None` estimates them here, the same way.
    pub contrast: Option<(RenderProfile, (f32, f32))>,
}

/// A rendered figure.
#[derive(Debug, Clone)]
pub struct RenderedFigure {
    pub bytes: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// For a figure without axes, where its pixels are in coordinates:
    /// see [`Sidecar`].
    pub sidecar: Option<Sidecar>,
}

/// What a figure without axes needs beside it to be plotted elsewhere:
/// the extent it shows, and each axis's value at every pixel's centre.
///
/// In matplotlib, `imshow(img, extent=[*x.extent, *y.extent[::-1]])` for
/// an axis that is linear, or `pcolormesh` with the centres for one that is
/// not (distance on a profile driven at varying speed).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureSidecar))]
pub struct Sidecar {
    pub view: View,
    pub width: usize,
    pub height: usize,
    /// `[first, last]` trace position of the left and right edges.
    pub traces: [f64; 2],
    /// `[top, bottom]` row position of the top and bottom edges.
    pub rows: [f64; 2],
    pub x: SidecarAxis,
    pub y: SidecarAxis,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "server", schema(as = FigureSidecarAxis))]
pub struct SidecarAxis {
    pub unit: String,
    pub label: String,
    /// The value at the two edges: left and right, or top and bottom.
    pub extent: [f64; 2],
    /// The value at the centre of each pixel column or row.
    pub centres: Vec<f64>,
}

/// Render `spec` of `input` as `format`, with `profile` already resolved
/// from `spec.profile` by the caller (who decides whether a profile may be
/// a file path).
pub fn render_figure<S: AmplitudeSource>(
    input: &FigureSource<S>,
    spec: &FigureSpec,
    profile: &RenderProfile,
    format: FigureFormat,
) -> Result<RenderedFigure, FigureError> {
    spec.validate().map_err(FigureError::Spec)?;
    if let (Some(expected), Some(actual)) = (&spec.revision_id, &input.revision_id) {
        if expected != actual {
            return Err(FigureError::RevisionMismatch {
                expected: expected.clone(),
                actual: actual.clone(),
            });
        }
    }
    if !spec.axes && format == FigureFormat::Svg {
        return Err(FigureError::Spec(
            "A figure without axes is the radargram alone, written as .png with a .json \
             sidecar; an SVG would only wrap the same picture."
                .to_string(),
        ));
    }
    if format == FigureFormat::Sidecar && spec.axes {
        return Err(FigureError::Spec(
            "Only a figure without axes (\"axes\": false) has a sidecar.".to_string(),
        ));
    }
    if spec.has_overlays() && input.overlays.is_none() && format != FigureFormat::Sidecar {
        return Err(FigureError::Spec(
            "Picks and derived layers are read from a project; give the project the \
             radargram belongs to."
                .to_string(),
        ));
    }

    match spec.view {
        View::Standard => {
            let y = match spec.y_axis.unit {
                Some(unit) => unit,
                None if input.coordinates.twtt.is_some() => YUnit::Twtt,
                None => YUnit::Sample,
            };
            let y_values = match y {
                YUnit::Sample => AxisValues::Index,
                YUnit::Twtt => table(&input.coordinates.twtt, "twtt", "two-way travel time")?,
                YUnit::Depth => table(&input.coordinates.depth, "depth", "depth")?,
                YUnit::Elevation => unreachable!("validate refuses elevation in the standard view"),
            };
            draw(
                input.source,
                input.source,
                input,
                spec,
                profile,
                format,
                spec.extent.rows,
                (y, y_values),
                None,
            )
        }
        View::Topo => {
            let (source_height, n_traces) = input.source.shape();
            let depth: Option<Vec<f32>> = input
                .coordinates
                .depth
                .as_ref()
                .map(|d| d.iter().map(|&v| v as f32).collect());
            let geometry = topo::resolve_topo_geometry(
                input.coordinates.elevation.as_deref(),
                depth.as_deref(),
                n_traces,
                source_height,
                input.elevation_range,
            )
            .map_err(|e| FigureError::Spec(e.message))?;
            let y_values = AxisValues::Linear {
                offset: geometry.elevation_top,
                slope: -geometry.dz,
            };
            // An elevation extent is converted to rows of *this* geometry,
            // which is the point of giving one: it survives an edit to the
            // elevation window that moves every row.
            let rows = match spec.extent.elevation {
                Some([top, bottom]) => Some([
                    (geometry.elevation_top - top) / geometry.dz,
                    (geometry.elevation_top - bottom) / geometry.dz,
                ]),
                None => spec.extent.rows,
            };
            let sheared = topo::TopoSource::new(input.source, &geometry);
            draw(
                &sheared,
                input.source,
                input,
                spec,
                profile,
                format,
                rows,
                (YUnit::Elevation, y_values),
                Some(&geometry.shift),
            )
        }
    }
}

/// A tabulated axis from an optional coordinate variable, or the error
/// saying the file does not have it.
fn table(values: &Option<Vec<f64>>, name: &str, what: &str) -> Result<AxisValues, FigureError> {
    match values {
        Some(values) if values.iter().any(|v| v.is_finite()) => {
            Ok(AxisValues::Table(values.clone()))
        }
        _ => Err(FigureError::Spec(format!(
            "This radargram has no usable '{name}' variable, so it cannot have a {what} axis. \
             Choose another unit."
        ))),
    }
}

/// The view-independent part of [`render_figure`]: `drawn` is what the
/// raster shows (the standard source, or the topographic shear of it), and
/// `stats` is what contrast is estimated from (always the standard source,
/// as the server does, so the shear's empty wedges do not shift it).
#[allow(clippy::too_many_arguments)]
fn draw<D: AmplitudeSource, S: AmplitudeSource, T: AmplitudeSource>(
    drawn: &D,
    stats: &S,
    input: &FigureSource<T>,
    spec: &FigureSpec,
    profile: &RenderProfile,
    format: FigureFormat,
    rows: Option<[f64; 2]>,
    (y_unit, y_values): (YUnit, AxisValues),
    shift: Option<&[f32]>,
) -> Result<RenderedFigure, FigureError> {
    let (n_rows, n_traces) = drawn.shape();
    let clip = |bounds: Option<[f64; 2]>, len: usize, name: &str| {
        let [a, b] = bounds.unwrap_or([0.0, len as f64]);
        let (a, b) = (a.max(0.0), b.min(len as f64));
        if a < b {
            Ok([a, b])
        } else {
            Err(FigureError::Spec(format!(
                "The figure's {name} extent does not overlap the radargram's (0 to {len})."
            )))
        }
    };
    let traces = clip(spec.extent.traces, n_traces, "trace")?;
    // In the topographic view the rows may reach past the data, to leave
    // room above the highest surface or below the floor: elevation is
    // defined there, and the raster is simply empty. A standard view's rows
    // are samples, and there is no time before the first one to show.
    let rows = match (spec.view, rows) {
        (View::Topo, Some([a, b])) if b > 0.0 && a < n_rows as f64 => [a, b],
        _ => clip(rows, n_rows, "row")?,
    };

    let x_unit = match spec.x_axis.unit {
        Some(unit) => unit,
        None if input.coordinates.distance.is_some() => XUnit::Distance,
        None => XUnit::Trace,
    };
    let x_values = match x_unit {
        XUnit::Trace => AxisValues::Index,
        XUnit::Distance => table(&input.coordinates.distance, "distance", "distance")?,
        XUnit::Time => table(&input.coordinates.time, "time", "time")?,
    };

    let x_ticks = axis_ticks(
        &x_values,
        traces,
        spec.x_axis.spacing,
        spec.x_axis.anchor,
        x_unit == XUnit::Time,
    )?;
    let y_ticks = axis_ticks(
        &y_values,
        rows,
        spec.y_axis.spacing,
        spec.y_axis.anchor,
        false,
    )?;
    let x_label = spec
        .x_axis
        .label
        .clone()
        .unwrap_or_else(|| x_unit.default_label().to_string());
    let y_label = spec
        .y_axis
        .label
        .clone()
        .unwrap_or_else(|| y_unit.default_label().to_string());

    let layout = Layout::new(spec, traces, rows, &x_ticks, &y_ticks, &x_label, &y_label)?;

    // Without the radargram there is nothing to estimate contrast for, and
    // no reason to read a single sample.
    let sidecar = (!spec.axes).then(|| {
        let centres = |values: &AxisValues, [a, b]: [f64; 2], n: usize| -> Vec<f64> {
            (0..n)
                .map(|i| values.value_at(a + (i as f64 + 0.5) * (b - a) / n as f64))
                .collect()
        };
        Sidecar {
            view: spec.view,
            width: layout.plot_w,
            height: layout.plot_h,
            traces,
            rows,
            x: SidecarAxis {
                unit: unit_name(&x_unit),
                label: x_label.clone(),
                extent: [x_values.value_at(traces[0]), x_values.value_at(traces[1])],
                centres: centres(&x_values, traces, layout.plot_w),
            },
            y: SidecarAxis {
                unit: unit_name(&y_unit),
                label: y_label.clone(),
                extent: [y_values.value_at(rows[0]), y_values.value_at(rows[1])],
                centres: centres(&y_values, rows, layout.plot_h),
            },
        }
    });
    if format == FigureFormat::Sidecar {
        let bytes = serde_json::to_vec_pretty(&sidecar)
            .map_err(|e| FigureError::Render(format!("Could not write the sidecar: {e}")))?;
        return Ok(RenderedFigure {
            bytes,
            width: layout.plot_w,
            height: layout.plot_h,
            sidecar,
        });
    }

    let raster = if spec.radargram {
        let (pinned, limits) = match &input.contrast {
            Some(resolved) => resolved.clone(),
            None => crate::render::oneshot::resolve_limits(stats, profile)
                .map_err(FigureError::Render)?,
        };
        let window = SourceWindow {
            row0: rows[0],
            row1: rows[1],
            col0: traces[0],
            col1: traces[1],
        };
        Some(
            Renderer::new(drawn)
                .render_window_rgba(&window, layout.plot_w, layout.plot_h, &pinned, limits)
                .map_err(FigureError::Render)?,
        )
    } else {
        None
    };

    let ppt = spec.size.px_per_pt();
    let overlay_svg = input
        .overlays
        .map(|o| {
            o.svg(
                &overlays::Placement {
                    plot: [
                        layout.plot_x as f64,
                        layout.plot_y as f64,
                        (layout.plot_x + layout.plot_w) as f64,
                        (layout.plot_y + layout.plot_h) as f64,
                    ],
                    traces,
                    rows,
                    shift,
                },
                &overlays::OverlayStyle {
                    pick_width: spec.picks.width * ppt,
                    pick_opacity: spec.picks.opacity,
                    derived_width: spec.derived.width * ppt,
                    derived_opacity: spec.derived.opacity,
                },
            )
        })
        .unwrap_or_default();

    let annotations = Annotations {
        layout: &layout,
        traces,
        rows,
        axes: spec.axes,
        x_ticks: &x_ticks,
        y_ticks: &y_ticks,
        x_label: &x_label,
        y_label: &y_label,
        color: spec::parse_colour("style.color", &spec.style.color).map_err(FigureError::Spec)?,
        overlays: &overlay_svg,
    };
    // A figure without axes is the radargram as data, so it keeps the
    // transparency of the places without any rather than a background.
    let background = if spec.axes {
        spec.style
            .background
            .as_deref()
            .map(|b| spec::parse_colour("style.background", b))
            .transpose()
            .map_err(FigureError::Spec)?
    } else {
        None
    };

    let bytes = match format {
        FigureFormat::Svg => {
            let png = raster.as_ref().map(encode_png).transpose()?;
            let svg = annotations.svg(
                background.as_deref(),
                png.as_deref(),
                Some((spec.size.unit, spec.size.dpi)),
            );
            svg.into_bytes()
        }
        FigureFormat::Png => {
            let overlay = annotations.svg(None, None, None);
            compose_png(&layout, background.as_deref(), raster.as_ref(), &overlay)?
        }
        FigureFormat::Sidecar => unreachable!("returned above, before anything is drawn"),
    };
    Ok(RenderedFigure {
        bytes,
        width: layout.width,
        height: layout.height,
        sidecar,
    })
}

/// The unit's name as the spec spells it.
fn unit_name<U: serde::Serialize>(unit: &U) -> String {
    serde_json::to_value(unit)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Ticks for one axis over `[p0, p1]`, choosing a spacing when the spec
/// does not give one.
fn axis_ticks(
    values: &AxisValues,
    [p0, p1]: [f64; 2],
    spacing: Option<f64>,
    anchor: f64,
    time: bool,
) -> Result<Vec<Tick>, FigureError> {
    let Some((lo, hi)) = values.range(p0, p1) else {
        return Ok(Vec::new());
    };
    let spacing = spacing.unwrap_or_else(|| {
        if time {
            axis::nice_time_spacing(hi - lo, AUTO_TICKS)
        } else {
            axis::nice_spacing(hi - lo, AUTO_TICKS)
        }
    });
    let format = if time {
        LabelFormat::TimeOfDay {
            seconds: spacing % 60.0 != 0.0 || anchor % 60.0 != 0.0,
        }
    } else {
        LabelFormat::Decimal(axis::decimals_for(spacing, anchor))
    };
    axis::ticks(values, p0, p1, spacing, anchor, format).map_err(FigureError::Spec)
}

/// Where everything goes, in output pixels.
#[derive(Debug, Clone, PartialEq)]
struct Layout {
    width: usize,
    height: usize,
    plot_x: usize,
    plot_y: usize,
    plot_w: usize,
    plot_h: usize,
    font_px: f64,
    tick_px: f64,
    pad_px: f64,
    line_px: f64,
}

/// Tick length, label padding and line width in points: matplotlib's
/// defaults, so the proportions look familiar.
const TICK_PT: f64 = 3.5;
const PAD_PT: f64 = 3.5;
const LINE_PT: f64 = 0.8;

/// Height of a digit above the baseline, as a fraction of the font size,
/// for DejaVu Sans. Places a label's baseline so the label is centred on
/// its tick or sits just below it.
const DIGIT_HEIGHT_EM: f64 = 0.73;

impl Layout {
    fn new(
        spec: &FigureSpec,
        traces: [f64; 2],
        rows: [f64; 2],
        x_ticks: &[Tick],
        y_ticks: &[Tick],
        x_label: &str,
        y_label: &str,
    ) -> Result<Self, FigureError> {
        let ppt = spec.size.px_per_pt();
        let font_px = spec.style.font_size * ppt;
        let tick_px = TICK_PT * ppt;
        let pad_px = PAD_PT * ppt;
        let line_px = LINE_PT * ppt;
        let total_w = spec.size.width_px();

        let widest = |ticks: &[Tick]| {
            ticks
                .iter()
                .map(|t| font::text_width(&t.label, font_px))
                .fold(0.0, f64::max)
        };
        let (left, right, top, bottom) = if spec.axes {
            let y_title = if y_label.is_empty() {
                0.0
            } else {
                font_px + pad_px
            };
            let x_title = if x_label.is_empty() {
                0.0
            } else {
                font_px + pad_px
            };
            (
                (pad_px + y_title + widest(y_ticks) + pad_px + tick_px).ceil(),
                // Half the widest x label: the last one is centred on a
                // tick that can be at the plot's right edge.
                (widest(x_ticks) / 2.0).max(pad_px).ceil(),
                (font_px / 2.0).max(pad_px).ceil(),
                (tick_px + pad_px + font_px + x_title + pad_px).ceil(),
            )
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };

        // The figure is the width asked for, to the pixel; the plot is
        // whatever the margins leave of it.
        let total_w = total_w.round();
        let plot_w = total_w - left - right;
        if plot_w < 10.0 {
            return Err(FigureError::Spec(format!(
                "A {total_w:.0} px wide figure leaves {plot_w:.0} px for the radargram once its \
                 labels have room. Make it wider, or the font smaller."
            )));
        }
        let plot_h =
            (plot_w * (rows[1] - rows[0]) / ((traces[1] - traces[0]) * spec.x_scale)).round();
        let plot_h = plot_h.max(1.0);
        // The y label is centred on the plot and rotated, so on a short plot
        // it is taller than the plot and would run off the figure. Enough
        // margin above and below to hold it, then.
        let (top, bottom) = if spec.axes && !y_label.is_empty() {
            let overhang = (font::text_width(y_label, font_px) - plot_h) / 2.0 + pad_px;
            (top.max(overhang.ceil()), bottom.max(overhang.ceil()))
        } else {
            (top, bottom)
        };
        let width = (left + plot_w + right) as usize;
        let height = (top + plot_h + bottom) as usize;
        if width > MAX_FIGURE_SIDE
            || height > MAX_FIGURE_SIDE
            || width.saturating_mul(height) > MAX_FIGURE_PIXELS
        {
            return Err(FigureError::Spec(format!(
                "The figure would be {width} x {height} px, above the limit of \
                 {MAX_FIGURE_PIXELS} px ({MAX_FIGURE_SIDE} px a side). The height follows from \
                 the extent and x_scale: a smaller width, a lower dpi, a larger x_scale or a \
                 shorter extent all make it smaller."
            )));
        }
        Ok(Self {
            width,
            height,
            plot_x: left as usize,
            plot_y: top as usize,
            plot_w: plot_w as usize,
            plot_h: plot_h as usize,
            font_px,
            tick_px,
            pad_px,
            line_px,
        })
    }

    fn x_px(&self, [t0, t1]: [f64; 2], trace: f64) -> f64 {
        self.plot_x as f64 + (trace - t0) / (t1 - t0) * self.plot_w as f64
    }

    fn y_px(&self, [r0, r1]: [f64; 2], row: f64) -> f64 {
        self.plot_y as f64 + (row - r0) / (r1 - r0) * self.plot_h as f64
    }
}

/// Everything drawn over the raster.
struct Annotations<'a> {
    layout: &'a Layout,
    traces: [f64; 2],
    rows: [f64; 2],
    /// `false` draws no frame, ticks or labels: only the overlays.
    axes: bool,
    x_ticks: &'a [Tick],
    y_ticks: &'a [Tick],
    x_label: &'a str,
    y_label: &'a str,
    color: String,
    /// The picks and derived layers, already drawn as SVG elements.
    overlays: &'a str,
}

impl Annotations<'_> {
    /// The figure as SVG.
    ///
    /// `raster` is the radargram's PNG, embedded under the annotations, or
    /// `None` for the overlay a PNG figure draws over its own raster.
    /// `physical` gives the document a size in centimetres or inches, so a
    /// word processor inserts it at the size it was made for; the overlay
    /// is sized in pixels, which is what resvg draws it at.
    ///
    /// Plain SVG 1.1 presentation attributes only, with no CSS, filters or
    /// masks, since Office and other non-browser renderers read those
    /// unevenly.
    fn svg(
        &self,
        background: Option<&str>,
        raster: Option<&[u8]>,
        physical: Option<(SizeUnit, f64)>,
    ) -> String {
        use std::fmt::Write;
        let l = self.layout;
        let (w, h) = (l.width, l.height);
        let size = |px: usize| match physical {
            Some((SizeUnit::Cm, dpi)) => format!("{:.3}cm", px as f64 / dpi * 2.54),
            Some((SizeUnit::In, dpi)) => format!("{:.3}in", px as f64 / dpi),
            _ => format!("{px}"),
        };
        let mut s = String::new();
        let _ = writeln!(
            s,
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" version="1.1" width="{}" height="{}" viewBox="0 0 {w} {h}">"#,
            size(w),
            size(h)
        );
        if let Some(background) = background {
            let _ = writeln!(
                s,
                r#"<rect x="0" y="0" width="{w}" height="{h}" fill="{background}"/>"#
            );
        }
        if let Some(png) = raster {
            let data = base64::engine::general_purpose::STANDARD.encode(png);
            let _ = writeln!(
                s,
                r#"<image x="{}" y="{}" width="{}" height="{}" preserveAspectRatio="none" image-rendering="optimizeSpeed" xlink:href="data:image/png;base64,{data}"/>"#,
                l.plot_x, l.plot_y, l.plot_w, l.plot_h
            );
        }

        // Picks and derived layers over the radargram, under the frame.
        s.push_str(self.overlays);
        if !self.axes {
            s.push_str("</svg>\n");
            return s;
        }

        // Frame and ticks, as one path each, outward from the plot.
        let (x0, y0) = (l.plot_x as f64, l.plot_y as f64);
        let (x1, y1) = (x0 + l.plot_w as f64, y0 + l.plot_h as f64);
        let mut ticks = String::new();
        for tick in self.x_ticks {
            let x = l.x_px(self.traces, tick.position);
            let _ = write!(ticks, "M{x:.2} {y1:.2}V{:.2}", y1 + l.tick_px);
        }
        for tick in self.y_ticks {
            let y = l.y_px(self.rows, tick.position);
            let _ = write!(ticks, "M{x0:.2} {y:.2}H{:.2}", x0 - l.tick_px);
        }
        let _ = writeln!(
            s,
            r#"<g fill="none" stroke="{}" stroke-width="{:.2}" stroke-linecap="square">"#,
            self.color, l.line_px
        );
        let _ = writeln!(
            s,
            r#"<rect x="{x0}" y="{y0}" width="{}" height="{}"/>"#,
            l.plot_w, l.plot_h
        );
        if !ticks.is_empty() {
            let _ = writeln!(s, r#"<path d="{ticks}" stroke-linecap="butt"/>"#);
        }
        let _ = writeln!(s, "</g>");

        // Labels.
        let fs = l.font_px;
        let _ = writeln!(
            s,
            r#"<g font-family="{}" font-size="{fs:.2}" fill="{}">"#,
            font::SVG_FONT_FAMILY,
            self.color
        );
        let x_label_baseline = y1 + l.tick_px + l.pad_px + DIGIT_HEIGHT_EM * fs;
        for tick in self.x_ticks {
            let x = l.x_px(self.traces, tick.position);
            let _ = writeln!(
                s,
                r#"<text x="{x:.2}" y="{x_label_baseline:.2}" text-anchor="middle">{}</text>"#,
                escape(&tick.label)
            );
        }
        let y_label_right = x0 - l.tick_px - l.pad_px;
        for tick in self.y_ticks {
            let y = l.y_px(self.rows, tick.position) + DIGIT_HEIGHT_EM * fs / 2.0;
            let _ = writeln!(
                s,
                r#"<text x="{y_label_right:.2}" y="{y:.2}" text-anchor="end">{}</text>"#,
                escape(&tick.label)
            );
        }
        if !self.x_label.is_empty() {
            // One line below the tick labels.
            let baseline = x_label_baseline + fs + l.pad_px;
            let _ = writeln!(
                s,
                r#"<text x="{:.2}" y="{baseline:.2}" text-anchor="middle">{}</text>"#,
                (x0 + x1) / 2.0,
                escape(self.x_label)
            );
        }
        if !self.y_label.is_empty() {
            // Rotated a quarter turn anticlockwise, so its glyphs stand to
            // the left of the baseline: the baseline goes one ascent in
            // from the figure's left edge.
            let x = l.pad_px + DIGIT_HEIGHT_EM * fs;
            let y = (y0 + y1) / 2.0;
            let _ = writeln!(
                s,
                r#"<text x="{x:.2}" y="{y:.2}" text-anchor="middle" transform="rotate(-90 {x:.2} {y:.2})">{}</text>"#,
                escape(self.y_label)
            );
        }
        let _ = writeln!(s, "</g>");
        s.push_str("</svg>\n");
        s
    }
}

/// Escape text for an SVG text node.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn encode_png(image: &image::RgbaImage) -> Result<Vec<u8>, FigureError> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image.clone())
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageOutputFormat::Png,
        )
        .map_err(|e| FigureError::Render(format!("Could not encode the figure: {e}")))?;
    Ok(bytes)
}

/// The font database resvg draws with: the bundled font and nothing else,
/// standing in for every generic family too.
fn font_database() -> Arc<resvg::usvg::fontdb::Database> {
    static DATABASE: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DATABASE
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_font_data(font::FONT_DATA.to_vec());
            db.set_sans_serif_family(font::FONT_FAMILY);
            db.set_serif_family(font::FONT_FAMILY);
            db.set_monospace_family(font::FONT_FAMILY);
            Arc::new(db)
        })
        .clone()
}

/// A PNG figure: background, then the raster, then `overlay` drawn by
/// resvg.
fn compose_png(
    layout: &Layout,
    background: Option<&str>,
    raster: Option<&image::RgbaImage>,
    overlay: &str,
) -> Result<Vec<u8>, FigureError> {
    use resvg::tiny_skia;
    let fail = |what: &str| FigureError::Render(format!("Could not draw the figure: {what}"));

    let mut pixmap = tiny_skia::Pixmap::new(layout.width as u32, layout.height as u32)
        .ok_or_else(|| fail("no room for its pixels"))?;
    if let Some(background) = background {
        let [r, g, b] = hex_rgb(background);
        pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
    }

    if let Some(raster) = raster {
        // Raster pixels are either opaque or fully transparent, so
        // premultiplying them is zeroing the transparent ones.
        let premultiplied: Vec<u8> = raster
            .pixels()
            .flat_map(|p| if p.0[3] == 0 { [0, 0, 0, 0] } else { p.0 })
            .collect();
        let size = tiny_skia::IntSize::from_wh(raster.width(), raster.height())
            .ok_or_else(|| fail("an empty raster"))?;
        let raster =
            tiny_skia::Pixmap::from_vec(premultiplied, size).ok_or_else(|| fail("a bad raster"))?;
        pixmap.draw_pixmap(
            layout.plot_x as i32,
            layout.plot_y as i32,
            raster.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
    }

    let options = resvg::usvg::Options {
        font_family: font::FONT_FAMILY.to_string(),
        fontdb: font_database(),
        ..resvg::usvg::Options::default()
    };
    let tree = resvg::usvg::Tree::from_str(overlay, &options)
        .map_err(|e| fail(&format!("the annotations did not parse ({e})")))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );

    pixmap
        .encode_png()
        .map_err(|e| FigureError::Render(format!("Could not encode the figure: {e}")))
}

/// `#rrggbb` to bytes. Only called on colours [`spec::parse_colour`]
/// already normalised.
fn hex_rgb(colour: &str) -> [u8; 3] {
    let channel = |i: usize| u8::from_str_radix(&colour[1 + 2 * i..3 + 2 * i], 16).unwrap_or(0);
    [channel(0), channel(1), channel(2)]
}

/// What [`render_figure_path`] wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct WrittenFigure {
    pub width: usize,
    pub height: usize,
    /// The sidecar's path, for a figure without axes.
    pub sidecar: Option<std::path::PathBuf>,
    /// Picks that could not be drawn as stored; see [`overlays::Overlays`].
    pub warnings: Vec<String>,
}

/// Render `spec` of the processed `.nc` at `input` to `output`, resolving
/// the profile with [`RenderProfile::resolve`] (a built-in name or a TOML
/// file).
///
/// `project` is the project directory the radargram belongs to. It is where
/// picks and derived layers are read from, with everything in it visible,
/// as whoever runs this can read its files anyway, and where the
/// radargram's elevation window for the topographic view comes from.
///
/// The format comes from `output`'s extension. A figure without axes also
/// writes its [`Sidecar`] beside `output`, with a `.json` extension.
pub fn render_figure_path(
    input: &Path,
    output: &Path,
    spec: &FigureSpec,
    project: Option<&Path>,
) -> Result<WrittenFigure, String> {
    let format = FigureFormat::for_path(output)?;
    let profile = RenderProfile::resolve(&spec.profile)?;
    let metadata = match crate::io::inspect_ridal_netcdf(input)? {
        crate::io::RidalNetcdfKind::Supported(metadata) => Some(metadata),
        _ => None,
    };
    let revision_id = metadata.as_ref().map(|m| m.revision_id().to_string());

    let mut elevation_range = topo::ElevationRange::NONE;
    let overlays = match project {
        Some(dir) => {
            let project = crate::project::Project::open(dir).map_err(|e| e.to_string())?;
            let metadata = metadata.as_ref().ok_or_else(|| {
                format!(
                    "{} has no radargram id, so its picks cannot be found in a project.",
                    input.display()
                )
            })?;
            if let Ok((overrides, _)) = crate::project::overrides::read(project.documents()) {
                if let Some(over) = overrides.radargrams.get(&metadata.radargram_id) {
                    elevation_range = topo::ElevationRange {
                        min: over.elevation_min,
                        max: over.elevation_max,
                    };
                }
            }
            Some(overlays::gather(
                &project,
                &metadata.radargram_id,
                input,
                &metadata.revision_id(),
                spec,
                &overlays::LayerAccess::everything(),
            )?)
        }
        None => None,
    };

    let reader = SourceReader::open(input)?;
    let coordinates = Coordinates::read(&reader);
    let figure = render_figure(
        &FigureSource {
            source: &reader,
            coordinates: &coordinates,
            revision_id,
            elevation_range,
            overlays: overlays.as_ref(),
            contrast: None,
        },
        spec,
        &profile,
        format,
    )?;

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(output, &figure.bytes)
        .map_err(|e| format!("Could not write {}: {e}", output.display()))?;
    let sidecar = match &figure.sidecar {
        Some(sidecar) => {
            let path = output.with_extension("json");
            let text = serde_json::to_string_pretty(sidecar).map_err(|e| e.to_string())?;
            std::fs::write(&path, text)
                .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
            Some(path)
        }
        None => None,
    };
    Ok(WrittenFigure {
        width: figure.width,
        height: figure.height,
        sidecar,
        warnings: overlays.map(|o| o.warnings).unwrap_or_default(),
    })
}

/// Read a figure spec from a JSON file.
pub fn read_spec(path: &Path) -> Result<FigureSpec, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read the figure spec {}: {e}", path.display()))?;
    FigureSpec::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod overlay_tests;
