//! Normalization, colormapping and image encoding (#118, #119, #246).
//!
//! Two rendering paths: a profile without a [`Colormap`] normalizes
//! straight to a single [`GrayImage`] channel (the original v1 path,
//! kept byte-identical for every grayscale profile), and one with a
//! colormap normalizes to a byte that indexes a 256-entry RGB LUT.
//!
//! The LUT is built once per render. Interpolation between stops is a
//! straight sRGB lerp on the 0--255 values, matching
//! `matplotlib.colors.LinearSegmentedColormap` and QGIS's gradient
//! editor; a perceptual space such as Oklab would ramp more evenly but
//! would no longer reproduce their output.
//!
//! Purple at overview zoom is expected, not a bug. The `seismic` ramp
//! contains no purple -- every entry is a blue, a white, a pink or a
//! red. But radar traces oscillate about zero, so adjacent samples land
//! on opposite sides of the white midpoint and red sits beside blue at
//! pixel scale, and anything that averages *pixels* blends them:
//! `avg(#ff0000, #0000ff) == #7f007f`. That happens in browser-side
//! scaling of the image chunks when zoomed out, in JPEG 4:2:0 chroma
//! subsampling, and optically. It does not come from the resampler:
//! `Mean` averages amplitude before colormapping, and +/- amplitude
//! averages toward zero, which is correctly white.

use image::{ColorType, GrayImage, ImageEncoder, Rgb, RgbImage};
use ndarray::Array2;

use super::profile::{
    AmplitudeLimits, AmplitudeTransform, ImageFormat, RenderProfile, SourceTransform,
};
use crate::filters;

/// One colour stop: a position in `[0, 1]` and its 8-bit sRGB colour.
///
/// Positions are explicit even though the built-in ramp is evenly
/// spaced, so a future editor (#183) is a UI problem rather than a
/// data-model migration.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColorStop {
    pub position: f32,
    pub color: [u8; 3],
}

/// A named gradient as a list of stops, linearly interpolated in sRGB.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Colormap {
    pub name: String,
    pub stops: Vec<ColorStop>,
}

impl Colormap {
    /// `seismic`: matplotlib's diverging blue--white--red ramp with
    /// darkened tails, at its five evenly spaced stops. The dark navy and
    /// maroon ends let a clipped extreme stay distinguishable from a
    /// merely strong one, which is why this is preferred over a plain
    /// `bwr` ramp.
    pub fn seismic() -> Self {
        Self {
            name: "seismic".to_string(),
            stops: vec![
                ColorStop {
                    position: 0.0,
                    color: [0x00, 0x00, 0x4c],
                },
                ColorStop {
                    position: 0.25,
                    color: [0x00, 0x00, 0xff],
                },
                ColorStop {
                    position: 0.5,
                    color: [0xff, 0xff, 0xff],
                },
                ColorStop {
                    position: 0.75,
                    color: [0xff, 0x00, 0x00],
                },
                ColorStop {
                    position: 1.0,
                    color: [0x80, 0x00, 0x00],
                },
            ],
        }
    }

    /// The colour at position `t`, linearly interpolated between the
    /// surrounding stops. Positions outside the stop range clamp to the
    /// first/last colour.
    pub fn sample(&self, t: f32) -> [u8; 3] {
        let first = self.stops[0];
        if t <= first.position {
            return first.color;
        }
        for pair in self.stops.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if t <= b.position {
                let span = b.position - a.position;
                let u = if span == 0.0 {
                    0.0
                } else {
                    (t - a.position) / span
                };
                return [
                    lerp_u8(a.color[0], b.color[0], u),
                    lerp_u8(a.color[1], b.color[1], u),
                    lerp_u8(a.color[2], b.color[2], u),
                ];
            }
        }
        self.stops[self.stops.len() - 1].color
    }

    /// The 256-entry lookup table for this colormap: entry `i` is the
    /// colour at position `i / 255`, so the normalized byte
    /// [`normalize_to_u8`] already produces indexes it directly.
    pub fn lut(&self) -> [[u8; 3]; 256] {
        let mut lut = [[0u8; 3]; 256];
        for (i, entry) in lut.iter_mut().enumerate() {
            *entry = self.sample(i as f32 / 255.0);
        }
        lut
    }

    /// Check the invariants [`sample`](Self::sample) and
    /// [`lut`](Self::lut) rely on, so a hand-written profile file fails
    /// with a contextual error instead of an index panic.
    ///
    /// Validated at the file boundary (`RenderProfile::from_toml_file`),
    /// not inside the render path: the built-ins are constructed valid.
    pub fn validate(&self) -> Result<(), String> {
        if self.stops.is_empty() {
            return Err(format!("colormap '{}' has no stops", self.name));
        }
        let mut previous = f32::NEG_INFINITY;
        for stop in &self.stops {
            if !(0.0..=1.0).contains(&stop.position) {
                return Err(format!(
                    "colormap '{}' has a stop at position {}; positions must be in [0, 1]",
                    self.name, stop.position
                ));
            }
            if stop.position <= previous {
                return Err(format!(
                    "colormap '{}' stops must be strictly increasing; \
                     {} follows {previous}",
                    self.name, stop.position
                ));
            }
            previous = stop.position;
        }
        Ok(())
    }

    /// A stable string identifying everything about this colormap that
    /// affects rendered pixels, for folding into the profile cache key.
    /// The stops are included, not just the name: a hand-written profile
    /// file can call any gradient `seismic`.
    pub fn cache_key_fragment(&self) -> String {
        let stops: Vec<String> = self
            .stops
            .iter()
            .map(|s| {
                format!(
                    "{}:{},{},{}",
                    s.position, s.color[0], s.color[1], s.color[2]
                )
            })
            .collect();
        format!("cm:{}:{}", self.name, stops.join(";"))
    }
}

/// A straight sRGB lerp between two 8-bit channel values, rounded to the
/// nearest byte.
fn lerp_u8(a: u8, b: u8, u: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * u)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// `NaN`-safe, infinite-safe cleanup shared by both domain functions below.
///
/// `NaN` (the resampler's "no valid source data in this footprint" signal,
/// #118) passes through unchanged -- it is handled separately, as
/// transparency/pad color, never averaged into anything. A literal
/// infinite value is a data anomaly, not an absence of data, and is
/// replaced with zero per #119 ("non-finite amplitude values are treated
/// as invalid input and replaced with zero for rendering").
fn sanitize(v: f32) -> f32 {
    if v.is_nan() {
        v
    } else if v.is_infinite() {
        0.0
    } else {
        v
    }
}

fn log_abs(v: f32) -> f32 {
    let a = v.abs();
    if a == 0.0 {
        f32::NEG_INFINITY
    } else {
        a.log10()
    }
}

/// Apply a profile's [`SourceTransform`] to one **source** sample, before
/// resampling.
///
/// `None` is the identity, so a profile with no source preprocessing reads
/// exactly as it did before this stage existed. `SigLog` reproduces the
/// `siglog` processing step's `(log10|v| - offset).max(0) * sign(v)` via
/// [`filters::siglog_value`], at the step's own default offset.
///
/// `NaN` (the "no data" signal the resampler must keep seeing) passes
/// through. A literal infinite value is sanitized to zero first, matching
/// [`to_display_domain`], so the resampler never sees an infinity it would
/// propagate across a whole footprint.
pub fn to_source_domain(v: f32, transform: SourceTransform) -> f32 {
    match transform {
        SourceTransform::None => v,
        SourceTransform::SigLog => {
            if v.is_nan() {
                return v;
            }
            filters::siglog_value(sanitize(v), filters::DEFAULT_SIGLOG_MINVAL_LOG10)
        }
    }
}

/// Transform a resampled amplitude value into the domain that the pixel
/// value actually normalized and painted lives in.
///
/// For `Linear` and `AbsLog`, this is the same domain percentile limits are
/// estimated in (`to_stats_domain` below), so those profiles' limits mean
/// the same thing to both functions. `Positive` is the deliberate
/// exception: the *signed* value is kept here even though its limits are
/// estimated from `|x|` (see `AmplitudeTransform::Positive`), which is what
/// lets the stretch push negative returns toward black.
pub fn to_display_domain(v: f32, transform: AmplitudeTransform) -> f32 {
    if v.is_nan() {
        return v;
    }
    let v = sanitize(v);
    match transform {
        AmplitudeTransform::Linear | AmplitudeTransform::Positive => v,
        AmplitudeTransform::AbsLog => log_abs(v),
    }
}

/// Transform a value into the domain percentile limits are estimated in
/// (`stats.rs`). See [`to_display_domain`] for why this differs from it for
/// `Positive`.
pub fn to_stats_domain(v: f32, transform: AmplitudeTransform) -> f32 {
    if v.is_nan() {
        return v;
    }
    match transform {
        AmplitudeTransform::Positive => sanitize(v).abs(),
        AmplitudeTransform::Linear | AmplitudeTransform::AbsLog => to_display_domain(v, transform),
    }
}

/// Resolve a profile's [`AmplitudeLimits`] to concrete `(min, max)` bounds.
///
/// `sampled` supplies the percentile-based estimate from `stats.rs` when
/// the profile calls for one; explicit limits need no sample at all
/// (#119). Both branches reject a degenerate `min >= max` (explicit) or
/// `low == high` (estimated) with an error rather than producing a
/// division-by-zero NaN-everywhere image.
///
/// `symmetric` forces the resolved bounds to `(-vmax, vmax)` with
/// `vmax = max(|low|, |high|)`, applied after estimation so it composes
/// with both limit kinds. A diverging colormap means something only if
/// its midpoint colour sits at a meaningful value -- for signed
/// amplitude, zero -- and the default 1--99% estimate is asymmetric, so
/// without this the white point would land wherever the midpoint of
/// `[min, max]` happens to fall and the ramp would look diverging while
/// meaning nothing.
pub fn resolve_limits(
    limits: &AmplitudeLimits,
    sampled: Option<(f32, f32)>,
    symmetric: bool,
) -> Result<(f32, f32), String> {
    let resolved = match *limits {
        AmplitudeLimits::Explicit { min, max } => {
            if min >= max {
                return Err(format!(
                    "invalid explicit amplitude limits: min ({min}) must be < max ({max})"
                ));
            }
            (min, max)
        }
        AmplitudeLimits::Percentile { .. } => {
            let (low, high) = sampled
                .ok_or("percentile amplitude limits requested but no sample was supplied")?;
            if low == high {
                return Err(format!(
                    "degenerate estimated amplitude limits: low == high == {low}"
                ));
            }
            (low, high)
        }
    };
    if !symmetric {
        return Ok(resolved);
    }
    let vmax = resolved.0.abs().max(resolved.1.abs());
    Ok((-vmax, vmax))
}

/// Map one already-display-domain value to a grayscale byte, or `None` for
/// "no data" (rendered as `pad_value` by the caller).
///
/// `contrast` and `black_level` generalize the plain min-max stretch into
/// PFA_website's `normalize()` formula: `contrast * ((v - min) / (max -
/// min) - black_level)`, clamped to `[0, 1]`. The default profile's neutral
/// values (`contrast: 1.0`, `black_level: 0.0`) reduce this back to the
/// original plain stretch exactly.
fn normalize_to_u8(v: f32, min: f32, max: f32, contrast: f32, black_level: f32) -> Option<u8> {
    if v.is_nan() {
        return None;
    }
    let t = (contrast * ((v - min) / (max - min) - black_level)).clamp(0.0, 1.0);
    Some((t * 255.0).round() as u8)
}

/// Render a resampled amplitude array (as produced by
/// [`super::resample::resample_area_weighted_mean`]) to a grayscale image
/// of the same dimensions. `pad_value` fills pixels with no valid source
/// data -- edge padding beyond the raster extent, or an empty footprint --
/// so the padded region is a flat, unobtrusive fill rather than noise.
pub fn render_grayscale(
    data: &Array2<f32>,
    profile: &RenderProfile,
    limits: (f32, f32),
    pad_value: u8,
) -> GrayImage {
    let (height, width) = (data.shape()[0], data.shape()[1]);
    let mut image = GrayImage::new(width as u32, height as u32);
    for y in 0..height {
        for x in 0..width {
            let raw = data[[y, x]];
            let displayed = to_display_domain(raw, profile.transform);
            let byte = normalize_to_u8(
                displayed,
                limits.0,
                limits.1,
                profile.contrast,
                profile.black_level,
            )
            .unwrap_or(pad_value);
            image.put_pixel(x as u32, y as u32, image::Luma([byte]));
        }
    }
    image
}

/// Render a resampled amplitude array to a colormapped RGB image of the
/// same dimensions, using a LUT built once by the caller
/// ([`Colormap::lut`]).
///
/// `pad_color` fills pixels with no valid source data, and is deliberately
/// a literal colour rather than `lut[PAD_VALUE]`: pushing the pad grey
/// through the colormap would paint no-data as mid-amplitude white and
/// make an empty footprint indistinguishable from a real zero crossing.
pub fn render_colormapped(
    data: &Array2<f32>,
    profile: &RenderProfile,
    limits: (f32, f32),
    lut: &[[u8; 3]; 256],
    pad_color: [u8; 3],
) -> RgbImage {
    let (height, width) = (data.shape()[0], data.shape()[1]);
    let mut image = RgbImage::new(width as u32, height as u32);
    for y in 0..height {
        for x in 0..width {
            let raw = data[[y, x]];
            let displayed = to_display_domain(raw, profile.transform);
            let color = normalize_to_u8(
                displayed,
                limits.0,
                limits.1,
                profile.contrast,
                profile.black_level,
            )
            .map(|byte| lut[byte as usize])
            .unwrap_or(pad_color);
            image.put_pixel(x as u32, y as u32, Rgb(color));
        }
    }
    image
}

/// Encode a grayscale image per the profile's [`ImageFormat`].
pub fn encode(image: &GrayImage, format: ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match format {
        ImageFormat::Jpeg { quality } => {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
                .encode(image, image.width(), image.height(), ColorType::L8)
                .map_err(|e| format!("JPEG encoding failed: {e}"))?;
        }
        ImageFormat::Png => {
            image::codecs::png::PngEncoder::new(&mut out)
                .write_image(image, image.width(), image.height(), ColorType::L8)
                .map_err(|e| format!("PNG encoding failed: {e}"))?;
        }
    }
    Ok(out)
}

/// Encode an RGB image per the profile's [`ImageFormat`].
///
/// The colormapped counterpart to [`encode`], kept separate rather than
/// generic over the pixel type so the grayscale path's byte-for-byte
/// output and its `ColorType::L8` JPEG (a genuinely one-component JPEG,
/// which is markedly smaller than a replicated grey RGB one) are
/// untouched.
pub fn encode_rgb(image: &RgbImage, format: ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match format {
        ImageFormat::Jpeg { quality } => {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
                .encode(image, image.width(), image.height(), ColorType::Rgb8)
                .map_err(|e| format!("JPEG encoding failed: {e}"))?;
        }
        ImageFormat::Png => {
            image::codecs::png::PngEncoder::new(&mut out)
                .write_image(image, image.width(), image.height(), ColorType::Rgb8)
                .map_err(|e| format!("PNG encoding failed: {e}"))?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_domain_linear_passthrough() {
        assert_eq!(to_display_domain(5.0, AmplitudeTransform::Linear), 5.0);
        assert_eq!(to_display_domain(-3.0, AmplitudeTransform::Linear), -3.0);
    }

    #[test]
    fn display_domain_abslog_matches_log10_abs() {
        assert!((to_display_domain(100.0, AmplitudeTransform::AbsLog) - 2.0).abs() < 1e-6);
        assert!((to_display_domain(-100.0, AmplitudeTransform::AbsLog) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn source_domain_siglog_keeps_the_sign_and_truncates_small_magnitudes() {
        // offset = -1, so magnitudes below 10^-1 == 0.1 truncate to zero.
        // 1000 -> log10(1000) + 1 == 4, and the sign survives.
        assert!((to_source_domain(1000.0, SourceTransform::SigLog) - 4.0).abs() < 1e-6);
        assert!((to_source_domain(-1000.0, SourceTransform::SigLog) + 4.0).abs() < 1e-6);
        assert!((to_source_domain(1.0, SourceTransform::SigLog) - 1.0).abs() < 1e-6);
        assert!((to_source_domain(-1.0, SourceTransform::SigLog) + 1.0).abs() < 1e-6);
        // Below the offset: zero, not a small negative log.
        assert_eq!(to_source_domain(0.05, SourceTransform::SigLog), 0.0);
        assert_eq!(to_source_domain(-0.05, SourceTransform::SigLog), 0.0);
        assert_eq!(to_source_domain(0.0, SourceTransform::SigLog), 0.0);
    }

    #[test]
    fn source_domain_siglog_matches_the_processing_filter() {
        // The render must reproduce the `siglog` step, so pin it against
        // the filter's own scalar and confirm the offset arithmetic and
        // the sign together.
        let offset = filters::DEFAULT_SIGLOG_MINVAL_LOG10;
        for v in [1000.0f32, 1.0, -1000.0, -1.0, 0.05, -0.05, 0.0] {
            let expected = (v.abs().log10() - offset).max(0.0) * v.signum();
            let got = to_source_domain(v, SourceTransform::SigLog);
            assert!(
                (got - expected).abs() < 1e-6,
                "siglog({v}) = {got}, expected {expected}"
            );
            assert_eq!(got.signum(), v.signum(), "sign differs for {v}");
        }
    }

    #[test]
    fn source_domain_none_is_the_identity() {
        // A profile with no source preprocessing must read exactly as it
        // did before the stage existed, NaN and infinity included.
        for v in [
            3.0f32,
            -3.0,
            0.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            let got = to_source_domain(v, SourceTransform::None);
            assert!(
                got == v || (got.is_nan() && v.is_nan()),
                "None changed {v} to {got}"
            );
        }
    }

    #[test]
    fn source_domain_siglog_preserves_nan_and_sanitizes_infinity() {
        // NaN is the resampler's "no data" signal and must survive so the
        // footprint is dropped, not averaged in as zero. An infinity is a
        // data anomaly, sanitized to zero like the display domain does.
        assert!(to_source_domain(f32::NAN, SourceTransform::SigLog).is_nan());
        assert_eq!(
            to_source_domain(f32::INFINITY, SourceTransform::SigLog),
            0.0
        );
        assert_eq!(
            to_source_domain(f32::NEG_INFINITY, SourceTransform::SigLog),
            0.0
        );
    }

    #[test]
    fn display_domain_positive_keeps_the_sign() {
        // Unlike stats domain (tested below), display domain for `Positive`
        // is the whole point of the profile: the signed value survives so
        // negative returns can clip toward black.
        assert_eq!(to_display_domain(5.0, AmplitudeTransform::Positive), 5.0);
        assert_eq!(to_display_domain(-5.0, AmplitudeTransform::Positive), -5.0);
    }

    #[test]
    fn stats_domain_positive_takes_absolute_value() {
        assert_eq!(to_stats_domain(5.0, AmplitudeTransform::Positive), 5.0);
        assert_eq!(to_stats_domain(-5.0, AmplitudeTransform::Positive), 5.0);
    }

    #[test]
    fn stats_domain_matches_display_domain_for_linear_and_abslog() {
        for transform in [AmplitudeTransform::Linear, AmplitudeTransform::AbsLog] {
            for v in [3.0f32, -3.0, 0.0] {
                assert_eq!(
                    to_stats_domain(v, transform),
                    to_display_domain(v, transform)
                );
            }
        }
    }

    #[test]
    fn display_domain_nan_passes_through_untouched() {
        assert!(to_display_domain(f32::NAN, AmplitudeTransform::Linear).is_nan());
        assert!(to_display_domain(f32::NAN, AmplitudeTransform::AbsLog).is_nan());
        assert!(to_display_domain(f32::NAN, AmplitudeTransform::Positive).is_nan());
        assert!(to_source_domain(f32::NAN, SourceTransform::SigLog).is_nan());
        assert!(to_source_domain(f32::NAN, SourceTransform::None).is_nan());
    }

    #[test]
    fn display_domain_infinite_becomes_zero_not_dropped() {
        // Linear: infinite -> 0.0 exactly.
        assert_eq!(
            to_display_domain(f32::INFINITY, AmplitudeTransform::Linear),
            0.0
        );
        assert_eq!(
            to_display_domain(f32::NEG_INFINITY, AmplitudeTransform::Linear),
            0.0
        );
    }

    #[test]
    fn resolve_limits_explicit_rejects_min_gte_max() {
        assert!(resolve_limits(
            &AmplitudeLimits::Explicit { min: 5.0, max: 1.0 },
            None,
            false
        )
        .is_err());
        assert!(resolve_limits(
            &AmplitudeLimits::Explicit { min: 1.0, max: 1.0 },
            None,
            false
        )
        .is_err());
        assert!(resolve_limits(
            &AmplitudeLimits::Explicit { min: 1.0, max: 5.0 },
            None,
            false
        )
        .is_ok());
    }

    #[test]
    fn resolve_limits_percentile_requires_sample_and_rejects_degenerate() {
        let limits = AmplitudeLimits::Percentile {
            low: 0.01,
            high: 0.99,
        };
        assert!(resolve_limits(&limits, None, false).is_err());
        assert!(resolve_limits(&limits, Some((1.0, 1.0)), false).is_err());
        assert_eq!(
            resolve_limits(&limits, Some((1.0, 5.0)), false).unwrap(),
            (1.0, 5.0)
        );
    }

    #[test]
    fn resolve_limits_symmetric_puts_zero_at_the_white_point() {
        // The whole reason a diverging colormap can be used on signed
        // amplitude: whatever the asymmetric estimate says, the midpoint
        // of the limits must be zero. Composes with both limit kinds, and
        // the larger absolute bound wins.
        assert_eq!(
            resolve_limits(
                &AmplitudeLimits::Percentile {
                    low: 0.01,
                    high: 0.99,
                },
                Some((2.0, 10.0)),
                true,
            )
            .unwrap(),
            (-10.0, 10.0)
        );
        assert_eq!(
            resolve_limits(
                &AmplitudeLimits::Percentile {
                    low: 0.01,
                    high: 0.99,
                },
                Some((-10.0, 2.0)),
                true,
            )
            .unwrap(),
            (-10.0, 10.0)
        );
        assert_eq!(
            resolve_limits(
                &AmplitudeLimits::Explicit {
                    min: -3.0,
                    max: 5.0,
                },
                None,
                true,
            )
            .unwrap(),
            (-5.0, 5.0)
        );
        // And `false` is the untouched path.
        assert_eq!(
            resolve_limits(
                &AmplitudeLimits::Explicit {
                    min: -3.0,
                    max: 5.0,
                },
                None,
                false,
            )
            .unwrap(),
            (-3.0, 5.0)
        );
    }

    #[test]
    fn seismic_lut_matches_matplotlib_at_eighths() {
        // Reference bytes sampled from matplotlib 3.10.5's `seismic`
        // (matplotlib.colors.LinearSegmentedColormap). The index rule is
        // matplotlib's own: a float x indexes its 256-entry LUT at
        // `int(x * N)`, clamped to 255 -- not at `(x * 255).round()`,
        // which is a different entry for some positions (0.625 -> 159 vs
        // 160). This test pins the LUT's interpolation against matplotlib;
        // the render path indexes the LUT by the normalized byte
        // `normalize_to_u8` produces, which is at most one entry away.
        let lut = Colormap::seismic().lut();
        let references: &[(f32, [u8; 3])] = &[
            (0.000, [0x00, 0x00, 0x4c]),
            (0.125, [0x00, 0x00, 0xa6]),
            (0.250, [0x01, 0x01, 0xff]),
            (0.375, [0x81, 0x81, 0xff]),
            (0.500, [0xff, 0xfd, 0xfd]),
            (0.625, [0xff, 0x7d, 0x7d]),
            (0.750, [0xfe, 0x00, 0x00]),
            (0.875, [0xbe, 0x00, 0x00]),
            (1.000, [0x80, 0x00, 0x00]),
        ];
        for (t, expected) in references {
            let index = ((t * 256.0) as usize).min(255);
            assert_eq!(lut[index], *expected, "seismic at {t} (LUT index {index})");
        }
        // The tail colours are the reason `seismic` is kept over `bwr`:
        // the ramp darkens rather than saturating.
        assert_eq!(lut[0], [0x00, 0x00, 0x4c]);
        assert_eq!(lut[255], [0x80, 0x00, 0x00]);
    }

    #[test]
    fn colormap_interpolates_stops_as_a_straight_srgb_lerp() {
        let cmap = Colormap {
            name: "test".to_string(),
            stops: vec![
                ColorStop {
                    position: 0.0,
                    color: [0, 0, 0],
                },
                ColorStop {
                    position: 1.0,
                    color: [255, 128, 64],
                },
            ],
        };
        assert_eq!(cmap.sample(0.0), [0, 0, 0]);
        assert_eq!(cmap.sample(0.5), [128, 64, 32]);
        assert_eq!(cmap.sample(1.0), [255, 128, 64]);
        // Outside the stop range, clamp to the end colours.
        assert_eq!(cmap.sample(-1.0), [0, 0, 0]);
        assert_eq!(cmap.sample(2.0), [255, 128, 64]);
    }

    #[test]
    fn colormap_validate_rejects_structures_sample_cannot_handle() {
        // The built-ins must pass their own validation.
        Colormap::seismic().validate().unwrap();
        // A single stop is fine: a constant colour, clamped everywhere.
        let constant = Colormap {
            name: "constant".to_string(),
            stops: vec![ColorStop {
                position: 0.5,
                color: [1, 2, 3],
            }],
        };
        constant.validate().unwrap();
        assert_eq!(constant.lut()[0], [1, 2, 3]);

        let empty = Colormap {
            name: "empty".to_string(),
            stops: vec![],
        };
        assert!(empty.validate().unwrap_err().contains("no stops"));

        let out_of_range = Colormap {
            name: "range".to_string(),
            stops: vec![
                ColorStop {
                    position: 0.0,
                    color: [0, 0, 0],
                },
                ColorStop {
                    position: 1.5,
                    color: [255, 255, 255],
                },
            ],
        };
        assert!(out_of_range.validate().unwrap_err().contains("[0, 1]"));

        let unordered = Colormap {
            name: "unordered".to_string(),
            stops: vec![
                ColorStop {
                    position: 0.75,
                    color: [0, 0, 0],
                },
                ColorStop {
                    position: 0.25,
                    color: [255, 255, 255],
                },
            ],
        };
        assert!(unordered.validate().unwrap_err().contains("increasing"));
    }

    #[test]
    fn colormap_cache_key_fragment_covers_the_stops_not_just_the_name() {
        // A hand-written profile file can call any gradient `seismic`, so
        // two profiles with the same name and different stops must not
        // share a key.
        let mut a = Colormap::seismic();
        let b = Colormap::seismic();
        assert_eq!(a.cache_key_fragment(), b.cache_key_fragment());
        a.stops[0].color = [1, 2, 3];
        assert_ne!(a.cache_key_fragment(), b.cache_key_fragment());
    }

    #[test]
    fn colormapped_render_fills_nan_with_the_literal_pad_color() {
        // Never `lut[PAD_VALUE]`: a no-data pixel must not be painted as
        // mid-amplitude white, indistinguishable from a real zero
        // crossing.
        let data = ndarray::array![[f32::NAN, 5.0]];
        let profile = RenderProfile::seismic_profile();
        let lut = Colormap::seismic().lut();
        let image = render_colormapped(&data, &profile, (0.0, 10.0), &lut, [96, 96, 96]);
        assert_eq!(image.get_pixel(0, 0).0, [96, 96, 96]);
        assert_ne!(image.get_pixel(1, 0).0, [96, 96, 96]);
    }

    #[test]
    fn colormapped_render_puts_zero_at_the_white_midpoint() {
        // With symmetric limits the normalized midpoint is white, so a
        // zero-amplitude sample reads as the colormap's midpoint rather
        // than wherever the asymmetric estimate happened to place it.
        let data = ndarray::array![[0.0f32, 5.0, -5.0]];
        let profile = RenderProfile::seismic_profile();
        let lut = Colormap::seismic().lut();
        let image = render_colormapped(&data, &profile, (-5.0, 5.0), &lut, [96, 96, 96]);
        assert_eq!(image.get_pixel(0, 0).0, lut[128]);
        assert_eq!(image.get_pixel(1, 0).0, lut[255]);
        assert_eq!(image.get_pixel(2, 0).0, lut[0]);
    }

    #[test]
    fn rgb_jpeg_and_png_encoding_round_trip_dimensions() {
        let mut image = RgbImage::new(4, 3);
        for (x, y, px) in image.enumerate_pixels_mut() {
            *px = Rgb([(x * 10) as u8, (y * 10) as u8, 0]);
        }
        for format in [ImageFormat::Jpeg { quality: 85 }, ImageFormat::Png] {
            let bytes = encode_rgb(&image, format).unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap();
            assert_eq!(decoded.width(), 4);
            assert_eq!(decoded.height(), 3);
            assert_eq!(decoded.color(), image::ColorType::Rgb8);
        }
    }

    #[test]
    fn grayscale_render_maps_min_max_to_black_white() {
        let data = ndarray::array![[0.0f32, 10.0]];
        let profile = RenderProfile::default_profile();
        let image = render_grayscale(&data, &profile, (0.0, 10.0), 128);
        assert_eq!(image.get_pixel(0, 0).0[0], 0);
        assert_eq!(image.get_pixel(1, 0).0[0], 255);
    }

    #[test]
    fn grayscale_render_clamps_out_of_range_values() {
        let data = ndarray::array![[-100.0f32, 1000.0]];
        let profile = RenderProfile::default_profile();
        let image = render_grayscale(&data, &profile, (0.0, 10.0), 128);
        assert_eq!(image.get_pixel(0, 0).0[0], 0);
        assert_eq!(image.get_pixel(1, 0).0[0], 255);
    }

    #[test]
    fn grayscale_render_fills_nan_with_pad_value() {
        let data = ndarray::array![[f32::NAN, 5.0]];
        let profile = RenderProfile::default_profile();
        let image = render_grayscale(&data, &profile, (0.0, 10.0), 200);
        assert_eq!(image.get_pixel(0, 0).0[0], 200);
        assert_ne!(image.get_pixel(1, 0).0[0], 200);
    }

    #[test]
    fn positive_profile_clips_negative_values_toward_black() {
        // Limits as if estimated from |x| over roughly [0, 10]: a positive
        // value near the high end should land bright, while a negative
        // value of the same magnitude clips fully to black -- the
        // asymmetry that is the entire point of the profile.
        let data = ndarray::array![[-8.0f32, 8.0]];
        let profile = RenderProfile::positive_profile();
        let image = render_grayscale(&data, &profile, (0.0, 10.0), 128);
        assert_eq!(image.get_pixel(0, 0).0[0], 0);
        assert!(image.get_pixel(1, 0).0[0] > 128);
    }

    #[test]
    fn jpeg_and_png_encoding_round_trip_dimensions() {
        let mut image = GrayImage::new(4, 3);
        for (x, y, px) in image.enumerate_pixels_mut() {
            *px = image::Luma([((x + y) * 10) as u8]);
        }
        for format in [ImageFormat::Jpeg { quality: 85 }, ImageFormat::Png] {
            let bytes = encode(&image, format).unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap();
            assert_eq!(decoded.width(), 4);
            assert_eq!(decoded.height(), 3);
        }
    }
}
