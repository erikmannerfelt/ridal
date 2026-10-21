//! Carrying picks across a moved time zero (#470), measured on the
//! committed Drønbreen Malå file.
//!
//! A pick belongs on the same horizon whatever the reprocessing decided
//! time zero is: onset or peak, a different crop, method or margin. Each
//! test places a pick, carries it, and compares it with where the same
//! time on the recording clock sits in the new file, which is where the
//! same data is.

use std::path::Path;

use crate::interp::anchors;
use crate::interp::carry;
use crate::interp::carry_separation_tests::{document, process};
use crate::interp::source::AxisDeclarations;
use crate::project::revisions::ledger::RevisionFacts;

const BASE: &str = "remove_empty_traces,zero_corr";
const TRACE: usize = 500;
const SAMPLES: [f64; 3] = [100.0, 300.0, 600.0];

struct Revision {
    revision_id: String,
    declared: AxisDeclarations,
    axes: anchors::Axes,
}

fn revision(path: &Path) -> Revision {
    let declared = crate::interp::source::read_axis_declarations(path).unwrap();
    Revision {
        revision_id: crate::interp::source::read_geometry(path)
            .unwrap()
            .revision_id,
        axes: anchors::axes_from_declarations(&declared),
        declared,
    }
}

fn crop(declared: &AxisDeclarations) -> f64 {
    match declared.twtt_crop.as_slice() {
        [one] => *one,
        per_trace => per_trace[TRACE],
    }
}

/// Carry a line along `SAMPLES` from `from` to `to`, with the warnings a
/// server adds; returns the anchor, how far each point is from the same
/// recording time (samples), and the warnings.
fn carry(from: &Revision, to: &Revision) -> (String, Vec<f64>, Vec<String>) {
    let points: Vec<(f64, f64)> = SAMPLES.iter().map(|s| (TRACE as f64, *s)).collect();
    let mut carried = carry::carry(
        &document(&from.revision_id, &from.axes, &points),
        &to.axes,
        &to.revision_id,
    );
    carry::add_processing_warnings(
        &mut carried,
        RevisionFacts::of(&from.declared),
        RevisionFacts::of(&to.declared),
    );
    let document = carried.document.expect("carried");
    let gprinterp::Geometry::LineString(coordinates) = &document.features[0].geometry else {
        panic!("not a line")
    };
    let errors = SAMPLES
        .iter()
        .zip(coordinates)
        .map(|(sample, position)| {
            let clock = crop(&from.declared) + sample * from.declared.dt_ns;
            let truth = (clock - crop(&to.declared)) / to.declared.dt_ns;
            position.0[1] - truth
        })
        .collect();
    (
        carried.report.y_anchor.expect("carried"),
        errors,
        carried.report.warnings,
    )
}

#[test]
fn a_pick_stays_on_its_horizon_when_time_zero_moves() {
    let directory = tempfile::tempdir().unwrap();
    let base = revision(&process(directory.path(), "base", BASE));
    for (name, steps, says_depths_changed) in [
        // The same samples, labelled 24 ns later: the case #470 found 8
        // samples off when carried by travel time.
        (
            "peak",
            "remove_empty_traces,zero_corr(time_zero=peak)",
            true,
        ),
        ("margin", "remove_empty_traces,zero_corr(margin=5)", false),
        ("max_peak", "remove_empty_traces,zero_corr(max_peak)", false),
        ("aic", "remove_empty_traces,zero_corr(aic)", false),
    ] {
        let other = revision(&process(directory.path(), name, steps));
        for (from, to) in [(&base, &other), (&other, &base)] {
            let (anchor, errors, warnings) = carry(from, to);
            assert_eq!(anchor, "recording_time", "{name}");
            for error in errors {
                assert!(error.abs() < 0.05, "{name}: {error} samples off");
            }
            assert_eq!(
                warnings.iter().any(|w| w.contains("depths changed")),
                says_depths_changed,
                "{name}: {warnings:?}"
            );
        }
    }
}

#[test]
fn a_per_trace_zero_correction_is_carried_by_travel_time() {
    // Its crops differ per trace, so it offers no regular recording clock,
    // and travel time is the only anchor in common: the pick follows each
    // trace's own time zero. The warning says so when the typical time
    // zero moved by more than half a sample.
    let directory = tempfile::tempdir().unwrap();
    let base = revision(&process(directory.path(), "base", BASE));
    let per_trace = revision(&process(
        directory.path(),
        "per-trace",
        "remove_empty_traces,zero_corr(coppens, trace)",
    ));
    let (anchor, _, warnings) = carry(&base, &per_trace);
    assert_eq!(anchor, "twtt");
    let from = RevisionFacts::of(&base.declared);
    let to = RevisionFacts::of(&per_trace.declared);
    let moved = (to.time_zero_ns.unwrap() - from.time_zero_ns.unwrap()).abs();
    assert_eq!(
        warnings
            .iter()
            .any(|w| w.contains("could only be carried by travel time")),
        moved > to.sample_interval_ns.unwrap() / 2.,
        "moved {moved} ns: {warnings:?}"
    );
}

#[test]
fn adding_a_zero_correction_carries_exactly_and_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let corrected = revision(&process(directory.path(), "corrected", BASE));
    let uncorrected = revision(&process(
        directory.path(),
        "uncorrected",
        "remove_empty_traces",
    ));
    for (from, to, says) in [
        (&uncorrected, &corrected, "never located"),
        (&corrected, &uncorrected, "has none"),
    ] {
        let (anchor, errors, warnings) = carry(from, to);
        assert_eq!(anchor, "recording_time");
        for error in errors {
            assert!(error.abs() < 0.05, "{error} samples off");
        }
        assert!(warnings.iter().any(|w| w.contains(says)), "{warnings:?}");
    }
}
