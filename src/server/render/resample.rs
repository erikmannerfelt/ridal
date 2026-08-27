//! Area-weighted mean resampling from a source window to an output grid
//! (#118).
//!
//! Not nearest-neighbor: the issue is explicit that nearest-neighbor
//! preserves high-frequency noise poorly for this data, so downsampling
//! must integrate over each output pixel's full source footprint. Ignores
//! NaN values, renormalizes weights over the remaining valid contributions,
//! and produces NaN where a footprint contains no valid values -- which
//! also happens to be exactly the right behavior for footprints that fall
//! partially or fully outside the source extent (an edge or out-of-range
//! chunk), since those samples don't exist any more than a NaN does.

use ndarray::{Array2, ArrayView2};

use super::grid::SourceWindow;
use super::profile::ResamplingMethod;

fn overlap_1d(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

/// Resample `source` over `window` (a possibly non-integer, possibly
/// partially out-of-bounds sub-region of `source`) into an
/// `out_height x out_width` grid, using area-weighted mean.
///
/// `source` is the *complete* source array; `window` selects the region of
/// it to read, in source row/col coordinates. This keeps the function
/// usable both against a fully in-memory array and, once the streaming
/// reader lands, against a source-aligned sub-array the caller has already
/// windowed -- the footprint math is identical either way as long as
/// `window` is expressed in the coordinates of whatever `source` covers.
pub fn resample_area_weighted_mean(
    source: ArrayView2<f32>,
    window: &SourceWindow,
    out_width: usize,
    out_height: usize,
) -> Array2<f32> {
    resample(
        source,
        window,
        out_width,
        out_height,
        ResamplingMethod::Mean,
    )
}

/// Resample `source` over `window` into an `out_height x out_width` grid
/// using `method`.
///
/// [`ResamplingMethod::Peak`] exists because the mean is the wrong
/// reducer for a profile that reads *signed* amplitude asymmetrically.
/// Radar traces oscillate about zero, so averaging a footprint of many
/// source samples largely cancels them out; the `positive` profile then
/// clips the collapsed result below its black level and the whole image
/// goes black. That is exactly what happened to `positive` overviews,
/// which downsample ~5x in each axis. Taking the largest value in the
/// footprint instead keeps "the strongest positive return here", which is
/// what that profile is trying to show in the first place. At a 1:1
/// footprint (a single source sample, i.e. the viewer's own chunks when
/// the raster is not downsampled) `Peak` and `Mean` agree exactly, so
/// this changes nothing about the full-resolution view.
pub fn resample(
    source: ArrayView2<f32>,
    window: &SourceWindow,
    out_width: usize,
    out_height: usize,
    method: ResamplingMethod,
) -> Array2<f32> {
    let mut out = Array2::from_elem((out_height, out_width), f32::NAN);
    if out_width == 0 || out_height == 0 {
        return out;
    }

    let (src_h, src_w) = (source.shape()[0], source.shape()[1]);
    let row_step = (window.row1 - window.row0) / out_height as f64;
    let col_step = (window.col1 - window.col0) / out_width as f64;

    for oy in 0..out_height {
        let r0 = window.row0 + oy as f64 * row_step;
        let r1 = window.row0 + (oy + 1) as f64 * row_step;
        let ir0 = r0.floor().max(0.0) as usize;
        let ir1 = ((r1.ceil().max(0.0)) as usize).min(src_h);
        if ir0 >= ir1 {
            continue;
        }

        for ox in 0..out_width {
            let c0 = window.col0 + ox as f64 * col_step;
            let c1 = window.col0 + (ox + 1) as f64 * col_step;
            let ic0 = c0.floor().max(0.0) as usize;
            let ic1 = ((c1.ceil().max(0.0)) as usize).min(src_w);
            if ic0 >= ic1 {
                continue;
            }

            let mut weighted_sum = 0.0_f64;
            let mut weight_sum = 0.0_f64;
            let mut peak = f32::NEG_INFINITY;
            let mut any_valid = false;
            for row in ir0..ir1 {
                let row_w = overlap_1d(row as f64, (row + 1) as f64, r0, r1);
                if row_w <= 0.0 {
                    continue;
                }
                for col in ic0..ic1 {
                    let col_w = overlap_1d(col as f64, (col + 1) as f64, c0, c1);
                    if col_w <= 0.0 {
                        continue;
                    }
                    let v = source[[row, col]];
                    if v.is_finite() {
                        // Peak is deliberately unweighted: a footprint's
                        // strongest return is its strongest return
                        // regardless of what fraction of the pixel it
                        // covers. Weighting would scale peaks down near
                        // footprint edges and reintroduce the very
                        // dimming this method exists to avoid.
                        any_valid = true;
                        peak = peak.max(v);
                        let w = row_w * col_w;
                        weighted_sum += v as f64 * w;
                        weight_sum += w;
                    }
                }
            }

            out[[oy, ox]] = match method {
                ResamplingMethod::Mean if weight_sum > 0.0 => (weighted_sum / weight_sum) as f32,
                ResamplingMethod::Peak if any_valid => peak,
                // No valid contributions: stays NaN, the caller's "no
                // data here" signal (rendered as the pad color).
                _ => f32::NAN,
            };
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::array;

    fn full_window(source: &Array2<f32>) -> SourceWindow {
        SourceWindow {
            row0: 0.0,
            row1: source.shape()[0] as f64,
            col0: 0.0,
            col1: source.shape()[1] as f64,
        }
    }

    #[test]
    fn identity_resampling_reproduces_source_exactly() {
        let source = array![[1.0f32, 2.0, 3.0], [4.0, 5.0, 6.0]];
        let window = full_window(&source);
        let out = resample_area_weighted_mean(source.view(), &window, 3, 2);
        assert_eq!(out, source);
    }

    #[test]
    fn integer_2x2_downsample_averages_exactly() {
        #[rustfmt::skip]
        let source = array![
            [1.0f32, 3.0, 1.0, 3.0],
            [1.0,    3.0, 1.0, 3.0],
            [5.0,    7.0, 5.0, 7.0],
            [5.0,    7.0, 5.0, 7.0],
        ];
        let window = full_window(&source);
        let out = resample_area_weighted_mean(source.view(), &window, 2, 2);
        // Each 2x2 block averages to (1+3+1+3)/4=2 and (5+7+5+7)/4=6.
        assert_eq!(out, array![[2.0f32, 2.0], [6.0, 6.0]]);
    }

    #[test]
    fn nan_pixels_are_excluded_and_weights_renormalized() {
        let source = array![[1.0f32, f32::NAN], [3.0, 5.0]];
        let window = full_window(&source);
        // Downsample the whole 2x2 block to a single pixel: mean of the
        // three valid values (1, 3, 5) = 3, NOT (1+0+3+5)/4.
        let out = resample_area_weighted_mean(source.view(), &window, 1, 1);
        assert!((out[[0, 0]] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn footprint_with_no_valid_values_is_nan() {
        let source = array![[f32::NAN, f32::NAN], [f32::NAN, f32::NAN]];
        let window = full_window(&source);
        let out = resample_area_weighted_mean(source.view(), &window, 1, 1);
        assert!(out[[0, 0]].is_nan());
    }

    #[test]
    fn non_integer_ratio_upsamples_with_shared_source_pixel_weight() {
        // 2x2 -> 3x3 is a non-integer ratio (2/3). Every output pixel must
        // still be finite (full coverage, no gaps), and the corners must
        // equal the corresponding exact source corner value, since a
        // corner output pixel's footprint touches only one source pixel
        // heavily.
        let source = array![[1.0f32, 2.0], [3.0, 4.0]];
        let window = full_window(&source);
        let out = resample_area_weighted_mean(source.view(), &window, 3, 3);
        for v in out.iter() {
            assert!(v.is_finite());
        }
        assert!((out[[0, 0]] - 1.0).abs() < 0.3);
        assert!((out[[2, 2]] - 4.0).abs() < 0.3);
    }

    #[test]
    fn window_partially_outside_source_extent_treated_like_nan() {
        let source = array![[1.0f32, 2.0], [3.0, 4.0]];
        // Window extends one row/col beyond the 2x2 source -- as an edge
        // chunk's window would when the raster doesn't divide evenly.
        let window = SourceWindow {
            row0: 0.0,
            row1: 3.0,
            col0: 0.0,
            col1: 3.0,
        };
        let out = resample_area_weighted_mean(source.view(), &window, 3, 3);
        // The bottom-right output pixel's footprint is entirely outside
        // the source (rows/cols 2..3 don't exist) -> NaN.
        assert!(out[[2, 2]].is_nan());
        // The top-left pixel is entirely inside -> finite, close to source[0,0].
        assert!(out[[0, 0]].is_finite());
    }

    #[test]
    fn zero_sized_output_does_not_panic() {
        let source = array![[1.0f32]];
        let window = full_window(&source);
        let out = resample_area_weighted_mean(source.view(), &window, 0, 0);
        assert_eq!(out.shape(), &[0, 0]);
    }

    #[test]
    fn asymmetric_source_is_not_transposed() {
        // 1 row x 3 cols: averaging down to 1x1 must reflect all three
        // columns, not silently read only one row/col due to a swapped
        // index somewhere.
        let source = array![[10.0f32, 20.0, 30.0]];
        let window = full_window(&source);
        let out = resample_area_weighted_mean(source.view(), &window, 1, 1);
        assert!((out[[0, 0]] - 20.0).abs() < 1e-6);
    }

    #[test]
    fn peak_survives_the_cancellation_that_collapses_the_mean() {
        // An oscillating trace, exactly the shape of real radar data: the
        // mean of a large footprint collapses to ~0, which the `positive`
        // profile's black level then clips away entirely. Peak keeps the
        // strongest positive return, which is what that profile is for.
        let source = array![[5.0f32, -5.0, 4.0, -4.0, 6.0, -6.0, 3.0, -3.0]];
        let window = full_window(&source);

        let mean = resample(source.view(), &window, 1, 1, ResamplingMethod::Mean);
        let peak = resample(source.view(), &window, 1, 1, ResamplingMethod::Peak);

        assert!(mean[[0, 0]].abs() < 0.5, "mean collapsed: {}", mean[[0, 0]]);
        assert!((peak[[0, 0]] - 6.0).abs() < 1e-6, "peak: {}", peak[[0, 0]]);
    }

    #[test]
    fn peak_and_mean_agree_at_a_one_to_one_footprint() {
        // The viewer's own chunks resample 1:1 whenever the raster is not
        // downsampled, so switching the `positive` profile to Peak must
        // leave the full-resolution view untouched.
        let source = array![[5.0f32, -5.0, 4.0], [-4.0, 6.0, -6.0]];
        let window = full_window(&source);

        let mean = resample(source.view(), &window, 3, 2, ResamplingMethod::Mean);
        let peak = resample(source.view(), &window, 3, 2, ResamplingMethod::Peak);
        for (m, p) in mean.iter().zip(peak.iter()) {
            assert!((m - p).abs() < 1e-6, "{m} vs {p}");
        }
    }

    #[test]
    fn peak_reports_no_data_for_an_all_nan_footprint() {
        let source = array![[f32::NAN, f32::NAN]];
        let window = full_window(&source);
        let out = resample(source.view(), &window, 1, 1, ResamplingMethod::Peak);
        assert!(out[[0, 0]].is_nan());
    }
}
