//! Geometry rules an interpretation must satisfy before it is stored.
//!
//! # Overhangs
//!
//! A picked line normally represents a reflector: one depth per position
//! along the profile. Expressed in index space, that means the line is a
//! *function of trace* -- no two vertices share a trace, and it never
//! doubles back. A line that does is an **overhang**, and it is almost
//! always a mis-click rather than an intention: the moment it exists,
//! "how deep is the bed at 400 m?" stops having one answer.
//!
//! PFA_website enforced this unconditionally. Ridal makes it a per-layer
//! setting that defaults to enforcing, because the constraint belongs to
//! what a layer *means* rather than to the tool: a bed horizon must be a
//! function of trace, while a crevasse wall or a water-body outline
//! legitimately is not.
//!
//! # The cost of allowing them
//!
//! Allowing overhangs on a layer and exporting that layer at even spacing
//! along the ground track are mutually exclusive. Even spacing works by
//! inverting distance to a trace and asking the line for its sample there,
//! which is exactly the question an overhang has two answers to. A layer
//! that permits overhangs is therefore exported as its own picked vertices
//! instead (see [`crate::interp::level2::Spacing::Vertices`]). This is not
//! a limitation to be engineered around later -- it is what the geometry
//! means.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the save-time guardrail runs in the server's write route; \
                  a CLI-only build still uses the same check at export time"
    )
)]

use gprinterp::{Document, Geometry, Position};

/// How close two samples at one trace must be to count as lines that merely
/// touch rather than a genuine duplicate (#207).
///
/// Two features of one layer may legitimately share an endpoint trace -- one
/// line ends where the next begins -- and their samples there are within
/// rounding of each other. A larger difference is two depths at one position,
/// which is the thing the reducer exists to resolve deliberately rather than
/// by accident.
pub const TOUCH_TOLERANCE_SAMPLES: f64 = 1.0;

/// A rule an interpretation breaks.
#[derive(Debug, Clone, PartialEq)]
pub enum Violation {
    /// A line doubles back in trace, so it is not a function of trace.
    Overhang {
        feature_index: usize,
        feature_id: Option<String>,
        layer: Option<String>,
        /// Index of the vertex that reverses or repeats a trace.
        at_vertex: usize,
        trace: f64,
    },
    /// One user has more than one value for one layer at one trace (#207).
    ///
    /// Checked over the raw picked vertices, across every feature sharing the
    /// layer, because that is where the ambiguity exists. A layer with
    /// `warn_on_duplicates = false` is exempt: it is declared multi-valued
    /// (a folded englacial reflector, say).
    DuplicateValue {
        layer: String,
        trace: f64,
        samples: Vec<f64>,
        feature_indices: Vec<usize>,
    },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::Overhang {
                feature_index,
                feature_id,
                layer,
                at_vertex,
                trace,
            } => {
                let which = feature_id
                    .as_deref()
                    .map(|id| format!("feature '{id}'"))
                    .unwrap_or_else(|| format!("feature {feature_index}"));
                let layer = layer.as_deref().unwrap_or("<unlabelled>");
                write!(
                    f,
                    "{which} in layer '{layer}' overhangs: vertex {at_vertex} returns to \
                     trace {trace}, so the layer has two depths at that position. Split \
                     it into separate lines, or allow overhangs on this layer if that is \
                     intended."
                )
            }
            Violation::DuplicateValue {
                layer,
                trace,
                samples,
                feature_indices,
            } => {
                let rendered: Vec<String> = samples.iter().map(|s| format!("{s:.1}")).collect();
                write!(
                    f,
                    "layer '{layer}' has {} values at trace {trace} (samples {}), from \
                     features {:?}. One user may have one value per layer per position; \
                     set a reducer to choose between them, or turn off duplicate warnings \
                     on this layer if it is legitimately multi-valued.",
                    samples.len(),
                    rendered.join(", "),
                    feature_indices
                )
            }
        }
    }
}

/// Where a line stops being a function of trace, if it does.
///
/// A line drawn right to left is not an overhang -- it is the same
/// interpretation recorded in the opposite order, so the check runs against
/// the line's own direction, taken from its first and last vertices.
/// Vertical segments (two vertices on one trace) *are* overhangs: they are
/// the degenerate case of two depths at one position.
/// Whether an interpretation may be exported against this radargram, and
/// what to warn about if so.
///
/// Lifted out of the CLI so the HTTP routes cannot forget it. They did:
/// the command line refused a mismatched radargram while the browser
/// download produced a plausible, wrong file from the same inputs.
///
/// A mismatched radargram is fatal -- the depths would be wrong in a way
/// nothing downstream could detect. A mismatched *revision* is a warning,
/// because the indices may still line up; whether they do depends on which
/// steps were re-run.
pub struct IdentityCheck {
    pub warning: Option<String>,
}

pub fn check_identity(
    document: &Document,
    geometry: &crate::interp::level2::RadargramGeometry,
) -> Result<IdentityCheck, String> {
    if document.key != geometry.radargram_id {
        return Err(format!(
            "this interpretation was drawn on radargram '{}', not '{}'. \
             Export it against the radargram it was drawn on.",
            document.key, geometry.radargram_id
        ));
    }
    let warning = document
        .source
        .as_ref()
        .and_then(|s| s.revision_id.as_deref())
        .filter(|revision| *revision != geometry.revision_id)
        .map(|revision| {
            format!(
                "the interpretation was drawn on revision {revision}, but the radargram is \
                 revision {}. It has been reprocessed since, so trace and sample indices may \
                 no longer line up.",
                geometry.revision_id
            )
        });
    Ok(IdentityCheck { warning })
}

pub fn overhang_at(positions: &[Position]) -> Option<(usize, f64)> {
    let traces: Vec<f64> = positions.iter().filter_map(|p| p.x()).collect();
    if traces.len() < 2 {
        return None;
    }
    let descending = traces[traces.len() - 1] < traces[0];

    for (i, window) in traces.windows(2).enumerate() {
        let (previous, current) = (window[0], window[1]);
        let advances = if descending {
            current < previous
        } else {
            current > previous
        };
        if !advances {
            return Some((i + 1, current));
        }
    }
    None
}

/// Check every line in `document`, skipping layers that permit overhangs.
///
/// `allows_overhangs` is a predicate over the layer label rather than a
/// [`crate::project::layers::LayerSet`], so this stays usable from the CLI,
/// where an export may have no project and therefore no vocabulary at all.
/// A feature with no label, or one naming a layer the vocabulary does not
/// define, is checked: the guardrail is the default, and an undefined layer
/// has not opted out of anything.
pub fn check(
    document: &Document,
    allows_overhangs: &dyn Fn(Option<&str>) -> bool,
    warns_on_duplicates: &dyn Fn(Option<&str>) -> bool,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (feature_index, feature) in document.features.iter().enumerate() {
        let layer = feature.label();
        if allows_overhangs(layer) {
            continue;
        }
        let Geometry::LineString(positions) = &feature.geometry else {
            // Only lines carry the function-of-trace expectation. Points
            // cannot overhang, and polygons are expected to close.
            continue;
        };
        if let Some((at_vertex, trace)) = overhang_at(positions) {
            violations.push(Violation::Overhang {
                feature_index,
                feature_id: feature.id().map(str::to_string),
                layer: layer.map(str::to_string),
                at_vertex,
                trace,
            });
        }
    }
    violations.extend(duplicate_values(document, warns_on_duplicates));
    violations
}

/// Find traces at which one layer holds more than one value (#207).
///
/// The overhang check is per-feature; this one is per-layer, since two
/// separate features of one layer can collide at a trace just as easily as
/// one feature can double back. Both express the same rule -- one user, one
/// layer, one position, one value -- which is why they live together rather
/// than in two modules that can drift apart.
fn duplicate_values(
    document: &Document,
    warns_on_duplicates: &dyn Fn(Option<&str>) -> bool,
) -> Vec<Violation> {
    use std::collections::BTreeMap;

    // layer -> (feature_index, trace, sample), for every picked vertex.
    let mut per_layer: BTreeMap<Option<&str>, Vec<(usize, f64, f64)>> = BTreeMap::new();
    for (feature_index, feature) in document.features.iter().enumerate() {
        let layer = feature.label();
        if !warns_on_duplicates(layer) {
            continue;
        }
        let Geometry::LineString(positions) = &feature.geometry else {
            continue;
        };
        for position in positions {
            if let (Some(trace), Some(sample)) = (position.x(), position.y()) {
                per_layer
                    .entry(layer)
                    .or_default()
                    .push((feature_index, trace, sample));
            }
        }
    }

    let mut violations = Vec::new();
    for (layer, vertices) in per_layer {
        let mut vertices = vertices;
        vertices.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)));

        let mut i = 0;
        while i < vertices.len() {
            let trace = vertices[i].1;
            let mut j = i;
            while j < vertices.len() && vertices[j].1 == trace {
                j += 1;
            }
            let group = &vertices[i..j];
            if group.len() >= 2 {
                // Two features that merely touch at a shared endpoint are
                // allowed. A single feature repeating a trace is not: that
                // is a vertical segment, and its two values are two depths.
                let one_feature = group.iter().all(|v| v.0 == group[0].0);
                let min = group.iter().map(|v| v.2).fold(f64::INFINITY, f64::min);
                let max = group.iter().map(|v| v.2).fold(f64::NEG_INFINITY, f64::max);
                if one_feature || max - min > TOUCH_TOLERANCE_SAMPLES {
                    let mut samples: Vec<f64> = group.iter().map(|v| v.2).collect();
                    samples.sort_by(f64::total_cmp);
                    let mut feature_indices: Vec<usize> = group.iter().map(|v| v.0).collect();
                    feature_indices.sort_unstable();
                    feature_indices.dedup();
                    violations.push(Violation::DuplicateValue {
                        layer: layer.unwrap_or("<unlabelled>").to_string(),
                        trace,
                        samples,
                        feature_indices,
                    });
                }
            }
            i = j;
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(coords: &[[f64; 2]]) -> Vec<Position> {
        coords.iter().map(|c| Position(c.to_vec())).collect()
    }

    fn document(features: &[(&str, &[[f64; 2]])]) -> Document {
        let features: Vec<serde_json::Value> = features
            .iter()
            .enumerate()
            .map(|(i, (label, coords))| {
                serde_json::json!({
                    "type": "Feature",
                    "geometry": {"type": "LineString", "coordinates": coords},
                    "properties": {"id": format!("f-{i}"), "label": label}
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({"key": "line-01", "features": features})).unwrap()
    }

    fn enforce_everywhere(_: Option<&str>) -> bool {
        false
    }

    fn warn_everywhere(_: Option<&str>) -> bool {
        true
    }

    #[test]
    fn a_rising_line_is_fine() {
        assert_eq!(
            overhang_at(&line(&[[0.0, 10.0], [50.0, 12.0], [100.0, 20.0]])),
            None
        );
    }

    #[test]
    fn a_line_drawn_right_to_left_is_not_an_overhang() {
        // Same interpretation, opposite click order.
        assert_eq!(
            overhang_at(&line(&[[100.0, 20.0], [50.0, 12.0], [0.0, 10.0]])),
            None
        );
    }

    #[test]
    fn a_line_that_doubles_back_is_caught_at_the_offending_vertex() {
        let found = overhang_at(&line(&[[0.0, 10.0], [50.0, 12.0], [30.0, 14.0]]));
        assert_eq!(found, Some((2, 30.0)));
    }

    #[test]
    fn a_vertical_segment_is_an_overhang() {
        // Two depths at one trace is the degenerate case, not an exception.
        assert_eq!(
            overhang_at(&line(&[[0.0, 10.0], [50.0, 12.0], [50.0, 30.0]])),
            Some((2, 50.0))
        );
    }

    #[test]
    fn a_two_vertex_line_and_a_single_point_cannot_overhang() {
        assert_eq!(overhang_at(&line(&[[0.0, 10.0], [50.0, 12.0]])), None);
        assert_eq!(overhang_at(&line(&[[0.0, 10.0]])), None);
    }

    #[test]
    fn checking_a_document_reports_the_feature_and_its_layer() {
        let doc = document(&[
            ("bed", &[[0.0, 10.0], [100.0, 20.0]]),
            ("bed", &[[0.0, 10.0], [50.0, 12.0], [30.0, 14.0]]),
        ]);
        let violations = check(&doc, &enforce_everywhere, &warn_everywhere);
        assert_eq!(violations.len(), 1);
        match &violations[0] {
            Violation::Overhang {
                feature_index,
                feature_id,
                layer,
                at_vertex,
                ..
            } => {
                assert_eq!(*feature_index, 1);
                assert_eq!(feature_id.as_deref(), Some("f-1"));
                assert_eq!(layer.as_deref(), Some("bed"));
                assert_eq!(*at_vertex, 2);
            }
            other => panic!("expected an overhang, got {other:?}"),
        }
    }

    #[test]
    fn a_layer_that_allows_overhangs_is_skipped() {
        let doc = document(&[("crevasse", &[[0.0, 10.0], [50.0, 12.0], [30.0, 14.0]])]);
        let allows = |layer: Option<&str>| layer == Some("crevasse");
        assert!(check(&doc, &allows, &warn_everywhere).is_empty());
        assert_eq!(check(&doc, &enforce_everywhere, &warn_everywhere).len(), 1);
    }

    #[test]
    fn an_undefined_or_missing_layer_is_still_checked() {
        // Opting out is a deliberate act. A label the vocabulary does not
        // define has not opted out of anything.
        let doc = document(&[(
            "not_in_the_vocabulary",
            &[[0.0, 1.0], [5.0, 2.0], [3.0, 3.0]],
        )]);
        let allows = |layer: Option<&str>| layer == Some("crevasse");
        assert_eq!(check(&doc, &allows, &warn_everywhere).len(), 1);

        let unlabelled: Document = serde_json::from_value(serde_json::json!({
            "key": "line-01",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": [[0.0, 1.0], [5.0, 2.0], [3.0, 3.0]]}
            }]
        }))
        .unwrap();
        assert_eq!(check(&unlabelled, &allows, &warn_everywhere).len(), 1);
    }

    #[test]
    fn non_line_geometries_are_not_subject_to_the_rule() {
        let doc: Document = serde_json::from_value(serde_json::json!({
            "key": "line-01",
            "features": [
                {"type": "Feature",
                 "geometry": {"type": "Point", "coordinates": [10.0, 20.0]},
                 "properties": {"label": "poi"}},
                {"type": "Feature",
                 "geometry": {"type": "Polygon", "coordinates": [[[0.0, 0.0], [5.0, 0.0], [5.0, 5.0], [0.0, 0.0]]]},
                 "properties": {"label": "lake"}}
            ]
        }))
        .unwrap();
        assert!(check(&doc, &enforce_everywhere, &warn_everywhere).is_empty());
    }

    fn duplicates(violations: &[Violation]) -> Vec<&Violation> {
        violations
            .iter()
            .filter(|v| matches!(v, Violation::DuplicateValue { .. }))
            .collect()
    }

    #[test]
    fn one_feature_with_two_vertices_on_a_trace_is_a_duplicate() {
        let doc = document(&[("bed", &[[500.0, 100.0], [500.0, 140.0]])]);
        let violations = check(&doc, &enforce_everywhere, &warn_everywhere);
        let found = duplicates(&violations);
        assert_eq!(found.len(), 1, "{violations:?}");
        match found[0] {
            Violation::DuplicateValue {
                layer,
                trace,
                samples,
                feature_indices,
            } => {
                assert_eq!(layer, "bed");
                assert_eq!(*trace, 500.0);
                assert_eq!(samples, &vec![100.0, 140.0]);
                assert_eq!(feature_indices, &vec![0]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn two_features_that_touch_at_a_trace_are_not_a_duplicate() {
        let doc = document(&[
            ("bed", &[[0.0, 100.0], [500.0, 100.0]]),
            ("bed", &[[500.0, 100.5], [900.0, 110.0]]),
        ]);
        let violations = check(&doc, &enforce_everywhere, &warn_everywhere);
        assert!(duplicates(&violations).is_empty(), "{violations:?}");
    }

    #[test]
    fn two_features_with_different_values_at_a_trace_are_a_duplicate() {
        let doc = document(&[
            ("bed", &[[0.0, 100.0], [500.0, 100.0]]),
            ("bed", &[[500.0, 140.0], [900.0, 110.0]]),
        ]);
        let violations = check(&doc, &enforce_everywhere, &warn_everywhere);
        let found = duplicates(&violations);
        assert_eq!(found.len(), 1, "{violations:?}");
        match found[0] {
            Violation::DuplicateValue {
                trace,
                feature_indices,
                ..
            } => {
                assert_eq!(*trace, 500.0);
                assert_eq!(feature_indices, &vec![0, 1]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_layer_that_allows_duplicates_reports_none() {
        let tolerates = |_: Option<&str>| false;
        let single = document(&[("bed", &[[500.0, 100.0], [500.0, 140.0]])]);
        assert!(duplicates(&check(&single, &enforce_everywhere, &tolerates)).is_empty());

        let crossed = document(&[
            ("bed", &[[0.0, 100.0], [500.0, 100.0]]),
            ("bed", &[[500.0, 140.0], [900.0, 110.0]]),
        ]);
        assert!(duplicates(&check(&crossed, &enforce_everywhere, &tolerates)).is_empty());
    }

    #[test]
    fn a_reversing_line_is_an_overhang_not_a_duplicate_value() {
        // The same document the overhang test uses. The reversal at trace 30
        // is a per-feature overhang; no trace actually holds two values.
        let doc = document(&[
            ("bed", &[[0.0, 10.0], [100.0, 20.0]]),
            ("bed", &[[0.0, 10.0], [50.0, 12.0], [30.0, 14.0]]),
        ]);
        let violations = check(&doc, &enforce_everywhere, &warn_everywhere);
        assert!(duplicates(&violations).is_empty(), "{violations:?}");
        assert!(violations
            .iter()
            .any(|v| matches!(v, Violation::Overhang { at_vertex: 2, .. })));
    }

    #[test]
    fn the_message_names_what_to_do_about_it() {
        let doc = document(&[("bed", &[[0.0, 10.0], [50.0, 12.0], [30.0, 14.0]])]);
        let message = check(&doc, &enforce_everywhere, &warn_everywhere)[0].to_string();
        assert!(message.contains("f-0"), "{message}");
        assert!(message.contains("bed"), "{message}");
        assert!(
            message.contains("Split it into separate lines"),
            "{message}"
        );
    }
}
