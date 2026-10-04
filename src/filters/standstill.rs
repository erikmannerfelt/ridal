//! `remove_standstills`: find stretches recorded while the radar stood still
//! (#327), from the radar data alone.
//!
//! A time-triggered radar keeps recording when it stops, and every trace in
//! the stop repeats the one before it. Moving, even over a flat bed, the
//! shallow scattering and internal layering keep changing. So a standstill
//! is a stretch where neighbouring traces are unusually *coherent*, and that
//! is what is measured: the semblance of a sliding window of traces, the
//! share of their energy that survives stacking them.
//!
//! Before that, two things that are coherent everywhere are taken out, or
//! they would swamp the measure:
//!
//! - the direct wave, whose large and slightly jittery energy dominates any
//!   sum over samples. Everything down to where it has faded, and at least
//!   one period of the antenna's nominal frequency below its peak, is left
//!   out;
//! - antenna ringing and banding, which is the same in every trace. Each
//!   trace's mean, and then the mean trace of the whole profile, are
//!   subtracted. The whole profile, because a running background would
//!   become a long standstill's own trace; the mean, because a median does
//!   too: in every sample, the traces of a long stop cluster at one value,
//!   and the median of a row lands on that cluster in most rows once the
//!   stop is a sizeable share of the profile. The mean only moves part of
//!   the way towards it.
//!
//! The semblance is turned into a score that means the same on any radar:
//! `-ln(1 - S)`, as a robust z against the profile's own median and spread.
//! A standstill is a stretch scoring above [`EXTEND_Z`] that reaches the
//! threshold somewhere. Its edges are then refined: every trace near it is
//! correlated with the stretch's median trace, and the stretch grows while
//! that correlation is closer to the stretch's own level than to the moving
//! traces' level. The semblance window blurs the edges by half its length,
//! and this is what puts them back.
//!
//! The score is measured against the profile's median, so a standstill is
//! something that differs from what most of the profile does. Standstills
//! that together make up more than half of it are not found.
//!
//! Developed on 800 MHz Malå data from Austfonna and 25 MHz Malå data from
//! Drønbreen, where it found every visually identified standstill and
//! nothing else. Moving traces there score up to about 6, and standstills
//! 10 to 36.

use ndarray::{s, Array2, ArrayView2};

use super::rolling::{self, Statistic};

/// Default threshold on the score (a robust z) that a standstill must reach.
pub const DEFAULT_STRENGTH: f32 = 8.;

/// A stretch around a detection belongs to it while it scores above this.
///
/// Half the default threshold: low enough to follow a standstill to where
/// the window starts to overlap moving traces, high enough that ordinary
/// moving traces (median 0) never chain two detections together.
pub const EXTEND_Z: f32 = 4.;

/// Traces in the sliding semblance window.
///
/// Long enough that a moving stretch decorrelates across it, short enough
/// to resolve a stop of a few seconds at common trace intervals.
pub const WINDOW: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// The score a standstill must reach somewhere.
    pub strength: f32,
    /// The fewest traces a standstill may have.
    pub min_traces: usize,
    /// First sample used: the samples above it hold the direct wave.
    pub first_sample: usize,
}

/// One standstill, as a half-open range of trace indices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Standstill {
    pub start: usize,
    pub end: usize,
    /// Its highest score, to compare with the threshold.
    pub peak: f32,
}

impl Standstill {
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// The input trace whose time and position the replacement takes.
    ///
    /// The middle, except at the ends of the profile: a standstill that
    /// opens the profile keeps its first trace's, and one that closes it its
    /// last trace's. Re-anchoring inverts the time axis, and an
    /// interpretation that starts in an opening standstill would otherwise
    /// fall before the first trace of the new revision and be dropped.
    pub fn representative(&self, width: usize) -> usize {
        if self.start == 0 {
            0
        } else if self.end == width {
            width - 1
        } else {
            (self.start + self.end) / 2
        }
    }
}

/// What was found.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub standstills: Vec<Standstill>,
    /// The highest score outside every standstill, or `None` when nothing
    /// is left outside them. How close the moving traces come to the
    /// threshold.
    pub highest_elsewhere: Option<f32>,
}

/// The first sample below the direct wave.
///
/// The direct wave is where the mean absolute trace (each trace's mean
/// removed) is largest, and it ends at the first sample below that peak
/// where the mean absolute trace has fallen under a tenth of the peak's.
/// With `period_samples`, one period of the antenna's nominal frequency,
/// the result is at least one period below the peak, whichever is deeper.
///
/// Both, because neither holds on its own. A direct wave of several lobes
/// can peak on an early one, and filtering changes which: on 800 MHz data
/// from Austfonna, `dewow` moved the peak five samples up while the wave
/// still ended in the same place, and one period below the peak was then
/// inside it, where its jitter hid seven of eight standstills. On 25 MHz
/// data from Drønbreen the tenth is reached a little early, and one period
/// is the deeper of the two.
pub fn below_direct_wave(data: ArrayView2<f32>, period_samples: Option<usize>) -> usize {
    let (height, width) = data.dim();
    if height == 0 || width == 0 {
        return 0;
    }
    let means: Vec<f64> = data
        .columns()
        .into_iter()
        .map(|c| c.iter().map(|&v| v as f64).sum::<f64>() / height as f64)
        .collect();
    let level: Vec<f64> = (0..height)
        .map(|i| {
            data.row(i)
                .iter()
                .zip(&means)
                .map(|(&v, m)| (v as f64 - m).abs())
                .sum::<f64>()
                / width as f64
        })
        .collect();
    let peak = level
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i);
    let tail = peak
        + level[peak..]
            .iter()
            .position(|&v| v < 0.1 * level[peak])
            .unwrap_or(height - peak);
    let one_period = peak + period_samples.unwrap_or(0);
    tail.max(one_period).min(height)
}

/// Find the standstills in `data` (samples along axis 0, traces along
/// axis 1). See the module docs.
///
/// Returns an error, and finds nothing, when there is too little to measure:
/// fewer traces than two windows, no samples below the direct wave, or a
/// profile so uniform that its scores have no spread.
pub fn detect(data: ArrayView2<f32>, settings: &Settings) -> Result<Detection, String> {
    let (height, width) = data.dim();
    if width < 2 * WINDOW {
        return Err(format!(
            "the profile has {width} traces, too few to measure coherence over \
             {WINDOW}-trace windows"
        ));
    }
    if settings.first_sample >= height {
        return Err(format!(
            "no samples are left below the direct wave (it ends at sample {} of {height})",
            settings.first_sample
        ));
    }

    let prepared = prepare(data.slice(s![settings.first_sample.., ..]));
    let scores = robust_z(&semblance(&prepared))
        .ok_or("the coherence does not vary along the profile, so nothing stands out")?;

    let mut standstills: Vec<Standstill> = runs_above(&scores, EXTEND_Z)
        .into_iter()
        .filter_map(|(start, end)| {
            let peak = scores[start..end]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            (peak >= settings.strength).then_some(Standstill { start, end, peak })
        })
        .map(|standstill| refine(&prepared, standstill))
        .collect();
    standstills = merge(standstills);
    standstills.retain(|s| s.len() >= settings.min_traces);

    let mut inside = vec![false; width];
    for s in &standstills {
        inside[s.start..s.end].iter_mut().for_each(|v| *v = true);
    }
    let highest_elsewhere = scores
        .iter()
        .zip(&inside)
        .filter(|(_, &inside)| !inside)
        .map(|(&z, _)| z)
        .reduce(f32::max);

    Ok(Detection {
        standstills,
        highest_elsewhere,
    })
}

/// Subtract each trace's mean, then the mean trace of the profile.
fn prepare(data: ArrayView2<f32>) -> Array2<f32> {
    let mut prepared = data.to_owned();
    for mut trace in prepared.columns_mut() {
        let mean = trace.iter().map(|&v| v as f64).sum::<f64>() / trace.len() as f64;
        trace.mapv_inplace(|v| v - mean as f32);
    }
    rolling::background_removal(&mut prepared, None, Statistic::Mean);
    prepared
}

/// The semblance of the [`WINDOW`] traces around each trace: the energy of
/// their stack over their mean energy, summed over samples. 1 when they are
/// identical, about `1 / WINDOW` when they are unrelated.
fn semblance(data: &Array2<f32>) -> Vec<f32> {
    let width = data.ncols();
    let mut stacked = vec![0_f64; width];
    let mut energy = vec![0_f64; width];
    let start_of = |j: usize| j.saturating_sub(WINDOW / 2).min(width - WINDOW);
    for row in data.rows() {
        // Running sums along the row make each window O(1).
        let mut sum = vec![0_f64; width + 1];
        let mut sum_sq = vec![0_f64; width + 1];
        for (j, &v) in row.iter().enumerate() {
            sum[j + 1] = sum[j] + v as f64;
            sum_sq[j + 1] = sum_sq[j] + (v as f64).powi(2);
        }
        for j in 0..width {
            let (lo, hi) = (start_of(j), start_of(j) + WINDOW);
            let mean = (sum[hi] - sum[lo]) / WINDOW as f64;
            stacked[j] += mean * mean;
            energy[j] += (sum_sq[hi] - sum_sq[lo]) / WINDOW as f64;
        }
    }
    stacked
        .iter()
        .zip(&energy)
        .map(|(&s, &e)| if e > 0. { (s / e) as f32 } else { 0. })
        .collect()
}

/// `-ln(1 - S)` as a robust z: its distance from the median in units of the
/// scaled median absolute deviation. `None` when that deviation is zero.
///
/// The log is what separates a standstill on smooth, low-frequency data: there
/// moving traces already reach S ≈ 0.85, and a standstill S ≈ 0.99, which a
/// linear z cannot tell apart and `-ln(1 - S)` puts far apart.
fn robust_z(semblance: &[f32]) -> Option<Vec<f32>> {
    let score: Vec<f32> = semblance
        .iter()
        .map(|&s| -(1. - s).max(1e-6).ln())
        .collect();
    let median = median(score.clone());
    let mad = 1.4826 * median_of(score.iter().map(|v| (v - median).abs()));
    (mad > 0. && mad.is_finite()).then(|| score.iter().map(|v| (v - median) / mad).collect())
}

fn median(mut values: Vec<f32>) -> f32 {
    let middle = values.len() / 2;
    *values.select_nth_unstable_by(middle, f32::total_cmp).1
}

fn median_of(values: impl Iterator<Item = f32>) -> f32 {
    median(values.collect())
}

/// Half-open ranges where `values` is above `threshold`.
fn runs_above(values: &[f32], threshold: f32) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, &v) in values.iter().enumerate() {
        match (v > threshold, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, values.len()));
    }
    runs
}

/// Grow a standstill to where its traces stop resembling its median trace.
///
/// Every trace within `3 * WINDOW` of it is correlated with its median
/// trace. The cut is halfway between the median correlation inside it and
/// the median correlation of the traces further than `WINDOW` from either
/// edge, and the standstill grows outwards while a trace is above the cut.
/// It never shrinks: the detection already lies inside the standstill.
fn refine(data: &Array2<f32>, standstill: Standstill) -> Standstill {
    let width = data.ncols();
    let reach = 3 * WINDOW;
    let (start, end) = (standstill.start, standstill.end);
    let lo = start.saturating_sub(reach);
    let hi = (end + reach).min(width);

    let median_trace: Vec<f64> = data
        .rows()
        .into_iter()
        .map(|row| median(row.slice(s![start..end]).to_vec()) as f64)
        .collect();
    let norm = median_trace.iter().map(|v| v * v).sum::<f64>().sqrt();
    if norm == 0. {
        return standstill;
    }
    let correlation: Vec<f32> = (lo..hi)
        .map(|j| {
            let trace = data.column(j);
            let dot: f64 = trace
                .iter()
                .zip(&median_trace)
                .map(|(&v, m)| v as f64 * m)
                .sum();
            let trace_norm = trace
                .iter()
                .map(|&v| (v as f64).powi(2))
                .sum::<f64>()
                .sqrt();
            if trace_norm > 0. {
                (dot / (trace_norm * norm)) as f32
            } else {
                0.
            }
        })
        .collect();
    let at = |j: usize| correlation[j - lo];

    let inside = median((start..end).map(at).collect());
    let outside: Vec<f32> = (lo..hi)
        .filter(|&j| j + WINDOW < start || j >= end + WINDOW)
        .map(at)
        .collect();
    let outside = if outside.is_empty() {
        0.
    } else {
        median(outside)
    };
    let cut = 0.5 * (inside + outside);

    let mut new_start = start;
    while new_start > lo && at(new_start - 1) > cut {
        new_start -= 1;
    }
    let mut new_end = end;
    while new_end < hi && at(new_end) > cut {
        new_end += 1;
    }
    Standstill {
        start: new_start,
        end: new_end,
        ..standstill
    }
}

/// Join standstills that overlap or touch after refining.
fn merge(mut standstills: Vec<Standstill>) -> Vec<Standstill> {
    standstills.sort_by_key(|s| s.start);
    let mut merged: Vec<Standstill> = Vec::with_capacity(standstills.len());
    for s in standstills {
        match merged.last_mut() {
            Some(last) if s.start <= last.end => {
                last.end = last.end.max(s.end);
                last.peak = last.peak.max(s.peak);
            }
            _ => merged.push(s),
        }
    }
    merged
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const HEIGHT: usize = 300;
    pub(crate) const WIDTH: usize = 2000;
    /// Samples above this hold the synthetic direct wave.
    const DIRECT_WAVE_END: usize = 30;

    /// A deterministic, noise-like value in [-1, 1).
    fn hash(i: usize, j: usize, seed: usize) -> f32 {
        let mut x = (i as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add((j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
            .wrapping_add(seed as u64);
        x ^= x >> 31;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 29;
        ((x % 2_000_003) as f32 / 1_000_001.5) - 1.
    }

    /// A radargram whose scene changes from trace to trace except in
    /// `stops`, where the radar stood still: the scene holds and only the
    /// noise changes. A strong direct wave with a little jitter sits on
    /// top, as in real data.
    pub(crate) fn radargram(stops: &[(usize, usize)]) -> Array2<f32> {
        // Which scene each trace sees: it advances unless stopped.
        let mut scene = vec![0_usize; WIDTH];
        for j in 1..WIDTH {
            let stopped = stops.iter().any(|&(a, b)| j > a && j < b);
            scene[j] = if stopped {
                scene[j - 1]
            } else {
                scene[j - 1] + 1
            };
        }
        Array2::from_shape_fn((HEIGHT, WIDTH), |(i, j)| {
            if i < DIRECT_WAVE_END {
                // A wavelet peaking at sample 12 and gone well before 30,
                // far stronger than anything below it, that arrives up to
                // half a sample early or late in each trace, as in real
                // recordings.
                let t = i as f32 - 12. - 0.5 * hash(0, j, 7);
                return 200. * (-(t / 4.).powi(2)).exp() * (t / 3.).cos();
            }
            // Scattering decorrelates over a few scene steps, as when moving.
            let s = scene[j];
            let scattering = 0.6 * hash(i, s / 3, 1) + 0.4 * hash(i, s / 3 + 1, 1);
            scattering + 0.2 * hash(i, j, 2)
        })
    }

    fn settings() -> Settings {
        Settings {
            strength: DEFAULT_STRENGTH,
            min_traces: 10,
            first_sample: DIRECT_WAVE_END,
        }
    }

    #[test]
    fn standstills_are_found_with_their_edges() {
        let stops = [(0, 80), (500, 620), (1300, 1340), (1900, 2000)];
        let data = radargram(&stops);
        let found = detect(data.view(), &settings()).unwrap();
        let ranges: Vec<(usize, usize)> =
            found.standstills.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(ranges.len(), stops.len(), "{ranges:?}");
        // The synthetic scene decorrelates over a few traces by construction,
        // so the traces next to a stop partly resemble it.
        for (&(a, b), &(start, end)) in stops.iter().zip(&ranges) {
            assert!(
                start.abs_diff(a) <= 5 && end.abs_diff(b) <= 5,
                "{stops:?} vs {ranges:?}"
            );
        }
        for s in &found.standstills {
            assert!(s.peak >= DEFAULT_STRENGTH);
        }
        assert!(found.highest_elsewhere.unwrap() < DEFAULT_STRENGTH);
    }

    #[test]
    fn a_moving_profile_has_no_standstills() {
        let found = detect(radargram(&[]).view(), &settings()).unwrap();
        assert!(found.standstills.is_empty(), "{:?}", found.standstills);
    }

    #[test]
    fn a_long_standstill_is_found() {
        // 40 % of the profile. A running background, or a median one, would
        // make a stop this long its own background.
        let data = radargram(&[(400, 1200)]);
        let found = detect(data.view(), &settings()).unwrap();
        assert_eq!(found.standstills.len(), 1, "{:?}", found.standstills);
        let s = found.standstills[0];
        assert!(
            s.start.abs_diff(400) <= 5 && s.end.abs_diff(1200) <= 5,
            "{s:?}"
        );
    }

    #[test]
    fn a_standstill_over_half_the_profile_is_not_found() {
        // The documented limit: the median score is then the standstill's
        // own, so nothing about it stands out.
        let data = radargram(&[(400, 1500)]);
        let found = detect(data.view(), &settings()).unwrap();
        assert!(
            !found.standstills.iter().any(|s| s.len() > 1000),
            "{:?}",
            found.standstills
        );
    }

    #[test]
    fn short_stops_are_left_by_the_minimum_duration() {
        let data = radargram(&[(500, 620)]);
        let strict = Settings {
            min_traces: 200,
            ..settings()
        };
        assert!(detect(data.view(), &strict).unwrap().standstills.is_empty());
    }

    #[test]
    fn a_higher_strength_finds_less() {
        let data = radargram(&[(500, 620)]);
        let found = detect(data.view(), &settings()).unwrap();
        let peak = found.standstills[0].peak;
        let above = Settings {
            strength: peak + 1.,
            ..settings()
        };
        assert!(detect(data.view(), &above).unwrap().standstills.is_empty());
    }

    #[test]
    fn the_direct_wave_must_be_left_out() {
        // With it, its jitter takes over the score: the reason `first_sample`
        // exists.
        let data = radargram(&[(500, 620)]);
        let only_the_stop = |found: &Detection| {
            found.standstills.len() == 1 && found.standstills[0].start.abs_diff(500) <= 5
        };
        assert!(only_the_stop(&detect(data.view(), &settings()).unwrap()));
        let with = Settings {
            first_sample: 0,
            ..settings()
        };
        let found = detect(data.view(), &with).unwrap();
        assert!(!only_the_stop(&found), "{:?}", found.standstills);
    }

    #[test]
    fn the_direct_wave_is_located() {
        let data = radargram(&[]);
        // The synthetic wavelet peaks at sample 12 and its envelope falls
        // to a tenth of that about 6 samples further down.
        let tail = below_direct_wave(data.view(), None);
        assert!((15..=20).contains(&tail), "{tail}");
        // One period below the peak, when that is deeper than the tail ...
        assert_eq!(below_direct_wave(data.view(), Some(10)), 22);
        // ... and the tail, when a short period would still be inside it.
        assert_eq!(below_direct_wave(data.view(), Some(2)), tail);
    }

    #[test]
    fn too_few_traces_is_an_error_not_a_panic() {
        let data = Array2::<f32>::zeros((50, WINDOW));
        assert!(detect(data.view(), &settings()).is_err());
    }

    #[test]
    fn the_representative_trace_keeps_the_ends_of_the_profile() {
        let at = |start, end| {
            Standstill {
                start,
                end,
                peak: 9.,
            }
            .representative(100)
        };
        assert_eq!(at(0, 20), 0);
        assert_eq!(at(80, 100), 99);
        assert_eq!(at(40, 60), 50);
    }

    #[test]
    fn overlapping_standstills_merge() {
        let s = |start, end, peak| Standstill { start, end, peak };
        assert_eq!(
            merge(vec![s(50, 80, 12.), s(10, 30, 9.), s(25, 40, 20.)]),
            vec![s(10, 40, 20.), s(50, 80, 12.)]
        );
    }
}
