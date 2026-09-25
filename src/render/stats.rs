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
use super::profile::{AmplitudeTransform, RenderProfile, SourceTransform};
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
/// `siglog(raw)` rather than of raw amplitude, and `siglog_minval_log10`
/// must be the same strength the renderer will use
/// (`RenderProfile::siglog_minval_log10`). It is ignored for
/// `SourceTransform::None`.
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

// Eight positional parameters, each a scalar the caller already has to
// hand (a profile's fields, or a test's literals). A parameter struct
// would only relocate the verbosity, and the two adjacent transform
// arguments are the pair `to_source_domain` takes.
#[allow(clippy::too_many_arguments)]
pub fn sampled_amplitude_limits(
    reader: &impl AmplitudeSource,
    source_transform: SourceTransform,
    siglog_minval_log10: f32,
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
        .map(|v| {
            to_stats_domain(
                to_source_domain(v, source_transform, siglog_minval_log10),
                transform,
            )
        })
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

/// Estimate a radargram's noise floor in log10 amplitude units, for
/// [`SourceTransform::AdaptiveSigLog`], from the same fixed-seed trace
/// runs [`sampled_amplitude_limits`] reads.
///
/// Whole traces, always: unlike the limits, the noise floor is a property
/// of the data rather than of a profile, so a profile's
/// `stats_skip_first_samples` does not apply, and every `siglog-*` profile
/// of one radargram (and the `adaptive_siglog` processing step, within
/// sampling error) resolves the same noise floor.
pub fn sampled_noise_floor_log10(reader: &impl AmplitudeSource, seed: u64) -> Result<f32, String> {
    let n_traces = reader.shape().1;
    if n_traces == 0 {
        return Err("cannot estimate a noise floor: radargram has zero traces".to_string());
    }
    let step = (n_traces / N_RUNS).max(1);
    let offset = (seed as usize) % step;
    let samples = reader.sample_trace_runs(N_RUNS, TRACES_PER_RUN, offset, 0)?;
    crate::filters::siglog::noise_floor_log10(samples).ok_or_else(|| {
        "cannot estimate a noise floor for adaptive siglog: every sampled amplitude is zero or \
         missing"
            .to_string()
    })
}

/// The profile to actually render with: an
/// [`AdaptiveSigLog`](SourceTransform::AdaptiveSigLog) profile pinned to
/// this radargram's noise floor
/// ([`RenderProfile::pin_siglog_strength`]), or any other profile
/// unchanged, without reading the source.
///
/// Every render entry point calls this before estimating limits, and
/// passes the result both to [`sampled_amplitude_limits`] and to the
/// renderer, so the two agree on the strength.
pub fn pin_profile(
    reader: &impl AmplitudeSource,
    profile: &RenderProfile,
    seed: u64,
) -> Result<RenderProfile, String> {
    if profile.source_transform != SourceTransform::AdaptiveSigLog {
        return Ok(profile.clone());
    }
    Ok(profile.pin_siglog_strength(sampled_noise_floor_log10(reader, seed)?))
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
        crate::filters::siglog::siglog(
            &mut transformed,
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
        );

        let raw_source = crate::source::ArraySource::new(raw.view());
        let transformed_source = crate::source::ArraySource::new(transformed.view());

        let from_raw = sampled_amplitude_limits(
            &raw_source,
            SourceTransform::SigLog,
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
    fn a_custom_siglog_strength_changes_the_estimated_limits() {
        // The strength the renderer applies must be the strength the
        // sampler estimates with, or `siglog-seismic`'s chunks and its
        // overview would normalize differently. No NetCDF.
        let raw = ndarray::Array2::from_shape_fn((10, 200), |(r, c)| {
            let v = (r as f32 * 0.7 + c as f32 * 0.13).sin() * 100.0 + 10.0;
            if (r + c) % 17 == 0 {
                -v
            } else {
                v
            }
        });
        let raw_source = crate::source::ArraySource::new(raw.view());
        let limits = |strength| {
            sampled_amplitude_limits(
                &raw_source,
                SourceTransform::SigLog,
                strength,
                AmplitudeTransform::Linear,
                42,
                0.01,
                0.99,
                0,
            )
            .unwrap()
        };
        let (low_default, high_default) =
            limits(crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10);
        let (low_strong, high_strong) = limits(1.0);
        assert_ne!((low_default, high_default), (low_strong, high_strong));
        // A higher strength truncates more, so the range only shrinks.
        assert!(
            low_strong >= low_default - 1e-6,
            "{low_strong} < {low_default}"
        );
        assert!(
            high_strong <= high_default + 1e-6,
            "{high_strong} > {high_default}"
        );
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
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
            crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10,
            AmplitudeTransform::Linear,
            0,
            0.01,
            0.99,
            5,
        )
        .unwrap();
        assert!(high < 10.0, "high={high} should exclude the skipped spike");
    }

    /// Deterministic noise of magnitude ~`scale` with a strong reflector
    /// band, so the noise floor is well defined and signal is a minority.
    fn noisy_radargram(height: usize, width: usize, scale: f32) -> ndarray::Array2<f32> {
        ndarray::Array2::from_shape_fn((height, width), |(row, col)| {
            let hash = (row * 7919 + col * 104_729) % 1000;
            let noise = (hash as f32 / 500.0 - 1.0) * scale;
            let bed = if row.abs_diff(height * 2 / 3) < 3 {
                400.0 * scale
            } else {
                0.0
            };
            noise + bed
        })
    }

    #[test]
    fn the_sampled_noise_floor_tracks_the_whole_array() {
        // The render path estimates from sampled trace runs; the processing
        // step from the whole array. They must agree closely or
        // `siglog-*` stops being a preview of `adaptive_siglog`.
        let data = noisy_radargram(300, 4000, 50.0);
        let source = crate::source::ArraySource::new(data.view());
        let sampled = sampled_noise_floor_log10(&source, SAMPLE_SEED).unwrap();
        let whole = crate::filters::siglog::noise_floor_log10(data.iter().copied()).unwrap();
        assert!(
            (sampled - whole).abs() < 0.02,
            "sampled {sampled} vs whole-array {whole}"
        );
    }

    #[test]
    fn pin_profile_resolves_only_adaptive_profiles() {
        let data = noisy_radargram(100, 500, 50.0);
        let source = crate::source::ArraySource::new(data.view());
        let noise = sampled_noise_floor_log10(&source, SAMPLE_SEED).unwrap();

        let adaptive = RenderProfile::siglog_seismic_profile();
        let pinned = pin_profile(&source, &adaptive, SAMPLE_SEED).unwrap();
        assert_eq!(pinned, adaptive.pin_siglog_strength(noise));

        for profile in [
            RenderProfile::default_profile(),
            RenderProfile::seismic_profile(),
        ] {
            assert_eq!(
                pin_profile(&source, &profile, SAMPLE_SEED).unwrap(),
                profile
            );
        }
    }

    #[test]
    fn a_radargram_without_a_noise_floor_is_a_clear_error() {
        let data = ndarray::Array2::<f32>::zeros((20, 30));
        let source = crate::source::ArraySource::new(data.view());
        let error = pin_profile(
            &source,
            &RenderProfile::siglog_default_profile(),
            SAMPLE_SEED,
        )
        .unwrap_err();
        assert!(error.contains("noise floor"), "{error}");
    }
}
