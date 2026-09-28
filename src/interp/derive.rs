//! Derived layers and attributes: named expressions over picked layers (#205).
//!
//! A **derived item** is a named Rhai expression evaluated over the reduced
//! picks of one radargram. If the expression yields a *position* it is a
//! **derived layer** (a line, available as depth, TWTT and sample number);
//! anything else is a **derived attribute** (a per-position number exported in
//! its own unit).
//!
//! This module is the pure numerical and evaluator core. It knows nothing
//! about files, HTTP or the project store: like [`crate::interp::level2`], it
//! takes a [`RadargramGeometry`] rather than opening a NetCDF. The document
//! model and persistence live in [`crate::project::derived`].
//!
//! # Reducers
//!
//! More than one pick for one user at one trace is normal -- a layer drawn in
//! several pieces, a line that doubles back -- and the project decides how to
//! collapse it with a [`Reducer`]. Reduction happens **here, at evaluation
//! time**. Stored picks are never rewritten, so changing a reducer is
//! non-destructive and reversible.
//!
//! # Units
//!
//! Every layer value is converted into the expression's unit before the
//! expression runs, and a position result is converted back to the other two
//! units for display and export. Sample numbers may be fractional. Attributes
//! are exported in their own unit and never converted.

#![allow(
    dead_code,
    reason = "the evaluator is consumed by the derived HTTP routes (P6) and the \
              regression test (P7); both land after this phase"
)]

use std::collections::BTreeMap;
use std::fmt;

use gprinterp::{Document, Geometry};
use rhai::{Engine, Scope, AST};
use serde::{Deserialize, Serialize};

use crate::interp::level2::RadargramGeometry;
use crate::project::layers::{LayerSet, Reducer};

/// What an expression's value means (#205).
///
/// Two kinds, not three. An earlier `Length`/`Scalar` split was never acted on
/// by any branch — the only decisions are "is it a layer" — and the stored
/// `unit` already distinguishes a thickness from a count, so the extra terms
/// only reached the UI and confused it. A **Layer** is a drawable position; an
/// **Attribute** is a number per position (a length, a count, a spread).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A point on the radargram: a single depth/trace position.
    Layer,
    /// A number per position with no absolute location (a thickness, a count).
    Attribute,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Layer => "layer",
            Kind::Attribute => "attribute",
        })
    }
}

/// The unit an expression is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Meters,
    Nanoseconds,
    Samples,
    /// No unit: a count, a ratio, a flag.
    ///
    /// The three vertical units all answer "how far down", and a count of
    /// contributors is not an answer to that question -- writing
    /// `count(bed)` as `samples` says something false about it, and only
    /// escapes notice because an attribute is never converted. A position
    /// may not be dimensionless, which [`Unit::allows`] enforces, so this
    /// cannot be used to smuggle a depth past the conversion.
    Dimensionless,
}

impl Unit {
    /// Whether an expression of this `kind` may be written in this unit.
    pub fn allows(self, kind: Kind) -> bool {
        match self {
            Unit::Dimensionless => kind == Kind::Attribute,
            _ => true,
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unit::Meters => "m",
            Unit::Nanoseconds => "ns",
            Unit::Samples => "samples",
            Unit::Dimensionless => "",
        })
    }
}

/// Why a derived item could not be evaluated or validated.
#[derive(Debug, Clone, PartialEq)]
pub enum DeriveError {
    EmptyExpression,
    Compile {
        expression: String,
        message: String,
    },
    Eval {
        expression: String,
        message: String,
    },
    /// The expression did not reduce to one value per position.
    NotScalar {
        expression: String,
        kind: Kind,
    },
    /// A bare layer name in the expression is not defined.
    MissingLayer {
        name: String,
    },
    /// A derived item referenced by the expression is not defined.
    MissingItem {
        name: String,
    },
    /// A cycle in the dependency graph; `path` is the loop, e.g. a, b, a.
    Cycle {
        path: Vec<String>,
    },
}

impl fmt::Display for DeriveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeriveError::EmptyExpression => write!(f, "the expression is empty"),
            DeriveError::Compile {
                expression,
                message,
            } => write!(
                f,
                "the expression '{expression}' could not be parsed: {message}"
            ),
            DeriveError::Eval {
                expression,
                message,
            } => write!(f, "the expression '{expression}' failed: {message}"),
            DeriveError::NotScalar { expression, kind } => write!(
                f,
                "the expression '{expression}' has type {kind}; a derived item must \
                 reduce to a single value per position (for example wrap it in median() \
                 or percentile())"
            ),
            DeriveError::MissingLayer { name } => {
                write!(f, "references missing layer '{name}'")
            }
            DeriveError::MissingItem { name } => {
                write!(f, "references missing derived item '{name}'")
            }
            DeriveError::Cycle { path } => {
                write!(f, "the derived items form a cycle: {}", path.join(" → "))
            }
        }
    }
}

impl std::error::Error for DeriveError {}

/// The evaluated result of one derived item.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedItem {
    pub kind: Kind,
    pub unit: Unit,
    /// One value per grid position, in `unit`.
    pub values: Vec<f64>,
}

/// One grid position an expression is evaluated at.
#[derive(Debug, Clone, PartialEq)]
pub struct GridPosition {
    pub trace: f64,
    pub distance_m: f64,
}

/// At most one value per `(layer, user, position)`. `NaN` where absent.
///
/// Every layer's array has the same shape -- `[position][user]` -- by
/// construction, which is what makes array arithmetic in expressions
/// well-defined even across layers picked by different people.
#[derive(Debug, Clone, PartialEq)]
pub struct ReducedPicks {
    /// Stable, sorted list of contributing users.
    pub users: Vec<String>,
    /// `layer -> [position][user]`, in sample index space (0 at the surface,
    /// increasing downward). `NaN` where a user has no value.
    pub layers: BTreeMap<String, Vec<Vec<f64>>>,
    pub grid: Vec<GridPosition>,
}

impl ReducedPicks {
    pub fn n_positions(&self) -> usize {
        self.grid.len()
    }

    /// The per-user values of one layer at one position, in sample space.
    pub fn values(&self, layer: &str, position: usize) -> Option<&[f64]> {
        self.layers
            .get(layer)
            .and_then(|per_position| per_position.get(position))
            .map(Vec::as_slice)
    }
}

/// Sample every user's features for every layer at every grid position, then
/// reduce and resolve conflicts.
/// `quantize_samples` rounds every sampled value to a whole sample before
/// reducing, and is how the legacy storage is reproduced: legacy pick `y` was
/// `uint16`, so the published consensus is quantised to whole samples. The
/// committed `.gprinterp` fixtures keep the real picked values -- rounding is
/// a property of the old storage, not of the picks -- so only the regression
/// test turns this on.
///
/// Samples outside `[0, n_samples)` are dropped, never clamped: the legacy
/// pipeline yielded NaN for them and dropped them, and clamping an
/// above-surface pick to sample 0 would turn "this contributor drew above the
/// top" into a confident vote at the surface.
pub fn reduce_picks(
    documents: &[(String, Document)],
    layer_set: &LayerSet,
    geometry: &RadargramGeometry,
    grid: &[GridPosition],
    quantize_samples: bool,
) -> ReducedPicks {
    let mut users: Vec<String> = documents.iter().map(|(user, _)| user.clone()).collect();
    users.sort();
    users.dedup();

    // Every layer that is either defined in the vocabulary or actually
    // labelled by a pick. A defined-but-unpicked layer is all NaN, which is
    // the honest answer and keeps the shape uniform.
    let mut layer_ids: Vec<String> = layer_set.layers.iter().map(|l| l.id.clone()).collect();
    for (_, document) in documents {
        for feature in &document.features {
            if let Some(label) = feature.label() {
                layer_ids.push(label.to_string());
            }
        }
    }
    layer_ids.sort();
    layer_ids.dedup();

    let mut layers: BTreeMap<String, Vec<Vec<f64>>> = BTreeMap::new();
    for layer_id in &layer_ids {
        let reducer = layer_set.reducer_for(layer_id);
        let mut per_position: Vec<Vec<f64>> = Vec::with_capacity(grid.len());

        for position in grid {
            // Gather by *slot in `users`*, never by position in `documents`.
            // `users` is sorted, so the two orders coincide only when the
            // caller happens to hand documents over already sorted -- and a
            // slot that does not mean the user `users` says it does turns
            // every per-contributor readout into someone else's data. Two
            // documents for one user merge into one slot rather than
            // overwriting, which also keeps `values` from being indexed past
            // its length.
            let mut raw_per_user: Vec<Vec<f64>> = vec![Vec::new(); users.len()];
            for (user, document) in documents {
                let Some(slot) = users.iter().position(|u| u == user) else {
                    continue;
                };
                for feature in &document.features {
                    if feature.label() != Some(layer_id.as_str()) {
                        continue;
                    }
                    if let Geometry::LineString(vertices) = &feature.geometry {
                        for value in samples_at_trace(vertices, position.trace) {
                            let value = if quantize_samples {
                                value.round()
                            } else {
                                value
                            };
                            if (0.0..geometry.n_samples() as f64).contains(&value) {
                                raw_per_user[slot].push(value);
                            }
                        }
                    }
                }
            }
            per_position.push(
                raw_per_user
                    .iter()
                    .map(|raw| reduce_values(raw, reducer))
                    .collect(),
            );
        }
        layers.insert(layer_id.clone(), per_position);
    }

    resolve_conflicts(&mut layers, layer_set, &layer_ids);

    ReducedPicks {
        users,
        layers,
        grid: grid.to_vec(),
    }
}

/// All feature samples at a trace, interpolated along each segment that spans
/// it. A vertical segment contributes both of its endpoints.
fn samples_at_trace(vertices: &[gprinterp::Position], trace: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for window in vertices.windows(2) {
        let (Some(x0), Some(y0)) = (window[0].x(), window[0].y()) else {
            continue;
        };
        let (Some(x1), Some(y1)) = (window[1].x(), window[1].y()) else {
            continue;
        };
        if x0 == x1 {
            if trace == x0 {
                out.push(y0);
                out.push(y1);
            }
            continue;
        }
        let (lo, hi) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
        if trace < lo || trace > hi {
            continue;
        }
        out.push(y0 + (trace - x0) / (x1 - x0) * (y1 - y0));
    }
    out
}

/// Collapse several values to one. `Shallowest` is the minimum sample index,
/// which is the minimum depth: sample 0 is the surface and the axis grows
/// downward, so "shallowest" and "smallest" are the same direction. (Stated
/// explicitly because the temptation to flip it is real.)
fn reduce_values(values: &[f64], reducer: Reducer) -> f64 {
    let finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return f64::NAN;
    }
    match reducer {
        Reducer::Shallowest => finite.iter().copied().fold(f64::INFINITY, f64::min),
        Reducer::Deepest => finite.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        Reducer::Median => median(&finite),
        Reducer::Mean => finite.iter().sum::<f64>() / finite.len() as f64,
    }
}

/// NaN out every layer involved in an exclusivity conflict (#208).
///
/// When one user holds values for two layers that share a group at one
/// position, *every* layer in any such conflict becomes NaN for that user at
/// that position. Doing it this way means group evaluation order cannot
/// matter, which is why it is the rule rather than "keep the first".
fn resolve_conflicts(
    layers: &mut BTreeMap<String, Vec<Vec<f64>>>,
    layer_set: &LayerSet,
    layer_ids: &[String],
) {
    let conflicts = layer_set.conflicting_pairs();
    if conflicts.is_empty() {
        return;
    }
    let n_positions = layers.values().next().map(Vec::len).unwrap_or(0);
    let n_users = layers
        .values()
        .next()
        .and_then(|p| p.first().map(Vec::len))
        .unwrap_or(0);

    for position in 0..n_positions {
        for user in 0..n_users {
            let present: Vec<&str> = layer_ids
                .iter()
                .filter(|id| {
                    layers
                        .get(*id)
                        .is_some_and(|p| p[position][user].is_finite())
                })
                .map(String::as_str)
                .collect();
            if present.len() < 2 {
                continue;
            }
            let mut tainted: Vec<&str> = Vec::new();
            for (a, b) in &conflicts {
                if present.contains(&a.as_str()) && present.contains(&b.as_str()) {
                    tainted.push(a);
                    tainted.push(b);
                }
            }
            tainted.sort();
            tainted.dedup();
            for id in tainted {
                if let Some(per_position) = layers.get_mut(id) {
                    per_position[position][user] = f64::NAN;
                }
            }
        }
    }
}

/// The order statistic at `floor(p/100 * (n - 1))`, NaN-skipping.
///
/// This is pandas' `quantile(p/100, interpolation="lower")`, the rule the
/// published consensus used. It never interpolates, so the result is always a
/// value some contributor actually picked — which is why it is the only
/// percentile the language offers: an interpolated percentile can return a
/// depth nobody drew, and `median` already covers the common case.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return f64::NAN;
    }
    finite.sort_by(f64::total_cmp);
    let index = ((p / 100.0) * (finite.len() - 1) as f64).floor() as usize;
    finite[index.min(finite.len() - 1)]
}

/// NaN-skipping sample standard deviation (ddof = 1, matching pandas).
pub fn std_dev(values: &[f64]) -> f64 {
    let finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.len() < 2 {
        return f64::NAN;
    }
    let mean = finite.iter().sum::<f64>() / finite.len() as f64;
    let variance =
        finite.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (finite.len() - 1) as f64;
    variance.sqrt()
}

/// NaN-skipping normalised median absolute deviation.
pub fn nmad(values: &[f64]) -> f64 {
    let finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return f64::NAN;
    }
    let centre = median(&finite);
    let deviations: Vec<f64> = finite.iter().map(|v| (v - centre).abs()).collect();
    1.4826 * median(&deviations)
}

fn median(values: &[f64]) -> f64 {
    let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return f64::NAN;
    }
    finite.sort_by(f64::total_cmp);
    let mid = finite.len() / 2;
    if finite.len().is_multiple_of(2) {
        (finite[mid - 1] + finite[mid]) / 2.0
    } else {
        finite[mid]
    }
}

/// A per-user vector of values at one position.
///
/// A newtype rather than a bare `Vec<f64>` so that Rhai's arithmetic operators
/// resolve to element-wise operations on picks rather than to something
/// ambiguous. `NaN` entries are absent values and are skipped by reductions.
#[derive(Debug, Clone, PartialEq)]
pub struct UserArray(pub Vec<f64>);

impl UserArray {
    fn map(self, other: &UserArray, f: impl Fn(f64, f64) -> f64) -> UserArray {
        // NaN propagates: an absent value on either side makes the result
        // absent, so a reduction never silently treats a missing pick as a
        // number.
        let values = self
            .0
            .iter()
            .zip(&other.0)
            .map(|(a, b)| {
                if a.is_nan() || b.is_nan() {
                    f64::NAN
                } else {
                    f(*a, *b)
                }
            })
            .collect();
        UserArray(values)
    }

    fn map_scalar(self, scalar: f64, f: impl Fn(f64, f64) -> f64) -> UserArray {
        UserArray(
            self.0
                .iter()
                .map(|a| if a.is_nan() { f64::NAN } else { f(*a, scalar) })
                .collect(),
        )
    }
}

/// A value used only for static kind inference.
#[derive(Debug, Clone, PartialEq)]
pub struct Kinded {
    is_array: bool,
    kind: Kind,
}

/// Build the sandboxed engine used for both evaluation and kind inference.
///
/// The sandbox is the whole point of using Rhai here: an expression arrives
/// over HTTP from a user, so loops, `eval`, string building and unbounded
/// recursion must all be off. The limits are deliberately tight -- a real
/// expression is a handful of arithmetic operations over a few dozen values.
pub fn build_engine() -> Engine {
    let mut engine = Engine::new();
    engine.register_type_with_name::<UserArray>("UserArray");
    engine.register_type_with_name::<Kinded>("Kinded");

    register_user_array(&mut engine);
    register_kinded(&mut engine);

    // Every number is a decimal number (#270). A whole number is an `i64` in
    // Rhai, and every function and operator here takes an `f64`, so `50`
    // matched nothing: `percentile(bed, 50)` was "Function not found", and
    // worse, Rhai answers a comparison it has no operator for with `false`,
    // so `where(bed > 50, bed, NaN)` silently took the `else` branch for
    // everyone. Registering an `i64` twin of every function would still
    // leave `1 / 2` as integer division and any future function without its
    // twin, so the literal is turned into a float before it is parsed and
    // there is only one number type left to register anything for.
    #[allow(
        deprecated,
        reason = "on_parse_token is marked volatile, not deprecated; \
                  whole_numbers_are_decimal pins what it does here"
    )]
    engine.on_parse_token(|token, _, _| match token {
        rhai::Token::IntegerConstant(n) => rhai::Token::FloatConstant(
            (rhai::FloatWrapper::new(n as f64), n.to_string().into()).into(),
        ),
        _ => token,
    });

    engine.disable_symbol("while");
    engine.disable_symbol("loop");
    engine.disable_symbol("for");
    engine.disable_symbol("do");
    engine.disable_symbol("eval");
    engine.set_max_operations(10_000);
    engine.set_max_call_levels(8);
    engine.set_max_expr_depths(32, 32);
    engine.set_max_array_size(4096);
    engine.set_max_string_size(1024);
    engine
}

fn register_user_array(engine: &mut Engine) {
    // Reductions: UserArray -> f64, all NaN-skipping.
    engine.register_fn("count", |a: UserArray| -> f64 {
        a.0.iter().filter(|v| v.is_finite()).count() as f64
    });
    engine.register_fn("median", |a: UserArray| -> f64 { median(&a.0) });
    engine.register_fn("mean", |a: UserArray| -> f64 {
        let finite: Vec<f64> = a.0.iter().copied().filter(|v| v.is_finite()).collect();
        if finite.is_empty() {
            f64::NAN
        } else {
            finite.iter().sum::<f64>() / finite.len() as f64
        }
    });
    engine.register_fn("std", |a: UserArray| -> f64 { std_dev(&a.0) });
    engine.register_fn("nmad", |a: UserArray| -> f64 { nmad(&a.0) });
    // An empty (or all-NaN) array is NaN, not infinity. Folding from
    // `f64::INFINITY` made `max(no_picks)` return `-inf`, which the sample
    // conversion then read as a real position and drew a fabricated line
    // across the whole radargram -- visible whenever a caller had no pick on
    // some layer.
    engine.register_fn("min", |a: UserArray| -> f64 {
        let finite: Vec<f64> = a.0.iter().copied().filter(|v| v.is_finite()).collect();
        if finite.is_empty() {
            f64::NAN
        } else {
            finite.iter().copied().fold(f64::INFINITY, f64::min)
        }
    });
    engine.register_fn("max", |a: UserArray| -> f64 {
        let finite: Vec<f64> = a.0.iter().copied().filter(|v| v.is_finite()).collect();
        if finite.is_empty() {
            f64::NAN
        } else {
            finite.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        }
    });
    engine.register_fn("percentile", |a: UserArray, p: f64| -> f64 {
        percentile(&a.0, p)
    });

    // Pooling is always explicit: there is no implicit union of layers.
    engine.register_fn("concatenate", |a: UserArray, b: UserArray| -> UserArray {
        let mut values = a.0;
        values.extend(b.0);
        UserArray(values)
    });

    // Element-wise operations. NaN propagates through every one of them.
    engine.register_fn("+", |a: UserArray, b: UserArray| a.map(&b, |x, y| x + y));
    engine.register_fn("-", |a: UserArray, b: UserArray| a.map(&b, |x, y| x - y));
    engine.register_fn("*", |a: UserArray, b: UserArray| a.map(&b, |x, y| x * y));
    engine.register_fn("/", |a: UserArray, b: UserArray| a.map(&b, |x, y| x / y));
    engine.register_fn("+", |a: UserArray, b: f64| a.map_scalar(b, |x, y| x + y));
    engine.register_fn("-", |a: UserArray, b: f64| a.map_scalar(b, |x, y| x - y));
    engine.register_fn("*", |a: UserArray, b: f64| a.map_scalar(b, |x, y| x * y));
    engine.register_fn("/", |a: UserArray, b: f64| a.map_scalar(b, |x, y| x / y));
    // `map_scalar` hands the closure (element, scalar), so with the number on
    // the left the operands are swapped back. `10.0 - bed` once computed
    // `bed - 10.0`.
    engine.register_fn("+", |a: f64, b: UserArray| b.map_scalar(a, |x, y| y + x));
    engine.register_fn("-", |a: f64, b: UserArray| b.map_scalar(a, |x, y| y - x));
    engine.register_fn("*", |a: f64, b: UserArray| b.map_scalar(a, |x, y| y * x));
    engine.register_fn("/", |a: f64, b: UserArray| b.map_scalar(a, |x, y| y / x));

    engine.register_fn("shallowest", |a: UserArray, b: UserArray| {
        a.map(&b, f64::min)
    });
    engine.register_fn("deepest", |a: UserArray, b: UserArray| a.map(&b, f64::max));
    engine.register_fn("shallowest", |a: UserArray, b: f64| {
        a.map_scalar(b, f64::min)
    });
    engine.register_fn("deepest", |a: UserArray, b: f64| a.map_scalar(b, f64::max));
    engine.register_fn("shallowest", |a: f64, b: UserArray| {
        b.map_scalar(a, f64::min)
    });
    engine.register_fn("deepest", |a: f64, b: UserArray| b.map_scalar(a, f64::max));
    engine.register_fn("clamp", |a: UserArray, lo: f64, hi: f64| {
        UserArray(
            a.0.iter()
                .map(|v| {
                    if v.is_nan() {
                        f64::NAN
                    } else {
                        v.clamp(lo, hi)
                    }
                })
                .collect(),
        )
    });
    engine.register_fn("where", |cond: UserArray, a: UserArray, b: UserArray| {
        let values = cond
            .0
            .iter()
            .zip(&a.0)
            .zip(&b.0)
            .map(|((c, x), y)| {
                if c.is_nan() {
                    f64::NAN
                } else if *c != 0.0 {
                    *x
                } else {
                    *y
                }
            })
            .collect();
        UserArray(values)
    });
    engine.register_fn("where", |cond: bool, a: UserArray, b: UserArray| {
        UserArray(
            a.0.iter()
                .zip(&b.0)
                .map(|(x, y)| if cond { *x } else { *y })
                .collect(),
        )
    });

    // Scalar forms of the element-wise helpers.
    //
    // A *layer* is a `UserArray`, but a reference to another derived item is
    // already reduced and binds as a plain `f64`, so `clamp(thickness, 0, d)`
    // is a scalar call. Without these the kind inference pass -- which sees
    // every scalar as a `Kinded`, and so matches the array registrations --
    // accepts such an expression and lets it be saved, and only evaluation
    // fails, with `ErrorFunctionNotFound`. An expression that validates, says
    // "position (a line)" in the editor, saves, and then fails on every read
    // is the worst shape a mistake here can take.
    //
    // Rhai's standard library already supplies scalar `min`, `max`, `abs` and
    // `is_nan`, so only the ones this module defines need a scalar twin.
    engine.register_fn("clamp", |a: f64, lo: f64, hi: f64| -> f64 {
        if a.is_nan() {
            f64::NAN
        } else {
            a.clamp(lo, hi)
        }
    });
    engine.register_fn("shallowest", |a: f64, b: f64| -> f64 {
        nan_aware(a, b, f64::min)
    });
    engine.register_fn("deepest", |a: f64, b: f64| -> f64 {
        nan_aware(a, b, f64::max)
    });
    engine.register_fn("where", |cond: bool, a: f64, b: f64| -> f64 {
        if cond {
            a
        } else {
            b
        }
    });

    // Mixed array/scalar branches. `where(bed > 2.0, bed, 0.0)` is the obvious
    // thing to write, and a scalar branch broadcasts across the users, exactly
    // as it does for `+` and the other element-wise operators.
    engine.register_fn("where", |cond: UserArray, a: UserArray, b: f64| {
        UserArray(pick(&cond.0, &a.0, &vec![b; a.0.len()]))
    });
    engine.register_fn("where", |cond: UserArray, a: f64, b: UserArray| {
        UserArray(pick(&cond.0, &vec![a; b.0.len()], &b.0))
    });
    engine.register_fn("where", |cond: UserArray, a: f64, b: f64| {
        let n = cond.0.len();
        UserArray(pick(&cond.0, &vec![a; n], &vec![b; n]))
    });
    engine.register_fn("where", |cond: bool, a: UserArray, b: f64| {
        UserArray(if cond { a.0 } else { vec![b; a.0.len()] })
    });
    engine.register_fn("where", |cond: bool, a: f64, b: UserArray| {
        UserArray(if cond { vec![a; b.0.len()] } else { b.0 })
    });

    // Comparisons produce a 1.0/0.0 per-user mask, which is what `where`
    // consumes. `if` cannot take one -- that is the error the sandbox explains
    // with a pointer to `where`.
    for op in ["==", "!=", ">", ">=", "<", "<="] {
        engine.register_fn(op, |a: UserArray, b: UserArray| {
            UserArray(
                a.0.iter()
                    .zip(&b.0)
                    .map(|(x, y)| compare(op, *x, *y))
                    .collect(),
            )
        });
        engine.register_fn(op, |a: UserArray, b: f64| {
            UserArray(a.0.iter().map(|x| compare(op, *x, b)).collect())
        });
        engine.register_fn(op, |a: f64, b: UserArray| {
            UserArray(b.0.iter().map(|y| compare(op, a, *y)).collect())
        });
        // Anything else compared with a layer is a mistake, and Rhai's
        // fallback for a comparison it has no operator for is a constant
        // `false`. That fallback is how `bed > 50` used to empty every
        // `where`, so it is refused here rather than trusted.
        engine.register_fn(op, |_a: UserArray, b: rhai::Dynamic| {
            refuse_comparison::<UserArray>(op, "a layer", &b)
        });
        engine.register_fn(op, |a: rhai::Dynamic, _b: UserArray| {
            refuse_comparison::<UserArray>(op, "a layer", &a)
        });
    }

    // `abs` of a layer is element-wise, like the arithmetic operators. Rhai's
    // standard library supplies the scalar form.
    engine.register_fn("abs", |a: UserArray| {
        UserArray(a.0.iter().map(|v| v.abs()).collect())
    });
}

/// The error for a comparison between a layer (or, in inference, any derived
/// value) and something that is not a number.
fn refuse_comparison<T>(
    op: &str,
    this: &str,
    other: &rhai::Dynamic,
) -> Result<T, Box<rhai::EvalAltResult>> {
    Err(format!(
        "cannot compare {this} with {} using '{op}'; compare it with a number or another layer",
        describe_type(other.type_name())
    )
    .into())
}

/// Choose per user from `a` or `b` according to a mask, propagating NaN.
///
/// A NaN in the mask means "this contributor has no opinion here", which is
/// neither branch -- so the result is absent rather than silently the `else`.
fn pick(cond: &[f64], a: &[f64], b: &[f64]) -> Vec<f64> {
    cond.iter()
        .zip(a)
        .zip(b)
        .map(|((c, x), y)| {
            if c.is_nan() {
                f64::NAN
            } else if *c != 0.0 {
                *x
            } else {
                *y
            }
        })
        .collect()
}

/// Combine two scalars the way [`UserArray::map`] combines two elements.
///
/// Explicitly, because `f64::min(NaN, 3.0)` is `3.0` -- Rust's `min`/`max`
/// ignore a NaN operand, while every array operation here *propagates* it so a
/// reduction never treats a missing pick as a number. Reusing `f64::min`
/// directly for the scalar form would make `shallowest` mean one thing on a
/// layer and another on a derived item.
fn nan_aware(a: f64, b: f64, f: impl Fn(f64, f64) -> f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        f(a, b)
    }
}

fn compare(op: &str, a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    let result = match op {
        "==" => a == b,
        "!=" => a != b,
        ">" => a > b,
        ">=" => a >= b,
        "<" => a < b,
        "<=" => a <= b,
        _ => false,
    };
    if result {
        1.0
    } else {
        0.0
    }
}

fn register_kinded(engine: &mut Engine) {
    fn array(kind: Kind) -> Kinded {
        Kinded {
            is_array: true,
            kind,
        }
    }
    fn scalar(kind: Kind) -> Kinded {
        Kinded {
            is_array: false,
            kind,
        }
    }
    /// A plain number, as inference sees it: one value with no position.
    fn number() -> Kinded {
        scalar(Kind::Attribute)
    }
    /// Refuse an array where evaluation only has a scalar form.
    fn single(a: Kinded, function: &str) -> Result<Kinded, Box<rhai::EvalAltResult>> {
        if a.is_array {
            return Err(format!(
                "{function}() takes a single value, not a layer's per-contributor values; \
                 reduce it first, for example with median()"
            )
            .into());
        }
        Ok(a)
    }

    // count/std/nmad flatten any input to Scalar.
    for name in ["count", "std", "nmad"] {
        engine.register_fn(name, |_a: Kinded| -> Kinded { scalar(Kind::Attribute) });
    }
    // median/mean/min/max reduce an array to a scalar while preserving the
    // element kind.
    for name in ["median", "mean", "min", "max"] {
        engine.register_fn(name, |a: Kinded| -> Kinded {
            Kinded {
                is_array: false,
                kind: a.kind,
            }
        });
    }
    engine.register_fn("percentile", |a: Kinded, _p: f64| -> Kinded {
        Kinded {
            is_array: false,
            kind: a.kind,
        }
    });
    engine.register_fn("concatenate", |a: Kinded, _b: Kinded| -> Kinded {
        array(a.kind)
    });
    // `shallowest`/`deepest` are element-wise, so the result is an array only
    // if an operand is. Returning `array(..)` unconditionally said that
    // `shallowest` of two derived items -- both already reduced to scalars --
    // was still an array, which is the opposite of true.
    for name in ["shallowest", "deepest"] {
        engine.register_fn(name, |a: Kinded, b: Kinded| -> Kinded {
            Kinded {
                is_array: a.is_array || b.is_array,
                kind: a.kind,
            }
        });
        engine.register_fn(name, |a: Kinded, _b: f64| -> Kinded { a });
        engine.register_fn(name, |_a: f64, b: Kinded| -> Kinded { b });
    }
    engine.register_fn("clamp", |a: Kinded, _lo: f64, _hi: f64| -> Kinded { a });
    // A bound may itself be a reduced expression (`clamp(x, 0.0,
    // median(bed))`), which is an `f64` at evaluation but a scalar `Kinded`
    // here. Accept those, and refuse an *array* bound, because the real
    // engine has no `clamp` taking one -- the two must agree on what exists.
    fn bounded(
        a: Kinded,
        lo: Option<Kinded>,
        hi: Option<Kinded>,
    ) -> Result<Kinded, Box<rhai::EvalAltResult>> {
        for bound in [lo, hi].into_iter().flatten() {
            if bound.is_array {
                return Err(
                    "clamp bounds must be single values; reduce the bound first, \
                            for example with median()"
                        .into(),
                );
            }
        }
        Ok(a)
    }
    engine.register_fn("clamp", |a: Kinded, lo: Kinded, hi: Kinded| {
        bounded(a, Some(lo), Some(hi))
    });
    engine.register_fn("clamp", |a: Kinded, lo: Kinded, _hi: f64| {
        bounded(a, Some(lo), None)
    });
    engine.register_fn("clamp", |a: Kinded, _lo: f64, hi: Kinded| {
        bounded(a, None, Some(hi))
    });

    // `where` in every combination the real engine has: a condition that is
    // either a per-contributor mask (a `Kinded` array) or a single true/false,
    // and branches that are each a `Kinded` or a plain number. The result is
    // per-contributor if anything going in is, because a mask broadcasts a
    // scalar branch across the contributors. A *scalar* `Kinded` condition is
    // refused, as evaluation has no `where` taking one number.
    fn chosen(mask: bool, a: Option<Kinded>, b: Option<Kinded>) -> Kinded {
        let is_array = mask
            || a.as_ref().is_some_and(|a| a.is_array)
            || b.as_ref().is_some_and(|b| b.is_array);
        let kind = a.or(b).map_or(Kind::Attribute, |k| k.kind);
        Kinded { is_array, kind }
    }
    fn mask(cond: Kinded) -> Result<bool, Box<rhai::EvalAltResult>> {
        if !cond.is_array {
            return Err(
                "where() needs a comparison as its condition, such as bed > 50; \
                 to choose between single values use if"
                    .into(),
            );
        }
        Ok(true)
    }
    engine.register_fn("where", |c: Kinded, a: Kinded, b: Kinded| {
        mask(c).map(|m| chosen(m, Some(a), Some(b)))
    });
    engine.register_fn("where", |c: Kinded, a: Kinded, _b: f64| {
        mask(c).map(|m| chosen(m, Some(a), None))
    });
    engine.register_fn("where", |c: Kinded, _a: f64, b: Kinded| {
        mask(c).map(|m| chosen(m, None, Some(b)))
    });
    engine.register_fn("where", |c: Kinded, _a: f64, _b: f64| {
        mask(c).map(|m| chosen(m, None, None))
    });
    // A comparison between two scalars is a native `bool`, so a `where` over
    // already-reduced derived items reaches these forms.
    engine.register_fn("where", |_c: bool, a: Kinded, b: Kinded| {
        chosen(false, Some(a), Some(b))
    });
    engine.register_fn("where", |_c: bool, a: Kinded, _b: f64| {
        chosen(false, Some(a), None)
    });
    engine.register_fn("where", |_c: bool, _a: f64, b: Kinded| {
        chosen(false, None, Some(b))
    });

    // Rhai's standard library supplies `min`, `max`, `abs` and `is_nan` for
    // `f64`, so evaluation has them for a derived item whether or not
    // inference does. Only `abs` also has a layer form (element-wise); the
    // others refuse a layer here because evaluation would.
    for name in ["min", "max"] {
        engine.register_fn(name, move |a: Kinded, b: Kinded| {
            single(a, name)?;
            single(b, name)
        });
        engine.register_fn(name, move |a: Kinded, _b: f64| single(a, name));
        engine.register_fn(name, move |_a: f64, b: Kinded| single(b, name));
    }
    engine.register_fn("abs", |a: Kinded| -> Kinded { a });
    engine.register_fn("is_nan", |a: Kinded| single(a, "is_nan").map(|_| true));

    // Arithmetic, with a plain number on either side (#270). A number is a
    // value with no position, so it is treated exactly as an attribute is:
    // `median(bed) - 10.0` and `10.0 - median(bed)` are both positions, the
    // second one mirrored about 10 -- and a position that lands above the
    // surface is NaN, which evaluation takes care of. Only a difference of
    // two positions is a distance.
    fn sum(a: Kinded, b: Kinded) -> Kinded {
        let kind = if a.kind == Kind::Layer || b.kind == Kind::Layer {
            Kind::Layer
        } else {
            Kind::Attribute
        };
        Kinded {
            is_array: a.is_array || b.is_array,
            kind,
        }
    }
    fn difference(a: Kinded, b: Kinded) -> Kinded {
        if a.kind == Kind::Layer && b.kind == Kind::Layer {
            Kinded {
                is_array: a.is_array || b.is_array,
                kind: Kind::Attribute,
            }
        } else {
            sum(a, b)
        }
    }
    // Scaling by a plain number keeps the kind, so `median(bed) * 2.0` and
    // `2.0 * median(bed)` are both layers. Two derived values multiplied or
    // divided are something else, and so an attribute.
    fn product(a: Kinded, b: Kinded) -> Kinded {
        Kinded {
            is_array: a.is_array || b.is_array,
            kind: Kind::Attribute,
        }
    }
    fn scaled(a: Kinded, b: Kinded) -> Kinded {
        Kinded {
            is_array: a.is_array || b.is_array,
            kind: a.kind,
        }
    }
    engine.register_fn("+", sum);
    engine.register_fn("+", |a: Kinded, _b: f64| sum(a, number()));
    engine.register_fn("+", |_a: f64, b: Kinded| sum(number(), b));
    engine.register_fn("-", difference);
    engine.register_fn("-", |a: Kinded, _b: f64| difference(a, number()));
    engine.register_fn("-", |_a: f64, b: Kinded| difference(number(), b));
    for op in ["*", "/"] {
        engine.register_fn(op, product);
        engine.register_fn(op, |a: Kinded, _b: f64| scaled(a, number()));
        engine.register_fn(op, |_a: f64, b: Kinded| scaled(b, number()));
    }

    // Comparisons. The actual boolean does not matter for kind inference, so
    // a comparison of two single values is a constant `true`, which lets a
    // data-dependent `if` still be typed. A comparison involving a layer is a
    // per-contributor mask, exactly as in evaluation, so `if bed > 50 {..}`
    // fails here and not first on every read.
    fn compared(a: Kinded, b: Kinded) -> rhai::Dynamic {
        if a.is_array || b.is_array {
            rhai::Dynamic::from(array(Kind::Attribute))
        } else {
            rhai::Dynamic::from(true)
        }
    }
    for op in ["==", "!=", ">", ">=", "<", "<="] {
        engine.register_fn(op, compared);
        engine.register_fn(op, |a: Kinded, _b: f64| compared(a, number()));
        engine.register_fn(op, |_a: f64, b: Kinded| compared(number(), b));
        engine.register_fn(op, |_a: Kinded, b: rhai::Dynamic| {
            refuse_comparison::<rhai::Dynamic>(op, "a layer or derived value", &b)
        });
        engine.register_fn(op, |a: rhai::Dynamic, _b: Kinded| {
            refuse_comparison::<rhai::Dynamic>(op, "a layer or derived value", &a)
        });
    }
}

/// Compile an expression once. Compiling per position would be both slow and
/// a different operations budget, so callers hold the AST.
pub fn compile(engine: &Engine, expression: &str) -> Result<AST, DeriveError> {
    if expression.trim().is_empty() {
        return Err(DeriveError::EmptyExpression);
    }
    engine
        .compile(expression)
        .map_err(|e| DeriveError::Compile {
            expression: expression.to_string(),
            message: e.to_string(),
        })
}

/// Infer an expression's kind without evaluating it on real data.
///
/// `layers` maps every definable layer id to `Position`; `items` maps every
/// derived item that may be referenced to its already-inferred kind. A name
/// missing from both is the `references missing layer 'x'` error.
pub fn infer_kind(
    engine: &Engine,
    ast: &AST,
    expression: &str,
    layers: &[String],
    items: &BTreeMap<String, Kind>,
) -> Result<Kind, DeriveError> {
    let mut scope = Scope::new();
    for layer in layers {
        scope.push(
            layer.clone(),
            Kinded {
                is_array: true,
                kind: Kind::Layer,
            },
        );
    }
    for (id, kind) in items {
        scope.push(
            id.clone(),
            Kinded {
                is_array: false,
                kind: *kind,
            },
        );
    }
    scope.push_constant("NaN", scalar_kinded());

    let result: rhai::Dynamic = engine
        .eval_ast_with_scope(&mut scope, ast)
        .map_err(|e| map_eval_error(expression, &e, layers, items))?;

    if result.is::<Kinded>() {
        let kinded = result.cast::<Kinded>();
        // An expression that is still per-user is not an item. Evaluation
        // refuses it with `NotScalar`, so inference must too: inference runs
        // when an item is saved and evaluation when it is read, and an
        // expression that infers a kind but cannot evaluate saves cleanly,
        // reports "position (a line)" in the editor, and then fails on every
        // read. `bed - temperate_ice` *is* a length, but it is a length per
        // contributor; `median(bed) - median(temperate_ice)` is the item.
        if kinded.is_array {
            return Err(DeriveError::NotScalar {
                expression: expression.to_string(),
                kind: kinded.kind,
            });
        }
        return Ok(kinded.kind);
    }
    // A plain number is a scalar with no spatial meaning.
    Ok(Kind::Attribute)
}

fn scalar_kinded() -> Kinded {
    Kinded {
        is_array: false,
        kind: Kind::Attribute,
    }
}

/// Turn a Rhai evaluation error into our own, resolving a missing variable to
/// the layer/item it names.
pub fn map_eval_error(
    expression: &str,
    error: &rhai::EvalAltResult,
    layers: &[String],
    items: &BTreeMap<String, Kind>,
) -> DeriveError {
    if let rhai::EvalAltResult::ErrorVariableNotFound(name, _) = error {
        if items.contains_key(name.as_str()) {
            return DeriveError::MissingItem {
                name: name.to_string(),
            };
        }
        if !layers.iter().any(|l| l == name.as_str()) {
            return DeriveError::MissingLayer {
                name: name.to_string(),
            };
        }
    }
    DeriveError::Eval {
        expression: expression.to_string(),
        message: describe_error(error),
    }
}

/// Say what went wrong in the terms the expression reference uses.
///
/// Rhai's own messages name the types the engines bind -- `Kinded`,
/// `UserArray`, `f64` -- which mean nothing to someone writing an expression,
/// and `Function not found: + (Kinded, i64)` does not say what to change.
fn describe_error(error: &rhai::EvalAltResult) -> String {
    use rhai::EvalAltResult as E;
    let at = |position: rhai::Position| {
        if position.is_none() {
            String::new()
        } else {
            format!(" ({position})")
        }
    };
    match error {
        E::ErrorFunctionNotFound(signature, position) => {
            // `name (T1, T2)`; an operator's name is its symbol.
            let (name, arguments) = signature
                .split_once(" (")
                .map(|(name, rest)| (name, rest.trim_end_matches(')')))
                .unwrap_or((signature.as_str(), ""));
            let arguments: Vec<&str> = arguments
                .split(", ")
                .filter(|a| !a.is_empty())
                .map(describe_type)
                .collect();
            // Also the error for a misspelt name, so it does not claim the
            // function exists.
            let what = if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                "function"
            } else {
                "operator"
            };
            format!(
                "no {what} '{name}' accepts ({}){}; the expression reference lists the \
                 functions and what each accepts",
                arguments.join(", "),
                at(*position),
            )
        }
        E::ErrorMismatchDataType(expected, actual, position) if expected == "bool" => format!(
            "a condition must be a single true or false, not {}{}; use where(cond, a, b) \
             to choose per contributor",
            describe_type(actual),
            at(*position),
        ),
        E::ErrorInFunctionCall(_, _, inner, _) => describe_error(inner),
        E::ErrorRuntime(message, position) => format!("{message}{}", at(*position)),
        other => other
            .to_string()
            .replace("UserArray", "a layer")
            .replace("Kinded", "a layer or derived value"),
    }
}

/// The name the expression reference uses for a type an engine binds.
fn describe_type(name: &str) -> &str {
    // A type without a registered name is reported by its full path.
    match name.rsplit("::").next().unwrap_or(name) {
        "UserArray" => "a layer",
        // Inference binds layers and derived items alike as a `Kinded`, so
        // it cannot say which of the two this was.
        "Kinded" => "a layer or derived value",
        "f64" | "i64" | "float" | "int" => "a number",
        "bool" => "true/false",
        "string" | "&str" | "ImmutableString" | "char" => "text",
        "()" => "nothing",
        other => other,
    }
}

// -- Unit conversion --------------------------------------------------------

/// The axis's change per sample at one end, from the outermost segment that
/// is not flat.
///
/// A depth axis can be flat near the top -- the legacy conversion put every
/// sample inside the antenna separation at 0 m -- and a flat edge says
/// nothing about how the axis continues. `None` if the whole axis is flat.
fn edge_slope(axis: &[f64], at_start: bool) -> Option<f64> {
    let slope = |i: usize| axis[i + 1] - axis[i];
    let mut segments = 0..axis.len().saturating_sub(1);
    let found = if at_start {
        segments.find(|i| slope(*i) != 0.0)
    } else {
        segments.rev().find(|i| slope(*i) != 0.0)
    };
    found.map(slope)
}

/// The axis value at a fractional sample index, extrapolated linearly past
/// either end.
///
/// A derived layer may lie outside the radargram: a depth corrected by a
/// known offset, `median(bed) + 5.0`, is a real position whether or not the
/// recording reaches it. Clamping it to the edge, or dropping it, would both
/// misstate it. The axes are close to linear at both ends -- sampling is
/// uniform, and depth is `t·v/2` above time zero and approaches that far
/// below the antenna separation -- so the edge spacing continues them.
fn axis_at(axis: &[f64], index: f64) -> f64 {
    if axis.is_empty() || index.is_nan() {
        return f64::NAN;
    }
    let last = axis.len() - 1;
    if index < 0.0 {
        return axis[0] + index * edge_slope(axis, true).unwrap_or(0.0);
    }
    if index > last as f64 {
        return axis[last] + (index - last as f64) * edge_slope(axis, false).unwrap_or(0.0);
    }
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    if lower == upper {
        return axis[lower];
    }
    let fraction = index - lower as f64;
    axis[lower] + fraction * (axis[upper] - axis[lower])
}

/// The fractional sample index of an axis value; the inverse of [`axis_at`],
/// extrapolated past either end in the same way.
fn axis_invert(axis: &[f64], value: f64) -> f64 {
    if axis.len() < 2 || value.is_nan() {
        return f64::NAN;
    }
    // Non-decreasing axes only; depth and twtt both are.
    let last = axis.len() - 1;
    if value < axis[0] {
        return edge_slope(axis, true).map_or(f64::NAN, |slope| (value - axis[0]) / slope);
    }
    if value > axis[last] {
        return edge_slope(axis, false)
            .map_or(f64::NAN, |slope| last as f64 + (value - axis[last]) / slope);
    }
    let index = axis.partition_point(|v| *v < value);
    if index == 0 {
        return 0.0;
    }
    let (lo, hi) = (index - 1, index);
    let span = axis[hi] - axis[lo];
    if span == 0.0 {
        return lo as f64;
    }
    lo as f64 + (value - axis[lo]) / span
}

/// Convert a sample index into `unit`.
pub fn sample_to_unit(sample: f64, unit: Unit, geometry: &RadargramGeometry) -> f64 {
    match unit {
        Unit::Samples | Unit::Dimensionless => sample,
        Unit::Meters => axis_at(&geometry.depth, sample),
        Unit::Nanoseconds => axis_at(&geometry.twtt, sample),
    }
}

/// Convert a `unit` value back into a sample index (the inverse of
/// [`sample_to_unit`]).
pub fn unit_to_sample(value: f64, unit: Unit, geometry: &RadargramGeometry) -> f64 {
    match unit {
        Unit::Samples | Unit::Dimensionless => value,
        Unit::Meters => axis_invert(&geometry.depth, value),
        Unit::Nanoseconds => axis_invert(&geometry.twtt, value),
    }
}

/// Convert a position from one unit to another, via the sample axis.
pub fn convert_position(value: f64, from: Unit, to: Unit, geometry: &RadargramGeometry) -> f64 {
    if from == to {
        return value;
    }
    let sample = unit_to_sample(value, from, geometry);
    sample_to_unit(sample, to, geometry)
}

/// Evaluate an item's AST at one position.
///
/// `layers` are the per-user values (one `UserArray` each) in the expression's
/// unit; `items` are referenced derived values, already in that unit. A
/// non-scalar result is a [`DeriveError::NotScalar`]: a derived item must be
/// one value per position.
pub fn evaluate_at(
    engine: &Engine,
    ast: &AST,
    expression: &str,
    layers: &BTreeMap<String, UserArray>,
    items: &BTreeMap<String, f64>,
    layer_ids: &[String],
    item_kinds: &BTreeMap<String, Kind>,
) -> Result<f64, DeriveError> {
    let mut scope = Scope::new();
    for (id, array) in layers {
        scope.push(id.clone(), array.clone());
    }
    for (id, value) in items {
        scope.push(id.clone(), *value);
    }
    scope.push_constant("NaN", f64::NAN);

    let result: rhai::Dynamic = engine
        .eval_ast_with_scope(&mut scope, ast)
        .map_err(|e| map_eval_error(expression, &e, layer_ids, item_kinds))?;

    if result.is::<f64>() {
        return Ok(result.cast::<f64>());
    }
    Err(DeriveError::NotScalar {
        expression: expression.to_string(),
        kind: Kind::Layer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gprinterp::Document;

    fn geometry() -> RadargramGeometry {
        let n = 101;
        RadargramGeometry {
            radargram_id: "test-line".into(),
            revision_id: "revabc123".into(),
            distance: (0..n).map(|i| i as f64).collect(),
            twtt: (0..50).map(|i| i as f64 * 0.4).collect(),
            depth: (0..50).map(|i| i as f64 * 0.04).collect(),
            easting: (0..n).map(|i| 400_000.0 + i as f64).collect(),
            northing: (0..n).map(|_| 8_700_000.0).collect(),
            longitude: (0..n).map(|i| 15.0 + i as f64 * 1e-5).collect(),
            latitude: (0..n).map(|_| 78.0).collect(),
            antenna_separation_effective_m: Some(0.0),
            twtt_anchor: Some("twtt_normal_incidence".into()),
            crs: "EPSG:32633".into(),
        }
    }

    fn document(lines: &[(&str, &[[f64; 2]])]) -> Document {
        let features: Vec<serde_json::Value> = lines
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
        serde_json::from_value(serde_json::json!({"key": "test-line", "features": features}))
            .unwrap()
    }

    fn layer_set() -> LayerSet {
        use crate::project::layers::Layer;
        let make = |id: &str| Layer {
            id: id.to_string(),
            name: id.to_string(),
            color: None,
            description: None,
            allow_overhangs: false,
            reducer: None,
            warn_on_duplicates: true,
            groups: Vec::new(),
            extra: Default::default(),
        };
        LayerSet {
            layers: vec![make("bed"), make("bed_no_temperate"), make("temperate_ice")],
            ..LayerSet::default()
        }
    }

    fn grid(n: usize) -> Vec<GridPosition> {
        (0..n)
            .map(|i| GridPosition {
                trace: i as f64,
                distance_m: i as f64 * 0.25,
            })
            .collect()
    }

    fn arrays(values: &[f64]) -> UserArray {
        UserArray(values.to_vec())
    }

    /// Kind inference and evaluation must accept exactly the same
    /// expressions.
    ///
    /// They are two engines over two parallel type tables, so a function
    /// registered on one and not the other makes them disagree — and the
    /// disagreement is silent in the worst direction: inference runs when an
    /// item is *saved*, evaluation when it is *read*. An expression that
    /// infers but does not evaluate validates, reports its kind in the editor,
    /// saves, and then fails on every read afterwards.
    ///
    /// This is the class of bug, not an instance: it caught a missing scalar
    /// `clamp` (inference accepted, evaluation did not) and a missing
    /// `where(bool, ..)` on the inference side (the reverse). Add a row
    /// whenever a function is registered.
    #[test]
    fn inference_and_evaluation_accept_the_same_expressions() {
        let engine = build_engine();
        // `bed` is a layer (an array); `dep` is a reference to another derived
        // item, which is already reduced and so binds as a scalar.
        let layers = vec!["bed".to_string()];
        let mut items = BTreeMap::new();
        items.insert("dep".to_string(), Kind::Layer);
        let mut other = BTreeMap::new();
        other.insert("dep".to_string(), 5.0_f64);

        for expression in [
            // Arrays.
            "median(bed)",
            "clamp(bed, 0.0, 10.0)",
            "shallowest(bed, 3.0)",
            "where(bed > 2.0, bed, 0.0 * bed)",
            // Scalars, via a derived-item reference. These are the ones the
            // two engines used to disagree about.
            "clamp(dep, 0.0, 10.0)",
            "shallowest(dep, 3.0)",
            "min(dep, 3.0)",
            "max(dep, 3.0)",
            "where(dep > 2.0, dep, 0.0)",
            "if is_nan(dep) { 0.0 } else { dep }",
            // Mixed array/scalar, which is what most real expressions are.
            "median(bed) - dep",
            "clamp(median(bed) - dep, 0.0, median(bed))",
            "where(bed > 2.0, bed, 0.0)",
            "where(bed > 2.0, 0.0, bed)",
            "shallowest(median(bed), dep)",
            "min(median(bed), dep)",
            // An array bound on clamp exists on neither engine.
            "clamp(median(bed), 0.0, bed)",
            // Whole numbers (#270), which used to be refused by one engine,
            // the other, or both.
            "median(where(bed > 50, bed, NaN))",
            "median(where(bed == 60, bed, NaN))",
            "median(bed) + 50",
            "median(bed) * 2",
            "median(bed + 50)",
            "percentile(bed, 50)",
            "clamp(median(bed), 0, 10)",
            "clamp(dep, 0, 10)",
            "shallowest(bed, 3)",
            // A number on the left, and `abs` of a derived value.
            "2 * median(bed)",
            "2.0 * median(bed)",
            "10.0 - median(bed)",
            "10 / median(bed)",
            "1 + dep",
            "median(2.0 * bed)",
            "median(10.0 - bed)",
            "abs(median(bed) - 5.0)",
            "abs(dep)",
            "median(abs(bed))",
            "shallowest(3.0, median(bed))",
            "shallowest(dep, bed)",
            "median(deepest(3, bed))",
            // Both sides of a comparison, and its per-contributor result.
            "median(where(50 < bed, bed, NaN))",
            "median(where(bed > dep, bed, NaN))",
            "where(median(bed) > 2, median(bed), NaN)",
            "if median(bed) > 2 { median(bed) } else { NaN }",
            "if bed > 2 { median(bed) } else { NaN }",
            "where(bed > 2, dep, 0)",
            "median(where(bed > 2, dep, 0))",
            "where(median(bed), bed, 0)",
            // Scalar-only helpers from Rhai's standard library.
            "min(bed, 3.0)",
            "max(dep, bed)",
            "if is_nan(bed) { 0.0 } else { dep }",
            // A comparison with something that is not a number.
            "median(where(bed > true, bed, NaN))",
            "median(where(true == bed, bed, NaN))",
            "median(where(bed > \"50\", bed, NaN))",
        ] {
            let ast = compile(&engine, expression).unwrap_or_else(|e| {
                panic!("{expression} did not compile: {e}");
            });
            let inferred = infer_kind(&engine, &ast, expression, &layers, &items);

            let bound: BTreeMap<String, UserArray> = layers
                .iter()
                .map(|id| (id.clone(), UserArray(vec![1.0, 4.0, f64::NAN])))
                .collect();
            let evaluated = evaluate_at(&engine, &ast, expression, &bound, &other, &layers, &items);

            assert_eq!(
                inferred.is_ok(),
                evaluated.is_ok(),
                "{expression}: inference {:?} but evaluation {:?}",
                inferred
                    .as_ref()
                    .map(|k| k.to_string())
                    .map_err(|e| e.to_string()),
                evaluated
                    .as_ref()
                    .map(|v| v.to_string())
                    .map_err(|e| e.to_string()),
            );
        }
    }

    /// A whole number is the same number as its decimal form everywhere
    /// (#270). Every row is from the issue, evaluated with the value the
    /// decimal spelling gives.
    #[test]
    fn whole_numbers_are_decimal() {
        let tens: &[(&str, &[f64])] = &[("bed", &[60.0, 70.0, 80.0])];
        let ones: &[(&str, &[f64])] = &[("bed", &[1.0, 2.0, 3.0])];
        for (expression, layers, expected) in [
            // These two used to be NaN, silently: `bed > 50` was `false`.
            ("median(where(bed > 50, bed, NaN))", tens, 70.0),
            ("median(where(bed == 60, bed, NaN))", tens, 60.0),
            ("median(where(bed > 50.0, bed, NaN))", tens, 70.0),
            ("median(where(65 < bed, bed, NaN))", tens, 75.0),
            ("median(bed) + 50", ones, 52.0),
            ("median(bed) * 2", ones, 4.0),
            ("median(bed + 50)", ones, 52.0),
            ("percentile(bed, 50)", ones, 2.0),
            ("clamp(median(bed), 0, 10)", ones, 2.0),
            ("2 * median(bed)", ones, 4.0),
            ("2.0 * median(bed)", ones, 4.0),
            ("10.0 - median(bed)", ones, 8.0),
            ("median(10 - bed)", ones, 8.0),
            ("abs(median(bed) - 5.0)", ones, 3.0),
            ("median(abs(bed - 3))", ones, 1.0),
            // Integer division would make this 0.
            ("median(bed) + 1 / 2", ones, 2.5),
            ("median(12 / bed)", ones, 6.0),
        ] {
            assert_eq!(eval(expression, layers), expected, "{expression}");
        }
    }

    /// Rhai answers a comparison it has no operator for with a constant
    /// `false`, which is how `bed > 50` used to empty every `where` without
    /// a word. Comparing a layer with anything but a number must fail, on
    /// both engines, and say so in the reference's terms.
    #[test]
    fn a_comparison_with_a_non_number_fails_rather_than_being_false() {
        let engine = build_engine();
        let layers = vec!["bed".to_string()];
        let expression = "median(where(bed > true, bed, NaN))";
        let ast = compile(&engine, expression).unwrap();

        let inferred = infer_kind(&engine, &ast, expression, &layers, &BTreeMap::new());
        let message = inferred.unwrap_err().to_string();
        assert!(message.contains("cannot compare"), "{message}");

        let mut bound = BTreeMap::new();
        bound.insert("bed".to_string(), arrays(&[60.0, 70.0]));
        let evaluated = evaluate_at(
            &engine,
            &ast,
            expression,
            &bound,
            &BTreeMap::new(),
            &layers,
            &BTreeMap::new(),
        );
        let message = evaluated.unwrap_err().to_string();
        assert!(
            message.contains("cannot compare a layer with true/false"),
            "{message}"
        );
    }

    /// An error names things the way the documentation does, never by the
    /// types the engines bind.
    #[test]
    fn errors_do_not_name_internal_types() {
        let engine = build_engine();
        let layers = vec!["bed".to_string()];
        let mut items = BTreeMap::new();
        items.insert("dep".to_string(), Kind::Layer);
        let mut other = BTreeMap::new();
        other.insert("dep".to_string(), 5.0_f64);
        let mut bound = BTreeMap::new();
        bound.insert("bed".to_string(), arrays(&[1.0, 2.0]));

        for expression in [
            "percentile(bed)",
            "median(bed, 2)",
            "concatenate(bed, 1)",
            "median(bed) + true",
            "if bed > 2 { 1 } else { 0 }",
            "min(bed, 3)",
            "is_nan(bed)",
            "where(median(bed), bed, 0)",
            "clamp(median(bed), 0, bed)",
            "median(bed > \"x\")",
            "no_such_function(bed)",
        ] {
            let ast = compile(&engine, expression).unwrap();
            let messages = [
                infer_kind(&engine, &ast, expression, &layers, &items).map(|_| ()),
                evaluate_at(&engine, &ast, expression, &bound, &other, &layers, &items).map(|_| ()),
            ]
            .map(|result| {
                result
                    .expect_err(&format!("{expression} should fail"))
                    .to_string()
            });
            for message in messages {
                for internal in ["Kinded", "UserArray", "f64", "i64", "Function not found"] {
                    assert!(
                        !message.contains(internal),
                        "{expression}: '{message}' names {internal}"
                    );
                }
            }
        }
    }

    #[test]
    fn percentile_selects_the_legacy_order_statistic() {
        assert_eq!(percentile(&[10.0, 20.0, 30.0, 40.0, 50.0], 49.0), 20.0);
        // floor(0.49 * 4) = 1 -> the second order statistic.
        assert_eq!(percentile(&[50.0, 10.0, 40.0, 20.0, 30.0], 49.0), 20.0);
        assert_eq!(percentile(&[], 49.0).is_nan(), true);
        // It never interpolates, so it cannot return a value nobody picked.
        assert_eq!(percentile(&[0.0, 10.0], 50.0), 0.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.0), 1.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 100.0), 4.0);
    }

    #[test]
    fn std_dev_matches_pandas_ddof_one() {
        // pandas: Series([1,2,3,4]).std() == 1.2909944487358056
        let value = std_dev(&[1.0, 2.0, 3.0, 4.0]);
        assert!((value - 1.2909944487358056).abs() < 1e-12, "{value}");
        assert!(std_dev(&[1.0]).is_nan());
    }

    #[test]
    fn nmad_matches_a_hand_computed_value() {
        // median = 3, abs deviations = [2,1,0,1,97], median = 1.
        assert!((nmad(&[1.0, 2.0, 3.0, 4.0, 100.0]) - 1.4826).abs() < 1e-12);
    }

    #[test]
    fn sample_and_unit_round_trip() {
        let geometry = geometry();
        let sample = 7.3;
        let metres = sample_to_unit(sample, Unit::Meters, &geometry);
        let back = unit_to_sample(metres, Unit::Meters, &geometry);
        assert!((back - sample).abs() < 1e-9, "{back} vs {sample}");

        let nanos = sample_to_unit(sample, Unit::Nanoseconds, &geometry);
        let back = unit_to_sample(nanos, Unit::Nanoseconds, &geometry);
        assert!((back - sample).abs() < 1e-9);
    }

    /// Past either end the axis continues from its outermost segment that
    /// is not flat, and the two directions stay inverses.
    #[test]
    fn conversion_extrapolates_past_the_axis() {
        let even = [0.0, 1.0, 2.0, 3.0];
        assert_eq!(axis_at(&even, -2.0), -2.0);
        assert_eq!(axis_at(&even, 5.5), 5.5);
        assert_eq!(axis_invert(&even, -2.0), -2.0);
        assert_eq!(axis_invert(&even, 5.5), 5.5);

        // Flat at the top, as the legacy depth conversion is inside the
        // antenna separation: the first rising segment sets the slope.
        let flat_top = [0.0, 0.0, 0.5, 1.0];
        assert_eq!(axis_at(&flat_top, -1.0), -0.5);
        assert_eq!(axis_invert(&flat_top, -0.5), -1.0);
        // Uneven spacing at the bottom: the last segment sets it.
        let uneven = [0.0, 1.0, 3.0];
        assert_eq!(axis_at(&uneven, 3.0), 5.0);
        assert_eq!(axis_invert(&uneven, 5.0), 3.0);

        // A wholly flat axis says nothing about where it goes.
        assert!(axis_invert(&[1.0, 1.0], 2.0).is_nan());
        assert!(axis_at(&[], 0.0).is_nan());
    }

    #[test]
    fn a_nanoseconds_expression_agrees_with_the_metres_form() {
        // 0.4 ns and 0.04 m per sample, so a thickness of 10 samples is 0.4 m
        // and 4.0 ns -- the same physical answer.
        let geometry = geometry();
        let bed_sample = 20.0;
        let cts_sample = 10.0;
        let metres = sample_to_unit(bed_sample, Unit::Meters, &geometry)
            - sample_to_unit(cts_sample, Unit::Meters, &geometry);
        let nanos = sample_to_unit(bed_sample, Unit::Nanoseconds, &geometry)
            - sample_to_unit(cts_sample, Unit::Nanoseconds, &geometry);
        assert!((metres - 0.4).abs() < 1e-9, "{metres}");
        assert!((nanos - 4.0).abs() < 1e-9, "{nanos}");
        // Convert the ns length back through samples for comparison.
        let nanos_in_samples = unit_to_sample(nanos, Unit::Nanoseconds, &geometry)
            - unit_to_sample(0.0, Unit::Nanoseconds, &geometry);
        let metres_in_samples = unit_to_sample(metres, Unit::Meters, &geometry);
        assert!((nanos_in_samples - metres_in_samples).abs() < 1e-9);
    }

    #[test]
    fn reducers_collapse_multiple_values_per_user() {
        // User a has a vertical segment at trace 5, so it holds two values
        // there: 10 and 40. User b is flat at 20.
        let documents = vec![
            (
                "a".to_string(),
                document(&[(
                    "bed",
                    &[[0.0, 10.0], [5.0, 10.0], [5.0, 40.0], [10.0, 40.0]],
                )]),
            ),
            (
                "b".to_string(),
                document(&[("bed", &[[0.0, 20.0], [10.0, 20.0]])]),
            ),
        ];
        let geometry = geometry();
        let grid = grid(11);

        let mut set = layer_set();
        set.layers[0].reducer = Some(Reducer::Shallowest);
        let reduced = reduce_picks(&documents, &set, &geometry, &grid, false);
        assert_eq!(reduced.users, vec!["a".to_string(), "b".to_string()]);
        // Shallowest is the minimum sample index.
        assert_eq!(reduced.values("bed", 5).unwrap(), &[10.0, 20.0]);

        // Deepest picks the larger.
        set.layers[0].reducer = Some(Reducer::Deepest);
        let deep = reduce_picks(&documents, &set, &geometry, &grid, false);
        assert_eq!(deep.values("bed", 5).unwrap(), &[40.0, 20.0]);

        // A user absent at a position is NaN.
        let documents = vec![(
            "a".to_string(),
            document(&[("bed", &[[0.0, 10.0], [1.0, 12.0]])]),
        )];
        let reduced = reduce_picks(&documents, &set, &geometry, &grid, false);
        assert!(reduced.values("bed", 5).unwrap()[0].is_nan());
    }

    #[test]
    fn conflicting_layers_are_nan_for_that_user() {
        use crate::project::layers::ExclusivityGroup;
        let documents = vec![(
            "a".to_string(),
            document(&[
                ("bed", &[[0.0, 10.0], [10.0, 10.0]]),
                ("bed_no_temperate", &[[0.0, 12.0], [10.0, 12.0]]),
            ]),
        )];
        let mut set = layer_set();
        set.groups = vec![ExclusivityGroup {
            id: "g1".into(),
            name: "g1".into(),
            members: vec!["bed".into(), "bed_no_temperate".into()],
            extra: Default::default(),
        }];
        let reduced = reduce_picks(&documents, &set, &geometry(), &grid(11), false);
        // Both conflicting layers are NaN for the user at every position.
        assert!(reduced.values("bed", 0).unwrap()[0].is_nan());
        assert!(reduced.values("bed_no_temperate", 0).unwrap()[0].is_nan());
        // A layer not in the conflict is untouched.
        assert_ne!(reduced.values("temperate_ice", 0), None);
    }

    /// The Svalbard bed vocabulary, including the "not visible" class. Kept
    /// separate from [`layer_set`] so the existing tests keep their shape.
    fn layer_set_with_not_visible() -> LayerSet {
        use crate::project::layers::Layer;
        let mut set = layer_set();
        set.layers.push(Layer {
            id: "bed_not_visible".to_string(),
            name: "Glacier bed not visible".to_string(),
            color: None,
            description: None,
            allow_overhangs: false,
            reducer: None,
            warn_on_duplicates: true,
            groups: Vec::new(),
            extra: Default::default(),
        });
        set
    }

    fn group(id: &str, members: &[&str]) -> crate::project::layers::ExclusivityGroup {
        crate::project::layers::ExclusivityGroup {
            id: id.to_string(),
            name: id.to_string(),
            members: members.iter().map(|m| m.to_string()).collect(),
            extra: Default::default(),
        }
    }

    fn line(label: &str, sample: f64) -> (&str, Vec<[f64; 2]>) {
        (label, vec![[0.0, sample], [10.0, sample]])
    }

    /// Reduce real documents, then evaluate `expression` at one grid position.
    ///
    /// Goes through `reduce_picks` rather than binding arrays by hand, so the
    /// reducer, the conflict rule and the expression are all exercised
    /// together -- which is the path a real request takes.
    fn eval_over_documents(
        expression: &str,
        set: &LayerSet,
        documents: &[(String, Document)],
        position: usize,
    ) -> f64 {
        let reduced = reduce_picks(documents, set, &geometry(), &grid(11), false);
        let engine = build_engine();
        let ast = compile(&engine, expression).unwrap();
        let bound: BTreeMap<String, UserArray> = reduced
            .layers
            .iter()
            .map(|(id, per_position)| (id.clone(), UserArray(per_position[position].clone())))
            .collect();
        let layer_ids: Vec<String> = bound.keys().cloned().collect();
        evaluate_at(
            &engine,
            &ast,
            expression,
            &bound,
            &BTreeMap::new(),
            &layer_ids,
            &BTreeMap::new(),
        )
        .unwrap()
    }

    /// The study's bed consensus, as shipped in the example project.
    const BED_CONSENSUS: &str = "if count(bed) + count(bed_no_temperate) >= \
                                 count(bed_not_visible) { \
                                 percentile(concatenate(bed, bed_no_temperate), 49.0) \
                                 } else { NaN }";

    /// The legacy dataset has no `bed_not_visible` picks at all, so the
    /// regression test can never take this branch: `count` of an absent layer
    /// is always zero and the guard always passes. Synthetic votes are the
    /// only way to pin the rule that #205 and #208 exist for.
    #[test]
    fn a_majority_of_not_visible_votes_erases_the_bed() {
        let documents = vec![
            (
                "a".to_string(),
                document(&[("bed", &[[0.0, 10.0], [10.0, 10.0]])]),
            ),
            (
                "b".to_string(),
                document(&[("bed_not_visible", &[[0.0, 30.0], [10.0, 30.0]])]),
            ),
            (
                "c".to_string(),
                document(&[("bed_not_visible", &[[0.0, 31.0], [10.0, 31.0]])]),
            ),
        ];
        let value =
            eval_over_documents(BED_CONSENSUS, &layer_set_with_not_visible(), &documents, 0);
        assert!(
            value.is_nan(),
            "one bed vote against two 'not visible' votes must erase the bed, got {value}"
        );
    }

    /// A tie is *kept*. The legacy pipeline dropped a position only where
    /// `missing > existing`, so the guard is `>=` on the existing side; a `>`
    /// here would silently shorten every profile at its ambiguous ends.
    #[test]
    fn a_tied_vote_keeps_the_bed() {
        let documents = vec![
            (
                "a".to_string(),
                document(&[("bed", &[[0.0, 10.0], [10.0, 10.0]])]),
            ),
            (
                "b".to_string(),
                document(&[("bed", &[[0.0, 20.0], [10.0, 20.0]])]),
            ),
            (
                "c".to_string(),
                document(&[("bed_not_visible", &[[0.0, 30.0], [10.0, 30.0]])]),
            ),
            (
                "d".to_string(),
                document(&[("bed_not_visible", &[[0.0, 31.0], [10.0, 31.0]])]),
            ),
        ];
        let value =
            eval_over_documents(BED_CONSENSUS, &layer_set_with_not_visible(), &documents, 0);
        // Two bed values, so the 49th lower percentile is the first of them.
        assert_eq!(value, 10.0, "a 2-2 tie must keep the bed");
    }

    /// `users[i]` must name the contributor whose value is at slot `i`, for
    /// any document order. Reductions are order-insensitive, so a misalignment
    /// here leaves every consensus number correct and silently attributes each
    /// value to the wrong person -- which only surfaces once something reads
    /// per-contributor, as the layer panel (#209) does.
    #[test]
    fn user_slots_line_up_with_the_user_list_whatever_order_documents_arrive_in() {
        let set = layer_set();
        let zoe = (
            "zoe".to_string(),
            document(&[("bed", &[[0.0, 10.0], [10.0, 10.0]])]),
        );
        let amy = (
            "amy".to_string(),
            document(&[("bed", &[[0.0, 20.0], [10.0, 20.0]])]),
        );

        for documents in [vec![zoe.clone(), amy.clone()], vec![amy, zoe]] {
            let reduced = reduce_picks(&documents, &set, &geometry(), &grid(11), false);
            let bed = reduced.values("bed", 0).unwrap();
            let slot = |name: &str| reduced.users.iter().position(|u| u == name).unwrap();
            assert_eq!(bed[slot("zoe")], 10.0, "zoe's value follows her name");
            assert_eq!(bed[slot("amy")], 20.0, "amy's value follows hers");
        }
    }

    /// Exclusivity is deliberately not transitive, which is the whole reason
    /// a layer may belong to several groups rather than carrying one "group"
    /// field. A and B conflict, B and C conflict, A and C coexist.
    #[test]
    fn exclusivity_is_not_transitive_across_groups() {
        let mut set = layer_set();
        set.groups = vec![
            group("g1", &["bed", "bed_no_temperate"]),
            group("g2", &["bed_no_temperate", "temperate_ice"]),
        ];

        // A + C: no shared group, so both survive.
        let both_ends = vec![(
            "a".to_string(),
            document(&[
                ("bed", &[[0.0, 10.0], [10.0, 10.0]]),
                ("temperate_ice", &[[0.0, 5.0], [10.0, 5.0]]),
            ]),
        )];
        let reduced = reduce_picks(&both_ends, &set, &geometry(), &grid(11), false);
        assert_eq!(reduced.values("bed", 0).unwrap()[0], 10.0);
        assert_eq!(reduced.values("temperate_ice", 0).unwrap()[0], 5.0);

        // B + C: they share g2, so both go NaN.
        let middle_and_end = vec![(
            "a".to_string(),
            document(&[
                ("bed_no_temperate", &[[0.0, 12.0], [10.0, 12.0]]),
                ("temperate_ice", &[[0.0, 5.0], [10.0, 5.0]]),
            ]),
        )];
        let reduced = reduce_picks(&middle_and_end, &set, &geometry(), &grid(11), false);
        assert!(reduced.values("bed_no_temperate", 0).unwrap()[0].is_nan());
        assert!(reduced.values("temperate_ice", 0).unwrap()[0].is_nan());
    }

    /// A conflict is one contributor's problem, not the position's. If it
    /// tainted every user the consensus would collapse wherever one person
    /// contradicted themselves.
    #[test]
    fn a_conflict_only_taints_the_contributor_who_has_it() {
        let mut set = layer_set();
        set.groups = vec![group("g1", &["bed", "bed_no_temperate"])];
        let documents = vec![
            (
                "conflicted".to_string(),
                document(&[
                    ("bed", &[[0.0, 10.0], [10.0, 10.0]]),
                    ("bed_no_temperate", &[[0.0, 12.0], [10.0, 12.0]]),
                ]),
            ),
            (
                "clean".to_string(),
                document(&[("bed", &[[0.0, 20.0], [10.0, 20.0]])]),
            ),
        ];
        let reduced = reduce_picks(&documents, &set, &geometry(), &grid(11), false);
        let bed = reduced.values("bed", 0).unwrap();
        let conflicted = reduced
            .users
            .iter()
            .position(|u| u == "conflicted")
            .unwrap();
        let clean = reduced.users.iter().position(|u| u == "clean").unwrap();
        assert!(
            bed[conflicted].is_nan(),
            "the conflicted contributor loses their bed"
        );
        assert_eq!(bed[clean], 20.0, "the other contributor keeps theirs");
    }

    fn eval(expression: &str, layers: &[(&str, &[f64])]) -> f64 {
        let engine = build_engine();
        let ast = compile(&engine, expression).unwrap();
        let bound: BTreeMap<String, UserArray> = layers
            .iter()
            .map(|(id, values)| (id.to_string(), arrays(values)))
            .collect();
        let layer_ids: Vec<String> = layers.iter().map(|(id, _)| id.to_string()).collect();
        evaluate_at(
            &engine,
            &ast,
            expression,
            &bound,
            &BTreeMap::new(),
            &layer_ids,
            &BTreeMap::new(),
        )
        .unwrap()
    }

    #[test]
    fn count_ignores_nan() {
        assert_eq!(eval("count(bed)", &[("bed", &[1.0, f64::NAN, 3.0])]), 2.0);
    }

    #[test]
    fn min_and_max_of_an_empty_pool_are_nan_not_infinity() {
        // Folding from +/-infinity made `max(no_picks)` a finite -inf, which
        // then converted to a real sample and drew a fabricated line.
        assert!(eval("max(bed)", &[("bed", &[f64::NAN, f64::NAN])]).is_nan());
        assert!(eval("min(bed)", &[("bed", &[f64::NAN])]).is_nan());
        assert_eq!(eval("max(bed)", &[("bed", &[1.0, f64::NAN, 3.0])]), 3.0);
        assert_eq!(eval("min(bed)", &[("bed", &[1.0, f64::NAN, 3.0])]), 1.0);
    }

    #[test]
    fn median_handles_even_lengths() {
        assert_eq!(eval("median(bed)", &[("bed", &[1.0, 2.0, 3.0, 4.0])]), 2.5);
    }

    #[test]
    fn concatenate_pools_with_nan_preserved() {
        assert_eq!(
            eval(
                "count(concatenate(a, b))",
                &[("a", &[1.0, f64::NAN]), ("b", &[2.0, 3.0])]
            ),
            3.0
        );
    }

    #[test]
    fn nan_propagates_through_elementwise_arithmetic() {
        // 10 + (NaN) is NaN; median skips it, so median of [10, NaN+10] is 10.
        assert_eq!(
            eval("median(bed + 10.0)", &[("bed", &[0.0, f64::NAN])]),
            10.0
        );
        assert_eq!(eval("count(bed + 10.0)", &[("bed", &[0.0, f64::NAN])]), 1.0);
    }

    #[test]
    fn percentile_builtin_matches_the_reference() {
        assert_eq!(
            eval(
                "percentile(bed, 49.0)",
                &[("bed", &[50.0, 10.0, 40.0, 20.0, 30.0])]
            ),
            20.0
        );
    }

    #[test]
    fn while_is_disabled_at_compile_time() {
        let engine = build_engine();
        assert!(compile(&engine, "while true { }").is_err());
        assert!(compile(&engine, "for x in [1,2] { }").is_err());
        assert!(compile(&engine, "eval(\"1\")").is_err());
    }

    #[test]
    fn an_expensive_expression_hits_the_operations_cap() {
        // A sum over a variable cannot be constant-folded, so each `+` costs
        // an operation. With a tiny budget the engine must refuse rather than
        // keep going. (The production budget is 10_000; this pins the
        // mechanism without needing a 10_000-term expression.)
        let mut engine = build_engine();
        engine.set_max_operations(5);
        let expression = std::iter::repeat("x")
            .take(20)
            .collect::<Vec<_>>()
            .join(" + ");
        let ast = engine.compile(&expression).unwrap();
        let mut scope = Scope::new();
        scope.push("x", 1.0_f64);
        let result = engine.eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &ast);
        assert!(result.is_err(), "the operations cap did not fire");
    }

    #[test]
    fn an_array_result_is_not_a_scalar() {
        let engine = build_engine();
        let ast = compile(&engine, "bed").unwrap();
        let mut bound = BTreeMap::new();
        bound.insert("bed".to_string(), arrays(&[1.0, 2.0]));
        let error = evaluate_at(
            &engine,
            &ast,
            "bed",
            &bound,
            &BTreeMap::new(),
            &["bed".to_string()],
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(matches!(error, DeriveError::NotScalar { .. }), "{error}");
    }

    #[test]
    fn kind_inference_pins_the_positional_rules() {
        let engine = build_engine();
        let layers = vec![
            "bed".to_string(),
            "temperate_ice".to_string(),
            "bed_no_temperate".to_string(),
            "bed_not_visible".to_string(),
        ];
        let items = BTreeMap::new();
        let infer = |expression: &str| {
            let ast = compile(&engine, expression).unwrap();
            infer_kind(&engine, &ast, expression, &layers, &items).unwrap()
        };
        assert_eq!(infer("median(bed)"), Kind::Layer);
        // Reduced on both sides: `bed - temperate_ice` is a length *per
        // contributor*, which is not an item -- see `infer_kind`.
        assert_eq!(
            infer("median(bed) - median(temperate_ice)"),
            Kind::Attribute
        );
        assert_eq!(infer("count(bed)"), Kind::Attribute);
        assert_eq!(infer("std(bed)"), Kind::Attribute);
        // A plain number shifts or scales a position from either side, and
        // an attribute shifts it just as a number does (#270).
        for expression in [
            "median(bed) + 2",
            "2 + median(bed)",
            "median(bed) - 10.0",
            "10.0 - median(bed)",
            "2 * median(bed)",
            "median(bed) / 2",
            "abs(median(bed))",
            "count(bed) + median(bed)",
            "std(bed) - median(bed)",
        ] {
            assert_eq!(infer(expression), Kind::Layer, "{expression}");
        }
        for expression in [
            "median(bed) * median(temperate_ice)",
            "count(bed) + 1",
            "10.0 - count(bed)",
        ] {
            assert_eq!(infer(expression), Kind::Attribute, "{expression}");
        }
        assert_eq!(
            infer(
                "if count(bed) + count(bed_no_temperate) >= count(bed_not_visible) { \
                 percentile(concatenate(bed, bed_no_temperate), 49.0) } else { NaN }"
            ),
            Kind::Layer
        );
    }

    #[test]
    fn a_missing_layer_is_named_in_the_error() {
        let engine = build_engine();
        let ast = compile(&engine, "median(no_such)").unwrap();
        let error = infer_kind(
            &engine,
            &ast,
            "median(no_such)",
            &["bed".to_string()],
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "references missing layer 'no_such'");
    }
}
