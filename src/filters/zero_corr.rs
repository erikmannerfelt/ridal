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
    /// Per trace, then the running median of the picks over
    /// [`Settings::window`] traces: for a time zero that drifts slowly,
    /// where the scatter of single picks is larger than the real
    /// trace-to-trace change.
    Smooth,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Scope::Global => "global",
            Scope::Trace => "trace",
            Scope::Smooth => "smooth",
        })
    }
}

impl FromStr for Scope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "global" => Ok(Scope::Global),
            "trace" => Ok(Scope::Trace),
            "smooth" => Ok(Scope::Smooth),
            _ => Err("expected `global`, `trace` or `smooth`".into()),
        }
    }
}

/// Fewest noise samples the noise-based pickers accept.
const MIN_NOISE_SAMPLES: usize = 4;

/// Traces on each side that a per-trace pick is compared with.
const OUTLIER_HALF_WINDOW: usize = 25;

/// Where the direct wave sits. Shared by every picker except `legacy`.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// The sign of the direct wave's largest value in most traces, `1.` or
    /// `-1.`. The peak is looked for with this sign in every trace, so that
    /// it cannot jump to an opposite lobe of similar size.
    pub polarity: f32,
}

/// How much of the record to keep above time zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Margin {
    /// Back to where the direct wave starts: nothing for the methods that
    /// pick the start, and the start-to-pick distance for `max_peak`, so
    /// that the whole wavelet is kept.
    Auto,
    /// A fixed margin in nanoseconds.
    Ns(f32),
}

impl fmt::Display for Margin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Margin::Auto => f.write_str("auto"),
            Margin::Ns(ns) => write!(f, "{ns}"),
        }
    }
}

impl FromStr for Margin {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "auto" {
            return Ok(Margin::Auto);
        }
        match s.parse::<f32>() {
            Ok(ns) if ns >= 0. && ns.is_finite() => Ok(Margin::Ns(ns)),
            _ => Err("expected `auto` or a non-negative number of nanoseconds".into()),
        }
    }
}

/// Which feature of the direct wave time zero is placed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    /// Where the direct wave starts.
    Onset,
    /// The direct wave's largest value.
    Peak,
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Reference::Onset => "onset",
            Reference::Peak => "peak",
        })
    }
}

impl FromStr for Reference {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "onset" => Ok(Reference::Onset),
            "peak" => Ok(Reference::Peak),
            _ => Err("expected `onset` or `peak`".into()),
        }
    }
}

/// How to pick time zero: everything `zero_corr` takes except `legacy`'s
/// factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub method: Method,
    pub scope: Scope,
    /// `first_break`'s threshold, in noise standard deviations.
    pub sigma: f32,
    pub time_zero: Reference,
    pub margin: Margin,
    /// Traces in the running median of [`Scope::Smooth`].
    pub window: usize,
}

/// The result of a pick. Every trace keeps the same number of samples
/// above its time zero, so that time zero lands on the same row in all of
/// them and they share one travel-time axis.
#[derive(Debug, Clone, PartialEq)]
pub struct Picks {
    /// Time zero, in samples from the top, one per trace. Equal throughout
    /// for [`Scope::Global`].
    pub time_zero: Vec<usize>,
    /// Samples kept above time zero, the same for every trace.
    pub margin: usize,
    /// The margin asked for, in samples, when a trace had too little
    /// record above its time zero to keep all of it.
    pub margin_wanted: usize,
    /// Per-trace picks that disagreed with their neighbours, or failed, and
    /// were replaced by the neighbours' median.
    pub replaced: usize,
    /// Samples from the feature the traces were aligned on to time zero:
    /// the median over the traces of the distance between the two. Zero
    /// when the method picks the requested reference itself.
    pub shift: isize,
    pub window: Window,
}

impl Picks {
    /// Samples to remove from the top of each trace.
    pub fn crops(&self) -> Vec<usize> {
        self.time_zero.iter().map(|t| t - self.margin).collect()
    }
}

/// Pick time zero with any method but `legacy`, which also changes the
/// data and so lives with the rest of [`crate::gpr::GPR`].
///
/// The method's own picks decide the alignment: each trace moves so that its
/// pick lands on the same row. Time zero is then put on `reference`. When
/// the method picks the other feature -- the peak, for an onset -- time zero
/// is moved by the median distance from each trace's pick to that trace's
/// `reference`, found by the picker the other methods use. So
/// `zero_corr(max_peak)` aligns on the peaks, which survive noisy onsets,
/// but puts time zero where `zero_corr(coppens)` would, on average.
///
/// `data` is `(samples, traces)` with `dt_ns` between samples. `Ok(None)`
/// when every trace is flat: there is no signal whose time zero could be
/// wrong.
pub fn pick(data: &Array2<f32>, settings: &Settings, dt_ns: f32) -> Result<Option<Picks>, String> {
    let Settings {
        method,
        scope,
        sigma,
        time_zero: reference,
        margin,
        window: smooth_window,
    } = *settings;
    if method == Method::Legacy {
        return Err("the legacy method is not a picker".into());
    }
    let stack = data
        .mean_axis(Axis(1))
        .ok_or("cannot pick time zero in a radargram without traces")?;
    let Some(window) = find_window(data)? else {
        return Ok(None);
    };
    let has_noise = window.noise_end >= MIN_NOISE_SAMPLES;
    if matches!(method, Method::FirstBreak | Method::Coppens) && !has_noise {
        return Err(format!(
            "`{method}` needs at least {MIN_NOISE_SAMPLES} samples of noise before the direct \
             wave, but it starts {} samples in; try `aic` or `max_peak`",
            window.noise_end
        ));
    }
    // How the start of the direct wave is found when the method itself
    // picks something else: the default method where it can run.
    let onset_method = if has_noise {
        Method::Coppens
    } else {
        Method::Aic
    };
    let picks = match method {
        Method::MaxPeak => Reference::Peak,
        _ => Reference::Onset,
    };

    // The other feature of the direct wave: its peak for a method that picks
    // the start, and the start for `max_peak`.
    let other_method = match picks {
        Reference::Onset => Method::MaxPeak,
        Reference::Peak => onset_method,
    };

    // Also returns the distance from each pick to the other feature, as the
    // same pickers see it: one number, the median over the traces for the
    // trace scope. Measuring it on the aligned stack instead found weak
    // early arrivals that stacking brings out and no single trace shows, so
    // `max_peak` put time zero at an onset no onset method would pick.
    let (aligned, replaced, distance) = match scope {
        Scope::Global => {
            let at = pick_trace(stack.view(), method, &window, sigma)
                .ok_or_else(|| format!("`{method}` found no direct wave in the mean trace"))?;
            let distance = pick_trace(stack.view(), other_method, &window, sigma)
                .map(|o| o as isize - at as isize);
            (vec![at; data.shape()[1]], 0, distance)
        }
        Scope::Trace | Scope::Smooth => {
            let traces: Vec<ArrayView1<f32>> = data.columns().into_iter().collect();
            let raw: Vec<Option<usize>> = traces
                .par_iter()
                .map(|trace| pick_trace(*trace, method, &window, sigma))
                .collect();
            // Also what each pick is checked against.
            let anchors: Vec<Option<usize>> = traces
                .par_iter()
                .map(|trace| pick_trace(*trace, other_method, &window, sigma))
                .collect();
            let distances: Vec<f32> = raw
                .iter()
                .zip(&anchors)
                .filter_map(|(p, a)| Some((*a)? as f32 - (*p)? as f32))
                .collect();
            let distance = (!distances.is_empty()).then(|| median(distances).round() as isize);
            // Three quarters of a period: a real trace-to-trace jump can
            // be most of a half period (the Scott Turnerbreen asset has
            // whole traces arriving ~10 samples late, with a half period of
            // 9), while a pick that slipped a cycle is a full period off.
            // A quarter period replaced the late traces' correct picks.
            let tolerance = (1.5 * window.half_period as f32).max(1.);
            let (aligned, replaced) =
                replace_outliers(&raw, &anchors, OUTLIER_HALF_WINDOW, tolerance)
                    .ok_or_else(|| format!("`{method}` found no direct wave in any trace"))?;
            let aligned = match scope {
                Scope::Smooth => running_median(&aligned, smooth_window),
                _ => aligned,
            };
            (aligned, replaced, distance)
        }
    };

    // From the picks to each feature.
    let to = |feature: Reference| -> Result<isize, String> {
        if feature == picks {
            return Ok(0);
        }
        distance.ok_or_else(|| {
            format!("`{method}` found no direct-wave {feature} to measure time zero from")
        })
    };
    let onset = to(Reference::Onset)?;
    let shift = to(reference)?;
    // Back to the start of the direct wave.
    let auto = (shift - onset).max(0) as usize;

    let time_zero = aligned
        .iter()
        .map(|&at| usize::try_from(at as isize + shift))
        .collect::<Result<Vec<usize>, _>>()
        .map_err(|_| {
            format!("time zero at the {reference} would be before the first sample in some traces")
        })?;
    let margin_wanted = match margin {
        Margin::Auto => auto,
        Margin::Ns(ns) => (ns / dt_ns).round() as usize,
    };
    let earliest = time_zero.iter().copied().min().unwrap_or(0);
    Ok(Some(Picks {
        margin: margin_wanted.min(earliest),
        margin_wanted,
        time_zero,
        replaced,
        shift,
        window,
    }))
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
    let noise_end = earliest_onset.saturating_sub(half_period).min(peak);
    let end = (latest_peak + 2 * half_period + 1).min(n);

    // A vote rather than the mean trace's sign, so that a few traces with
    // a large opposite lobe cannot decide it, and traces that wander do not
    // cancel out.
    let positive = traces
        .par_iter()
        .filter(|trace| {
            let dc = median(trace.iter().copied().collect());
            let largest = trace
                .slice(ndarray::s![noise_end..end])
                .iter()
                .map(|v| v - dc)
                .fold(0_f32, |a, v| if v.abs() > a.abs() { v } else { a });
            largest > 0.
        })
        .count();
    Ok(Some(Window {
        noise_end,
        peak,
        half_period,
        end,
        polarity: if 2 * positive >= traces.len() {
            1.
        } else {
            -1.
        },
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

    // Half a period: the distance to the opposite lobe, which is the first
    // opposite-signed value at least half the peak's size, climbed to its
    // extreme. Smaller sign flips on the way are not a lobe: on a trace
    // whose samples zig-zag (interleaved sampling gone wrong), taking the
    // first flip would make half a period one or two samples.
    let positive = centred[peak] > 0.;
    let is_opposite = |v: f32| (v > 0.) != positive && v.abs() >= 0.5 * centred[peak].abs();
    let mut opposite = peak;
    if let Some(start) = (peak + 1..n).find(|&i| is_opposite(centred[i])) {
        opposite = start;
        for i in start + 1..n {
            if (centred[i] > 0.) == positive {
                break;
            }
            if centred[i].abs() > centred[opposite].abs() {
                opposite = i;
            }
        }
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

    // This trace's own direct-wave peak: the largest value with the
    // radargram's polarity, so it cannot jump to an opposite lobe of
    // similar size. `max_by` would return the last of equal values; the
    // first sample of a clipped plateau is the one wanted.
    let start = window.noise_end.min(n);
    let end = window.end.min(n);
    let mut peak = start;
    for i in start..end {
        if centred[i] * window.polarity > centred[peak] * window.polarity {
            peak = i;
        }
    }
    let segment_end = (peak + window.half_period + 1).min(n);
    // A quarter period, at least 3 samples: how long a real onset stays out
    // of the noise at the least.
    let run = (window.half_period / 2).max(3);

    match method {
        Method::Legacy => None,
        Method::MaxPeak => Some(peak),
        Method::FirstBreak => sustained_from(centred.view(), start, peak, sigma * std, run),
        Method::Aic => {
            let split = aic_onset(centred.slice(ndarray::s![..segment_end]))?;
            sustained_from(centred.view(), split, peak, 3. * std, run).or(Some(split))
        }
        Method::Coppens => {
            // Half a period: a longer window moves the steepest rise of
            // the ratio from the onset towards the energy peak.
            let w = window.half_period.max(2);
            // Stabilises the ratio in the noise, where both energies are
            // small; proportional to the noise energy of one window.
            let beta = (w as f32 * std * std).max(f32::MIN_POSITIVE);
            let rise = coppens_onset(centred.slice(ndarray::s![..segment_end]), w, beta)?;
            // An isolated blip also makes the ratio jump; move on to where
            // the signal stays out of the noise.
            sustained_from(centred.view(), rise, peak, 3. * std, run).or(Some(rise))
        }
    }
}

/// The first sample in `from..=to` that starts a sustained excursion beyond
/// `threshold`: it and at least two thirds of the `run` samples from it
/// exceed it. A few isolated samples before the real arrival -- noise
/// spikes, or a mistimed interleaved sampling series -- then do not count
/// as the onset.
fn sustained_from(
    centred: ArrayView1<f32>,
    from: usize,
    to: usize,
    threshold: f32,
    run: usize,
) -> Option<usize> {
    let n = centred.len();
    let beyond = |i: usize| centred[i].abs() > threshold;
    (from..=to.min(n.saturating_sub(1))).find(|&i| {
        let end = (i + run).min(n);
        beyond(i) && 3 * (i..end).filter(|&j| beyond(j)).count() >= 2 * (end - i)
    })
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

/// The median of the values within `half_window` traces of `i`, and how
/// far from it a value may be: `tolerance`, or three robust standard
/// deviations if more. Widens until there is something to compare with, so
/// a long run of failed picks still gets a value; `None` when nothing is.
fn neighbourhood(
    values: &[Option<f32>],
    i: usize,
    half_window: usize,
    tolerance: f32,
) -> Option<(f32, f32)> {
    let mut half = half_window;
    loop {
        let near: Vec<f32> = values[i.saturating_sub(half)..(i + half + 1).min(values.len())]
            .iter()
            .flatten()
            .copied()
            .collect();
        if !near.is_empty() {
            let centre = median(near.clone());
            let mad = median(near.iter().map(|v| (v - centre).abs()).collect());
            return Some((centre, (3. * 1.4826 * mad).max(tolerance)));
        }
        if half >= values.len() {
            return None;
        }
        half *= 2;
    }
}

/// Replace the per-trace picks that failed or went wrong.
///
/// `anchors` is a second feature of the same trace's direct wave -- its
/// peak for a pick of the start, its start for a pick of the peak. The
/// timing of the direct wave may jump from trace to trace, which is what a
/// per-trace correction is for, but its shape does not, so the distance
/// between the two features stays put. A pick is wrong when it is far from
/// its neighbours' picks *and* its distance to its own anchor is unlike
/// theirs; far from its neighbours alone is the jump being corrected. A
/// wrong or missing pick is rebuilt from its own anchor and the
/// neighbours' distance, which keeps the trace's jitter, or from the
/// neighbours' picks when the anchor failed too.
///
/// Returns the picks and how many were replaced, or `None` if every pick
/// is missing.
pub fn replace_outliers(
    picks: &[Option<usize>],
    anchors: &[Option<usize>],
    half_window: usize,
    tolerance: f32,
) -> Option<(Vec<usize>, usize)> {
    if picks.iter().all(Option::is_none) {
        return None;
    }
    let as_f32 = |v: &Option<usize>| v.map(|p| p as f32);
    let times: Vec<Option<f32>> = picks.iter().map(as_f32).collect();
    let distances: Vec<Option<f32>> = picks
        .iter()
        .zip(anchors)
        .map(|(p, a)| Some(as_f32(p)? - as_f32(a)?))
        .collect();

    let mut replaced = 0;
    let out = (0..picks.len())
        .map(|i| {
            let (time_centre, time_limit) =
                neighbourhood(&times, i, half_window, tolerance).unwrap_or_default();
            let distance = neighbourhood(&distances, i, half_window, tolerance);
            let shape_is_off = match (distances[i], distance) {
                (Some(d), Some((centre, limit))) => (d - centre).abs() > limit,
                _ => true,
            };
            match picks[i] {
                Some(p) if (p as f32 - time_centre).abs() <= time_limit || !shape_is_off => p,
                _ => {
                    replaced += 1;
                    match (anchors[i], distance) {
                        (Some(anchor), Some((centre, _))) => {
                            (anchor as f32 + centre).round().max(0.) as usize
                        }
                        _ => time_centre.round() as usize,
                    }
                }
            }
        })
        .collect();
    Some((out, replaced))
}

/// The median of the `window` values centred on each one (fewer at the
/// ends), rounded to a sample.
fn running_median(values: &[usize], window: usize) -> Vec<usize> {
    let half = window / 2;
    (0..values.len())
        .map(|i| {
            let near = &values[i.saturating_sub(half)..(i + half + 1).min(values.len())];
            median(near.iter().map(|&v| v as f32).collect()).round() as usize
        })
        .collect()
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

    fn settings(
        method: Method,
        scope: Scope,
        sigma: f32,
        time_zero: Reference,
        margin: Margin,
    ) -> Settings {
        Settings {
            method,
            scope,
            sigma,
            time_zero,
            margin,
            window: 51,
        }
    }
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
            let picks = pick(
                &data,
                &settings(method, Scope::Trace, 5., Reference::Onset, Margin::Auto),
                1.,
            )
            .unwrap()
            .unwrap();
            for &p in &picks.time_zero {
                assert!((39..=42).contains(&p), "{method}: picked {p}, onset 40");
            }
        }
        // The peak of a sine starting at 40 is a quarter period later.
        for method in [Method::MaxPeak, Method::Coppens] {
            for scope in [Scope::Global, Scope::Trace] {
                let peaks = pick(
                    &data,
                    &settings(method, scope, 5., Reference::Peak, Margin::Auto),
                    1.,
                )
                .unwrap()
                .unwrap();
                for &p in &peaks.time_zero {
                    assert!((42..=46).contains(&p), "{method}, {scope}: peak at {p}");
                }
            }
        }
    }

    #[test]
    fn max_peak_aligns_on_the_peaks_but_puts_time_zero_at_the_onset() {
        // The onsets wander, and the peaks with them.
        let onsets: Vec<usize> = (0..30).map(|i| 40 + (i * 5) % 7).collect();
        let data = radargram(&onsets, 1000.);
        let picks = pick(
            &data,
            &settings(
                Method::MaxPeak,
                Scope::Trace,
                5.,
                Reference::Onset,
                Margin::Auto,
            ),
            1.,
        )
        .unwrap()
        .unwrap();
        assert!((-5..=-2).contains(&picks.shift), "{}", picks.shift);
        for (p, o) in picks.time_zero.iter().zip(&onsets) {
            assert!(p.abs_diff(*o) <= 1, "time zero {p}, onset {o}");
        }
        assert_eq!(picks.margin, 0);
    }

    #[test]
    fn scaling_or_clipping_does_not_move_the_pick() {
        let onsets: Vec<usize> = (0..30).map(|i| 40 + i % 4).collect();
        let data = radargram(&onsets, 1.);
        let scaled = data.mapv(|v| v * 1000.);
        let clipped = data.mapv(|v| v.clamp(2.7, 3.3));
        for method in [Method::Aic, Method::FirstBreak, Method::Coppens] {
            let base = pick(
                &data,
                &settings(method, Scope::Trace, 5., Reference::Onset, Margin::Auto),
                1.,
            )
            .unwrap()
            .unwrap()
            .time_zero;
            assert_eq!(
                base,
                pick(
                    &scaled,
                    &settings(method, Scope::Trace, 5., Reference::Onset, Margin::Auto),
                    1.
                )
                .unwrap()
                .unwrap()
                .time_zero,
                "{method} moved when scaled"
            );
            let clip = pick(
                &clipped,
                &settings(method, Scope::Trace, 5., Reference::Onset, Margin::Auto),
                1.,
            )
            .unwrap()
            .unwrap()
            .time_zero;
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
        let picks = pick(
            &data,
            &settings(
                Method::Aic,
                Scope::Trace,
                5.,
                Reference::Onset,
                Margin::Auto,
            ),
            1.,
        )
        .unwrap()
        .unwrap();
        for (p, o) in picks.time_zero.iter().zip(&onsets) {
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
                let picks = pick(
                    &data,
                    &settings(method, scope, 5., Reference::Onset, Margin::Auto),
                    1.,
                )
                .unwrap()
                .unwrap();
                assert!(
                    picks.time_zero.iter().all(|&p| p < 60),
                    "{method}, {scope}: {:?}",
                    picks.time_zero
                );
            }
        }
    }

    #[test]
    fn outliers_are_replaced_and_jitter_is_kept() {
        // Onsets that jump by up to 8 samples, with the peak 5 after each.
        let onsets: Vec<usize> = (0..40).map(|i| 40 + (i * 7) % 9).collect();
        let peaks: Vec<Option<usize>> = onsets.iter().map(|o| Some(o + 5)).collect();
        let mut picks: Vec<Option<usize>> = onsets.iter().map(|&o| Some(o)).collect();
        picks[10] = Some(70);
        picks[20] = None;
        let (out, replaced) = replace_outliers(&picks, &peaks, 5, 2.).unwrap();
        assert_eq!(replaced, 2);
        // Rebuilt from their own peaks, not from the neighbours.
        assert_eq!(out[10], onsets[10]);
        assert_eq!(out[20], onsets[20]);
        // Every real jump survives.
        for i in (0..40).filter(|i| ![10, 20].contains(i)) {
            assert_eq!(out[i], onsets[i], "trace {i}");
        }
        // Without an anchor, a pick is judged by its neighbours alone.
        let (out, _) = replace_outliers(&picks, &[None; 40], 5, 2.).unwrap();
        assert!((40..=48).contains(&out[10]));
        assert!(replace_outliers(&[None, None], &[None, None], 5, 2.).is_none());
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
    fn an_auto_margin_keeps_the_whole_wavelet_above_the_peak() {
        let onsets: Vec<usize> = (0..30).map(|i| 40 + i % 3).collect();
        let data = radargram(&onsets, 1000.);
        for scope in [Scope::Global, Scope::Trace] {
            let peak = pick(
                &data,
                &settings(Method::MaxPeak, scope, 5., Reference::Peak, Margin::Auto),
                1.,
            )
            .unwrap()
            .unwrap();
            // A quarter of the 12-sample period between onset and peak.
            assert!((2..=4).contains(&peak.margin), "{scope}: {}", peak.margin);
            for (crop, onset) in peak.crops().iter().zip(&onsets) {
                assert!(
                    *crop <= onset + 1,
                    "{scope}: cropped at {crop}, onset {onset}"
                );
            }
        }
        // Nothing to keep for a method that already picks the start.
        let start = pick(
            &data,
            &settings(
                Method::Coppens,
                Scope::Trace,
                5.,
                Reference::Onset,
                Margin::Auto,
            ),
            1.,
        )
        .unwrap()
        .unwrap();
        assert_eq!(start.margin, 0);
        assert_eq!(start.crops(), start.time_zero);
    }

    #[test]
    fn a_fixed_margin_is_capped_by_the_record_above_time_zero() {
        let data = radargram(&[40; 5], 1000.);
        let picks = pick(
            &data,
            &settings(
                Method::Coppens,
                Scope::Global,
                5.,
                Reference::Onset,
                Margin::Ns(10.),
            ),
            2.,
        )
        .unwrap()
        .unwrap();
        assert_eq!(picks.margin, 5, "10 ns at 2 ns per sample");
        let picks = pick(
            &data,
            &settings(
                Method::Coppens,
                Scope::Global,
                5.,
                Reference::Onset,
                Margin::Ns(1000.),
            ),
            1.,
        )
        .unwrap()
        .unwrap();
        assert_eq!(picks.margin, picks.time_zero[0]);
        assert_eq!(picks.margin_wanted, 1000);
    }

    #[test]
    fn a_small_sign_flip_after_the_peak_is_not_the_opposite_lobe() {
        // A 12-sample period, so half a period is 6, with a small
        // opposite-signed blip just after the peak, as zig-zagging samples
        // give: it used to make half a period 1 or 2 samples.
        let mut trace = synthetic_trace(200, 40, 1000., 1., 0);
        let peak = (40..60)
            .max_by(|&a, &b| trace[a].abs().total_cmp(&trace[b].abs()))
            .unwrap();
        trace[peak + 1] = -0.2 * trace[peak];
        let (_, found, half_period) = direct_wave(trace.view()).unwrap();
        assert_eq!(found, peak);
        assert!((5..=7).contains(&half_period), "{half_period}");
    }

    #[test]
    fn the_peak_keeps_the_polarity_most_traces_have() {
        // The synthetic wavelet's first lobe is negative and its largest.
        // In three traces the positive lobe after it is doubled, so it is
        // their largest absolute value: the peak must still be negative.
        let mut data = radargram(&[40; 12], 1000.);
        for j in 0..3 {
            data.column_mut(j)
                .slice_mut(ndarray::s![46..53])
                .mapv_inplace(|v| 3000. + 2. * (v - 3000.));
        }
        let picks = pick(
            &data,
            &settings(
                Method::MaxPeak,
                Scope::Trace,
                5.,
                Reference::Peak,
                Margin::Auto,
            ),
            1.,
        )
        .unwrap()
        .unwrap();
        assert_eq!(picks.window.polarity, -1.);
        for &p in &picks.time_zero {
            assert!((42..=44).contains(&p), "peak at {p}");
        }
    }

    #[test]
    fn the_polarity_can_be_negative_or_positive() {
        let data = radargram(&[40; 5], 1000.);
        let flipped = data.mapv(|v| 6000. - v);
        let window = |d: &Array2<f32>| find_window(d).unwrap().unwrap();
        assert_eq!(window(&data).polarity, -1.);
        assert_eq!(window(&flipped).polarity, 1.);
        let pick_at = |d: &Array2<f32>| {
            pick(
                d,
                &settings(
                    Method::MaxPeak,
                    Scope::Global,
                    5.,
                    Reference::Peak,
                    Margin::Auto,
                ),
                1.,
            )
            .unwrap()
            .unwrap()
            .time_zero[0]
        };
        assert_eq!(pick_at(&data), pick_at(&flipped));
    }

    /// Traces with onsets `onsets` and noise at `noise` of the amplitude.
    fn noisy_radargram(onsets: &[usize], noise: f32) -> Array2<f32> {
        let mut data = Array2::<f32>::zeros((200, onsets.len()));
        for (j, &onset) in onsets.iter().enumerate() {
            data.column_mut(j).assign(&synthetic_trace(
                200,
                onset,
                1000.,
                1000. * noise,
                j as u64 + 7,
            ));
        }
        data
    }

    fn mean_error(picks: &[usize], truth: &[usize]) -> f32 {
        picks
            .iter()
            .zip(truth)
            .map(|(p, t)| p.abs_diff(*t) as f32)
            .sum::<f32>()
            / picks.len() as f32
    }

    /// The standard deviation of `picks - truth`: scatter, not a constant
    /// offset between what a picker calls the onset and the synthetic one.
    fn scatter(picks: &[usize], truth: &[usize]) -> f32 {
        let d: Vec<f32> = picks
            .iter()
            .zip(truth)
            .map(|(p, t)| *p as f32 - *t as f32)
            .collect();
        let mean = d.iter().sum::<f32>() / d.len() as f32;
        (d.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / d.len() as f32).sqrt()
    }

    #[test]
    fn smoothing_follows_a_slow_drift_more_closely_than_single_picks() {
        // Time zero drifts by 8 samples over 400 traces, and the noise makes
        // single picks scatter around it.
        let onsets: Vec<usize> = (0..400).map(|i| 40 + i * 8 / 400).collect();
        let data = noisy_radargram(&onsets, 0.6);
        let error = |scope| {
            let picks = pick(
                &data,
                &settings(Method::Coppens, scope, 5., Reference::Onset, Margin::Auto),
                1.,
            )
            .unwrap()
            .unwrap();
            scatter(&picks.time_zero, &onsets)
        };
        let (trace, smooth) = (error(Scope::Trace), error(Scope::Smooth));
        assert!(smooth < 0.8 * trace, "smooth {smooth} vs trace {trace}");
    }

    #[test]
    fn smoothing_flattens_real_jumps_between_traces() {
        // The trade-off, pinned: where every third trace really arrives 8
        // samples late, `trace` follows it and `smooth` cannot.
        let onsets: Vec<usize> = (0..120).map(|i| if i % 3 == 2 { 48 } else { 40 }).collect();
        let data = noisy_radargram(&onsets, 0.01);
        let picks = |scope| {
            pick(
                &data,
                &settings(Method::Coppens, scope, 5., Reference::Onset, Margin::Auto),
                1.,
            )
            .unwrap()
            .unwrap()
            .time_zero
        };
        assert!(mean_error(&picks(Scope::Trace), &onsets) <= 1.);
        let smooth = picks(Scope::Smooth);
        assert!(smooth.iter().all(|&p| p.abs_diff(40) <= 1), "{smooth:?}");
    }

    #[test]
    fn isolated_blips_before_the_onset_are_not_the_onset() {
        // Every third sample from 30 carries a blip of 20 noise standard
        // deviations, as a mistimed interleaved sampling series gives, and
        // the real wavelet starts at 40.
        let mut data = noisy_radargram(&[40; 30], 0.01);
        for mut column in data.columns_mut() {
            for i in (31..40).step_by(3) {
                column[i] += 20. * 1000. * 0.01 * 0.3;
            }
        }
        for method in [Method::Coppens, Method::Aic, Method::FirstBreak] {
            let picks = pick(
                &data,
                &settings(method, Scope::Trace, 5., Reference::Onset, Margin::Auto),
                1.,
            )
            .unwrap()
            .unwrap();
            for &p in &picks.time_zero {
                assert!((39..=42).contains(&p), "{method}: onset picked at {p}");
            }
        }
    }

    #[test]
    fn a_flat_radargram_has_nothing_to_pick() {
        let data = Array2::from_elem((50, 4), 7_f32);
        assert_eq!(
            pick(
                &data,
                &settings(
                    Method::Aic,
                    Scope::Trace,
                    5.,
                    Reference::Onset,
                    Margin::Auto
                ),
                1.
            ),
            Ok(None)
        );
    }

    #[test]
    fn noise_based_methods_refuse_a_record_without_noise() {
        let data = radargram(&[1; 5], 1000.);
        for method in [Method::FirstBreak, Method::Coppens] {
            let err = pick(
                &data,
                &settings(method, Scope::Global, 5., Reference::Onset, Margin::Auto),
                1.,
            )
            .unwrap_err();
            assert!(err.contains("samples of noise"), "{err}");
        }
    }

    #[test]
    fn a_threshold_where_the_method_goes_says_how_to_migrate() {
        let err = "0.9".parse::<Method>().unwrap_err();
        assert!(err.contains("zero_corr(legacy, factor=0.9)"), "{err}");
    }
}
