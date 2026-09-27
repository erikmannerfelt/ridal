//! `auto_gain`'s estimate of a display gain (#266).
//!
//! The gain it measures is the exponential that levels the radargram's
//! amplitude envelope below the direct wave. It is **not** an attenuation
//! estimate: that needs bed returns against bed travel time, which belongs
//! with interpretation, not with a processing step.
//!
//! The envelope of real data is not one exponential decay. Across the 138
//! radargrams of the published Svalbard study it drops ~30 dB in the first
//! 200–400 ns as the direct wave rings down, then sits on a plateau a few dB
//! above the deepest noise, and declines slowly from there. A fit that
//! includes the ring-down measures the ring-down (0.03–0.4 dB/ns, which
//! lifts the deep noise by tens of dB), so the ring-down is found and
//! skipped, and the slope is the median of the bin-to-bin changes below it.
//! A median of local slopes, unlike a line fit, is not pulled by the bed
//! reflection or by a bright englacial layer.

use ndarray::{Array2, Axis, Slice};

/// A bin counts as past the ring-down once its envelope is within this many
/// dB of the median envelope from the peak down.
///
/// Chosen on the published study's radargrams: it puts the start at the
/// knee between the steep ring-down and the plateau on all of them.
pub const RINGDOWN_MARGIN_DB: f32 = 6.;

/// Fewer neighbouring-bin pairs than this below the ring-down, and the
/// median slope is not worth trusting.
pub const MIN_BIN_PAIRS: usize = 5;

/// What [`estimate_display_gain`] measured, for the step to apply and log.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainEstimate {
    /// Gain that levels the envelope, in dB per ns of two-way travel time.
    /// Negative when the envelope grows with time.
    pub db_per_ns: f32,
    /// Centre of the first bin below the ring-down (ns from sample 0).
    pub fit_start_ns: f32,
    /// Centre of the last bin used (ns from sample 0).
    pub fit_end_ns: f32,
    /// How many neighbouring-bin slopes the median was taken over.
    pub bin_pairs: usize,
}

/// Measure the gain that levels the median amplitude envelope of `data`.
///
/// # Arguments
/// - `data`: samples × traces.
/// - `step_ns`: time between samples (ns).
/// - `n_bins`: how many bins the samples are split into, top to bottom.
///
/// # Method
/// 1. Each bin's envelope is `20·log10` of the median `|a|` over every
///    sample and trace in it. A median is unaffected by the exact zeros a
///    median `dewow` or `background_removal` leaves, and non-finite samples
///    are left out. A bin whose median is 0 has no level and is skipped.
/// 2. The ring-down ends at the first bin, from the envelope's peak in the
///    upper half down, within [`RINGDOWN_MARGIN_DB`] of the median envelope
///    over that range.
/// 3. The gain is minus the median of the slopes between neighbouring bins
///    from there to the bottom.
///
/// # Errors
/// If `n_bins` is under 2 or above the sample count, or fewer than
/// [`MIN_BIN_PAIRS`] slopes remain below the ring-down.
pub fn estimate_display_gain(
    data: &Array2<f32>,
    step_ns: f32,
    n_bins: usize,
) -> Result<GainEstimate, String> {
    let height = data.nrows();
    if n_bins < 2 || n_bins > height {
        return Err(format!(
            "auto_gain needs between 2 and {height} bins (one per sample at most), got {n_bins}"
        ));
    }
    if !(step_ns.is_finite() && step_ns > 0.) {
        return Err(format!(
            "auto_gain needs a positive sample interval, got {step_ns} ns"
        ));
    }

    // Bin edges spread the remainder over the bins rather than dropping it.
    let edges: Vec<usize> = (0..=n_bins).map(|k| k * height / n_bins).collect();
    let centres_ns: Vec<f32> = edges
        .windows(2)
        .map(|e| (e[0] + e[1] - 1) as f32 / 2. * step_ns)
        .collect();
    let levels_db: Vec<Option<f32>> = edges
        .windows(2)
        .map(|e| {
            let bin = data.slice_axis(Axis(0), Slice::from(e[0]..e[1]));
            let mut magnitudes: Vec<f32> = bin
                .iter()
                .filter(|v| v.is_finite())
                .map(|v| v.abs())
                .collect();
            let level = 20. * median(&mut magnitudes)?.log10();
            level.is_finite().then_some(level)
        })
        .collect();

    // The direct wave is near the top of any record, so its peak is looked
    // for in the upper half only. Otherwise a record whose amplitude grows
    // with time would put the "peak" at the bottom and leave nothing to fit.
    let (peak, _) = levels_db[..n_bins.div_ceil(2)]
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.map(|l| (i, l)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .ok_or(
            "auto_gain found no amplitude to measure: every bin's median in the upper half is zero",
        )?;
    let mut below_peak: Vec<f32> = levels_db[peak..].iter().flatten().copied().collect();
    let plateau = median(&mut below_peak).expect("the peak bin has a level");
    let start = peak
        + levels_db[peak..]
            .iter()
            .position(|l| l.is_some_and(|l| l <= plateau + RINGDOWN_MARGIN_DB))
            .expect("the median is at or above the minimum");

    let mut slopes: Vec<f32> = Vec::new();
    let mut end = start;
    for k in start..n_bins - 1 {
        if let (Some(a), Some(b)) = (levels_db[k], levels_db[k + 1]) {
            slopes.push((b - a) / (centres_ns[k + 1] - centres_ns[k]));
            end = k + 1;
        }
    }
    if slopes.len() < MIN_BIN_PAIRS {
        return Err(format!(
            "auto_gain found {} pairs of neighbouring bins below the direct wave's ring-down \
             (which ends at {:.0} ns), and needs at least {MIN_BIN_PAIRS}. Use more bins, \
             e.g. `auto_gain({})`, or a longer record",
            slopes.len(),
            centres_ns[start],
            n_bins * 2,
        ));
    }
    let bin_pairs = slopes.len();
    let slope = median(&mut slopes).expect("checked non-empty above");

    Ok(GainEstimate {
        db_per_ns: -slope,
        fit_start_ns: centres_ns[start],
        fit_end_ns: centres_ns[end],
        bin_pairs,
    })
}

/// Median of `values`, averaging the middle two of an even count. `None`
/// when empty. Reorders `values`.
fn median(values: &mut [f32]) -> Option<f32> {
    let n = values.len();
    if n == 0 {
        return None;
    }
    let (_, &mut upper, _) = values.select_nth_unstable_by(n / 2, f32::total_cmp);
    if n % 2 == 1 {
        return Some(upper);
    }
    let lower = values[..n / 2]
        .iter()
        .copied()
        .max_by(f32::total_cmp)
        .expect("n >= 2");
    Some((lower + upper) / 2.)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEP_NS: f32 = 2.;
    const HEIGHT: usize = 1000;
    const WIDTH: usize = 60;

    /// Deterministic pseudo-noise in [-1, 1], so the tests need no RNG crate.
    fn noise(i: usize, j: usize) -> f32 {
        let h = (i as u32).wrapping_mul(2_654_435_761) ^ (j as u32).wrapping_mul(2_246_822_519);
        let h = h ^ (h >> 15);
        (h.wrapping_mul(2_654_435_761) >> 8) as f32 / (1u32 << 23) as f32 - 1.
    }

    /// A radargram shaped like the published ones: a direct wave that rings
    /// down 40 dB in 200 ns, then a decay of `decay_db_per_ns` down to a
    /// noise floor `floor_db` below the start of the decay.
    fn synthetic(decay_db_per_ns: f32, floor_db: f32) -> Array2<f32> {
        Array2::from_shape_fn((HEIGHT, WIDTH), |(i, j)| {
            let t = i as f32 * STEP_NS;
            let ringdown = 40. * (-t / 50.).exp();
            let signal = -decay_db_per_ns * t;
            let level_db = ringdown + signal.max(-floor_db);
            10f32.powf(level_db / 20.) * noise(i, j)
        })
    }

    #[test]
    fn recovers_decay_and_skips_the_ringdown() {
        let est = estimate_display_gain(&synthetic(0.01, 100.), STEP_NS, 100).unwrap();
        assert!(
            (est.db_per_ns - 0.01).abs() < 0.001,
            "measured {} dB/ns",
            est.db_per_ns
        );
        // The ring-down is over by ~4 e-foldings of 50 ns.
        assert!(
            est.fit_start_ns > 100. && est.fit_start_ns < 400.,
            "{est:?}"
        );
    }

    #[test]
    fn a_noise_floor_does_not_inflate_the_gain() {
        // The decay reaches the floor 1/3 of the way down; below it, the
        // envelope is flat, and so most bin-to-bin slopes are ~0.
        let est = estimate_display_gain(&synthetic(0.03, 20.), STEP_NS, 100).unwrap();
        assert!(
            est.db_per_ns >= 0. && est.db_per_ns < 0.03,
            "measured {} dB/ns",
            est.db_per_ns
        );
    }

    #[test]
    fn pure_noise_gives_about_zero() {
        let data = Array2::from_shape_fn((HEIGHT, WIDTH), |(i, j)| noise(i, j));
        let est = estimate_display_gain(&data, STEP_NS, 100).unwrap();
        assert!(est.db_per_ns.abs() < 0.002, "measured {est:?}");
    }

    #[test]
    fn exact_zeros_and_nan_do_not_break_it() {
        let mut data = synthetic(0.01, 100.);
        // A median background removal zeros a whole trace; a stray NaN
        // must not reach the median.
        data.column_mut(3).fill(0.);
        data[[500, 5]] = f32::NAN;
        // Whole rows of zeros, as a crop or a median dewow can leave.
        data.row_mut(700).fill(0.);
        let est = estimate_display_gain(&data, STEP_NS, 100).unwrap();
        assert!(est.db_per_ns.is_finite());
        assert!((est.db_per_ns - 0.01).abs() < 0.001, "measured {est:?}");
    }

    #[test]
    fn amplitude_growing_with_time_is_negative() {
        let data = Array2::from_shape_fn((HEIGHT, WIDTH), |(i, j)| {
            10f32.powf(0.01 * i as f32 * STEP_NS / 20.) * noise(i, j)
        });
        let est = estimate_display_gain(&data, STEP_NS, 100).unwrap();
        assert!(est.db_per_ns < -0.005, "measured {est:?}");
    }

    #[test]
    fn errors_say_why() {
        let data = synthetic(0.01, 100.);
        let err = estimate_display_gain(&data, STEP_NS, 1).unwrap_err();
        assert!(err.contains("between 2 and"), "{err}");
        let err = estimate_display_gain(&data, STEP_NS, HEIGHT + 1).unwrap_err();
        assert!(err.contains("between 2 and"), "{err}");
        let err = estimate_display_gain(&data, 0., 100).unwrap_err();
        assert!(err.contains("sample interval"), "{err}");

        let zeros = Array2::<f32>::zeros((HEIGHT, WIDTH));
        let err = estimate_display_gain(&zeros, STEP_NS, 100).unwrap_err();
        assert!(err.contains("median in the upper half is zero"), "{err}");

        // Only a ring-down, and a handful of bins: too few left below it.
        let err = estimate_display_gain(&data, STEP_NS, 4).unwrap_err();
        assert!(err.contains("needs at least"), "{err}");
    }

    #[test]
    fn median_matches_definition() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [3., 1., 2.]), Some(2.));
        assert_eq!(median(&mut [4., 1., 3., 2.]), Some(2.5));
    }
}
