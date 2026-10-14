//! What one interpretation contains, as numbers (#178).
//!
//! The pick manager lists everyone's interpretations to people who may not
//! read them: an operator sees who picked what and how much, and opening
//! the picks is still the `Picks` download scope's decision. So everything
//! here is a statistic over a document and never a coordinate from it.
//!
//! # Coverage
//!
//! The share of a radargram's traces with at least one picked line over
//! them, in percent. Picks are stored in source trace/sample index space
//! (gprinterp's default `index2d`, SPEC §7.1), so a line's trace extent is
//! read straight off its `x` coordinates and the denominator is the
//! radargram's trace count: no NetCDF is opened.
//!
//! A trace counts as covered when it lies between a line's first and last
//! trace, so a line that doubles back counts its traces once and two lines
//! over the same traces count them once. Points cover the one trace they
//! sit on. A document's `coordinates.space` is not consulted, as nothing
//! else in Ridal consults it: the viewer, level 2 export and carrying all
//! read geometry as indices, so coverage measures what they draw.
#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "summaries are served by the pick manager's HTTP route"
    )
)]

use std::collections::BTreeMap;

use gprinterp::{Document, Geometry, Position};

/// Numbers describing one layer's picks within one document.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerStats {
    /// The `label` the features carry, or `None` for unlabelled features.
    pub layer: Option<String>,
    pub features: usize,
    pub vertices: usize,
    /// Percent of traces under this layer's lines, `0.0..=100.0`.
    pub coverage: Option<f64>,
}

/// Numbers describing one document.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentStats {
    pub features: usize,
    pub vertices: usize,
    /// Percent of traces under any line, `0.0..=100.0`.
    pub coverage: Option<f64>,
    /// One entry per layer in use, ordered by label with unlabelled last.
    pub layers: Vec<LayerStats>,
}

/// One layer's running counts while a document is walked.
struct Tally {
    features: usize,
    vertices: usize,
    /// Which traces are covered, when there is a trace count to measure.
    mask: Option<Vec<bool>>,
}

/// Summarize `document`, measuring coverage against `n_traces` when known.
///
/// `n_traces` is `None` when there is no radargram to measure against: an
/// archived document, or one whose radargram is gone.
pub fn summarize(document: &Document, n_traces: Option<usize>) -> DocumentStats {
    let n_traces = n_traces.filter(|n| *n > 0);

    let mut all = n_traces.map(|n| vec![false; n]);
    let mut by_layer: BTreeMap<Option<&str>, Tally> = BTreeMap::new();
    let mut vertices = 0;
    for feature in &document.features {
        let tally = by_layer.entry(feature.label()).or_insert_with(|| Tally {
            features: 0,
            vertices: 0,
            mask: n_traces.map(|n| vec![false; n]),
        });
        let mut count = 0;
        each_run(&feature.geometry, &mut |run| {
            count += run.len();
            if let Some(mask) = all.as_mut() {
                cover(mask, run);
            }
            if let Some(mask) = tally.mask.as_mut() {
                cover(mask, run);
            }
        });
        tally.features += 1;
        tally.vertices += count;
        vertices += count;
    }

    let mut layers: Vec<LayerStats> = by_layer
        .into_iter()
        .map(|(label, tally)| LayerStats {
            layer: label.map(str::to_string),
            features: tally.features,
            vertices: tally.vertices,
            coverage: tally.mask.as_deref().map(percent),
        })
        .collect();
    // `None` sorts first in a `BTreeMap`; unlabelled features read better
    // after the named layers.
    let unlabelled_first = layers.first().is_some_and(|l| l.layer.is_none());
    layers.rotate_left(usize::from(unlabelled_first));

    DocumentStats {
        features: document.features.len(),
        vertices,
        coverage: all.as_deref().map(percent),
        layers,
    }
}

/// Call `f` with every run of positions that makes up `geometry`: each line,
/// ring or point set.
fn each_run(geometry: &Geometry, f: &mut impl FnMut(&[Position])) {
    match geometry {
        Geometry::Point(p) => f(std::slice::from_ref(p)),
        Geometry::MultiPoint(ps) => ps.iter().for_each(|p| f(std::slice::from_ref(p))),
        Geometry::LineString(ps) => f(ps),
        Geometry::MultiLineString(runs) | Geometry::Polygon(runs) => {
            runs.iter().for_each(|run| f(run))
        }
        Geometry::MultiPolygon(polygons) => polygons.iter().flatten().for_each(|run| f(run)),
        Geometry::GeometryCollection(geometries) => geometries.iter().for_each(|g| each_run(g, f)),
        Geometry::Unknown(_) => {}
    }
}

/// Mark the traces between the run's first and last `x` as covered.
fn cover(mask: &mut [bool], run: &[Position]) {
    let xs = run.iter().filter_map(Position::x).filter(|x| x.is_finite());
    let (low, high) = xs.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| {
        (lo.min(x), hi.max(x))
    });
    if low > high {
        return;
    }
    // Inclusive at both ends, clamped to the radargram: a vertex dragged a
    // little past the last trace still covers up to it, and one wholly
    // outside covers nothing.
    let last = mask.len() as f64 - 1.0;
    let first = low.ceil().max(0.0);
    let end = high.floor().min(last);
    if first > end {
        return;
    }
    mask[first as usize..=end as usize].fill(true);
}

fn percent(mask: &[bool]) -> f64 {
    100.0 * mask.iter().filter(|&&covered| covered).count() as f64 / mask.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(features: serde_json::Value) -> Document {
        serde_json::from_value(serde_json::json!({"key": "line-01", "features": features})).unwrap()
    }

    fn line(label: &str, coordinates: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": coordinates},
            "properties": {"label": label}
        })
    }

    #[test]
    fn a_line_over_half_the_traces_covers_half() {
        let stats = summarize(
            &document(serde_json::json!([line(
                "bed",
                serde_json::json!([[0, 1], [49, 2]])
            )])),
            Some(100),
        );
        assert_eq!(stats.features, 1);
        assert_eq!(stats.vertices, 2);
        assert_eq!(stats.coverage, Some(50.0));
        assert_eq!(stats.layers[0].layer.as_deref(), Some("bed"));
        assert_eq!(stats.layers[0].coverage, Some(50.0));
    }

    #[test]
    fn overlapping_lines_are_not_counted_twice() {
        let stats = summarize(
            &document(serde_json::json!([
                line("bed", serde_json::json!([[0, 1], [9, 2]])),
                line("bed", serde_json::json!([[5, 1], [14, 2]])),
                line("surface", serde_json::json!([[0, 0], [19, 0]])),
            ])),
            Some(20),
        );
        assert_eq!(stats.coverage, Some(100.0));
        assert_eq!(stats.layers[0].layer.as_deref(), Some("bed"));
        assert_eq!(stats.layers[0].features, 2);
        assert_eq!(stats.layers[0].coverage, Some(75.0));
        assert_eq!(stats.layers[1].coverage, Some(100.0));
    }

    #[test]
    fn a_line_past_the_last_trace_is_clamped() {
        let stats = summarize(
            &document(serde_json::json!([line(
                "bed",
                serde_json::json!([[-5.5, 1], [7.2, 1], [30, 1]])
            )])),
            Some(10),
        );
        assert_eq!(stats.coverage, Some(100.0));

        let outside = summarize(
            &document(serde_json::json!([line(
                "bed",
                serde_json::json!([[12, 1], [30, 1]])
            )])),
            Some(10),
        );
        assert_eq!(outside.coverage, Some(0.0));
    }

    #[test]
    fn a_point_covers_its_own_trace() {
        let stats = summarize(
            &document(serde_json::json!([{
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [3, 4]},
                "properties": {"label": "bed"}
            }])),
            Some(4),
        );
        assert_eq!(stats.coverage, Some(25.0));
    }

    #[test]
    fn unlabelled_features_come_after_named_layers() {
        let stats = summarize(
            &document(serde_json::json!([
                {"type": "Feature",
                 "geometry": {"type": "LineString", "coordinates": [[0, 1], [1, 1]]}},
                line("bed", serde_json::json!([[0, 1], [1, 1]])),
            ])),
            None,
        );
        let labels: Vec<_> = stats.layers.iter().map(|l| l.layer.as_deref()).collect();
        assert_eq!(labels, vec![Some("bed"), None]);
    }

    #[test]
    fn without_a_trace_count_there_is_no_coverage() {
        let stats = summarize(
            &document(serde_json::json!([line(
                "bed",
                serde_json::json!([[0, 1], [9, 1]])
            )])),
            None,
        );
        assert_eq!(stats.coverage, None);
        assert_eq!(stats.layers[0].coverage, None);
        assert_eq!(stats.vertices, 2);
    }
}
