//! Showing an interpretation on a revision it was not drawn on (#148).
//!
//! ```text
//! GET /api/v1/datasets/{id}/interpretations/{user}/carried
//! ```
//!
//! A radargram can be reprocessed. The picks drawn on the old revision are
//! still what somebody meant, but the coordinates in them index the *old*
//! grid, and drawing them against the new one puts them wherever the two
//! happen to disagree. gprinterp SPEC §8.1 carries a coordinate across by
//! evaluating it through the axes it was drawn against and inverting the
//! axes it is being read against, which is what
//! [`gprinterp::reanchor`] does.
//!
//! # Nothing is written
//!
//! This is the read half of #148. The stored document is never touched:
//! what comes back is a *view* of it on the current revision, and the
//! picker refuses to save a document whose `source.revision_id` is not the
//! one it is looking at. Promoting a carried document to the authored one
//! is a deliberate act with a consequence report in front of it, and that
//! is the next piece of work rather than this one.
//!
//! Keeping the two apart is the whole safety property. A carried view can
//! be wrong — the tiers below exist precisely because it can be — and a
//! wrong view is a thing you look at and disbelieve, while a wrong stored
//! document is a thing you find out about later.
//!
//! # Why tiers rather than a boolean
//!
//! "Did it work" has more than two answers here, and they have genuinely
//! different consequences for the person looking at the screen:
//!
//! - a pick that moved by a hundredth of a trace is the same pick;
//! - a `y` carried across an antenna-separation correction is off by a
//!   real distance that SPEC §8.3 forbids presenting as exact;
//! - a pick that fell outside the new revision is *gone from the view*,
//!   and someone counting layers needs to know one is missing;
//! - a document with no shared anchor cannot be placed at all, and the
//!   honest answer is a refusal rather than a best effort.
//!
//! Collapsing those into "approximate" would put the first and the third
//! behind the same word.

use serde::Serialize;

use crate::interp::anchors::Axes;

/// How much the carried view can be trusted, worst first when reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Drawn on this revision. Nothing was carried and nothing can be
    /// wrong.
    Current,
    /// Carried, with every feature placed and nothing moved far enough to
    /// land on a different trace or sample.
    ///
    /// Still not *exact*: SPEC §8.3 forbids presenting a carried `y` as
    /// exact however small the displacement, because the two revisions may
    /// measure travel time against different things. This tier says the
    /// picks are where they were, not that the depths are.
    Carried,
    /// Carried, every feature placed, but coordinates moved by enough to
    /// matter — more than half an index unit, so a pick draws on a
    /// different trace or sample than it was put on.
    Approximate,
    /// Carried, and some features fell outside the new revision. §8.1 is
    /// explicit that these are dropped and counted, never clamped to the
    /// edge: a pick pushed onto the boundary is a pick in a place nobody
    /// put it.
    Partial,
    /// Not carried at all. There is no shared anchor axis, or the document
    /// carries no axes to carry it through.
    Refused,
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the callers arrive with the promotion gate (F3 of #148): \
                  deciding whether a carried view may be written down means \
                  asking both of these, and the browser asks them of the \
                  serialized tier rather than through Rust"
    )
)]
impl Severity {
    /// Whether there is a view to draw at all.
    pub fn has_view(self) -> bool {
        !matches!(self, Severity::Refused)
    }

    /// Whether the coordinates may be presented as exact.
    ///
    /// True only for the revision the picks were drawn on. SPEC §8.3 is
    /// unconditional about this: a `y` carried across revisions is never
    /// exact, however small the displacement, because the two revisions
    /// may not measure travel time against the same thing. A depth read
    /// off a carried view is an estimate.
    pub fn is_exact(self) -> bool {
        matches!(self, Severity::Current)
    }
}

/// How far a coordinate may move and still be the same pick.
///
/// Half an index unit: below this a vertex draws on the same trace and the
/// same sample it was put on, so nothing has visibly moved. Above it, the
/// pick is somewhere else on screen, which is worth saying even when every
/// feature was placed.
const SAME_PLACE: f64 = 0.5;

/// A feature that could not be placed on the new revision.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dropped {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// How far the carried coordinates moved, in index units.
///
/// Traces and samples rather than metres or nanoseconds, because that is
/// what the viewer draws in and what "did this pick move" means on screen.
/// The median says whether the revisions broadly agree; the worst says
/// whether any single vertex went somewhere else, which an average would
/// hide behind a thousand that did not.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Displacement {
    pub median_traces: f64,
    pub worst_traces: f64,
    pub median_samples: f64,
    pub worst_samples: f64,
}

/// What carrying this document onto this revision did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarryReport {
    pub severity: Severity,
    /// The revision the document says it was drawn on, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_revision: Option<String>,
    pub to_revision: String,
    /// Which anchor axis each side was carried through, when it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y_anchor: Option<String>,
    pub kept: usize,
    pub dropped: Vec<Dropped>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved: Option<Displacement>,
    /// Why it could not be carried. Set exactly when the severity is
    /// [`Severity::Refused`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// One sentence for the banner, written for whoever is looking at the
    /// radargram rather than for a log.
    pub headline: String,
}

/// The carried view, and what it cost.
#[derive(Debug, Clone)]
pub struct Carried {
    /// The document as it should be *drawn*. `None` when refused.
    pub document: Option<gprinterp::Document>,
    pub report: CarryReport,
}

/// The revision a document says it was drawn on.
fn source_revision(document: &gprinterp::Document) -> Option<String> {
    document.source.as_ref().and_then(|s| s.revision_id.clone())
}

/// Convert ridal's axis block into the target description gprinterp wants.
///
/// Through JSON rather than field by field. Both sides model SPEC §7.5 and
/// ridal's type exists to *serialize* to it, so the JSON is the shared
/// definition; a hand-written conversion would be a second, silently
/// diverging copy of a shape the SPEC already fixes.
fn target_axes(axes: &Axes) -> Result<gprinterp::RevisionAxes, &'static str> {
    let pull = |axis: &Option<crate::interp::anchors::Axis>| -> Vec<gprinterp::AnchorAxis> {
        axis.as_ref()
            .map(|axis| {
                axis.anchor
                    .iter()
                    .filter_map(|a| {
                        serde_json::to_value(a)
                            .ok()
                            .and_then(|v| serde_json::from_value(v).ok())
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let x = pull(&axes.x);
    let y = pull(&axes.y);
    // Named separately, because the two are missing for different reasons
    // and only one of them is common. A `y` axis is absent when time zero
    // has never been located — no zero correction has run — and "this
    // radargram does not describe its axes" would send someone looking for
    // a problem in the wrong place.
    match (x.is_empty(), y.is_empty()) {
        (false, false) => Ok(gprinterp::RevisionAxes { x, y }),
        (true, true) => Err("this revision describes neither of its axes"),
        (true, false) => Err("this revision does not describe its trace axis"),
        (false, true) => Err(
            "this revision has no anchored travel-time axis: time zero has never been \
             located on it, so its samples are counted from whenever the instrument \
             started recording rather than from the transmitted pulse",
        ),
    }
}

/// Give a document the axes a snapshot preserved, when it carries none.
///
/// The case this exists for: interpretations saved before the picker
/// emitted `coordinates.axes` at all. They record which revision they were
/// drawn on and nothing about what its indices meant, so §8.1 has nothing
/// to evaluate them through and refuses — permanently, for picks that are
/// otherwise perfectly good.
///
/// #148's snapshots are exactly the missing half. A superseded revision's
/// mapping is kept precisely so a document drawn on it can still be
/// placed, and rescuing documents that never carried their own axes is
/// named there as one of the reasons for keeping them.
///
/// An `explicit` mapping, one value per index, because that is what a
/// snapshot is: the axis as it stood, not a summary of it. A document that
/// already carries its own axes is returned untouched — what it says about
/// itself beats what was inferred about it.
pub fn with_snapshot_axes(
    document: &gprinterp::Document,
    snapshot: &crate::project::revisions::AxisSnapshot,
) -> gprinterp::Document {
    let already = document
        .coordinates
        .as_ref()
        .and_then(|c| c.axes.as_ref())
        .is_some_and(|axes| {
            !axes
                .x
                .as_ref()
                .map(|a| a.anchors())
                .unwrap_or(&[])
                .is_empty()
                && !axes
                    .y
                    .as_ref()
                    .map(|a| a.anchors())
                    .unwrap_or(&[])
                    .is_empty()
        });
    if already {
        return document.clone();
    }
    let Some(y_anchor) = snapshot.y_anchor.as_deref() else {
        // A snapshot that never recorded which anchor its values are is
        // not a mapping anything may be carried through: the numbers alone
        // cannot say whether they are travel time or a recording clock,
        // and guessing is the mistake §8.3 is about.
        return document.clone();
    };
    if snapshot.x_values.len() < 2 || snapshot.y_values.len() < 2 {
        return document.clone();
    }

    let explicit = |name: &str, values: &[f64], unit: &str| {
        serde_json::json!({
            "name": name,
            "unit": unit,
            "type": "explicit",
            "values": values,
        })
    };
    let axes = serde_json::json!({
        "x": {"anchor": [explicit("trace_time", &snapshot.x_values, "s")]},
        "y": {"anchor": [explicit(y_anchor, &snapshot.y_values, "ns")]},
    });

    let mut out = document.clone();
    let coordinates = out.coordinates.get_or_insert_with(Default::default);
    coordinates.axes = serde_json::from_value(axes).ok();
    out
}

/// Every `(x, y)` vertex of every feature, in order.
fn vertices(document: &gprinterp::Document) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for feature in &document.features {
        for position in feature.geometry.positions() {
            if let (Some(x), Some(y)) = (position.x(), position.y()) {
                out.push((x, y));
            }
        }
    }
    out
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        return values[middle];
    }
    // The mean of the two middle values, not the upper one. A two-vertex
    // line displaced by 0 and 10 has a typical movement of 5, and
    // reporting 10 overstates it — on the number the banner offers as the
    // ordinary case rather than the worst.
    (values[middle - 1] + values[middle]) / 2.0
}

/// How far the kept vertices moved.
///
/// Only meaningful when nothing was dropped: a drop changes which vertices
/// exist, so the two lists stop corresponding and pairing them by position
/// would compare unrelated points. A partial carry reports its drops
/// instead, which is the more serious fact anyway.
fn displacement(before: &gprinterp::Document, after: &gprinterp::Document) -> Option<Displacement> {
    let (from, to) = (vertices(before), vertices(after));
    if from.len() != to.len() || from.is_empty() {
        return None;
    }
    let dx: Vec<f64> = from
        .iter()
        .zip(&to)
        .map(|(a, b)| (b.0 - a.0).abs())
        .collect();
    let dy: Vec<f64> = from
        .iter()
        .zip(&to)
        .map(|(a, b)| (b.1 - a.1).abs())
        .collect();
    let worst = |v: &[f64]| v.iter().copied().fold(0.0_f64, f64::max);
    Some(Displacement {
        worst_traces: worst(&dx),
        worst_samples: worst(&dy),
        median_traces: median(dx),
        median_samples: median(dy),
    })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Show `document` on the revision `axes` describes.
///
/// `to_revision` is the revision those axes belong to, already checked
/// against the file by [`crate::interp::anchors::axes_for_revision`].
pub fn carry(document: &gprinterp::Document, axes: &Axes, to_revision: &str) -> Carried {
    let from_revision = source_revision(document);
    let total = document.features.len();

    let refuse = |reason: String, headline: String| Carried {
        document: None,
        report: CarryReport {
            severity: Severity::Refused,
            from_revision: from_revision.clone(),
            to_revision: to_revision.to_string(),
            x_anchor: None,
            y_anchor: None,
            kept: 0,
            dropped: Vec::new(),
            moved: None,
            refusal: Some(reason),
            headline,
        },
    };

    // Drawn on this revision: there is nothing to carry, and saying so is
    // not the same as saying a carry succeeded.
    if from_revision.as_deref() == Some(to_revision) {
        return Carried {
            document: Some(document.clone()),
            report: CarryReport {
                severity: Severity::Current,
                from_revision,
                to_revision: to_revision.to_string(),
                x_anchor: None,
                y_anchor: None,
                kept: total,
                dropped: Vec::new(),
                moved: None,
                refusal: None,
                headline: "These picks were drawn on the revision you are looking at.".to_string(),
            },
        };
    }

    let target = match target_axes(axes) {
        Ok(target) => target,
        Err(why) => {
            return refuse(
                why.to_string(),
                format!(
                    "These picks were drawn on an earlier version and cannot be placed \
                     on this one, because {why}. They are kept exactly as drawn."
                ),
            )
        }
    };

    let outcome = match gprinterp::reanchor(document, &target) {
        Ok(outcome) => outcome,
        Err(e) => {
            return refuse(
                e.to_string(),
                format!(
                    "These picks were drawn on an earlier revision and cannot be \
                     placed on this one: {e}."
                ),
            )
        }
    };

    let dropped: Vec<Dropped> = outcome
        .dropped
        .iter()
        .map(|d| Dropped {
            index: d.index,
            id: d.id.clone(),
            label: d.label.clone(),
        })
        .collect();
    let kept = outcome.document.features.len();
    let moved = dropped
        .is_empty()
        .then(|| displacement(document, &outcome.document))
        .flatten();

    // Worst first. A partial carry is more serious than one that merely
    // moved, because something is missing from the view rather than being
    // imprecise in it, and the banner should name the worse of the two.
    //
    // `outcome.y_is_approximate` is not consulted: gprinterp sets it for
    // every cross-revision carry, because §8.3 is unconditional, so it
    // does not distinguish between two carries. The displacement does.
    let moved_far =
        moved.is_some_and(|m| m.worst_traces > SAME_PLACE || m.worst_samples > SAME_PLACE);
    let severity = if !dropped.is_empty() {
        Severity::Partial
    } else if moved_far || moved.is_none() {
        // `None` means the vertex lists could not be compared, which on
        // this branch means the document had no vertices to compare. With
        // nothing measured there is nothing to claim, so it takes the
        // more careful of the two tiers.
        Severity::Approximate
    } else {
        Severity::Carried
    };

    let headline = match severity {
        Severity::Partial => format!(
            "Carried from an earlier revision, and {} did not fit on this one — \
             {} shown. A pick outside the new revision is left out rather than \
             pushed to the edge.",
            plural(dropped.len(), "line", "lines"),
            plural(kept, "line is", "lines are"),
        ),
        Severity::Approximate => format!(
            "Carried from an earlier revision through '{}' and '{}', and the picks \
             moved: {} on screen. Depth here is an estimate — the two revisions \
             need not measure travel time against the same thing.",
            outcome.x_anchor,
            outcome.y_anchor,
            moved
                .map(|m| format!(
                    "up to {:.1} traces and {:.1} samples",
                    m.worst_traces, m.worst_samples
                ))
                .unwrap_or_else(|| "by an unmeasured amount".to_string()),
        ),
        _ => format!(
            "Carried from an earlier revision through '{}' and '{}'. {} placed, \
             none of them visibly moved. Depth is still an estimate: a carried \
             travel time is never exact.",
            outcome.x_anchor,
            outcome.y_anchor,
            plural(kept, "line", "lines"),
        ),
    };

    Carried {
        document: Some(outcome.document),
        report: CarryReport {
            severity,
            from_revision,
            to_revision: to_revision.to_string(),
            x_anchor: Some(outcome.x_anchor),
            y_anchor: Some(outcome.y_anchor),
            kept,
            dropped,
            moved,
            refusal: None,
            headline,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::anchors::{AnchorAxis, Axis};

    /// A `regular` axis, as a radargram with even spacing declares it.
    fn regular(name: &str, unit: &str, t0: f64, dt: f64) -> Axis {
        Axis {
            anchor: vec![AnchorAxis {
                name: name.to_string(),
                unit: unit.to_string(),
                type_: "regular",
                t0: Some(t0),
                dt: Some(dt),
                points: None,
                interpolation: None,
            }],
        }
    }

    /// A `tiepoints` axis running from trace 0 to `last`.
    ///
    /// A `regular` axis is unbounded — SPEC §7.5.1 gives it a start and a
    /// step and no end — so nothing can fall outside one. Ridal emits
    /// tiepoints for `x` on any real radargram, because acquisition time
    /// is not evenly spaced, and that is what gives the axis an extent.
    fn tiepoints(name: &str, unit: &str, t0: f64, last: f64) -> Axis {
        Axis {
            anchor: vec![AnchorAxis {
                name: name.to_string(),
                unit: unit.to_string(),
                type_: "tiepoints",
                t0: None,
                dt: None,
                points: Some(vec![
                    crate::interp::anchors::Tiepoint { trace: 0.0, x: t0 },
                    crate::interp::anchors::Tiepoint {
                        trace: last,
                        x: t0 + last,
                    },
                ]),
                interpolation: Some("linear"),
            }],
        }
    }

    /// Axes for a revision whose traces start at `t0` seconds and whose
    /// samples start at `y0` nanoseconds.
    fn axes(t0: f64, y0: f64, y_name: &str) -> Axes {
        Axes {
            x: Some(regular("trace_time", "s", t0, 1.0)),
            y: Some(regular(y_name, "ns", y0, 1.0)),
        }
    }

    /// A document drawn on `from`, with one line through the given
    /// `(trace, sample)` pairs.
    fn document(from: &str, axes: &Axes, points: &[(f64, f64)]) -> gprinterp::Document {
        let coordinates = serde_json::json!({
            "space": "index",
            "axes": {
                "x": serde_json::to_value(axes.x.as_ref().unwrap()).unwrap(),
                "y": serde_json::to_value(axes.y.as_ref().unwrap()).unwrap(),
            }
        });
        serde_json::from_value(serde_json::json!({
            "key": "line-01",
            "source": {"radargram_id": "line-01", "revision_id": from},
            "coordinates": coordinates,
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": points.iter().map(|(x, y)| vec![*x, *y]).collect::<Vec<_>>(),
                },
                "properties": {"id": "f-0001", "label": "bed"}
            }]
        }))
        .unwrap()
    }

    #[test]
    fn the_typical_movement_is_the_middle_of_an_even_count_too() {
        // The banner offers this as the ordinary case next to the worst.
        // Taking the upper middle value reported a two-vertex line
        // displaced by 0 and 10 as typically moving 10, which is the worst
        // case wearing the label of the ordinary one.
        assert_eq!(median(vec![0.0, 10.0]), 5.0);
        assert_eq!(median(vec![10.0, 0.0]), 5.0, "order does not matter");
        assert_eq!(median(vec![1.0, 2.0, 3.0]), 2.0, "odd counts unchanged");
        assert_eq!(median(vec![7.0]), 7.0);
        assert_eq!(median(Vec::new()), 0.0);
    }

    #[test]
    fn a_document_with_no_axes_of_its_own_can_borrow_a_snapshot() {
        // Interpretations saved before the picker emitted
        // `coordinates.axes` record which revision they were drawn on and
        // nothing about what its indices meant, so §8.1 refuses them
        // permanently. #148 keeps the superseded revision's mapping for
        // exactly this, and the snapshot was being written, kept and
        // listed without ever being read by the one thing it was for.
        let drawn_on = axes(0.0, 0.0, "twtt");
        let mut doc = document("rev-a", &drawn_on, &[(2.0, 3.0)]);
        doc.coordinates = None;

        let now = axes(0.0, 0.0, "twtt");
        let bare = carry(&doc, &now, "rev-b");
        assert_eq!(
            bare.report.severity,
            Severity::Refused,
            "nothing to evaluate it through"
        );

        // The mapping the old revision had, kept when it was superseded.
        let snapshot = crate::project::revisions::AxisSnapshot {
            radargram_id: "line-01".to_string(),
            revision_id: "rev-a".to_string(),
            y_anchor: Some("twtt".to_string()),
            y_values: (0..64).map(|i| f64::from(i)).collect(),
            x_values: (0..64).map(|i| 1_000.0 + f64::from(i)).collect(),
        };
        let rescued = with_snapshot_axes(&doc, &snapshot);
        let carried = carry(&rescued, &now, "rev-b");
        assert_ne!(
            carried.report.severity,
            Severity::Refused,
            "{}",
            carried.report.headline
        );
        assert!(carried.document.is_some());

        // And a document that already has axes keeps its own: what it says
        // about itself beats what was inferred about it.
        let with_own = document("rev-a", &drawn_on, &[(2.0, 3.0)]);
        assert_eq!(with_snapshot_axes(&with_own, &snapshot), with_own);
    }

    #[test]
    fn a_snapshot_that_never_named_its_anchor_lends_nothing() {
        // Numbers alone cannot say whether they are travel time or a
        // recording clock, and guessing is the mistake §8.3 is about.
        let drawn_on = axes(0.0, 0.0, "twtt");
        let mut doc = document("rev-a", &drawn_on, &[(2.0, 3.0)]);
        doc.coordinates = None;
        let snapshot = crate::project::revisions::AxisSnapshot {
            radargram_id: "line-01".to_string(),
            revision_id: "rev-a".to_string(),
            y_anchor: None,
            y_values: (0..64).map(f64::from).collect(),
            x_values: (0..64).map(|i| 1_000.0 + f64::from(i)).collect(),
        };
        assert_eq!(with_snapshot_axes(&doc, &snapshot), doc);
    }

    #[test]
    fn a_document_drawn_on_this_revision_is_not_reported_as_carried() {
        // "Nothing to do" and "carried successfully" are different facts,
        // and a banner that cannot tell them apart would announce a
        // migration on every radargram anyone opens.
        let axes = axes(0.0, 0.0, "twtt");
        let doc = document("rev-a", &axes, &[(5.0, 2.0), (30.0, 3.0)]);
        let carried = carry(&doc, &axes, "rev-a");
        assert_eq!(carried.report.severity, Severity::Current);
        assert!(carried.report.moved.is_none(), "nothing was carried");
        assert_eq!(carried.document.unwrap().features.len(), 1);
    }

    #[test]
    fn a_shifted_revision_carries_and_reports_how_far_it_moved() {
        // The new revision starts ten traces later, so the same instant is
        // ten traces earlier in its index space. That is the ordinary case
        // a subset produces.
        let drawn_on = axes(0.0, 0.0, "twtt");
        let now = axes(10.0, 0.0, "twtt");
        let doc = document("rev-a", &drawn_on, &[(15.0, 2.0), (30.0, 3.0)]);

        let carried = carry(&doc, &now, "rev-b");
        assert_eq!(
            carried.report.severity,
            Severity::Approximate,
            "ten traces is a visible move"
        );
        let moved = carried.report.moved.expect("a displacement");
        assert!((moved.worst_traces - 10.0).abs() < 1e-9, "{moved:?}");
        assert_eq!(moved.worst_samples, 0.0, "y did not move");

        let out = carried.document.unwrap();
        let positions = out.features[0].geometry.positions();
        assert!((positions[0].x().unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_pick_outside_the_new_revision_is_dropped_and_counted() {
        // SPEC §8.1 is explicit that these are dropped rather than clamped:
        // a pick pushed onto the boundary is a pick in a place nobody put
        // it, and it would look entirely reasonable there.
        let drawn_on = Axes {
            x: Some(tiepoints("trace_time", "s", 0.0, 40.0)),
            y: Some(regular("twtt", "ns", 0.0, 1.0)),
        };
        // Starts 100 seconds in, so a pick at trace 5 of the old revision
        // is before this one begins.
        let now = Axes {
            x: Some(tiepoints("trace_time", "s", 100.0, 40.0)),
            y: Some(regular("twtt", "ns", 0.0, 1.0)),
        };
        let doc = document("rev-a", &drawn_on, &[(5.0, 2.0), (6.0, 3.0)]);

        let carried = carry(&doc, &now, "rev-b");
        assert_eq!(carried.report.severity, Severity::Partial);
        assert_eq!(carried.report.dropped.len(), 1);
        assert_eq!(carried.report.dropped[0].label.as_deref(), Some("bed"));
        assert_eq!(carried.report.kept, 0);
        assert!(
            carried.report.moved.is_none(),
            "a drop changes which vertices exist, so pairing them by \
             position would compare unrelated points"
        );
        assert!(carried.report.headline.contains("did not fit"));
    }

    #[test]
    fn a_carry_that_moves_nothing_visibly_is_still_not_exact() {
        // Two revisions that agree to well within a sample. Every pick
        // lands where it was put, so the tier says so -- but SPEC §8.3 is
        // unconditional that a carried travel time is never exact, and the
        // banner has to keep saying it. `is_exact` is therefore false here
        // and true only for the revision the picks were drawn on.
        let drawn_on = axes(0.0, 0.0, "twtt");
        let now = axes(0.01, 0.0, "twtt");
        let doc = document("rev-a", &drawn_on, &[(15.0, 2.0), (30.0, 3.0)]);

        let carried = carry(&doc, &now, "rev-b");
        assert_eq!(carried.report.severity, Severity::Carried);
        assert!(carried.report.severity.has_view());
        assert!(
            !carried.report.severity.is_exact(),
            "§8.3 does not care how small the move was"
        );
        assert!(carried.report.headline.contains("estimate"));

        // And the revision it was drawn on is the only exact case.
        let same = carry(&doc, &drawn_on, "rev-a");
        assert!(same.report.severity.is_exact());
    }

    #[test]
    fn a_revision_that_cannot_describe_its_axes_refuses_rather_than_guessing() {
        // §8.1: never silently fall back to the raw index. Drawing the old
        // coordinates against the new grid is exactly that fallback, and it
        // is the failure this whole feature exists to prevent.
        let drawn_on = axes(0.0, 0.0, "twtt");
        let doc = document("rev-a", &drawn_on, &[(5.0, 2.0)]);

        let carried = carry(&doc, &Axes::default(), "rev-b");
        assert_eq!(carried.report.severity, Severity::Refused);
        assert!(carried.document.is_none(), "nothing to draw");
        assert!(carried.report.refusal.is_some());
        assert!(!carried.report.severity.has_view());
    }

    #[test]
    fn no_shared_anchor_refuses_rather_than_carrying_through_a_different_axis() {
        // Two revisions that each describe their axes, and name them
        // differently. Carrying `twtt` through `twtt_normal_incidence`
        // would re-anchor a normal-incidence axis onto an antenna-pair one.
        let drawn_on = axes(0.0, 0.0, "twtt");
        let now = axes(0.0, 0.0, "twtt_normal_incidence");
        let doc = document("rev-a", &drawn_on, &[(5.0, 2.0)]);

        let carried = carry(&doc, &now, "rev-b");
        assert_eq!(carried.report.severity, Severity::Refused);
        assert!(
            carried.report.refusal.as_deref().unwrap().contains("both"),
            "{:?}",
            carried.report.refusal
        );
    }
}
