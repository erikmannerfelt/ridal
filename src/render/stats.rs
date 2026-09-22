//! Fixed-seed sampled amplitude limits (#119).
//!
//! Sampling whole traces (not scattered pixels, not a row subset) is what
//! makes the source wavelet -- a narrow band of very high amplitude near
//! the top of the radargram, this data's main heteroscedasticity -- show up
//! in the sample at its true share of the data: every trace carries the
//! full vertical structure, so any set of complete traces reproduces the
//! row-wise mixture in correct proportion regardless of which traces are
//! drawn.

use super::colormap::{to_source_domain, to_stats_domain};
use super::profile::{AmplitudeTransform, SourceTransform};
use crate::source::AmplitudeSource;

/// Spread across the profile. 128 well-separated locations is ample for a
/// percentile dominated by vertical (not horizontal) structure; the cost
/// of sampling more is small (~0.2s per radargram, once, cached) if a
/// wider net is ever wanted.
const N_RUNS: usize = 128;
/// Contiguous traces per run -- short enough that a run stays inside a
/// handful of storage chunks (`SourceReader::sample_trace_runs`).
const TRACES_PER_RUN: usize = 16;

/// Estimate `(low, high)` amplitude limits in the *display domain* (i.e.
/// after the same transforms the colormap applies), via fixed-seed sampled
/// percentiles.
///
/// `source_transform` is applied to each sampled source value first, before
/// `transform`, exactly as the renderer applies it before resampling -- so
/// a `SourceTransform::SigLog` profile's limits are percentiles of
/// `siglog(raw)` rather than of raw amplitude.
///
/// `seed` should be derived from the revision ID, not the clock, so limits
/// are reproducible across restarts and identical between the CLI and the
/// server for the same processed file.
///
/// `skip_first_samples` drops that many sample rows from the top of every
/// sampled trace before estimating percentiles -- the `positive` profile's
/// way of excluding the direct-wave band (see
/// `RenderProfile::stats_skip_first_samples`). `0` reproduces the original
/// whole-trace behavior.
/// Seed for the trace sampling below, shared by every caller.
///
/// Fixed, and fixed *once for the whole program*, which is the load-bearing
/// part. The seed chooses which traces the percentile estimate looks at, so
/// two callers with different seeds can disagree about a radargram's
/// amplitude limits and therefore about every pixel they draw from it.
/// `ridal render`, `ridal process --render` and the browser all promise to
/// produce the same picture; a per-caller seed quietly broke that promise
/// in a way that looks like noise rather than like a bug.
///
/// A constant rather than something derived from the file: determinism is
/// the only requirement -- the same input must give the same output across
/// runs and restarts -- and a constant additionally means two profiles of
/// one radargram sample the same traces, so comparing them shows the
/// difference between the profiles and nothing else.
pub const SAMPLE_SEED: u64 = 0x5249_4441_4c00_0001;

pub fn sampled_amplitude_limits(
    reader: &impl AmplitudeSource,
    source_transform: SourceTransform,
    transform: AmplitudeTransform,
    seed: u64,
    low_pct: f32,
    high_pct: f32,
    skip_first_samples: usize,
) -> Result<(f32, f32), String> {
    let n_traces = reader.shape().1;
    if n_traces == 0 {
        return Err("cannot estimate amplitude limits: radargram has zero traces".to_string());
    }
    let step = (n_traces / N_RUNS).max(1);
    let offset = (seed as usize) % step;

    let samples = reader.sample_trace_runs(N_RUNS, TRACES_PER_RUN, offset, skip_first_samples)?;
    let mut transformed: Vec<f32> = samples
        .into_iter()
        .map(|v| to_stats_domain(to_source_domain(v, source_transform), transform))
        .filter(|v| v.is_finite())
        .collect();
    if transformed.is_empty() {
        return Err("no finite amplitude samples available for limit estimation".to_string());
    }
    transformed.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let low = percentile(&transformed, low_pct);
    let high = percentile(&transformed, high_pct);
    Ok((low, high))
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    let idx = (((sorted.len() - 1) as f32) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceReader;

    fn write_test_nc_with(path: &std::path::Path, height: usize, width: usize, values: &[f32]) {
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", height).unwrap();
        file.add_dimension("x", width).unwrap();
        let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        var.put_values(values, ..).unwrap();
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn percentile_limits_bracket_uniform_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        // Every trace is the ramp 0..20 vertically, so any sample of
        // complete traces reproduces the exact same value distribution.
        let height = 20;
        let width = 200;
        let mut values = Vec::with_capacity(height * width);
        for row in 0..height {
            for _ in 0..width {
                values.push(row as f32);
            }
        }
        write_test_nc_with(&path, height, width, &values);
        let reader = SourceReader::open(&path).unwrap();

        let (low, high) = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            0,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        assert!(low >= 0.0 && low < 5.0, "low={low}");
        assert!(high > 15.0 && high <= 19.0, "high={high}");
        assert!(low < high);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn different_seeds_still_agree_closely_on_uniform_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        let height = 10;
        let width = 500;
        let mut values = Vec::with_capacity(height * width);
        for row in 0..height {
            for _ in 0..width {
                values.push(row as f32);
            }
        }
        write_test_nc_with(&path, height, width, &values);
        let reader = SourceReader::open(&path).unwrap();

        let (low_a, high_a) = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            1,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        let (low_b, high_b) = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            999,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        assert!((low_a - low_b).abs() < 1.0);
        assert!((high_a - high_b).abs() < 1.0);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn same_seed_is_fully_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        write_test_nc_with(&path, 5, 300, &vec![1.0; 5 * 300]);
        let reader = SourceReader::open(&path).unwrap();

        let a = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            42,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        let b = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            42,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn abslog_transform_changes_the_estimated_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        let height = 5;
        let width = 300;
        write_test_nc_with(&path, height, width, &vec![100.0f32; height * width]);
        let reader = SourceReader::open(&path).unwrap();

        let (low_lin, high_lin) = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            7,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        let (low_log, high_log) = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::AbsLog,
            7,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        assert!((low_lin - 100.0).abs() < 1e-3);
        assert!((low_log - 2.0).abs() < 1e-3); // log10(100) == 2
        assert_ne!((low_lin, high_lin), (low_log, high_log));
    }

    #[test]
    fn source_siglog_limits_match_limits_on_presigloged_data() {
        // The renderer compresses each source sample before resampling, so
        // limits must be estimated in that same domain -- and must equal
        // what a caller who had run `filters::siglog` on the data first
        // would get. No NetCDF: the two sources are in-memory arrays.
        let raw = ndarray::Array2::from_shape_fn((10, 200), |(r, c)| {
            let v = (r as f32 * 0.7 + c as f32 * 0.13).sin() * 100.0 + 10.0;
            if (r + c) % 17 == 0 {
                -v
            } else {
                v
            }
        });
        let mut transformed = raw.clone();
        crate::filters::siglog(
            &mut transformed,
            crate::filters::DEFAULT_SIGLOG_MINVAL_LOG10,
        );

        let raw_source = crate::source::ArraySource::new(raw.view());
        let transformed_source = crate::source::ArraySource::new(transformed.view());

        let from_raw = sampled_amplitude_limits(
            &raw_source,
            SourceTransform::SigLog,
            AmplitudeTransform::Linear,
            42,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        let from_transformed = sampled_amplitude_limits(
            &transformed_source,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            42,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        assert_eq!(from_raw, from_transformed);

        // And the source transform genuinely changes the estimate versus
        // raw amplitude, so the assertion above is not vacuous.
        let plain = sampled_amplitude_limits(
            &raw_source,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            42,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        assert_ne!(from_raw, plain);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn positive_transform_estimates_limits_from_absolute_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        let height = 5;
        let width = 300;
        // All-negative data: a `Linear` estimate would report both bounds
        // negative, but `Positive` estimates from `|x|`, so both bounds
        // should come back positive.
        write_test_nc_with(&path, height, width, &vec![-100.0f32; height * width]);
        let reader = SourceReader::open(&path).unwrap();

        let (low, high) = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::Positive,
            7,
            0.01,
            0.99,
            0,
        )
        .unwrap();
        assert!((low - 100.0).abs() < 1e-3, "low={low}");
        assert!((high - 100.0).abs() < 1e-3, "high={high}");
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn skip_first_samples_excludes_the_direct_wave_band() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.nc");
        let height = 20;
        let width = 300;
        // A huge-amplitude "direct wave" in the first 5 rows, small
        // amplitude everywhere else -- skipping those rows should keep the
        // estimate near the small values instead of the spike.
        let mut values = Vec::with_capacity(height * width);
        for row in 0..height {
            let v = if row < 5 { 1000.0 } else { 1.0 };
            for _ in 0..width {
                values.push(v);
            }
        }
        write_test_nc_with(&path, height, width, &values);
        let reader = SourceReader::open(&path).unwrap();

        let (_, high) = sampled_amplitude_limits(
            &reader,
            SourceTransform::None,
            AmplitudeTransform::Linear,
            0,
            0.01,
            0.99,
            5,
        )
        .unwrap();
        assert!(high < 10.0, "high={high} should exclude the skipped spike");
    }
}
