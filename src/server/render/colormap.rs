//! Normalization, colormapping and image encoding (#118, #119).
//!
//! Grayscale only in v1 -- every built-in profile in `profile.rs` uses
//! linear or log-transformed amplitude mapped straight to a single
//! channel. A named colormap field is a natural v2 extension but is not
//! implemented here, since nothing in the current profile set needs it.

use image::{ColorType, GrayImage, ImageEncoder};
use ndarray::Array2;

use super::profile::{AmplitudeLimits, ImageFormat, RenderProfile};

/// Transform a resampled amplitude value into the domain that limits and
/// normalization operate in. Applied consistently by both the sampler
/// (`stats.rs`, when estimating percentile limits) and this module (when
/// mapping pixels), so a profile's limits always mean the same thing
/// regardless of who computed them.
///
/// `NaN` (the resampler's "no valid source data in this footprint" signal,
/// #118) passes through unchanged -- it is handled separately, as
/// transparency/pad color, never averaged into anything. A literal
/// infinite value is a data anomaly, not an absence of data, and is
/// replaced with zero per #119 ("non-finite amplitude values are treated
/// as invalid input and replaced with zero for rendering").
pub fn to_display_domain(v: f32, abslog: bool) -> f32 {
    if v.is_nan() {
        return v;
    }
    let v = if v.is_infinite() { 0.0 } else { v };
    if abslog {
        let a = v.abs();
        if a == 0.0 {
            f32::NEG_INFINITY
        } else {
            a.log10()
        }
    } else {
        v
    }
}

/// Resolve a profile's [`AmplitudeLimits`] to concrete `(min, max)` bounds.
///
/// `sampled` supplies the percentile-based estimate from `stats.rs` when
/// the profile calls for one; explicit limits need no sample at all
/// (#119). Both branches reject a degenerate `min >= max` (explicit) or
/// `low == high` (estimated) with an error rather than producing a
/// division-by-zero NaN-everywhere image.
pub fn resolve_limits(
    limits: &AmplitudeLimits,
    sampled: Option<(f32, f32)>,
) -> Result<(f32, f32), String> {
    match *limits {
        AmplitudeLimits::Explicit { min, max } => {
            if min >= max {
                return Err(format!(
                    "invalid explicit amplitude limits: min ({min}) must be < max ({max})"
                ));
            }
            Ok((min, max))
        }
        AmplitudeLimits::Percentile { .. } => {
            let (low, high) = sampled
                .ok_or("percentile amplitude limits requested but no sample was supplied")?;
            if low == high {
                return Err(format!(
                    "degenerate estimated amplitude limits: low == high == {low}"
                ));
            }
            Ok((low, high))
        }
    }
}

/// Map one already-display-domain value to a grayscale byte, or `None` for
/// "no data" (rendered as `pad_value` by the caller).
fn normalize_to_u8(v: f32, min: f32, max: f32) -> Option<u8> {
    if v.is_nan() {
        return None;
    }
    let t = ((v - min) / (max - min)).clamp(0.0, 1.0);
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
            let displayed = to_display_domain(raw, profile.abslog);
            let byte = normalize_to_u8(displayed, limits.0, limits.1).unwrap_or(pad_value);
            image.put_pixel(x as u32, y as u32, image::Luma([byte]));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_domain_linear_passthrough() {
        assert_eq!(to_display_domain(5.0, false), 5.0);
        assert_eq!(to_display_domain(-3.0, false), -3.0);
    }

    #[test]
    fn display_domain_abslog_matches_log10_abs() {
        assert!((to_display_domain(100.0, true) - 2.0).abs() < 1e-6);
        assert!((to_display_domain(-100.0, true) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn display_domain_nan_passes_through_untouched() {
        assert!(to_display_domain(f32::NAN, false).is_nan());
        assert!(to_display_domain(f32::NAN, true).is_nan());
    }

    #[test]
    fn display_domain_infinite_becomes_zero_not_dropped() {
        // Linear: infinite -> 0.0 exactly.
        assert_eq!(to_display_domain(f32::INFINITY, false), 0.0);
        assert_eq!(to_display_domain(f32::NEG_INFINITY, false), 0.0);
    }

    #[test]
    fn resolve_limits_explicit_rejects_min_gte_max() {
        assert!(resolve_limits(&AmplitudeLimits::Explicit { min: 5.0, max: 1.0 }, None).is_err());
        assert!(resolve_limits(&AmplitudeLimits::Explicit { min: 1.0, max: 1.0 }, None).is_err());
        assert!(resolve_limits(&AmplitudeLimits::Explicit { min: 1.0, max: 5.0 }, None).is_ok());
    }

    #[test]
    fn resolve_limits_percentile_requires_sample_and_rejects_degenerate() {
        let limits = AmplitudeLimits::Percentile {
            low: 0.01,
            high: 0.99,
        };
        assert!(resolve_limits(&limits, None).is_err());
        assert!(resolve_limits(&limits, Some((1.0, 1.0))).is_err());
        assert_eq!(
            resolve_limits(&limits, Some((1.0, 5.0))).unwrap(),
            (1.0, 5.0)
        );
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
