//! `remove_standstills`: find stretches recorded while the radar stood still
//! (#327), from the radar data, and optionally the positions too ([`gps`]).
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
//! What is left is weighted by energy, except that the loudest depths are
//! scaled down to four times the median depth's level ([`ROW_CAP`]): a
//! ring-down that jitters through a standstill would otherwise drown it.
//!
//! The semblance is turned into a score that means the same on any radar:
//! `-ln(1 - S)`, as a robust z against the profile's own median and spread.
//! A standstill is a stretch scoring above [`EXTEND_Z`] that reaches the
//! threshold somewhere. The semblance window blurs its edges by up to half
//! its length either way, so they are then put back: every trace near it is
//! correlated with the median trace of the stretch's middle half, and the
//! standstill grows out from that middle half while the correlation is
//! closer to the standstill's own level than to the moving traces'.
//!
//! The score is measured against the profile's median, so a standstill is
//! something that differs from what most of the profile does. Standstills
//! that together make up more than half of it are not found.
//!
//! Developed on Malå data: 800 MHz from Austfonna, 25 MHz from Drønbreen and
//! 100 MHz (unshielded) from Kroppbreen. With the defaults it found every
//! standstill identified by eye and nothing else, except one weak 100 MHz
//! standstill that scored 7.3. Many more Austfonna radargrams later showed
//! what the radar data alone cannot do: stretches where the signal is lost,
//! or weak, while driving are as coherent as standstills. That is what the
//! positions are for.

use ndarray::{s, Array1, Array2, ArrayView2};

use super::rolling::{self, Statistic};
use crate::gpr::{TimeSource, GPR};
use crate::processing::steps::StepReport;

pub mod gps;

/// Default threshold on the score (a robust z) that a standstill must reach.
pub const DEFAULT_STRENGTH: f32 = 8.;

/// A stretch around a detection belongs to it while it scores above this.
///
/// Half the default threshold: low enough to follow a standstill to where
/// the window starts to overlap moving traces, high enough that ordinary
/// moving traces (median 0) never chain two detections together.
pub const EXTEND_Z: f32 = 4.;

/// How many times louder than the median sample row (by RMS) a row may be
/// before it is scaled down to that level.
///
/// The semblance is weighted by energy, which is right where signal is
/// strong and noise weak. But the loudest rows are often a ring-down that
/// continues through a standstill and jitters from trace to trace: on an
/// unshielded 100 MHz antenna at Kroppbreen, about the first 80 samples
/// below the direct wave held 86 % of the energy, and the four standstills
/// that showed only below them scored 1.7 to 7.3. Capped at four times the
/// median, the weakest of them scored 7.3. Quiet rows are not raised: on
/// 800 MHz data from Austfonna, giving every row the same weight raised
/// interference that is coherent from trace to trace into false
/// standstills. The cap also widened the margin between standstills and
/// moving traces there and at Drønbreen, from about 2 to about 3.6.
pub const ROW_CAP: f32 = 4.;

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
    /// With positions, a standstill whose `peak` is below the strength is one
    /// the positions vouched for.
    pub standstills: Vec<Standstill>,
    /// Stretches that reached the threshold but were shorter than the
    /// minimum duration, and so were left in place.
    pub too_short: Vec<Standstill>,
    /// The highest score outside every standstill and every stretch in
    /// `too_short` and `ruled_out`, or `None` when nothing is left outside
    /// them. How close the moving traces come to the threshold.
    pub highest_elsewhere: Option<f32>,
    /// Stretches that reached the threshold but that the positions show
    /// moving throughout. Always empty without positions.
    pub ruled_out: Vec<Standstill>,
    /// How many stretches the positions show moving in part, and so cut
    /// short or split.
    pub cut: usize,
    /// How many stretches had too few fixes for the positions to judge.
    pub unjudged: usize,
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
/// With `gps`, the positions have their say ([`gps`]): what they show moving
/// is not a standstill however coherent it is, and a stretch scoring above
/// [`EXTEND_Z`] that they show standing still for more than twice their
/// window is one, strength or not. Where they cannot tell, the radar decides
/// alone.
///
/// Returns an error, and finds nothing, when there is too little to measure:
/// fewer traces than two windows, no samples below the direct wave, or a
/// profile so uniform that its scores have no spread.
pub fn detect(
    data: ArrayView2<f32>,
    settings: &Settings,
    gps: Option<&gps::Gps>,
) -> Result<Detection, String> {
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

    let mut detected = Vec::new();
    let mut found = Vec::new();
    let mut ruled_out = Vec::new();
    let mut cut = 0;
    let mut unjudged = 0;
    for (start, end) in runs_above(&scores, EXTEND_Z) {
        let peak = scores[start..end]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let run = Standstill { start, end, peak };
        let strong = peak >= settings.strength;
        let Some(gps) = gps else {
            if strong {
                detected.push(run);
                found.push(refine(&prepared, run));
            }
            continue;
        };

        let refined = refine(&prepared, run);
        let moved = gps.moved(refined.start, refined.end);
        if !strong
            && (moved != Some(false)
                || gps.duration_s(refined.start, refined.end) < 2. * gps.window_s)
        {
            continue;
        }
        detected.push(run);
        match moved {
            None => {
                unjudged += 1;
                found.push(refined);
            }
            Some(false) => found.push(refined),
            Some(true) => {
                // Only what the positions show standing still is left. Its
                // edges are at fixes; the radar's are finer, so each piece
                // is widened to them unless that takes in movement again.
                let (lo, hi) = (start.min(refined.start), end.max(refined.end));
                let pieces = gps.still_pieces(lo, hi);
                if pieces.is_empty() {
                    ruled_out.push(refined);
                } else {
                    cut += 1;
                }
                for (piece_start, piece_end) in pieces {
                    let piece = Standstill {
                        start: piece_start,
                        end: piece_end,
                        peak,
                    };
                    // `refine` needs a middle half to take the median of.
                    if piece.len() < 4 {
                        found.push(piece);
                        continue;
                    }
                    // Only widened: within it, the positions already show
                    // standing still.
                    let redrawn = refine(&prepared, piece);
                    let wider = Standstill {
                        start: piece.start.min(redrawn.start),
                        end: piece.end.max(redrawn.end),
                        peak,
                    };
                    found.push(if gps.moved(wider.start, wider.end) == Some(true) {
                        piece
                    } else {
                        wider
                    });
                }
            }
        }
    }
    let mut standstills = merge(found);
    let too_short: Vec<Standstill> = standstills
        .iter()
        .filter(|s| s.len() < settings.min_traces)
        .copied()
        .collect();
    standstills.retain(|s| s.len() >= settings.min_traces);

    // "Elsewhere" is the moving profile: neither a standstill nor a stretch
    // that reached the threshold. The traces refining trimmed off a
    // detection score high only because the window reached into it, so its
    // detection is left out too.
    let mut inside = vec![false; width];
    let overlaps = |a: &Standstill, b: &Standstill| a.start < b.end && b.start < a.end;
    for s in standstills.iter().chain(&too_short).chain(&ruled_out) {
        inside[s.start..s.end].iter_mut().for_each(|v| *v = true);
        for d in detected.iter().filter(|d| overlaps(d, s)) {
            inside[d.start..d.end].iter_mut().for_each(|v| *v = true);
        }
    }
    let highest_elsewhere = scores
        .iter()
        .zip(&inside)
        .filter(|(_, &inside)| !inside)
        .map(|(&z, _)| z)
        .reduce(f32::max);

    Ok(Detection {
        standstills,
        too_short,
        highest_elsewhere,
        ruled_out,
        cut,
        unjudged,
    })
}

/// Subtract each trace's mean, then the mean trace of the profile, then
/// scale down every sample row louder than [`ROW_CAP`] times the median
/// row (by RMS) to that level.
fn prepare(data: ArrayView2<f32>) -> Array2<f32> {
    let mut prepared = data.to_owned();
    for mut trace in prepared.columns_mut() {
        let mean = trace.iter().map(|&v| v as f64).sum::<f64>() / trace.len() as f64;
        trace.mapv_inplace(|v| v - mean as f32);
    }
    rolling::background_removal(&mut prepared, None, Statistic::Mean);

    let rms: Vec<f32> = prepared
        .rows()
        .into_iter()
        .map(|row| {
            (row.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / row.len() as f64).sqrt() as f32
        })
        .collect();
    let cap = ROW_CAP * median(rms.clone());
    for (mut row, &r) in prepared.rows_mut().into_iter().zip(&rms) {
        if r > cap {
            row.mapv_inplace(|v| v * cap / r);
        }
    }
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

/// Put a standstill's edges where its traces stop resembling its median
/// trace.
///
/// The detection is only roughly in place: where the semblance window
/// straddles an edge it is partly coherent, so the stretch above
/// [`EXTEND_Z`] can run up to half a window past an edge, or stop short of
/// one. Its middle half, though, is inside the standstill. Every trace
/// within `3 * WINDOW` of the stretch is correlated with the median trace
/// of that middle half, and the correlation is smoothed over five traces so
/// that one noisy trace does not end a standstill. The cut is halfway
/// between the median correlation of the middle half and that of the traces
/// further than `WINDOW` from the detection, and the standstill grows out
/// from its middle half while the correlation is above the cut. Its edges
/// can therefore end up inside the detection as well as outside it.
fn refine(data: &Array2<f32>, standstill: Standstill) -> Standstill {
    let width = data.ncols();
    let reach = 3 * WINDOW;
    let (start, end) = (standstill.start, standstill.end);
    let lo = start.saturating_sub(reach);
    let hi = (end + reach).min(width);
    let quarter = (end - start) / 4;
    let (core_start, core_end) = (start + quarter, end - quarter);

    let median_trace: Vec<f64> = data
        .rows()
        .into_iter()
        .map(|row| median(row.slice(s![core_start..core_end]).to_vec()) as f64)
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
    let correlation = rolling::rolling(&correlation, 2, Statistic::Median);
    let at = |j: usize| correlation[j - lo];

    let inside = median((core_start..core_end).map(at).collect());
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

    let mut new_start = core_start;
    while new_start > lo && at(new_start - 1) > cut {
        new_start -= 1;
    }
    let mut new_end = core_end;
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

/// Replace each stretch recorded while the radar stood still with its
/// median trace. See [`crate::processing::steps::standstill`].
///
/// With `gps_uncertainty` (m), the positions are used as well
/// ([`gps`]). Positions that cannot help (too few
/// fixes, or a track that never moves) are left out with a warning.
///
/// Never fails on the data: too little to measure, or a duration in
/// seconds without a usable trace interval, leaves the data unchanged
/// and the log says why.
pub(crate) fn run_remove_standstills(
    gpr: &mut GPR,
    strength: f32,
    min_duration: crate::processing::steps::balance::Span,
    gps_uncertainty: Option<f32>,
) -> StepReport {
    let seconds_per_trace = gpr.metadata.time_interval;
    let min_traces = match min_duration.traces(seconds_per_trace) {
        Ok(n) => n,
        Err(reason) => {
            return StepReport::warning("remove_standstills", format!("{reason}; changed nothing"));
        }
    };

    // One period of the antenna's nominal frequency, in samples, puts the
    // first sample used below the direct wave.
    let step_ns = gpr.vertical_resolution_ns();
    let antenna_mhz = gpr.metadata.antenna_mhz;
    let period = (antenna_mhz > 0. && antenna_mhz.is_finite() && step_ns > 0.)
        .then(|| ((1000. / antenna_mhz) / step_ns).round() as usize);
    let no_frequency = if period.is_none() {
        format!(
            " Warning: the antenna frequency is {antenna_mhz} MHz, so the direct wave was \
             taken to end where the mean trace falls below a tenth of its peak."
        )
    } else {
        String::new()
    };
    let settings = Settings {
        strength,
        min_traces,
        first_sample: below_direct_wave(gpr.data.view(), period),
    };

    let seconds: Vec<f64> = gpr
        .location
        .cor_points
        .iter()
        .map(|p| p.time_seconds)
        .collect();
    let easting: Vec<f64> = gpr.location.cor_points.iter().map(|p| p.easting).collect();
    let northing: Vec<f64> = gpr.location.cor_points.iter().map(|p| p.northing).collect();
    let track = gps::Track {
        seconds: &seconds,
        easting: &easting,
        northing: &northing,
    };
    let (gps, gps_note) = match gps_uncertainty {
        None => (None, String::new()),
        // Times made up from a trace interval say nothing about when
        // the radar stood still, and nor do positions interpolated
        // along with them.
        Some(_) if gpr.location.time_source == TimeSource::Synthetic => (
            None,
            " Warning: the trace times are synthetic, so the GPS was not used.".to_string(),
        ),
        Some(uncertainty) => match gps::Gps::new(track, uncertainty as f64) {
            Ok(gps) => {
                let note = format!(
                    " GPS uncertainty {uncertainty} m: stretches of {:.1} s or more can be \
                     judged.",
                    gps.window_s
                );
                (Some(gps), note)
            }
            Err(reason) => (
                None,
                format!(" Warning: {reason}, so the GPS was not used."),
            ),
        },
    };

    // Either note above is then a warning.
    let warned = period.is_none() || (gps_uncertainty.is_some() && gps.is_none());

    let detection = match detect(gpr.data.view(), &settings, gps.as_ref()) {
        Ok(detection) => detection,
        Err(reason) => {
            return StepReport::warning(
                "remove_standstills",
                format!("{reason}; changed nothing.{no_frequency}{gps_note}"),
            );
        }
    };

    let width = gpr.width();
    // A duration in seconds where the trace interval gives one.
    let duration = |n: usize| {
        if seconds_per_trace > 0. && seconds_per_trace.is_finite() {
            format!("{:.1} s", n as f32 * seconds_per_trace)
        } else {
            format!("{n} traces")
        }
    };
    let elsewhere = detection.highest_elsewhere.map_or(String::new(), |z| {
        format!(" Highest strength elsewhere: {z:.1}.")
    });
    // The minimum duration as it was given.
    let shortest = match min_duration {
        crate::processing::steps::balance::Span::Seconds(s) => format!("{s} s"),
        crate::processing::steps::balance::Span::Traces(n) => format!("{n} traces"),
    };
    let threshold = match gps_uncertainty {
        None => format!("strength >= {strength}, >= {shortest}"),
        Some(u) => format!("strength >= {strength}, >= {shortest}, gps_uncertainty = {u} m"),
    };
    // What the GPS changed.
    let mut gps_changes = String::new();
    if !detection.ruled_out.is_empty() {
        let listed: Vec<String> = detection
            .ruled_out
            .iter()
            .map(|s| format!("{}-{} ({:.1})", s.start, s.end - 1, s.peak))
            .collect();
        gps_changes += &format!(
            " The GPS showed movement through {} that reached the strength (traces \
             (strength): {}).",
            listed.len(),
            listed.join(", ")
        );
    }
    if detection.cut > 0 {
        gps_changes += &format!(
            " The GPS showed movement in part of {}, which kept only the rest.",
            detection.cut
        );
    }
    if detection.unjudged > 0 {
        gps_changes += &format!(
            " {} had too few fixes for the GPS to judge.",
            detection.unjudged
        );
    }
    let vouched = detection
        .standstills
        .iter()
        .filter(|s| s.peak < strength)
        .count();
    if vouched > 0 {
        gps_changes += &format!(
            " {vouched} below the strength {} kept because the GPS showed standing still.",
            if vouched == 1 { "was" } else { "were" }
        );
    }
    let skipped = match detection.too_short.as_slice() {
        [] => String::new(),
        short => format!(
            " Skipped {} shorter than {shortest} (strongest {:.1}).",
            short.len(),
            short
                .iter()
                .map(|s| s.peak)
                .fold(f32::NEG_INFINITY, f32::max)
        ),
    };
    if detection.standstills.is_empty() {
        return StepReport::new(
            "remove_standstills",
            format!(
                "Found no standstills ({threshold}).{skipped}{elsewhere}{gps_changes}\
                 {no_frequency}{gps_note}"
            ),
        )
        .warn_if(warned);
    }

    // Each output trace stands for one input trace (its time, position
    // and offsets) and, inside a standstill, for the whole stretch.
    let mut representatives = Vec::with_capacity(width);
    let mut medians = Vec::with_capacity(detection.standstills.len());
    let mut next = 0;
    for standstill in &detection.standstills {
        representatives.extend(next..standstill.start);
        medians.push((representatives.len(), *standstill));
        representatives.push(standstill.representative(width));
        next = standstill.end;
    }
    representatives.extend(next..width);

    // The median trace of each standstill, from the traces before
    // selection, which are what a standstill's range indexes.
    let median_traces = |data: &Array2<f32>| -> Vec<Array1<f32>> {
        medians
            .iter()
            .map(|(_, standstill)| {
                data.slice(ndarray::s![.., standstill.start..standstill.end])
                    .rows()
                    .into_iter()
                    .map(|row| {
                        let mut values = row.to_vec();
                        let middle = values.len() / 2;
                        *values.select_nth_unstable_by(middle, f32::total_cmp).1
                    })
                    .collect()
            })
            .collect()
    };
    let data_medians = median_traces(&gpr.data);
    let topo_medians = gpr.topo_data.as_ref().map(median_traces);
    gpr.select_traces(&representatives);
    for ((column, _), median) in medians.iter().zip(&data_medians) {
        gpr.data.column_mut(*column).assign(median);
    }
    if let (Some(topo_data), Some(topo_medians)) = (&mut gpr.topo_data, &topo_medians) {
        for ((column, _), median) in medians.iter().zip(topo_medians) {
            topo_data.column_mut(*column).assign(median);
        }
    }

    let listed: Vec<String> = detection
        .standstills
        .iter()
        .map(|s| {
            format!(
                "{}-{} ({}, {:.1})",
                s.start,
                s.end - 1,
                duration(s.len()),
                s.peak
            )
        })
        .collect();
    let replaced = match detection.standstills.len() {
        1 => "1 standstill with its median trace".to_string(),
        n => format!("{n} standstills with their median trace"),
    };
    StepReport::new(
        "remove_standstills",
        format!(
            "Replaced {replaced} ({threshold}). Traces (duration, strength): {}. {width} -> {} \
             traces.{skipped}{elsewhere}{gps_changes}{no_frequency}{gps_note}",
            listed.join(", "),
            gpr.width()
        ),
    )
    .warn_if(warned)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::gpr::test_fixtures::*;

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
        let found = detect(data.view(), &settings(), None).unwrap();
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
    fn the_overall_amplitude_does_not_matter() {
        // Radars record on very different scales (GSSI's integers against
        // Malå's millivolts). Every stage is a ratio, a correlation or
        // relative to the profile itself, so a factor changes nothing.
        let data = radargram(&[(0, 80), (500, 620), (1900, 2000)]);
        let at = |factor: f32| {
            let scaled = data.mapv(|v| v * factor);
            let first_sample = below_direct_wave(scaled.view(), Some(10));
            let found = detect(
                scaled.view(),
                &Settings {
                    first_sample,
                    ..settings()
                },
                None,
            )
            .unwrap();
            (
                first_sample,
                found
                    .standstills
                    .iter()
                    .map(|s| (s.start, s.end))
                    .collect::<Vec<_>>(),
            )
        };
        let reference = at(1.);
        assert_eq!(reference.1.len(), 3);
        for factor in [1e-6, 1e-3, 1e3, 1e6] {
            assert_eq!(at(factor), reference, "factor {factor}");
        }
    }

    #[test]
    fn a_moving_profile_has_no_standstills() {
        let found = detect(radargram(&[]).view(), &settings(), None).unwrap();
        assert!(found.standstills.is_empty(), "{:?}", found.standstills);
    }

    #[test]
    fn a_long_standstill_is_found() {
        // 40 % of the profile. A running background, or a median one, would
        // make a stop this long its own background.
        let data = radargram(&[(400, 1200)]);
        let found = detect(data.view(), &settings(), None).unwrap();
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
        let found = detect(data.view(), &settings(), None).unwrap();
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
        let found = detect(data.view(), &strict, None).unwrap();
        assert!(found.standstills.is_empty());
        // Reported as skipped, and not counted as the moving profile.
        assert_eq!(found.too_short.len(), 1, "{:?}", found.too_short);
        assert!(found.highest_elsewhere.unwrap() < DEFAULT_STRENGTH);
    }

    #[test]
    fn a_higher_strength_finds_less() {
        let data = radargram(&[(500, 620)]);
        let found = detect(data.view(), &settings(), None).unwrap();
        let peak = found.standstills[0].peak;
        let above = Settings {
            strength: peak + 1.,
            ..settings()
        };
        assert!(detect(data.view(), &above, None)
            .unwrap()
            .standstills
            .is_empty());
    }

    #[test]
    fn the_direct_wave_must_be_left_out() {
        // With it, its jitter takes over the score: the reason `first_sample`
        // exists.
        let data = radargram(&[(500, 620)]);
        let only_the_stop = |found: &Detection| {
            found.standstills.len() == 1 && found.standstills[0].start.abs_diff(500) <= 5
        };
        assert!(only_the_stop(
            &detect(data.view(), &settings(), None).unwrap()
        ));
        let with = Settings {
            first_sample: 0,
            ..settings()
        };
        let found = detect(data.view(), &with, None).unwrap();
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
        assert!(detect(data.view(), &settings(), None).is_err());
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

    /// `radargram`, with extra noise in `stop` so that it is only a little
    /// more coherent than the moving traces.
    fn with_noise(stops: &[(usize, usize)], weak: (usize, usize), noise: f32) -> Array2<f32> {
        let mut data = radargram(stops);
        for j in weak.0..weak.1 {
            for i in DIRECT_WAVE_END..HEIGHT {
                data[[i, j]] += noise * hash(i, j, 3);
            }
        }
        data
    }

    /// Positions for `radargram`, standing still in `stops` (traces) and
    /// driving at 4 m/s elsewhere, with fixes off by up to half a metre.
    fn positions(stops: &[(usize, usize)]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        gps::tests::track(WIDTH, stops, 0.5)
    }

    fn with_gps<'a>(track: &'a (Vec<f64>, Vec<f64>, Vec<f64>), uncertainty: f64) -> gps::Gps<'a> {
        gps::Gps::new(
            gps::Track {
                seconds: &track.0,
                easting: &track.1,
                northing: &track.2,
            },
            uncertainty,
        )
        .unwrap()
    }

    fn ranges(found: &Detection) -> Vec<(usize, usize)> {
        found.standstills.iter().map(|s| (s.start, s.end)).collect()
    }

    #[test]
    fn the_gps_rules_out_coherent_driving() {
        // 1200..1350 is as coherent as a stop, as lost signal is, but the
        // positions show it driving.
        let data = radargram(&[(500, 620), (1200, 1350)]);
        let track = positions(&[(500, 620)]);
        let radar = detect(data.view(), &settings(), None).unwrap();
        assert_eq!(radar.standstills.len(), 2);

        let found = detect(data.view(), &settings(), Some(&with_gps(&track, 1.))).unwrap();
        assert_eq!(ranges(&found), ranges(&radar)[..1], "{found:?}");
        assert_eq!(found.ruled_out.len(), 1, "{found:?}");
        assert!(found.ruled_out[0].start.abs_diff(1200) <= 5);
        // Ruled out, it does not count as the moving profile's strength.
        assert!(found.highest_elsewhere.unwrap() < DEFAULT_STRENGTH);
    }

    #[test]
    fn a_stop_run_together_with_coherent_driving_keeps_only_the_stop() {
        // The radar sees one stretch, 1000..1300; the positions stand still
        // only from 1150.
        let data = radargram(&[(1000, 1300)]);
        let track = positions(&[(1150, 1300)]);
        let found = detect(data.view(), &settings(), Some(&with_gps(&track, 1.))).unwrap();
        let [(start, end)] = ranges(&found)[..] else {
            panic!("{found:?}")
        };
        // Fixes are ten traces apart.
        assert!(
            start.abs_diff(1150) <= 15 && end.abs_diff(1300) <= 5,
            "{found:?}"
        );
        assert_eq!(found.cut, 1);
    }

    #[test]
    fn the_gps_vouches_for_a_weak_stop() {
        let data = with_noise(&[(500, 800)], (500, 800), 0.8);
        assert!(detect(data.view(), &settings(), None)
            .unwrap()
            .standstills
            .is_empty());

        let track = positions(&[(500, 800)]);
        let found = detect(data.view(), &settings(), Some(&with_gps(&track, 1.))).unwrap();
        let [s] = found.standstills[..] else {
            panic!("{found:?}")
        };
        assert!(s.peak < DEFAULT_STRENGTH);
        assert!(
            s.start.abs_diff(500) <= 10 && s.end.abs_diff(800) <= 10,
            "{s:?}"
        );

        // Not when the positions show it moving, nor when they cannot see
        // movement over it: at 50 m, a stretch has to last 300 s.
        let moving = positions(&[]);
        let found = detect(data.view(), &settings(), Some(&with_gps(&moving, 1.))).unwrap();
        assert!(found.standstills.is_empty(), "{found:?}");
        let found = detect(data.view(), &settings(), Some(&with_gps(&track, 50.))).unwrap();
        assert!(found.standstills.is_empty(), "{found:?}");
    }

    #[test]
    fn a_large_uncertainty_leaves_the_radar_to_decide() {
        // At 50 m, the 15 s of coherent driving moved too little to tell.
        let data = radargram(&[(500, 620), (1200, 1350)]);
        let track = positions(&[(500, 620)]);
        let radar = detect(data.view(), &settings(), None).unwrap();
        let found = detect(data.view(), &settings(), Some(&with_gps(&track, 50.))).unwrap();
        assert_eq!(ranges(&found), ranges(&radar));
        assert!(found.ruled_out.is_empty());
    }

    /// A radargram with the synthetic standstills of
    /// `tests`, on a grid where one antenna period is
    /// ten samples.
    fn make_gpr_with_standstills(stops: &[(usize, usize)]) -> super::GPR {
        use tests::{radargram, HEIGHT, WIDTH};
        let mut gpr = make_dummy_gpr(WIDTH, HEIGHT, None);
        gpr.data = radargram(stops);
        // 500 MHz is a 2 ns period: ten samples at 0.2 ns.
        gpr.metadata.time_window = HEIGHT as f32 * 0.2;
        gpr
    }

    #[test]
    fn remove_standstills_replaces_each_with_its_median_trace_and_logs_it() {
        let stops = [(0, 80), (500, 620), (1900, 2000)];
        let mut gpr = make_gpr_with_standstills(&stops);
        let before: Vec<f64> = gpr
            .location
            .cor_points
            .iter()
            .map(|p| p.time_seconds)
            .collect();
        gpr.process("remove_standstills").unwrap();

        // About 79 + 119 + 99 traces fewer, give or take the edges.
        let removed = before.len() - gpr.width();
        assert!((280..=310).contains(&removed), "removed {removed}");
        let log = gpr.log.last().unwrap();
        assert!(log.contains("Replaced 3 standstills"), "{log}");
        assert!(log.contains("(strength >= 8, >= 5 s)"), "{log}");
        assert!(log.contains("Traces (duration, strength): 0-"), "{log}");
        assert!(log.contains("Highest strength elsewhere"), "{log}");

        // Times stay strictly increasing, and the ends of the profile keep
        // their times: what keeps interpretations carryable.
        let after: Vec<f64> = gpr
            .location
            .cor_points
            .iter()
            .map(|p| p.time_seconds)
            .collect();
        assert!(after.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(after.first(), before.first());
        assert_eq!(after.last(), before.last());
    }

    #[test]
    fn remove_standstills_with_nothing_to_find_changes_nothing() {
        let mut gpr = make_gpr_with_standstills(&[]);
        let data = gpr.data.clone();
        gpr.process("remove_standstills").unwrap();
        assert_eq!(gpr.data, data);
        assert!(gpr.log.last().unwrap().contains("Found no standstills"));
    }

    #[test]
    fn remove_standstills_without_a_trace_interval_warns_for_seconds_only() {
        let mut gpr = make_gpr_with_standstills(&[(500, 620)]);
        gpr.metadata.time_interval = 0.;
        let width = gpr.width();
        gpr.process("remove_standstills").unwrap();
        assert_eq!(gpr.width(), width);
        assert!(gpr.log.last().unwrap().contains("Warning"));
        // In traces it needs no interval.
        gpr.process("remove_standstills(min_duration=10)").unwrap();
        assert!(gpr.width() < width);
    }

    /// `make_gpr_with_standstills`, with positions that stand still only in
    /// `still` and drive at 4 m/s elsewhere, at 0.1 s per trace.
    fn make_gpr_with_positions(stops: &[(usize, usize)], still: &[(usize, usize)]) -> super::GPR {
        let mut gpr = make_gpr_with_standstills(stops);
        let (seconds, easting, northing) = gps::tests::track(gpr.width(), still, 0.5);
        gpr.metadata.time_interval = gps::tests::SECONDS_PER_TRACE as f32;
        for (i, point) in gpr.location.cor_points.iter_mut().enumerate() {
            point.time_seconds = seconds[i];
            point.easting = easting[i];
            point.northing = northing[i];
        }
        gpr
    }

    #[test]
    fn remove_standstills_with_gps_keeps_only_what_stood_still_and_logs_it() {
        // 1200..1350 is coherent, as lost signal is, while driving.
        let stops = [(500, 620), (1200, 1350)];
        let mut radar = make_gpr_with_positions(&stops, &[(500, 620)]);
        let mut gps = make_gpr_with_positions(&stops, &[(500, 620)]);
        radar.process("remove_standstills").unwrap();
        gps.process("remove_standstills(gps_uncertainty=1)")
            .unwrap();
        assert!(
            gps.width() > radar.width() + 100,
            "{}",
            gps.log.last().unwrap()
        );

        let log = gps.log.last().unwrap();
        assert!(log.contains("Replaced 1 standstill"), "{log}");
        assert!(log.contains("gps_uncertainty = 1 m"), "{log}");
        assert!(log.contains("The GPS showed movement through 1"), "{log}");
        assert!(
            log.contains("stretches of 3.0 s or more can be judged"),
            "{log}"
        );
        assert_eq!(
            gps.steps.last().unwrap(),
            "remove_standstills(strength=8, min_duration=5s, gps_uncertainty=1)"
        );
    }

    #[test]
    fn remove_standstills_does_not_trust_synthetic_times() {
        // #13: times made up by `ridal coords` say nothing about stops, so
        // the GPS is left out, as with unusable positions.
        let stops = [(500, 620), (1200, 1350)];
        let mut radar = make_gpr_with_positions(&stops, &[(500, 620)]);
        let mut synthetic = make_gpr_with_positions(&stops, &[(500, 620)]);
        synthetic.location.time_source = super::TimeSource::Synthetic;
        radar.process("remove_standstills").unwrap();
        synthetic
            .process("remove_standstills(gps_uncertainty=1)")
            .unwrap();
        assert_eq!(synthetic.width(), radar.width());
        let log = synthetic.log.last().unwrap();
        assert!(
            log.contains("the trace times are synthetic, so the GPS was not used"),
            "{log}"
        );
    }

    #[test]
    fn remove_standstills_without_usable_positions_uses_the_radar() {
        // `make_dummy_gpr` positions move one metre per trace; these do not.
        let still = || {
            let mut gpr = make_gpr_with_standstills(&[(500, 620)]);
            for point in gpr.location.cor_points.iter_mut() {
                point.easting = 0.;
                point.northing = 0.;
            }
            gpr
        };
        let (mut gpr, mut radar) = (still(), still());
        radar.process("remove_standstills").unwrap();
        gpr.process("remove_standstills(gps_uncertainty=1)")
            .unwrap();
        assert_eq!(gpr.data, radar.data);
        let log = gpr.log.last().unwrap();
        // No bends in the track: no fixes, as without a GPS at all.
        assert!(log.contains("Warning: there are only 2 fixes"), "{log}");
        assert!(log.contains("so the GPS was not used"), "{log}");
    }

    /// Interpretations drawn before `remove_standstills` carry onto its
    /// output through the `trace_time` anchor, with the real gprinterp
    /// re-anchoring, including lines that start in an opening standstill or
    /// end in a closing one (#327).
    #[test]
    fn interpretations_carry_across_remove_standstills() {
        let stops = [(0, 80), (500, 620), (1900, 2000)];
        let mut gpr = make_gpr_with_standstills(&stops);
        let times = |gpr: &super::GPR| -> Vec<f64> {
            gpr.location
                .cor_points
                .iter()
                .map(|p| p.time_seconds)
                .collect()
        };
        let before = times(&gpr);
        gpr.process("remove_standstills").unwrap();
        let after = times(&gpr);

        let anchor = |times: &[f64]| -> gprinterp::AnchorAxis {
            let axis = crate::interp::anchors::trace_time_axis(times).unwrap();
            serde_json::from_value(serde_json::to_value(axis).unwrap()).unwrap()
        };
        let twtt: gprinterp::AnchorAxis = serde_json::from_value(serde_json::json!(
            {"name": "twtt", "unit": "ns", "type": "regular", "t0": 0.0, "dt": 0.2}
        ))
        .unwrap();
        let lines = [
            (
                "starts in the opening standstill",
                vec![[5., 100.], [300., 110.]],
            ),
            (
                "crosses the middle one",
                vec![[400., 50.], [560., 55.], [700., 60.]],
            ),
            ("lies inside the middle one", vec![[520., 70.], [600., 72.]]),
            (
                "ends in the closing standstill",
                vec![[1700., 40.], [1990., 45.]],
            ),
        ];
        let features: Vec<serde_json::Value> = lines
            .iter()
            .map(|(label, coordinates)| {
                serde_json::json!({
                    "type": "Feature",
                    "geometry": {"type": "LineString", "coordinates": coordinates},
                    "properties": {"label": label},
                })
            })
            .collect();
        let document: gprinterp::Document = serde_json::from_value(serde_json::json!({
            "key": "test-radargram",
            "coordinates": {"space": "index2d", "axes": {
                "x": {"primary": {"name": "trace", "unit": "index"}, "anchor": [anchor(&before)]},
                "y": {"primary": {"name": "sample", "unit": "index"}, "anchor": [twtt.clone()]},
            }},
            "features": features,
        }))
        .unwrap();
        let target = gprinterp::RevisionAxes {
            x: vec![anchor(&after)],
            y: vec![twtt],
        };
        let carried = gprinterp::reanchor(&document, &target).unwrap();
        assert!(carried.dropped.is_empty(), "{:?}", carried.dropped);
        for feature in &carried.document.features {
            let gprinterp::Geometry::LineString(points) = &feature.geometry else {
                panic!("not a line");
            };
            let x: Vec<f64> = points.iter().map(|p| p.0[0]).collect();
            assert!(x.windows(2).all(|w| w[1] >= w[0]), "doubles back: {x:?}");
            assert!(
                x.iter().all(|&v| v >= 0. && v <= (gpr.width() - 1) as f64),
                "{x:?}"
            );
        }
    }
}
