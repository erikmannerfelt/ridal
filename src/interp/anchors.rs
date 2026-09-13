//! The `coordinates.axes` block a saved interpretation carries (#146).
//!
//! gprinterp SPEC §8.1 re-anchors a stored coordinate by evaluating it
//! through the revision it was drawn on and inverting the revision it is
//! being read against. Without an axis mapping there is nothing to
//! evaluate, and §8.1 is explicit that a consumer "MUST NOT silently fall
//! back to using the raw index" — so a document with no axes can never be
//! carried across a reprocess, however much else it records.
//!
//! Nothing here is new data. `time` and `twtt` have been exported all
//! along; #144 added the declarations that say what they mean, and this
//! writes down which of them a set of picks was drawn against.

use serde::Serialize;

/// One axis of the `coordinates.axes` block.
///
/// Serialized to match SPEC §7.5's two forms rather than modelled as an
/// enum with a tag, because the shape *is* the tag: `type` names it and
/// the fields that belong to the other form are absent.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnchorAxis {
    pub name: String,
    pub unit: String,
    #[serde(rename = "type")]
    pub type_: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t0: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<Tiepoint>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interpolation: Option<&'static str>,
}

/// SPEC §7.5.2: a `(trace, value)` pair. `x` is the field name the SPEC
/// uses for the value on either axis, not a horizontal coordinate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tiepoint {
    pub trace: f64,
    pub x: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Axes {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub x: Vec<AnchorAxis>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<AnchorAxis>,
}

impl Axes {
    /// Whether anything is here to re-anchor through.
    ///
    /// An axis block with one side empty is worse than useless: §8.1 needs
    /// both, and half of one invites a consumer to believe it has a mapping.
    pub fn is_usable(&self) -> bool {
        !self.x.is_empty() && !self.y.is_empty()
    }
}

/// The most tiepoints an `x` axis will carry.
///
/// A backstop, not a target. [`reduce_tiepoints`] keeps only what the
/// tolerance demands — two for a steadily driven profile — and this stops
/// pathological timing from putting one point per trace into every
/// document saved against that radargram.
///
/// Sized from a measurement rather than a guess. A real 2529-trace profile
/// (`dronbreen-20220329`, GPS-timed at three traces a second over varying
/// ground speed) needs 214 points to stay inside half a trace, and 8.8 kB
/// is a reasonable thing to carry when the alternative is picks that cannot
/// be moved. An earlier cap of 64 was hit by that same profile and cost it
/// 1.8 traces of error — silently, which is the shape of mistake this whole
/// module exists to avoid.
pub const MAX_TIEPOINTS: usize = 1024;

/// Reduce `values` to the tiepoints needed to reproduce it by linear
/// interpolation to within `tolerance`.
///
/// Douglas–Peucker, which is exact about the thing that matters: the
/// guarantee is on the *worst* point, not the average, so a single paused
/// trace cannot be averaged away. A fixed stride would have no such
/// guarantee — it would sail straight over a gap where the operator
/// stopped, and a coordinate re-anchored across that gap would land on the
/// wrong trace while looking entirely reasonable.
///
/// Returns indices into `values`, always including the first and last.
fn reduce_tiepoints(values: &[f64], tolerance: f64) -> Vec<usize> {
    if values.len() <= 2 {
        return (0..values.len()).collect();
    }

    let mut keep = vec![false; values.len()];
    keep[0] = true;
    keep[values.len() - 1] = true;

    // Iterative rather than recursive: a 100k-trace profile with awkward
    // timing would otherwise recurse as deep as the trace count.
    let mut pending = vec![(0usize, values.len() - 1)];
    while let Some((first, last)) = pending.pop() {
        if last <= first + 1 {
            continue;
        }
        let span = (last - first) as f64;
        let rise = values[last] - values[first];
        let mut worst = 0.0;
        let mut worst_at = first;
        for i in (first + 1)..last {
            let straight = values[first] + rise * ((i - first) as f64 / span);
            let error = (values[i] - straight).abs();
            if error > worst {
                worst = error;
                worst_at = i;
            }
        }
        if worst > tolerance {
            keep[worst_at] = true;
            pending.push((first, worst_at));
            pending.push((worst_at, last));
        }
    }

    keep.iter()
        .enumerate()
        .filter_map(|(i, k)| k.then_some(i))
        .collect()
}

/// Thin an over-long tiepoint list to `MAX_TIEPOINTS`, keeping the ends.
///
/// Only reached when the timing is pathological enough that the tolerance
/// cannot be met within the cap. The result is a worse approximation, and
/// that is the trade being made deliberately: a document tens of times
/// larger than the picks it carries is its own kind of failure.
fn thin(indices: Vec<usize>) -> Vec<usize> {
    if indices.len() <= MAX_TIEPOINTS {
        return indices;
    }
    let last = indices.len() - 1;
    let step = last as f64 / (MAX_TIEPOINTS - 1) as f64;
    let mut out: Vec<usize> = (0..MAX_TIEPOINTS)
        .map(|i| indices[(i as f64 * step).round() as usize])
        .collect();
    out.dedup();
    out
}

/// Drop tiepoints that do not advance the value, as §7.5.2 requires.
///
/// The last trace is kept whatever happens: it bounds the axis, and a
/// consumer resolving past the final tiepoint is extrapolating, which
/// §7.5.2 asks it to flag. When the final point ties with the one before
/// it the earlier one gives way rather than the endpoint — same value,
/// later trace, and the set stays strictly increasing in both.
///
/// Dropping a point widens the segment around it, so the half-trace
/// guarantee does not hold *at a tie*. Measured on the real profile: one
/// trace of 2529 exceeds the tolerance, and it is the tied trace itself.
/// That is the data rather than the reduction — two traces sharing a
/// timestamp are ambiguous by one trace however they are described, and no
/// choice of tiepoints can separate them.
fn strictly_increasing(points: Vec<Tiepoint>) -> Vec<Tiepoint> {
    let mut out: Vec<Tiepoint> = Vec::with_capacity(points.len());
    let last = points.len().saturating_sub(1);
    for (i, point) in points.into_iter().enumerate() {
        match out.last() {
            Some(previous) if point.x <= previous.x => {
                if i == last {
                    out.pop();
                    out.push(point);
                }
            }
            _ => out.push(point),
        }
    }
    out
}

/// Build the `x` anchor from per-trace acquisition times, in epoch seconds.
///
/// `None` when the times are absent, too few to interpolate between, or
/// decreasing. A clock that ran backwards describes no mapping worth
/// inverting, and claiming one would be worse than having none.
///
/// Repeated timestamps are *not* a reason to refuse. §7.5.2 requires the
/// `points` to be strictly increasing, not the series they were chosen
/// from, and a radargram logging three traces a second has ties wherever
/// two land in the same tick — a real profile has exactly one such tie in
/// 2529 traces. Rejecting the whole axis over it would throw away
/// re-anchoring for the entire radargram to avoid an ambiguity of one
/// trace, which is inside the half-trace tolerance the points are chosen
/// to anyway. Ties are resolved when the points are picked.
pub fn trace_time_axis(times: &[f64]) -> Option<AnchorAxis> {
    if times.len() < 2 || times.windows(2).any(|w| w[1] < w[0]) {
        return None;
    }

    // Half a median trace interval: the tightest tolerance that still
    // means something, since a coordinate resolved to better than half a
    // trace lands on the same trace either way.
    let mut gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_by(|a, b| a.partial_cmp(b).expect("no NaN: checked increasing"));
    let tolerance = gaps[gaps.len() / 2] / 2.0;

    let points = strictly_increasing(
        thin(reduce_tiepoints(times, tolerance))
            .into_iter()
            .map(|i| Tiepoint {
                trace: i as f64,
                x: times[i],
            })
            .collect(),
    );
    if points.len() < 2 {
        return None;
    }

    Some(AnchorAxis {
        name: "trace_time".to_string(),
        unit: "s".to_string(),
        type_: "tiepoints",
        t0: None,
        dt: None,
        points: Some(points),
        interpolation: Some("linear"),
    })
}

/// Build the `y` anchor from the travel-time axis and what #144 recorded
/// about it.
///
/// `t0` is the travel-time value of sample 0, which is `twtt_crop -
/// twtt_time_zero` — neither of those variables on its own. See
/// `GPR::twtt_time_zero_ns`.
///
/// `None` when the radargram does not say what its axis means, or when the
/// offset differs per trace. A `regular` axis has one `t0`, and a mean
/// would be an offset no trace actually has — the exact error #144 removed
/// from the export. Better no anchor than a plausible wrong one.
pub fn twtt_axis(
    anchor_name: Option<&str>,
    crop_ns: &[f64],
    time_zero_ns: &[f64],
    dt_ns: f64,
) -> Option<AnchorAxis> {
    let name = anchor_name?;
    if !dt_ns.is_finite() || dt_ns <= 0.0 {
        return None;
    }

    // Scalars in the file read back as a single value; per-trace ones as
    // one each. Either way the offset has to be the same everywhere.
    let mut offsets = crop_ns
        .iter()
        .zip(time_zero_ns.iter())
        .map(|(crop, zero)| crop - zero);
    let first = offsets.next()?;
    if !offsets.all(|offset| (offset - first).abs() < f64::EPSILON) {
        return None;
    }
    // A scalar for one and a vector for the other cannot be compared
    // element-wise, and a length mismatch means exactly that.
    if crop_ns.len() != time_zero_ns.len() {
        return None;
    }

    Some(AnchorAxis {
        name: name.to_string(),
        unit: "ns".to_string(),
        type_: "regular",
        t0: Some(first),
        dt: Some(dt_ns),
        points: None,
        interpolation: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steadily_driven_profile_needs_two_tiepoints() {
        // Evenly spaced times are exactly linear, so everything between the
        // ends is redundant. This is the ordinary case, and it is what
        // keeps the block small enough to put in every document.
        let times: Vec<f64> = (0..2500)
            .map(|i| 1_700_000_000.0 + i as f64 * 0.1)
            .collect();
        let axis = trace_time_axis(&times).unwrap();
        let points = axis.points.unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].trace, 0.0);
        assert_eq!(points[1].trace, 2499.0);
    }

    #[test]
    fn a_pause_in_the_survey_becomes_a_tiepoint() {
        // The case a fixed stride would sail over: the operator stopped for
        // a minute at trace 1200. Interpolating across it puts every later
        // trace at the wrong time, and a coordinate re-anchored through
        // that lands on the wrong trace while looking entirely reasonable.
        let mut times: Vec<f64> = (0..2500)
            .map(|i| 1_700_000_000.0 + i as f64 * 0.1)
            .collect();
        for t in times.iter_mut().skip(1200) {
            *t += 60.0;
        }

        let axis = trace_time_axis(&times).unwrap();
        let points = axis.points.unwrap();
        assert!(points.len() >= 3, "the pause must survive: {points:?}");

        // Reproduced to within half a trace interval everywhere, which is
        // the promise -- and the pause is where it would otherwise break.
        for (i, want) in times.iter().enumerate() {
            let got = interpolate(&points, i as f64);
            assert!(
                (got - want).abs() <= 0.05,
                "trace {i}: {got} vs {want}, {} points",
                points.len()
            );
        }
    }

    /// What a consumer does with the axis: linear between tiepoints.
    fn interpolate(points: &[Tiepoint], trace: f64) -> f64 {
        let after = points
            .iter()
            .position(|p| p.trace >= trace)
            .unwrap_or(points.len() - 1)
            .max(1);
        let (a, b) = (&points[after - 1], &points[after]);
        a.x + (b.x - a.x) * (trace - a.trace) / (b.trace - a.trace)
    }

    #[test]
    fn pathological_timing_is_capped_rather_than_written_out_in_full() {
        // A survey stopped and restarted every few traces cannot be
        // described within the tolerance by any small set of points. The
        // cap is the deliberate trade: a worse approximation over a
        // document tens of times larger than the picks it carries.
        let times: Vec<f64> = (0..4000)
            .map(|i| 1_700_000_000.0 + i as f64 * 0.1 + if i % 3 == 0 { 5.0 } else { 0.0 })
            .collect();
        let axis = trace_time_axis(&times);
        if let Some(axis) = axis {
            assert!(axis.points.unwrap().len() <= MAX_TIEPOINTS);
        }
    }

    #[test]
    fn one_repeated_timestamp_does_not_cost_the_whole_axis() {
        // A real radargram: 2529 traces at three a second, with exactly one
        // pair landing in the same tick. An earlier version of this
        // required the whole series to be strictly increasing and threw
        // away re-anchoring for the entire profile over that one tie --
        // an ambiguity of one trace, inside the half-trace tolerance the
        // tiepoints are chosen to anyway.
        let mut times: Vec<f64> = (0..2529)
            .map(|i| 1_648_557_660.0 + i as f64 / 3.0)
            .collect();
        times[1000] = times[999];

        let axis = trace_time_axis(&times).expect("one tie is not a reason to refuse");
        let points = axis.points.unwrap();
        assert!(points.len() >= 2);
        // §7.5.2's requirement is on the points, and it holds.
        assert!(
            points
                .windows(2)
                .all(|w| w[1].x > w[0].x && w[1].trace > w[0].trace),
            "{points:?}"
        );
    }

    #[test]
    fn a_clock_that_does_not_advance_gets_no_axis() {
        // Strictly increasing points cannot be chosen from a series that
        // never advances, and an axis that cannot be inverted re-anchors
        // nothing.
        let stalled = vec![1_700_000_000.0; 8];
        assert!(trace_time_axis(&stalled).is_none());

        let backwards = vec![1_700_000_000.0, 1_700_000_002.0, 1_700_000_001.0];
        assert!(trace_time_axis(&backwards).is_none());

        assert!(trace_time_axis(&[1_700_000_000.0]).is_none());
        assert!(trace_time_axis(&[]).is_none());
    }

    #[test]
    fn the_travel_time_anchor_is_the_difference_not_either_variable() {
        // The gprinterp `t0` is the travel-time value of sample 0. For an
        // ordinarily zero-corrected radargram the crop landed on time zero
        // and it is 0; cropping further without re-zeroing makes it
        // positive.
        let axis = twtt_axis(Some("twtt"), &[37.22], &[37.22], 1.2407).unwrap();
        assert_eq!(axis.name, "twtt");
        assert_eq!(axis.type_, "regular");
        assert_eq!(axis.t0, Some(0.0));
        assert_eq!(axis.dt, Some(1.2407));

        let subsetted = twtt_axis(Some("twtt"), &[99.254], &[37.22], 1.2407).unwrap();
        assert!((subsetted.t0.unwrap() - 62.034).abs() < 1e-9);
    }

    #[test]
    fn a_per_trace_zero_correction_still_gives_one_offset() {
        // `zero_corr_max_peak` crops each trace differently and sets time
        // zero to where each crop landed, so the difference is zero
        // everywhere. A per-trace *correction* is not a per-trace *anchor*.
        let crop = vec![40.0, 41.2, 39.6, 42.8];
        let axis = twtt_axis(Some("twtt"), &crop, &crop, 0.4).unwrap();
        assert_eq!(axis.t0, Some(0.0));
    }

    #[test]
    fn an_offset_that_differs_per_trace_gets_no_anchor() {
        // A `regular` axis has one `t0`. A mean would be an offset no trace
        // actually has, which is the error #144 took out of the export;
        // putting it back into the document would be worse, since a
        // document outlives the file it was drawn on.
        let crop = vec![40.0, 41.2, 39.6];
        let zero = vec![40.0, 40.0, 40.0];
        assert!(twtt_axis(Some("twtt"), &crop, &zero, 0.4).is_none());
    }

    #[test]
    fn a_radargram_that_does_not_say_what_its_axis_means_gets_no_anchor() {
        // Processed before #144. Guessing `twtt` would re-anchor a
        // normal-incidence axis onto an antenna-pair one, which is the
        // whole thing gprinterp#9 exists to prevent.
        assert!(twtt_axis(None, &[40.0], &[40.0], 0.4).is_none());
        assert!(twtt_axis(Some("twtt"), &[40.0], &[40.0], 0.0).is_none());
    }

    #[test]
    fn half_an_axis_block_is_not_usable() {
        // §8.1 needs both. Half of one invites a consumer to believe it has
        // a mapping.
        let axis = twtt_axis(Some("twtt"), &[0.0], &[0.0], 0.4).unwrap();
        assert!(!Axes {
            x: Vec::new(),
            y: vec![axis.clone()],
        }
        .is_usable());
        assert!(!Axes::default().is_usable());
    }
}
