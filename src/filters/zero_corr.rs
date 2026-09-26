//! Finding time zero: where the direct wave starts, in the stack or in each
//! trace.
//!
//! Every method except `legacy` works in the same place: a window around
//! the direct wave, located once for the whole radargram. That keeps a
//! strong later reflection, or a trace whose largest lobe is not the first
//! one, from being taken for the direct wave, and it gives the pickers that
//! need one an estimate of the noise before the pulse.
//!
//! None of the pickers depend on the amplitude scale. The noise statistics
//! scale with the data, the AIC is a difference of log-variances, and the
//! energy ratio is a ratio; so gain or a different digitiser does not move
//! the pick.

use std::fmt;
use std::str::FromStr;

use ndarray::{Array2, ArrayView1, Axis};
use rayon::prelude::*;

/// How to pick time zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The first sample where the mean trace jumps by more than half its
    /// standard deviation. The pre-0.7 `zero_corr`, kept as it was.
    Legacy,
    /// The largest absolute value near the direct wave.
    MaxPeak,
    /// The first sample that leaves the pre-signal noise by `sigma` standard
    /// deviations.
    FirstBreak,
    /// The minimum of the Akaike information criterion: the split point
    /// that best divides the record into noise and signal (Maeda, 1985).
    Aic,
    /// The steepest rise of the smoothed energy ratio (modified Coppens;
    /// Sabbione & Velis, 2010).
    Coppens,
}

impl Method {
    const ALL: [(Method, &'static str); 5] = [
        (Method::Legacy, "legacy"),
        (Method::MaxPeak, "max_peak"),
        (Method::FirstBreak, "first_break"),
        (Method::Aic, "aic"),
        (Method::Coppens, "coppens"),
    ];

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(m, _)| *m == self)
            .map(|(_, n)| *n)
            .unwrap_or_default()
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Method {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some((method, _)) = Self::ALL.iter().find(|(_, n)| *n == s) {
            return Ok(*method);
        }
        // `zero_corr(0.9)` was the pre-0.7 way to set the threshold.
        if s.parse::<f32>().is_ok() {
            return Err(format!(
                "the first argument is now the method; for the old threshold \
                 multiplier, use `zero_corr(legacy, factor={s})`"
            ));
        }
        let names: Vec<&str> = Self::ALL.iter().map(|(_, n)| *n).collect();
        Err(format!("expected one of {}", names.join(", ")))
    }
}

/// Whether one time zero is picked for the whole radargram or one per
/// trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Trace,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Scope::Global => "global",
            Scope::Trace => "trace",
        })
    }
}

impl FromStr for Scope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "global" => Ok(Scope::Global),
            "trace" => Ok(Scope::Trace),
            _ => Err("expected `global` or `trace`".into()),
        }
    }
}

/// Fewest noise samples the noise-based pickers accept.
const MIN_NOISE_SAMPLES: usize = 4;

/// Traces on each side that a per-trace pick is compared with.
const OUTLIER_HALF_WINDOW: usize = 25;

/// Where the direct wave sits. Shared by every picker except `legacy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Samples before this are pre-signal noise (exclusive end).
    pub noise_end: usize,
    /// The direct wave's first strong lobe, median over the traces.
    pub peak: usize,
    /// Half the dominant period, in samples: from the direct wave's
    /// largest lobe to the opposite one after it.
    pub half_period: usize,
    /// Pickers look for a trace's direct wave no later than this
    /// (exclusive).
    pub end: usize,
}

/// The result of a pick: how many samples to remove from the top of each
/// trace.
#[derive(Debug, Clone, PartialEq)]
pub struct Picks {
    /// One per trace. Equal throughout for [`Scope::Global`].
    pub samples: Vec<usize>,
    /// Per-trace picks that disagreed with their neighbours, or failed, and
    /// were replaced by the neighbours' median.
    pub replaced: usize,
    pub window: Option<Window>,
}

/// Pick time zero with any method but `legacy`, which also changes the
/// data and so lives with the rest of [`crate::gpr::GPR`].
///
/// `data` is `(samples, traces)`. `Ok(None)` when every trace is flat:
/// there is no signal whose time zero could be wrong.
pub fn pick(
    data: &Array2<f32>,
    method: Method,
    scope: Scope,
    sigma: f32,
) -> Result<Option<Picks>, String> {
    if method == Method::Legacy {
        return Err("the legacy method is not a picker".into());
    }
    let stack = data
        .mean_axis(Axis(1))
        .ok_or("cannot pick time zero in a radargram without traces")?;
    let Some(window) = find_window(data)? else {
        return Ok(None);
    };
    if matches!(method, Method::FirstBreak | Method::Coppens)
        && window.noise_end < MIN_NOISE_SAMPLES
    {
        return Err(format!(
            "`{method}` needs at least {MIN_NOISE_SAMPLES} samples of noise before the direct \
             wave, but it starts {} samples in; try `aic` or `max_peak`",
            window.noise_end
        ));
    }

    match scope {
        Scope::Global => {
            let at = pick_trace(stack.view(), method, &window, sigma)
                .ok_or_else(|| format!("`{method}` found no direct wave in the mean trace"))?;
            Ok(Some(Picks {
                samples: vec![at; data.shape()[1]],
                replaced: 0,
                window: Some(window),
            }))
        }
        Scope::Trace => {
            let traces: Vec<ArrayView1<f32>> = data.columns().into_iter().collect();
            let raw: Vec<Option<usize>> = traces
                .par_iter()
                .map(|trace| pick_trace(*trace, method, &window, sigma))
                .collect();
            let tolerance = (window.half_period as f32 / 2.).max(1.);
            let (samples, replaced) = replace_outliers(&raw, OUTLIER_HALF_WINDOW, tolerance)
                .ok_or_else(|| format!("`{method}` found no direct wave in any trace"))?;
            Ok(Some(Picks {
                samples,
                replaced,
                window: Some(window),
            }))
        }
    }
}

/// Locate the direct wave from every trace's own first strong lobe.
///
/// Per trace rather than from the stack, because a direct wave that
/// wanders between traces smears out in the stack, and a later reflector
/// that does not wander can then outweigh it. Percentiles over the traces
/// make the window wide enough for the wander without letting a few odd
/// traces stretch it. `Ok(None)` when every trace is flat.
pub fn find_window(data: &Array2<f32>) -> Result<Option<Window>, String> {
    let n = data.shape()[0];
    if n < 5 {
        return Err("too few samples to pick time zero".into());
    }
    let traces: Vec<ArrayView1<f32>> = data.columns().into_iter().collect();
    let features: Vec<(usize, usize, usize)> = traces
        .par_iter()
        .filter_map(|trace| direct_wave(*trace))
        .collect();
    if features.is_empty() {
        return Ok(None);
    }
    let column = |f: fn(&(usize, usize, usize)) -> usize| -> Vec<f32> {
        features.iter().map(|x| f(x) as f32).collect()
    };
    let onsets = column(|x| x.0);
    let peaks = column(|x| x.1);
    let half_period = (median(column(|x| x.2)).round() as usize).max(1);
    let peak = median(peaks.clone()).round() as usize;
    let earliest_onset = percentile(onsets, 0.05).round() as usize;
    let latest_peak = percentile(peaks, 0.95).round() as usize;
    Ok(Some(Window {
        noise_end: earliest_onset.saturating_sub(half_period).min(peak),
        peak,
        half_period,
        end: (latest_peak + 2 * half_period + 1).min(n),
    }))
}

/// A trace's direct wave: its rough onset, its first lobe to reach half the
/// trace's maximum, and half its period. `None` for a flat trace.
///
/// The first lobe to reach half the maximum, not the maximum itself: a
/// strong surface or bed return later in the record must not win.
fn direct_wave(trace: ArrayView1<f32>) -> Option<(usize, usize, usize)> {
    let n = trace.len();
    // Late samples are mostly near the DC level, so the median finds it
    // regardless of how strong the direct wave is.
    let dc = median(trace.iter().copied().collect());
    let centred = trace.mapv(|v| v - dc);
    let max = centred.fold(0_f32, |a, &b| a.max(b.abs()));
    if max <= 0. || !max.is_finite() {
        return None;
    }
    let first = centred.iter().position(|&v| v.abs() >= 0.5 * max)?;
    let mut peak = first;
    while peak + 1 < n && centred[peak + 1].abs() > centred[peak].abs() {
        peak += 1;
    }

    // Half a period: the distance to the largest opposite-signed value
    // before the sign flips back.
    let positive = centred[peak] > 0.;
    let mut opposite = peak;
    let mut i = peak + 1;
    while i < n && (centred[i] > 0.) == positive {
        i += 1;
    }
    while i < n && (centred[i] > 0.) != positive {
        if opposite == peak || centred[i].abs() > centred[opposite].abs() {
            opposite = i;
        }
        i += 1;
    }
    let half_period = (opposite - peak).max(1);
    let onset = aic_onset(centred.slice(ndarray::s![..(peak + half_period + 1).min(n)]))
        .unwrap_or(peak)
        .min(peak);
    Some((onset, peak, half_period))
}

/// Pick one trace, or the stack. `None` when the method finds nothing.
pub fn pick_trace(
    trace: ArrayView1<f32>,
    method: Method,
    window: &Window,
    sigma: f32,
) -> Option<usize> {
    let n = trace.len();
    let noise = trace.slice(ndarray::s![..window.noise_end.min(n)]);
    let (mean, std) = if noise.len() >= MIN_NOISE_SAMPLES {
        (noise.mean().unwrap_or(0.), noise.std(1.))
    } else {
        (median(trace.iter().copied().collect()), 0.)
    };
    let centred = trace.mapv(|v| v - mean);

    // This trace's own direct-wave peak. `max_by` would return the last of
    // equal values; the first sample of a clipped plateau is the one wanted.
    let start = window.noise_end.min(n);
    let end = window.end.min(n);
    let mut peak = start;
    for i in start..end {
        if centred[i].abs() > centred[peak].abs() {
            peak = i;
        }
    }
    let segment_end = (peak + window.half_period + 1).min(n);

    match method {
        Method::Legacy => None,
        Method::MaxPeak => Some(peak),
        Method::FirstBreak => {
            let threshold = sigma * std;
            (start..=peak).find(|&i| centred[i].abs() > threshold)
        }
        Method::Aic => aic_onset(centred.slice(ndarray::s![..segment_end])),
        Method::Coppens => {
            // Half a period: a longer window moves the steepest rise of
            // the ratio from the onset towards the energy peak.
            let w = window.half_period.max(2);
            // Stabilises the ratio in the noise, where both energies are
            // small; proportional to the noise energy of one window.
            let beta = (w as f32 * std * std).max(f32::MIN_POSITIVE);
            coppens_onset(centred.slice(ndarray::s![..segment_end]), w, beta)
        }
    }
}

/// The first sample of the second segment in the split that minimises
///
/// `AIC(k) = k ln var(x[..k]) + (n - k - 1) ln var(x[k..])`
///
/// Scale-free: multiplying `x` adds the same constant to every `k`.
pub fn aic_onset(x: ArrayView1<f32>) -> Option<usize> {
    let n = x.len();
    if n < 5 {
        return None;
    }
    let x: Vec<f64> = x.iter().map(|&v| v as f64).collect();
    let mut sum = vec![0_f64; n + 1];
    let mut sum2 = vec![0_f64; n + 1];
    for i in 0..n {
        sum[i + 1] = sum[i] + x[i];
        sum2[i + 1] = sum2[i] + x[i] * x[i];
    }
    let variance = |a: usize, b: usize| {
        let len = (b - a) as f64;
        let mean = (sum[b] - sum[a]) / len;
        ((sum2[b] - sum2[a]) / len - mean * mean).max(0.)
    };
    // A floor relative to the whole segment: constant (e.g. zero-padded)
    // noise would otherwise be ln 0, and a fixed floor would not scale.
    let floor = (variance(0, n) * 1e-12).max(f64::MIN_POSITIVE);
    (2..n - 1)
        .map(|k| {
            let aic = k as f64 * (variance(0, k) + floor).ln()
                + (n - k - 1) as f64 * (variance(k, n) + floor).ln();
            (k, aic)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k)
}

/// The steepest rise of the edge-preserving-smoothed energy ratio
/// `E1 / (E2 + beta)`, with `E1` the energy of the last `w` samples and
/// `E2` the energy since the start.
pub fn coppens_onset(x: ArrayView1<f32>, w: usize, beta: f32) -> Option<usize> {
    let n = x.len();
    if n < w + 2 {
        return None;
    }
    let energy: Vec<f64> = x.iter().map(|&v| (v as f64).powi(2)).collect();
    let mut cumulative = vec![0_f64; n + 1];
    for i in 0..n {
        cumulative[i + 1] = cumulative[i] + energy[i];
    }
    let ratio: Vec<f64> = (0..n)
        .map(|i| {
            let e1 = cumulative[i + 1] - cumulative[(i + 1).saturating_sub(w)];
            let e2 = cumulative[i + 1];
            e1 / (e2 + beta as f64)
        })
        .collect();
    let smooth = edge_preserving_smooth(&ratio, w);
    (1..n)
        .map(|i| (i, smooth[i] - smooth[i - 1]))
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|(i, _)| i)
}

/// For each sample, the mean of the least variable `w`-long window that
/// contains it. Smooths noise without rounding off a step.
fn edge_preserving_smooth(x: &[f64], w: usize) -> Vec<f64> {
    let n = x.len();
    let w = w.min(n).max(1);
    let stats: Vec<(f64, f64)> = (0..=n - w)
        .map(|start| {
            let window = &x[start..start + w];
            let mean = window.iter().sum::<f64>() / w as f64;
            let var = window.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / w as f64;
            (mean, var)
        })
        .collect();
    (0..n)
        .map(|i| {
            let first = i.saturating_sub(w - 1);
            let last = i.min(n - w);
            (first..=last)
                .map(|s| stats[s])
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(mean, _)| mean)
                .unwrap_or(x[i])
        })
        .collect()
}

/// Replace per-trace picks that are missing or further than `tolerance`
/// samples (or three robust standard deviations, if more) from the median
/// of the `half_window` traces on each side.
///
/// Returns the picks and how many were replaced, or `None` if every pick
/// is missing. Only the outliers change: smoothing every pick would erase
/// the real trace-to-trace jitter a per-trace correction exists to remove.
pub fn replace_outliers(
    picks: &[Option<usize>],
    half_window: usize,
    tolerance: f32,
) -> Option<(Vec<usize>, usize)> {
    if picks.iter().all(Option::is_none) {
        return None;
    }
    let mut replaced = 0;
    let out = (0..picks.len())
        .map(|i| {
            let neighbours = |half: usize| -> Vec<f32> {
                picks[i.saturating_sub(half)..(i + half + 1).min(picks.len())]
                    .iter()
                    .flatten()
                    .map(|&p| p as f32)
                    .collect()
            };
            // Widen until there is something to compare with; a long run
            // of failed picks still gets a value.
            let mut half = half_window;
            let mut values = neighbours(half);
            while values.is_empty() {
                half *= 2;
                values = neighbours(half);
            }
            let centre = median(values.clone());
            let mad = median(values.iter().map(|v| (v - centre).abs()).collect());
            let limit = (3. * 1.4826 * mad).max(tolerance);
            match picks[i] {
                Some(p) if (p as f32 - centre).abs() <= limit => p,
                _ => {
                    replaced += 1;
                    centre.round() as usize
                }
            }
        })
        .collect();
    Some((out, replaced))
}

/// The `q` quantile (0 to 1) by the nearest rank.
fn percentile(mut values: Vec<f32>, q: f32) -> f32 {
    if values.is_empty() {
        return 0.;
    }
    values.sort_by(f32::total_cmp);
    values[((values.len() - 1) as f32 * q).round() as usize]
}

fn median(mut values: Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.;
    }
    values.sort_by(f32::total_cmp);
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.
    } else {
        values[mid]
    }
}

/// Shift each trace up by its pick and trim the bottom to the shortest
/// result, so that no trace is padded with zeros (#6).
pub fn apply_shifts(data: &Array2<f32>, shifts: &[usize]) -> Array2<f32> {
    let max_shift = shifts.iter().copied().max().unwrap_or(0);
    let height = data.shape()[0].saturating_sub(max_shift);
    let mut out = Array2::<f32>::zeros((height, data.shape()[1]));
    for ((mut column, trace), &shift) in out
        .columns_mut()
        .into_iter()
        .zip(data.columns())
        .zip(shifts)
    {
        column.assign(&trace.slice(ndarray::s![shift..shift + height]));
    }
    out
}

/// A synthetic trace: weak noise, then a decaying sine starting at
/// `onset`. For tests.
#[cfg(test)]
fn synthetic_trace(
    n: usize,
    onset: usize,
    amplitude: f32,
    noise: f32,
    seed: u64,
) -> ndarray::Array1<f32> {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    let mut rand = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) as f32 / (1u64 << 31) as f32) - 0.5
    };
    ndarray::Array1::from_iter((0..n).map(|i| {
        let t = i as f32 - onset as f32;
        let pulse = if t < 0. {
            0.
        } else {
            // One and a half cycles of a 12-sample period, decaying.
            let phase = t / 12. * std::f32::consts::TAU;
            -(phase.sin()) * (-t / 18.).exp()
        };
        amplitude * pulse + noise * rand()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::Array2;

    fn radargram(onsets: &[usize], amplitude: f32) -> Array2<f32> {
        let n = 200;
        let mut data = Array2::<f32>::zeros((n, onsets.len()));
        for (j, &onset) in onsets.iter().enumerate() {
            data.column_mut(j).assign(&synthetic_trace(
                n,
                onset,
                amplitude,
                amplitude * 0.01,
                j as u64,
            ));
        }
        // A DC offset, like the 25 MHz asset has.
        data += 3. * amplitude;
        data
    }

    #[test]
    fn onset_pickers_find_the_synthetic_onset() {
        let data = radargram(&[40; 20], 1000.);
        for method in [Method::Aic, Method::FirstBreak, Method::Coppens] {
            let picks = pick(&data, method, Scope::Trace, 5.).unwrap().unwrap();
            for &p in &picks.samples {
                assert!((39..=42).contains(&p), "{method}: picked {p}, onset 40");
            }
        }
        // The peak of a sine starting at 40 is a quarter period later.
        let peaks = pick(&data, Method::MaxPeak, Scope::Global, 5.)
            .unwrap()
            .unwrap();
        assert!((42..=46).contains(&peaks.samples[0]), "{:?}", peaks.samples);
    }

    #[test]
    fn scaling_or_clipping_does_not_move_the_pick() {
        let onsets: Vec<usize> = (0..30).map(|i| 40 + i % 4).collect();
        let data = radargram(&onsets, 1.);
        let scaled = data.mapv(|v| v * 1000.);
        let clipped = data.mapv(|v| v.clamp(2.7, 3.3));
        for method in [Method::Aic, Method::FirstBreak, Method::Coppens] {
            let base = pick(&data, method, Scope::Trace, 5.)
                .unwrap()
                .unwrap()
                .samples;
            assert_eq!(
                base,
                pick(&scaled, method, Scope::Trace, 5.)
                    .unwrap()
                    .unwrap()
                    .samples,
                "{method} moved when scaled"
            );
            let clip = pick(&clipped, method, Scope::Trace, 5.)
                .unwrap()
                .unwrap()
                .samples;
            for (a, b) in base.iter().zip(&clip) {
                assert!(
                    a.abs_diff(*b) <= 1,
                    "{method} moved from {a} to {b} when clipped"
                );
            }
        }
    }

    #[test]
    fn per_trace_picks_follow_the_traces() {
        let onsets: Vec<usize> = (0..30).map(|i| 40 + i % 4).collect();
        let data = radargram(&onsets, 1000.);
        let picks = pick(&data, Method::Aic, Scope::Trace, 5.).unwrap().unwrap();
        for (p, o) in picks.samples.iter().zip(&onsets) {
            assert!(p.abs_diff(*o) <= 1, "picked {p}, onset {o}");
        }
        assert_eq!(picks.replaced, 0);
    }

    #[test]
    fn a_later_stronger_reflection_is_not_the_direct_wave() {
        // A surface return half again as strong as the direct wave, as from
        // an antenna carried above a bright surface. More than twice as
        // strong would win: the window starts at the first lobe to reach
        // half the maximum.
        let mut data = radargram(&[40; 10], 1000.);
        for (j, mut column) in data.columns_mut().into_iter().enumerate() {
            let echo = synthetic_trace(200, 120, 1500., 0., j as u64);
            column += &echo;
        }
        for method in [
            Method::MaxPeak,
            Method::Aic,
            Method::FirstBreak,
            Method::Coppens,
        ] {
            for scope in [Scope::Global, Scope::Trace] {
                let picks = pick(&data, method, scope, 5.).unwrap().unwrap();
                assert!(
                    picks.samples.iter().all(|&p| p < 60),
                    "{method}, {scope}: {:?}",
                    picks.samples
                );
            }
        }
    }

    #[test]
    fn outliers_are_replaced_and_jitter_is_kept() {
        let mut picks: Vec<Option<usize>> = (0..40).map(|i| Some(40 + i % 2)).collect();
        picks[10] = Some(70);
        picks[20] = None;
        let (out, replaced) = replace_outliers(&picks, 5, 2.).unwrap();
        assert_eq!(replaced, 2);
        assert!((40..=41).contains(&out[10]));
        assert!((40..=41).contains(&out[20]));
        // The one-sample jitter survives.
        assert_eq!(out[0..4], [40, 41, 40, 41]);
        assert!(replace_outliers(&[None, None], 5, 2.).is_none());
    }

    #[test]
    fn no_trace_is_zero_padded_after_a_per_trace_shift() {
        // #6: the old per-trace correction padded the bottom with zeros.
        let data = Array2::from_shape_fn((10, 3), |(i, j)| (i + 1 + j) as f32);
        let out = apply_shifts(&data, &[0, 2, 1]);
        assert_eq!(out.shape(), &[8, 3]);
        assert!(out.iter().all(|&v| v != 0.));
        assert_eq!(out[[0, 1]], data[[2, 1]]);
    }

    #[test]
    fn a_flat_radargram_has_nothing_to_pick() {
        let data = Array2::from_elem((50, 4), 7_f32);
        assert_eq!(pick(&data, Method::Aic, Scope::Trace, 5.), Ok(None));
    }

    #[test]
    fn noise_based_methods_refuse_a_record_without_noise() {
        let data = radargram(&[1; 5], 1000.);
        for method in [Method::FirstBreak, Method::Coppens] {
            let err = pick(&data, method, Scope::Global, 5.).unwrap_err();
            assert!(err.contains("samples of noise"), "{err}");
        }
    }

    #[test]
    fn a_threshold_where_the_method_goes_says_how_to_migrate() {
        let err = "0.9".parse::<Method>().unwrap_err();
        assert!(err.contains("zero_corr(legacy, factor=0.9)"), "{err}");
    }
}
