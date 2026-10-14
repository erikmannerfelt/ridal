//! Picks and derived layers drawn over a figure (#142).
//!
//! Two halves, kept apart so the figure itself never reads a project:
//!
//! - [`gather`] reads what a spec asks for from a project, through the same
//!   functions the viewer's routes use, and returns plain [`Overlays`]:
//!   lines in `(trace, sample)` and per-trace sample values. Who may see
//!   which picks is decided by its [`LayerAccess`], which the caller builds.
//!   On the command line that is everything, since whoever runs it can read
//!   the project's files; the HTTP API builds it from the caller's role.
//! - [`Overlays::svg`] draws them in the figure's pixel space, clipped to
//!   the plot.
//!
//! What is drawn is what the viewer draws. Picks are carried to the
//! revision being drawn, as `GET .../carried` carries them, so a pick lands
//! where the viewer shows it. Picks that do not name a revision at all are
//! drawn as stored, as the viewer's contributor overlay draws them. Derived
//! layers are evaluated over the stored picks per trace, as
//! `GET .../derived/{item}` evaluates them, and a range fill is drawn only
//! between two items that are both drawn.

use std::collections::BTreeMap;
use std::path::Path;

use crate::identity::{RadargramId, RevisionId, UserId};
use crate::interp::derive::{self, GridPosition, Kind};
use crate::project::derived::{self, Audience};
use crate::project::{interpretations, layers, Project};

use super::spec::{FigureSpec, PickSelector};
use super::FigureError;

/// The colour of a pick or derived layer with none of its own, as in the
/// viewer.
pub const DEFAULT_COLOUR: &str = "#ffcc00";

/// Whose picks something may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readable {
    All,
    /// One user's, or nobody's when `None`.
    Only(Option<UserId>),
}

impl Readable {
    fn permits(&self, user: &UserId) -> bool {
        match self {
            Readable::All => true,
            Readable::Only(own) => own.as_ref() == Some(user),
        }
    }
}

/// What a figure's caller may see. Built by the caller, so the permission
/// rules stay with whoever owns them.
#[derive(Debug, Clone)]
pub struct LayerAccess {
    /// Whose picks may be drawn.
    pub picks: Readable,
    /// Whose picks feed a derived item evaluated for its owner's picks only
    /// ([`Audience::OwnPicks`]).
    pub own_picks_items: Readable,
    /// Whose picks feed a derived item released to everyone
    /// ([`Audience::Released`]).
    pub released_items: Readable,
    /// The display name private derived items are checked against, or
    /// `None` to see every item.
    pub viewer: Option<String>,
}

impl LayerAccess {
    /// Everything: the command line's access to a project on disk.
    pub fn everything() -> Self {
        Self {
            picks: Readable::All,
            own_picks_items: Readable::All,
            released_items: Readable::All,
            viewer: None,
        }
    }

    fn inputs(&self, audience: Audience) -> &Readable {
        match audience {
            Audience::OwnPicks => &self.own_picks_items,
            Audience::Released => &self.released_items,
        }
    }
}

/// Everything drawn over the radargram, in source coordinates.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overlays {
    pub picks: Vec<PickLine>,
    pub derived: Vec<DerivedLine>,
    pub fills: Vec<FillBand>,
    /// What could not be drawn as asked, for the caller to report: picks
    /// that could not be carried to this revision, or were only partly.
    pub warnings: Vec<String>,
}

/// One picked line: its vertices as `[trace, sample]`.
#[derive(Debug, Clone, PartialEq)]
pub struct PickLine {
    pub user: String,
    /// The layer id, or `None` for an unlabelled pick.
    pub layer: Option<String>,
    pub colour: String,
    pub vertices: Vec<[f64; 2]>,
}

/// One derived layer: a sample position per trace, `NaN` where it has none.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedLine {
    pub id: String,
    pub colour: String,
    pub samples: Vec<f64>,
}

/// A range fill between two derived layers, per trace.
#[derive(Debug, Clone, PartialEq)]
pub struct FillBand {
    pub colour: String,
    pub opacity: f64,
    pub a: Vec<f64>,
    pub b: Vec<f64>,
}

impl Overlays {
    pub fn is_empty(&self) -> bool {
        self.picks.is_empty() && self.derived.is_empty() && self.fills.is_empty()
    }
}

/// Read the picks and derived layers `spec` asks for from `project`, for
/// the radargram `radargram` as the file at `path` (revision `revision`)
/// has it.
///
/// A derived item the spec names that does not exist, is not visible to
/// the caller, cannot be evaluated, or is an attribute rather than a
/// position is an error: a figure silently missing the line it was asked
/// for is worse than no figure.
pub fn gather(
    project: &Project,
    radargram: &RadargramId,
    path: &Path,
    revision: &RevisionId,
    spec: &FigureSpec,
    access: &LayerAccess,
) -> Result<Overlays, FigureError> {
    let mut overlays = Overlays::default();
    if !spec.has_overlays() {
        return Ok(overlays);
    }
    let read_failed = |what: &str, e: &dyn std::fmt::Display| {
        FigureError::Render(format!("Could not read the project's {what}: {e}"))
    };
    let store = project.documents();
    let (layer_set, _) = layers::read(store).map_err(|e| read_failed("layers", &e))?;
    let layer_colour = |id: Option<&str>| -> String {
        id.and_then(|id| layer_set.layers.iter().find(|l| l.id == id))
            .and_then(|l| l.color.clone())
            .unwrap_or_else(|| DEFAULT_COLOUR.to_string())
    };

    if !spec.picks.items.is_empty() {
        gather_picks(
            project,
            radargram,
            path,
            revision,
            &spec.picks.items,
            access,
            &layer_colour,
            &mut overlays,
        )?;
    }

    if spec.derived.items.is_empty() {
        return Ok(overlays);
    }
    let (set, _) = derived::read(store).map_err(|e| read_failed("derived layers", &e))?;
    let mut items = Vec::with_capacity(spec.derived.items.len());
    for id in &spec.derived.items {
        let item = set
            .get(id)
            .filter(|item| {
                access
                    .viewer
                    .as_deref()
                    .is_none_or(|viewer| item.visible_to(viewer))
            })
            .ok_or_else(|| FigureError::Spec(format!("There is no derived layer '{id}'.")))?;
        items.push(item);
    }

    let geometry = crate::interp::source::read_geometry(path).map_err(FigureError::Render)?;
    let grid: Vec<GridPosition> = (0..geometry.n_traces())
        .map(|trace| GridPosition {
            trace: trace as f64,
            distance_m: crate::interp::level2::interpolate_index(&geometry.distance, trace as f64),
        })
        .collect();

    // Evaluated once per audience the requested items have, each over
    // exactly the picks that audience permits: never over a wider set that
    // is filtered afterwards, which is how a cross-user value leaks.
    let mut evaluations: BTreeMap<Audience, derived::Evaluation> = BTreeMap::new();
    for item in &items {
        if evaluations.contains_key(&item.audience) {
            continue;
        }
        let documents = stored_documents(project, radargram, access.inputs(item.audience))?;
        let reduced = derive::reduce_picks(&documents, &layer_set, &geometry, &grid, false);
        let evaluation = set.evaluate(&reduced, &geometry).map_err(|e| {
            FigureError::Spec(format!("The derived layers cannot be evaluated: {e}"))
        })?;
        evaluations.insert(item.audience, evaluation);
    }

    let mut samples: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for item in &items {
        let evaluation = &evaluations[&item.audience];
        if let Some(error) = evaluation.failures.get(&item.id) {
            return Err(FigureError::Spec(format!(
                "The derived layer '{}' cannot be evaluated: {error}",
                item.id
            )));
        }
        let Some(result) = evaluation.results.get(&item.id) else {
            return Err(FigureError::Spec(format!(
                "The derived layer '{}' was not evaluated.",
                item.id
            )));
        };
        if result.kind != Kind::Layer {
            return Err(FigureError::Spec(format!(
                "'{}' is a derived attribute, a number per position, not a layer that can be \
                 drawn.",
                item.id
            )));
        }
        let values: Vec<f64> = result
            .values
            .iter()
            .map(|&v| {
                if v.is_finite() {
                    derive::unit_to_sample(v, result.unit, &geometry)
                } else {
                    f64::NAN
                }
            })
            .collect();
        let colour = item
            .color
            .clone()
            .unwrap_or_else(|| layer_colour(item.fill_to.as_ref().map(|f| f.target.as_str())));
        overlays.derived.push(DerivedLine {
            id: item.id.clone(),
            colour,
            samples: values.clone(),
        });
        samples.insert(item.id.clone(), values);
    }

    if spec.derived.fills {
        for item in &items {
            let Some(fill) = &item.fill_to else {
                continue;
            };
            // Only between two drawn items, as in the viewer: a band whose
            // other edge is not drawn has nothing to read it against.
            let Some(b) = samples.get(&fill.target) else {
                continue;
            };
            let colour = fill
                .color
                .clone()
                .or_else(|| item.color.clone())
                .unwrap_or_else(|| layer_colour(Some(&fill.target)));
            overlays.fills.push(FillBand {
                colour,
                opacity: fill.opacity,
                a: samples[&item.id].clone(),
                b: b.clone(),
            });
        }
    }
    Ok(overlays)
}

/// The requested picks, carried to `revision` as the viewer draws them.
#[allow(clippy::too_many_arguments)]
fn gather_picks(
    project: &Project,
    radargram: &RadargramId,
    path: &Path,
    revision: &RevisionId,
    selectors: &[PickSelector],
    access: &LayerAccess,
    layer_colour: &dyn Fn(Option<&str>) -> String,
    overlays: &mut Overlays,
) -> Result<(), FigureError> {
    let store = project.documents();
    let (users, _) = interpretations::list_users_checked(store, radargram)
        .map_err(|e| FigureError::Render(format!("Could not list the picks: {e}")))?;
    for user in users {
        if !access.picks.permits(&user)
            || !selectors
                .iter()
                .any(|s| s.user.as_deref().is_none_or(|u| u == user.as_str()))
        {
            continue;
        }
        let Some(stored) = interpretations::read(store, radargram, &user)
            .map_err(|e| FigureError::Render(format!("Could not read the picks: {e}")))?
        else {
            continue;
        };
        let carried = crate::interp::carry::carried_for_drawing(
            project,
            radargram,
            path,
            revision,
            &stored.document,
        );
        use crate::interp::carry::Severity;
        let unanchored = stored
            .document
            .source
            .as_ref()
            .and_then(|s| s.revision_id.as_deref())
            .is_none();
        let document = match carried.document {
            Some(document) => {
                if !matches!(
                    carried.report.severity,
                    Severity::Current | Severity::Carried
                ) {
                    overlays.warnings.push(format!(
                        "{}'s picks: {}",
                        user.as_str(),
                        carried.report.headline
                    ));
                }
                document
            }
            // Picks that do not say which revision they were drawn on
            // (imported from an older tool) cannot be carried, and there is
            // nothing to carry them from. They are drawn as stored, which is
            // how the viewer's contributor overlay shows them and how derived
            // layers are evaluated over them, so a figure's picks and its
            // consensus agree.
            None if unanchored => {
                overlays.warnings.push(format!(
                    "{}'s picks do not say which revision they were drawn on, so they are \
                     drawn as stored.",
                    user.as_str()
                ));
                stored.document
            }
            None => {
                overlays.warnings.push(format!(
                    "{}'s picks are not drawn: {}",
                    user.as_str(),
                    carried.report.headline
                ));
                continue;
            }
        };
        for feature in &document.features {
            let gprinterp::Geometry::LineString(vertices) = &feature.geometry else {
                continue;
            };
            let label = feature.label();
            if !selectors.iter().any(|s| s.matches(user.as_str(), label)) {
                continue;
            }
            let vertices: Vec<[f64; 2]> = vertices
                .iter()
                .filter_map(|p| Some([p.x()?, p.y()?]))
                .collect();
            if vertices.len() < 2 {
                continue;
            }
            overlays.picks.push(PickLine {
                user: user.as_str().to_string(),
                layer: label.map(str::to_string),
                colour: layer_colour(label),
                vertices,
            });
        }
    }
    Ok(())
}

/// The stored picks `readable` permits, as derived layers are evaluated
/// over them.
fn stored_documents(
    project: &Project,
    radargram: &RadargramId,
    readable: &Readable,
) -> Result<Vec<(String, gprinterp::Document)>, FigureError> {
    let store = project.documents();
    let (users, _) = interpretations::list_users_checked(store, radargram)
        .map_err(|e| FigureError::Render(format!("Could not list the picks: {e}")))?;
    let mut documents = Vec::new();
    for user in users {
        if !readable.permits(&user) {
            continue;
        }
        if let Some(stored) = interpretations::read(store, radargram, &user)
            .map_err(|e| FigureError::Render(format!("Could not read the picks: {e}")))?
        {
            documents.push((user.as_str().to_string(), stored.document));
        }
    }
    Ok(documents)
}

/// Where overlays go on the page: the plot rectangle in pixels, the extent
/// it shows, and the topographic view's per-trace shift.
pub(super) struct Placement<'a> {
    pub plot: [f64; 4],
    pub traces: [f64; 2],
    pub rows: [f64; 2],
    /// Per-trace downward shift in samples, in the topographic view.
    pub shift: Option<&'a [f32]>,
}

impl Placement<'_> {
    /// The pixel position of `(trace, sample)`.
    fn to_px(&self, trace: f64, sample: f64) -> [f64; 2] {
        let [x0, y0, x1, y1] = self.plot;
        let row = sample + self.shift_at(trace);
        [
            x0 + (trace - self.traces[0]) / (self.traces[1] - self.traces[0]) * (x1 - x0),
            y0 + (row - self.rows[0]) / (self.rows[1] - self.rows[0]) * (y1 - y0),
        ]
    }

    /// The shift at a fractional trace, interpolated and clamped at the ends
    /// as the viewer's `shiftAt` does.
    fn shift_at(&self, trace: f64) -> f64 {
        let Some(shift) = self.shift.filter(|s| !s.is_empty()) else {
            return 0.0;
        };
        let last = shift.len() - 1;
        let t = trace.clamp(0.0, last as f64);
        let i0 = t.floor() as usize;
        let i1 = (i0 + 1).min(last);
        let f = t - i0 as f64;
        shift[i0] as f64 * (1.0 - f) + shift[i1] as f64 * f
    }
}

/// The style each group is drawn with, in pixels.
pub(super) struct OverlayStyle {
    pub pick_width: f64,
    pub pick_opacity: f64,
    pub derived_width: f64,
    pub derived_opacity: f64,
}

impl Overlays {
    /// SVG for the overlays: fills, then picks, then derived lines, all
    /// clipped to the plot so nothing draws over the axes.
    ///
    /// Clipped here, in the geometry, rather than with an SVG `clipPath`,
    /// which not every renderer that opens an SVG honours.
    pub(super) fn svg(&self, placement: &Placement, style: &OverlayStyle) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let colour = |c: &str| {
            super::spec::parse_colour("colour", c).unwrap_or_else(|_| DEFAULT_COLOUR.to_string())
        };

        for fill in &self.fills {
            let mut d = String::new();
            for ring in fill_rings(&fill.a, &fill.b) {
                let polygon: Vec<[f64; 2]> = ring
                    .iter()
                    .map(|&(trace, sample)| placement.to_px(trace, sample))
                    .collect();
                let clipped = clip_polygon(&polygon, placement.plot);
                if clipped.len() >= 3 {
                    write_path(&mut d, &clipped);
                    d.push('Z');
                }
            }
            if !d.is_empty() {
                let _ = writeln!(
                    s,
                    r#"<path d="{d}" fill="{}" fill-opacity="{:.3}" stroke="none"/>"#,
                    colour(&fill.colour),
                    fill.opacity.clamp(0.0, 1.0)
                );
            }
        }

        let line = |s: &mut String, points: &[[f64; 2]], c: &str, width: f64, opacity: f64| {
            let mut d = String::new();
            for run in clip_polyline(points, placement.plot) {
                if run.len() >= 2 {
                    write_path(&mut d, &run);
                }
            }
            if !d.is_empty() {
                let _ = writeln!(
                    s,
                    r#"<path d="{d}" fill="none" stroke="{}" stroke-width="{width:.2}" stroke-opacity="{opacity:.3}" stroke-linejoin="round" stroke-linecap="round"/>"#,
                    colour(c)
                );
            }
        };

        for pick in &self.picks {
            let points: Vec<[f64; 2]> = pick
                .vertices
                .iter()
                .map(|&[trace, sample]| placement.to_px(trace, sample))
                .collect();
            line(
                &mut s,
                &points,
                &pick.colour,
                style.pick_width,
                style.pick_opacity,
            );
        }
        for derived in &self.derived {
            // One run per stretch of finite values, so a gap stays a gap.
            for run in finite_runs(&derived.samples) {
                let points: Vec<[f64; 2]> = run
                    .map(|trace| placement.to_px(trace as f64, derived.samples[trace]))
                    .collect();
                line(
                    &mut s,
                    &points,
                    &derived.colour,
                    style.derived_width,
                    style.derived_opacity,
                );
            }
        }
        s
    }
}

fn write_path(d: &mut String, points: &[[f64; 2]]) {
    use std::fmt::Write;
    for (i, [x, y]) in points.iter().enumerate() {
        let _ = write!(d, "{}{x:.2} {y:.2}", if i == 0 { 'M' } else { 'L' });
    }
}

/// The index ranges of the consecutive finite values in `values`.
fn finite_runs(values: &[f64]) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, v) in values.iter().enumerate() {
        match (v.is_finite(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push(s..values.len());
    }
    runs
}

/// A fill between two per-trace sample arrays as rings of
/// `(trace, sample)`, the viewer's `fillRings`: one ring per run where both
/// are finite, and a new ring wherever they cross, so a ring never folds
/// over itself into a bowtie. A gap on either side ends the run.
fn fill_rings(a: &[f64], b: &[f64]) -> Vec<Vec<(f64, f64)>> {
    let mut rings = Vec::new();
    // (trace, a, b) per point of the current ring.
    let mut ring: Vec<(f64, f64, f64)> = Vec::new();
    let close = |ring: &mut Vec<(f64, f64, f64)>, rings: &mut Vec<Vec<(f64, f64)>>| {
        if ring.len() > 1 {
            let mut points: Vec<(f64, f64)> = ring.iter().map(|&(t, a, _)| (t, a)).collect();
            points.extend(ring.iter().rev().map(|&(t, _, b)| (t, b)));
            rings.push(points);
        }
        ring.clear();
    };
    for i in 0..a.len().min(b.len()) {
        let (ai, bi) = (a[i], b[i]);
        if !(ai.is_finite() && bi.is_finite()) {
            close(&mut ring, &mut rings);
            continue;
        }
        if let Some(&(_, pa, pb)) = ring.last() {
            let (prev, diff) = (pa - pb, ai - bi);
            if prev != 0.0 && diff != 0.0 && (prev > 0.0) != (diff > 0.0) {
                let t = prev / (prev - diff);
                let crossing = (i as f64 - 1.0 + t, pa + t * (ai - pa));
                ring.push((crossing.0, crossing.1, crossing.1));
                close(&mut ring, &mut rings);
                ring.push((crossing.0, crossing.1, crossing.1));
            }
        }
        ring.push((i as f64, ai, bi));
    }
    close(&mut ring, &mut rings);
    rings
}

/// Clip a polyline to the rectangle `[x0, y0, x1, y1]`, as the runs of it
/// that lie inside.
fn clip_polyline(points: &[[f64; 2]], rect: [f64; 4]) -> Vec<Vec<[f64; 2]>> {
    let mut runs: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();
    for pair in points.windows(2) {
        match clip_segment(pair[0], pair[1], rect) {
            Some((p, q)) => {
                if current.last() != Some(&p) {
                    if current.len() >= 2 {
                        runs.push(std::mem::take(&mut current));
                    }
                    current = vec![p];
                }
                current.push(q);
                // Left the rectangle partway: this run ends here.
                if q != pair[1] {
                    runs.push(std::mem::take(&mut current));
                }
            }
            None => {
                if current.len() >= 2 {
                    runs.push(std::mem::take(&mut current));
                }
                current.clear();
            }
        }
    }
    if current.len() >= 2 {
        runs.push(current);
    }
    runs
}

/// Liang-Barsky: the part of the segment `p`-`q` inside `rect`.
fn clip_segment(
    p: [f64; 2],
    q: [f64; 2],
    [x0, y0, x1, y1]: [f64; 4],
) -> Option<([f64; 2], [f64; 2])> {
    let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (denominator, numerator) in [
        (-dx, p[0] - x0),
        (dx, x1 - p[0]),
        (-dy, p[1] - y0),
        (dy, y1 - p[1]),
    ] {
        if denominator == 0.0 {
            if numerator < 0.0 {
                return None;
            }
        } else {
            let t = numerator / denominator;
            if denominator < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    let at = |t: f64| [p[0] + t * dx, p[1] + t * dy];
    Some((
        if t0 == 0.0 { p } else { at(t0) },
        if t1 == 1.0 { q } else { at(t1) },
    ))
}

/// Sutherland-Hodgman: `polygon` clipped to the rectangle
/// `[x0, y0, x1, y1]`.
fn clip_polygon(polygon: &[[f64; 2]], [x0, y0, x1, y1]: [f64; 4]) -> Vec<[f64; 2]> {
    type Edge = (
        fn([f64; 2], f64) -> bool,
        fn([f64; 2], [f64; 2], f64) -> [f64; 2],
        f64,
    );
    fn cross_x(p: [f64; 2], q: [f64; 2], x: f64) -> [f64; 2] {
        let t = (x - p[0]) / (q[0] - p[0]);
        [x, p[1] + t * (q[1] - p[1])]
    }
    fn cross_y(p: [f64; 2], q: [f64; 2], y: f64) -> [f64; 2] {
        let t = (y - p[1]) / (q[1] - p[1]);
        [p[0] + t * (q[0] - p[0]), y]
    }
    let edges: [Edge; 4] = [
        (|p, x| p[0] >= x, cross_x, x0),
        (|p, x| p[0] <= x, cross_x, x1),
        (|p, y| p[1] >= y, cross_y, y0),
        (|p, y| p[1] <= y, cross_y, y1),
    ];
    let mut output = polygon.to_vec();
    for (inside, cross, bound) in edges {
        let input = std::mem::take(&mut output);
        let Some(&last) = input.last() else {
            break;
        };
        let mut previous = last;
        for &point in &input {
            match (inside(point, bound), inside(previous, bound)) {
                (true, true) => output.push(point),
                (true, false) => {
                    output.push(cross(previous, point, bound));
                    output.push(point);
                }
                (false, true) => output.push(cross(previous, point, bound)),
                (false, false) => {}
            }
            previous = point;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: [f64; 4] = [0.0, 0.0, 10.0, 10.0];

    #[test]
    fn a_polyline_leaving_and_reentering_the_plot_becomes_two_runs() {
        let runs = clip_polyline(&[[2.0, 5.0], [15.0, 5.0], [15.0, 8.0], [5.0, 8.0]], RECT);
        assert_eq!(
            runs,
            vec![vec![[2.0, 5.0], [10.0, 5.0]], vec![[10.0, 8.0], [5.0, 8.0]]]
        );
    }

    #[test]
    fn a_polyline_inside_is_untouched_and_one_outside_is_gone() {
        let inside = [[1.0, 1.0], [2.0, 3.0], [4.0, 2.0]];
        assert_eq!(clip_polyline(&inside, RECT), vec![inside.to_vec()]);
        assert!(clip_polyline(&[[-5.0, -5.0], [-1.0, 20.0]], RECT).is_empty());
    }

    #[test]
    fn a_polygon_is_cut_to_the_rectangle() {
        let clipped = clip_polygon(&[[5.0, 5.0], [15.0, 5.0], [15.0, 8.0], [5.0, 8.0]], RECT);
        let mut xs: Vec<f64> = clipped.iter().map(|p| p[0]).collect();
        xs.sort_by(f64::total_cmp);
        assert_eq!(xs.first(), Some(&5.0));
        assert_eq!(xs.last(), Some(&10.0));
        assert!(clipped.iter().all(|p| p[0] <= 10.0));
    }

    #[test]
    fn fill_rings_break_at_gaps_and_split_where_the_bounds_cross() {
        // a above b, a gap, then a crossing between traces 3 and 4.
        let a = [1.0, 1.0, f64::NAN, 1.0, 3.0];
        let b = [2.0, 2.0, 2.0, 2.0, 2.0];
        let rings = fill_rings(&a, &b);
        assert_eq!(rings.len(), 3);
        // The first run: traces 0 and 1, along a and back along b.
        assert_eq!(
            rings[0],
            vec![(0.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0)]
        );
        // The crossing at trace 3.5, shared by the two rings either side.
        assert!(rings[1].contains(&(3.5, 2.0)));
        assert!(rings[2].contains(&(3.5, 2.0)));
    }

    #[test]
    fn the_topographic_shift_is_interpolated_and_clamped() {
        let shift = [0.0_f32, 2.0, 4.0];
        let placement = Placement {
            plot: [0.0, 0.0, 100.0, 100.0],
            traces: [0.0, 10.0],
            rows: [0.0, 100.0],
            shift: Some(&shift),
        };
        assert_eq!(placement.shift_at(0.5), 1.0);
        assert_eq!(placement.shift_at(9.0), 4.0);
        assert_eq!(placement.to_px(1.0, 10.0), [10.0, 12.0]);
    }

    #[test]
    fn selectors_match_users_and_layers() {
        let all = PickSelector::default();
        assert!(all.matches("erik", Some("bed")) && all.matches("erik", None));
        let bed = PickSelector {
            user: None,
            layer: Some("bed".into()),
        };
        assert!(bed.matches("anyone", Some("bed")));
        assert!(!bed.matches("anyone", Some("surface")) && !bed.matches("anyone", None));
        let theirs = PickSelector {
            user: Some("erik".into()),
            layer: None,
        };
        assert!(theirs.matches("erik", Some("x")) && !theirs.matches("other", Some("x")));
    }
}
