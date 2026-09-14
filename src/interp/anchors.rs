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

/// One axis of `coordinates.axes`, as SPEC §7.4 shapes it.
///
/// An object with the anchor mappings under `anchor`, **not** a bare array
/// of them. The first version of this emitted the array, which gprinterp
/// rejects outright with `invalid type: sequence, expected struct Axis` --
/// so every save of a radargram that could describe its axes returned 400,
/// and the feature was broken in exactly the case it existed for.
///
/// The tests missed it because they checked that the server *offered* the
/// axes and never that a document carrying them could be *saved*. The
/// round-trip test below is the one that was missing.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Axis {
    /// Anchor mappings, in the producer's preference order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub anchor: Vec<AnchorAxis>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Axes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<Axis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<Axis>,
}

impl Axes {
    /// Whether anything is here to re-anchor through.
    ///
    /// An axis block with one side empty is worse than useless: §8.1 needs
    /// both, and half of one invites a consumer to believe it has a mapping.
    pub fn is_usable(&self) -> bool {
        let filled = |axis: &Option<Axis>| axis.as_ref().is_some_and(|a| !a.anchor.is_empty());
        filled(&self.x) && filled(&self.y)
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
/// interpolation to within `tolerance[i]` at each point.
///
/// Per point, not one figure for the series. The tolerance is expressed in
/// seconds and the thing being bounded is an error in *traces*, and the two
/// are only interchangeable where the rate is constant. A profile that
/// mostly moves at a second per trace and has a stretch at a millisecond
/// per trace would, under half the global median, allow half a second of
/// error inside that stretch -- five hundred traces.
///
/// Douglas–Peucker, which is exact about the thing that matters: the
/// guarantee is on the *worst* point, not the average, so a single paused
/// trace cannot be averaged away. A fixed stride would have no such
/// guarantee — it would sail straight over a gap where the operator
/// stopped, and a coordinate re-anchored across that gap would land on the
/// wrong trace while looking entirely reasonable.
///
/// Returns indices into `values`, always including the first and last.
fn reduce_tiepoints(values: &[f64], tolerance: &[f64]) -> Vec<usize> {
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
        // Ranked by how far past its *own* tolerance each point is, not by
        // absolute error. A point in a fast stretch may be off by less in
        // seconds and far more in traces, and traces are the unit that
        // matters.
        let mut worst = 0.0;
        let mut worst_at = first;
        for i in (first + 1)..last {
            let straight = values[first] + rise * ((i - first) as f64 / span);
            let excess = (values[i] - straight).abs() / tolerance[i];
            if excess > worst {
                worst = excess;
                worst_at = i;
            }
        }
        if worst > 1.0 {
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
    // Non-finite first. `w[1] < w[0]` is *false* for a NaN pair, so a NaN
    // sails past the monotonicity check and reaches the `partial_cmp`
    // below, where it panics -- taking down the page render of a lenient
    // reader whose whole promise is to fall back to no anchor.
    if times.iter().any(|t| !t.is_finite()) {
        return None;
    }
    if times.len() < 2 || times.windows(2).any(|w| w[1] < w[0]) {
        return None;
    }

    // Half a trace, locally: a coordinate resolved to better than half a
    // trace lands on the same trace either way, and "a trace" is however
    // long one took *there* rather than on average.
    let gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let mut sorted: Vec<f64> = gaps.iter().copied().filter(|g| *g > 0.0).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN: rejected above"));
    // Where every gap is zero the series does not advance at all, and the
    // strictness filter below will reject it; any positive number keeps the
    // arithmetic well behaved until then.
    let typical = sorted.get(sorted.len() / 2).copied().unwrap_or(1.0);
    let tolerance: Vec<f64> = (0..times.len())
        .map(|i| {
            // The shorter of the two intervals this point sits between,
            // ignoring ties -- two traces sharing a timestamp say nothing
            // about how fast the profile was moving.
            let before = i.checked_sub(1).and_then(|j| gaps.get(j)).copied();
            let after = gaps.get(i).copied();
            let local = [before, after]
                .into_iter()
                .flatten()
                .filter(|g| *g > 0.0)
                .fold(f64::INFINITY, f64::min);
            if local.is_finite() {
                local / 2.0
            } else {
                typical / 2.0
            }
        })
        .collect();

    let points = strictly_increasing(
        thin(reduce_tiepoints(times, &tolerance))
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

    // Non-finite first, and before the tolerance is computed from them.
    // A single NaN is the dangerous shape: with one trace, `offsets.next()`
    // takes it and `offsets.all(...)` is then vacuously true over an empty
    // rest, so the axis came back with `t0 = NaN` -- which the snapshot
    // path would persist as a mapping and report as `has_axes: true`,
    // while every coordinate evaluated through it is NaN.
    if crop_ns.iter().chain(time_zero_ns).any(|v| !v.is_finite()) {
        return None;
    }

    // A scalar for one and a vector for the other cannot be compared
    // element-wise, and a length mismatch means exactly that. Checked
    // before the zip, which would otherwise truncate to the shorter and
    // compare a prefix.
    if crop_ns.len() != time_zero_ns.len() {
        return None;
    }

    // Both variables are stored as `f32` and widened on the way in, so two
    // mathematically identical offsets can differ by an ulp of the *f32*
    // they came from -- around 1e-5 ns at a hundred nanoseconds, which is
    // 1e11 times `f64::EPSILON`. Comparing at f64 precision rejected axes
    // that are uniform in every sense that matters and left those
    // radargrams with no anchor at all, which is the one outcome #148
    // cannot work with.
    //
    // Scaled to the axis rather than fixed, because "a difference too small
    // to be real" depends on how large the numbers are. `dt_ns` floors the
    // scale so a radargram whose offsets are all near zero still gets a
    // usable tolerance, and the few ulps of headroom cover the rounding
    // accumulated across store, widen and subtract. Real per-trace
    // structure -- a zero correction that actually moves between traces --
    // differs by a visible fraction of a sample and is still rejected, by
    // a margin of some six orders of magnitude.
    let scale = crop_ns
        .iter()
        .chain(time_zero_ns.iter())
        .fold(dt_ns.abs(), |m, v| m.max(v.abs()));
    let tolerance = f64::from(f32::EPSILON) * scale * 8.0;

    // Scalars in the file read back as a single value; per-trace ones as
    // one each. Either way the offset has to be the same everywhere.
    let mut offsets = crop_ns
        .iter()
        .zip(time_zero_ns.iter())
        .map(|(crop, zero)| crop - zero);
    let first = offsets.next()?;
    if !offsets.all(|offset| (offset - first).abs() <= tolerance) {
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

/// The anchor axes a radargram currently offers, as SPEC §7.4 shapes them.
///
/// The revision id is checked against the file rather than trusted. The id
/// beside these axes comes from the catalog snapshot; the axes come from
/// the file as it is right now. If something reprocessed it in place since,
/// returning them anyway would hand out one revision's mapping labelled
/// with another's id — the exact cross-revision mistake this whole feature
/// exists to prevent, produced by the feature itself. A file that has
/// moved on offers nothing until the catalog is rebuilt.
pub fn axes_for_revision(
    path: &std::path::Path,
    radargram_id: &crate::identity::RadargramId,
    revision_id: &crate::identity::RevisionId,
) -> Axes {
    let declared = crate::interp::source::read_axis_declarations(path);
    let same_revision = declared.processing_datetime.as_deref().is_some_and(|when| {
        &crate::identity::RevisionId::fingerprint_v1(radargram_id, when) == revision_id
    });
    if !same_revision {
        eprintln!(
            "Warning: {radargram_id} changed on disk since it was catalogued; serving it \
             without anchor axes until the catalog is rebuilt."
        );
        return Axes::default();
    }
    axes_from_declarations(&declared)
}

/// The same, for declarations already read and already trusted.
pub fn axes_from_declarations(declared: &crate::interp::source::AxisDeclarations) -> Axes {
    let wrap = |anchor: Option<AnchorAxis>| {
        anchor.map(|anchor| Axis {
            anchor: vec![anchor],
        })
    };
    Axes {
        x: wrap(trace_time_axis(&declared.time)),
        y: wrap(twtt_axis(
            declared.twtt_anchor.as_deref(),
            &declared.twtt_crop,
            &declared.twtt_time_zero,
            declared.dt_ns,
        )),
    }
}

/// The axis values an #148 snapshot keeps, on the anchor's scale.
///
/// The `y` values are travel time from time zero — the stored `twtt` plus
/// the anchor offset, not the array as the file holds it, which starts at
/// zero whether or not sample zero is time zero (#153). Taking the offset
/// out here means a later fix to that array changes nothing about what a
/// snapshot means.
///
/// `None` when the revision cannot describe its axes, which is the same
/// condition that stops a document carrying them. A snapshot with no
/// mapping in it is a file that says nothing.
pub fn snapshot_values(
    declared: &crate::interp::source::AxisDeclarations,
) -> Option<(Option<String>, Vec<f64>, Vec<f64>)> {
    let y = twtt_axis(
        declared.twtt_anchor.as_deref(),
        &declared.twtt_crop,
        &declared.twtt_time_zero,
        declared.dt_ns,
    )?;
    let t0 = y.t0?;
    let dt = y.dt?;
    if declared.n_samples == 0 {
        return None;
    }
    // Validated through the same function that builds the live axis, not
    // merely counted. A count says two timestamps arrived; it does not say
    // they advance, are finite, or can be inverted -- and a snapshot of
    // times that cannot be inverted is a mapping that reports
    // `has_axes: true` and relates nothing. What is *stored* is still the
    // raw series, because tiepoint reduction is a lossy summary and a
    // snapshot is the thing later revisions are related through.
    trace_time_axis(&declared.time)?;
    Some((
        Some(y.name),
        (0..declared.n_samples)
            .map(|i| t0 + i as f64 * dt)
            .collect(),
        declared.time.clone(),
    ))
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
    fn a_fast_curving_stretch_is_resolved_in_traces_rather_than_in_seconds() {
        // A long slow section sets the median, then a fast section whose
        // rate *ramps*. The curvature there is a fraction of a second --
        // comfortably inside half the median gap, so a single global
        // tolerance places no point in it -- while being tens of traces at
        // the rate it is actually moving.
        //
        // Measured: the global tolerance leaves 40.5 traces of error here,
        // the local one 0.5. A sharp rate *change* would not show it, since
        // the kink is a huge deviation in seconds and gets a tiepoint on
        // its own; it takes a smooth one to slip past.
        let mut times = Vec::new();
        let mut clock = 1_700_000_000.0;
        let mut gaps = Vec::new();
        gaps.extend(std::iter::repeat_n(1.0, 800));
        gaps.extend((0..300).map(|i| 0.001 + (0.02 - 0.001) * i as f64 / 299.0));
        gaps.extend(std::iter::repeat_n(1.0, 400));
        times.push(clock);
        for gap in &gaps {
            clock += gap;
            times.push(clock);
        }

        let axis = trace_time_axis(&times).unwrap();
        let points = axis.points.unwrap();

        let mut worst = 0.0f64;
        for (i, actual) in times.iter().enumerate() {
            let after = points
                .iter()
                .position(|p| p.trace >= i as f64)
                .unwrap_or(points.len() - 1)
                .max(1);
            let (a, b) = (&points[after - 1], &points[after]);
            let estimate = a.x + (b.x - a.x) * (i as f64 - a.trace) / (b.trace - a.trace);
            // In traces, at the rate this part of the profile was moving.
            let local = gaps
                .get(i.saturating_sub(1))
                .copied()
                .unwrap_or(1.0)
                .min(gaps.get(i).copied().unwrap_or(1.0));
            worst = worst.max((estimate - actual).abs() / local);
        }
        assert!(
            worst <= 1.0,
            "worst error was {worst:.1} traces, with {} points",
            points.len()
        );
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
    fn a_time_series_with_no_number_in_it_gets_no_axis_rather_than_a_panic() {
        // `w[1] < w[0]` is false for a NaN pair, so a NaN used to sail past
        // the monotonicity check and reach `partial_cmp`, which panics --
        // and it did so while rendering the viewer page, for a reader whose
        // entire promise is to fall back to no anchor.
        let mut times: Vec<f64> = (0..64).map(|i| 1_700_000_000.0 + i as f64).collect();
        times[10] = f64::NAN;
        assert!(trace_time_axis(&times).is_none());

        times[10] = f64::INFINITY;
        assert!(trace_time_axis(&times).is_none());

        assert!(trace_time_axis(&[f64::NAN, f64::NAN]).is_none());
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
    fn an_f32_rounding_difference_is_not_a_per_trace_offset() {
        // Both variables are stored as `f32` and widened on the way in, so
        // two mathematically identical offsets differ by an ulp of the f32
        // they came from. At `f64::EPSILON` those radargrams got no anchor
        // at all -- and a radargram with no anchor is the one case #148
        // cannot carry forward.
        let exact = 62.034_f64;
        let crop: Vec<f64> = (0..64)
            .map(|i| f64::from((exact + f64::from(i) * 1.2407) as f32))
            .collect();
        let zero: Vec<f64> = (0..64)
            .map(|i| f64::from((f64::from(i) * 1.2407) as f32))
            .collect();

        // The differences are real at f64 precision -- this is not a test
        // that happens to compare equal numbers.
        let offsets: Vec<f64> = crop.iter().zip(&zero).map(|(c, z)| c - z).collect();
        let spread = offsets
            .iter()
            .fold(0.0_f64, |m, o| m.max((o - offsets[0]).abs()));
        assert!(spread > f64::EPSILON, "spread was {spread:e}");

        let axis = twtt_axis(Some("twtt"), &crop, &zero, 1.2407).expect("a uniform offset");
        assert!(
            (axis.t0.unwrap() - exact).abs() < 1e-4,
            "t0 was {:?}",
            axis.t0
        );
    }

    #[test]
    fn a_real_per_trace_offset_is_still_rejected_at_the_wider_tolerance() {
        // The tolerance is scaled to the axis, so it has to stay far below
        // anything a zero correction would actually produce. A thousandth
        // of a sample is already six orders above an f32 ulp here.
        let crop: Vec<f64> = (0..64).map(|i| 62.034 + f64::from(i) * 0.001).collect();
        let zero = vec![0.0; 64];
        assert!(twtt_axis(Some("twtt"), &crop, &zero, 1.2407).is_none());
    }

    #[test]
    fn a_single_non_finite_trace_does_not_become_a_nan_anchor() {
        // The dangerous shape is one trace, not many. `offsets.next()`
        // consumed the sole NaN and `offsets.all(...)` was then vacuously
        // true over an empty rest, so the axis came back with `t0 = NaN` --
        // persisted by the snapshot path as a mapping, reported as
        // `has_axes: true`, and evaluating every coordinate to NaN.
        assert!(twtt_axis(Some("twtt"), &[f64::NAN], &[4.0], 0.4).is_none());
        assert!(twtt_axis(Some("twtt"), &[4.0], &[f64::NAN], 0.4).is_none());
        assert!(twtt_axis(Some("twtt"), &[f64::INFINITY], &[4.0], 0.4).is_none());
        // And in a longer series, where it was already caught.
        assert!(twtt_axis(Some("twtt"), &[4.0, f64::NAN], &[1.0, 1.0], 0.4).is_none());
    }

    #[test]
    fn a_snapshot_needs_times_that_can_actually_be_inverted() {
        // Counting the timestamps says two arrived. It does not say they
        // advance, are finite, or can be inverted -- and a snapshot of
        // times that cannot be inverted is a mapping that relates nothing
        // while reporting `has_axes: true`.
        let declared = |time: Vec<f64>| crate::interp::source::AxisDeclarations {
            time,
            twtt_anchor: Some("twtt".to_string()),
            twtt_crop: vec![4.0],
            twtt_time_zero: vec![4.0],
            dt_ns: 0.4,
            n_samples: 16,
            processing_datetime: None,
        };
        assert!(snapshot_values(&declared(vec![0.0, 1.0, 2.0])).is_some());
        assert!(
            snapshot_values(&declared(vec![2.0, 1.0, 0.0])).is_none(),
            "time running backwards cannot be inverted"
        );
        assert!(snapshot_values(&declared(vec![0.0, f64::NAN])).is_none());
        assert!(
            snapshot_values(&declared(vec![5.0, 5.0, 5.0])).is_none(),
            "every trace at one instant relates nothing to anything"
        );
        // A repeated timestamp inside an advancing series is ordinary --
        // two traces can share a GPS second -- and stays usable.
        assert!(snapshot_values(&declared(vec![0.0, 1.0, 1.0, 2.0])).is_some());
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
            x: None,
            y: Some(Axis {
                anchor: vec![axis.clone()]
            }),
        }
        .is_usable());
        assert!(!Axes::default().is_usable());
        // An axis that exists but names no anchor is the same nothing.
        assert!(!Axes {
            x: Some(Axis::default()),
            y: Some(Axis { anchor: vec![axis] }),
        }
        .is_usable());
    }

    #[test]
    fn what_we_emit_is_what_gprinterp_accepts() {
        // The test that was missing. Everything else here checked that the
        // right *numbers* were produced; nothing checked that a document
        // carrying them could be read back by the crate that has to read
        // it. The first version emitted `axes.x` as a bare array and
        // gprinterp rejected it with "invalid type: sequence, expected
        // struct Axis" -- so every save returned 400, on exactly the
        // radargrams the feature was for.
        let axes = Axes {
            x: Some(Axis {
                anchor: vec![
                    trace_time_axis(&[1_700_000_000.0, 1_700_000_001.0, 1_700_000_002.0]).unwrap(),
                ],
            }),
            y: Some(Axis {
                anchor: vec![twtt_axis(Some("twtt"), &[40.0], &[40.0], 0.4).unwrap()],
            }),
        };
        let document = serde_json::json!({
            "schema": "gprinterp",
            "schema_version": "0.1",
            "key": "line-01",
            "features": [],
            "coordinates": { "axes": axes },
        });

        let parsed: gprinterp::Document = serde_json::from_value(document)
            .expect("the document Ridal writes must be one gprinterp can read");
        let report = gprinterp::validate(&parsed);
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        // And the anchors survive the round trip, rather than parsing into
        // an empty shell that happens to be valid.
        let coordinates = parsed.coordinates.expect("coordinates");
        let axes = coordinates.axes.expect("axes");
        assert_eq!(axes.x.expect("x").anchors()[0].name, "trace_time");
        assert_eq!(axes.y.expect("y").anchors()[0].name, "twtt");
    }
}
