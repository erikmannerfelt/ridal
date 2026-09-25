//! The sign-corrected log transform (`siglog`) and its noise-adapted form
//! (`adaptive_siglog`, #254).
//!
//! `siglog` compresses each sample to `(log10|v| - strength).max(0) *
//! sign(v)`. The strength sets what truncates to zero, and a fixed strength
//! only suits data on one amplitude scale: `10^0` is well below the noise
//! of a mV-scale 25 MHz recording but would truncate everything in data
//! recorded or scaled to amplitudes below one. `adaptive_siglog` sets the strength from the data instead, as
//! `log10(noise floor) + offset`, so the same offset gives the same picture
//! on any amplitude scale.

use ndarray::Array;
use num::Float;

/// The `siglog` step's default magnitude offset (`minval_log10`), and the
/// default strength of the render layer's `source_transform = SigLog`.
///
/// Shared so a render profile with a fixed-strength `SigLog` source
/// transform and a `siglog` processing step at its default strength show
/// the same picture.
///
/// `0` means magnitudes below `10^0 == 1` truncate to zero, i.e. the
/// transform is `log10|v|` above one. It is the offset the published
/// processing used (doi:10.31223/X5P19C), and it reads better on
/// mV-scale amplitude than the previous `-1`, which truncated only
/// below 0.1 mV and so compressed almost nothing.
pub const DEFAULT_SIGLOG_MINVAL_LOG10: f32 = 0.0;

/// Default `adaptive_siglog` offset: the strength sits this far (in log10
/// units) below the noise floor [`noise_floor_log10`] estimates.
///
/// `-1.7` keeps grayscale siglog views looking as they did at the fixed
/// [`DEFAULT_SIGLOG_MINVAL_LOG10`] on the 25/100 MHz data it was tuned on:
/// `siglog(0)` sits 1.67 and 1.74 below the noise floor on
/// `dronbreen-20250327-DAT_0066_A1` and `dat_0130_b1`, and on
/// `dronbreen-20220329-DAT_0237_A1` the adaptive `-1.7` is visually
/// indistinguishable from `siglog(0)`. A grayscale ramp shows noise as
/// neutral texture, so it wants a strength well below the noise, where the
/// compression mostly equalizes amplitude. See #254 for the comparison.
pub const DEFAULT_ADAPTIVE_SIGLOG_OFFSET: f32 = -1.7;

/// `adaptive_siglog` offset for a diverging colour ramp (`siglog-seismic`).
///
/// A diverging ramp draws zero as white and magnitude as colour, so noise
/// left above the truncation reads as saturated red/blue speckle rather
/// than texture, and the strength has to sit close to the noise floor.
/// Compared at native resolution across `-0.3 / 0 / +0.3` from a deep-noise
/// estimate, `-0.3` kept the most weak scattering and near-surface layering
/// while leaving the ice white on `dronbreen-20250327-DAT_0066_A1` and
/// `dat_0130_b1`; `dronbreen-20220329-DAT_0237_A1` preferred `0`, keeping a
/// faint haze at `-0.3`; `+0.3` visibly thinned englacial scattering
/// everywhere. The whole-line median this module uses sits ~0.1 below that
/// deep estimate, hence `-0.2` here. See #254.
pub const DIVERGING_ADAPTIVE_SIGLOG_OFFSET: f32 = -0.2;

/// At most this many values feed [`noise_floor_log10`] when a whole
/// radargram is estimated. A median of a regular stride over the array is
/// stable to far better than the 0.1 log10 units that matter here, and it
/// bounds the working copy of a large radargram.
const MAX_NOISE_SAMPLES: usize = 2_000_000;

/// The scalar `siglog` transform: `(log10|v| - minval_log10).max(0) *
/// sign(v)`, the sign-corrected log compression.
///
/// A scalar rather than only the array-wide [`siglog`] below because the
/// renderer applies it a value at a time before resampling, and must keep
/// doing exactly what the processing step does. `NaN` passes through
/// (`NaN`'s sign is `NaN`, so the result stays `NaN`) -- the renderer's
/// "no data" signal must survive the transform.
pub fn siglog_value<T: Float>(v: T, minval_log10: T) -> T {
    (v.abs().log10() - minval_log10).max(T::zero()) * v.signum()
}

pub fn siglog<T: Float, D: ndarray::Dimension>(data: &mut Array<T, D>, minval_log10: T) {
    data.mapv_inplace(|v| siglog_value(v, minval_log10));
}

/// Estimate a radargram's noise floor as the median of `log10|v|`, in
/// log10 amplitude units.
///
/// Every value counts -- no depth window, no assumption about where the
/// bed is. That holds up because noise is most of any recording that runs
/// past the ice it images: signal occupies a minority of samples and spreads
/// over a wide range of magnitudes, while noise piles into one hump, so the
/// median lands just below that hump's peak. On three 25 and 100 MHz lines
/// (`dronbreen-20250327-DAT_0066_A1`, `dat_0130_b1`,
/// `dronbreen-20220329-DAT_0237_A1`) it sat 0.08--0.10 below a median taken
/// only below the bed.
///
/// Zeros and non-finite values are skipped: zeros are padding or values an
/// earlier truncation removed, and neither is noise. `None` when nothing
/// remains. For an even count this is the upper median, which is immaterial
/// at these sample sizes.
pub fn noise_floor_log10<I: IntoIterator<Item = f32>>(values: I) -> Option<f32> {
    let mut logs: Vec<f32> = values
        .into_iter()
        .filter(|v| v.is_finite() && *v != 0.0)
        .map(|v| v.abs().log10())
        .collect();
    if logs.is_empty() {
        return None;
    }
    let middle = logs.len() / 2;
    let (_, median, _) = logs.select_nth_unstable_by(middle, f32::total_cmp);
    Some(*median)
}

/// The strength `adaptive_siglog` uses: `offset` log10 units above (or,
/// usually, below) the noise floor.
pub fn adaptive_strength(noise_floor_log10: f32, offset: f32) -> f32 {
    noise_floor_log10 + offset
}

/// Run [`siglog`] at a strength of `log10(noise floor) + offset`, with the
/// noise floor estimated from `data` itself by [`noise_floor_log10`], and
/// return that strength so it can be logged.
///
/// Large arrays are estimated from a regular stride of at most
/// [`MAX_NOISE_SAMPLES`] values, which keeps the result deterministic.
///
/// # Errors
/// If `data` has no finite non-zero value to estimate a noise floor from.
pub fn adaptive_siglog<D: ndarray::Dimension>(
    data: &mut Array<f32, D>,
    offset: f32,
) -> Result<f32, String> {
    let stride = data.len().div_ceil(MAX_NOISE_SAMPLES).max(1);
    let noise = noise_floor_log10(data.iter().step_by(stride).copied()).ok_or(
        "adaptive_siglog needs at least one finite, non-zero sample to estimate a noise floor from",
    )?;
    let strength = adaptive_strength(noise, offset);
    siglog(data, strength);
    Ok(strength)
}

#[cfg(test)]
mod tests {
    use ndarray::AssignElem;

    #[test]
    fn test_siglog() {
        let arr = ndarray::arr1(&[1000_f32, -1000_f32, 0_f32]);
        let mut arr0 = arr.clone();
        super::siglog(&mut arr0, 0.);
        assert_eq!(arr0, ndarray::arr1(&[3., -3., 0.]));
        let mut arr1 = arr.clone();
        arr1[2].assign_elem(0.0001);
        super::siglog(&mut arr1, 0.);
        assert_eq!(arr1, ndarray::arr1(&[3., -3., 0.]));
        let mut arr2 = arr.clone();
        arr2[2].assign_elem(0.1);
        super::siglog(&mut arr2, -2.);
        assert_eq!(arr2, ndarray::arr1(&[5., -5., 1.]));
    }

    #[test]
    fn noise_floor_is_the_median_log_magnitude_ignoring_sign() {
        // Five noise-level samples of either sign against two strong
        // returns: the median sits on the noise, not the signal.
        let values = [-10.0, 10.0, 10.0, -10.0, 10.0, 1e4, -1e5];
        assert_eq!(super::noise_floor_log10(values), Some(1.0));
    }

    #[test]
    fn noise_floor_skips_zeros_and_non_finite_values() {
        // Padding zeros outnumber the data; counting them would put the
        // median at log10(0) = -inf.
        let values = [
            0.0,
            0.0,
            0.0,
            0.0,
            f32::NAN,
            f32::INFINITY,
            100.0,
            -100.0,
            100.0,
        ];
        assert_eq!(super::noise_floor_log10(values), Some(2.0));
        assert_eq!(super::noise_floor_log10([0.0, f32::NAN]), None);
        assert_eq!(super::noise_floor_log10([]), None);
    }

    #[test]
    fn adaptive_siglog_is_invariant_to_amplitude_scale() {
        // The point of #254: the same recording at a 1000x different
        // amplitude scale must come out identical, which no fixed strength
        // can do.
        let base = ndarray::arr2(&[[5.0_f32, -40.0, 3000.0], [-8.0, 20.0, -2e4]]);
        let mut a = base.clone();
        let mut b = base.mapv(|v| v * 1000.0);
        let strength_a = super::adaptive_siglog(&mut a, -0.2).unwrap();
        let strength_b = super::adaptive_siglog(&mut b, -0.2).unwrap();
        assert!((strength_b - strength_a - 3.0).abs() < 1e-5);
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() < 1e-4, "{x} != {y}");
        }
    }

    #[test]
    fn adaptive_siglog_truncates_relative_to_the_noise_floor() {
        // Noise at 10 (log10 1), offset -0.2: strength 0.8, so a noise
        // sample becomes 0.2 and a 1000 return becomes 2.2, sign kept.
        let mut data = ndarray::arr1(&[10.0_f32, -10.0, 10.0, -1000.0, 1.0]);
        let strength = super::adaptive_siglog(&mut data, -0.2).unwrap();
        assert!((strength - 0.8).abs() < 1e-6);
        let expected = [0.2, -0.2, 0.2, -2.2, 0.0];
        for (got, want) in data.iter().zip(expected) {
            assert!((got - want).abs() < 1e-5, "{got} != {want}");
        }
    }

    #[test]
    fn adaptive_siglog_refuses_data_without_a_noise_floor() {
        let mut data = ndarray::Array2::<f32>::zeros((3, 3));
        assert!(super::adaptive_siglog(&mut data, -1.7).is_err());
    }
}
