//! Derived items: named expressions over picked layers (#205).
//!
//! Stored as `derived/derived.json` beside the layer vocabulary, following
//! [`crate::project::layers`] exactly: a schema header, an `extra` passthrough
//! so a future field survives a rewrite, a `validate`, and `read`/`write`
//! through the [`DocumentStore`] with an [`Expectation`].
//!
//! # Kind is inferred, never chosen
//!
//! An expression that yields a **position** is a derived layer (a line); one
//! that yields anything else is a derived attribute (a per-position number).
//! The author does not declare which, because the two are not alternatives --
//! `median(bed)` *is* a depth, and calling it an attribute would be a
//! category error, not a choice.
//!
//! # Dependencies
//!
//! A derived item may reference another. The graph is built when the set is
//! saved; a cycle is rejected with the loop named. References are **not**
//! rewritten after a layer merge (#195), because that silently changes what an
//! expression means; the merge dialog lists affected expressions instead.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the write half of the project API is reached through the \
                  server's HTTP routes; a CLI-only build still needs the \
                  types to read and evaluate a project"
    )
)]
#![allow(
    dead_code,
    reason = "the derived HTTP routes (P6) and the regression test (P7) are the \
              consumers; both land after this phase"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::interp::derive::{
    self, DeriveError, EvaluatedItem, Kind, ReducedPicks, Unit, UserArray,
};
use crate::interp::level2::RadargramGeometry;
use crate::project::layers::LayerSet;
use crate::project::store::{DocumentStore, Expectation, StoreError, Version};

pub const FILE: &str = "derived.json";
pub const SCHEMA: &str = "ridal-derived";
pub const SCHEMA_VERSION: &str = "1";

/// Who may see and edit a derived item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Visible to everyone with viewing rights.
    #[default]
    Project,
    /// Visible only to its owner.
    Private { user: String },
}

/// A range fill between a derived layer and another layer (#209).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillTo {
    /// The layer id to fill toward.
    pub target: String,
    /// Fill colour; falls back to the item's own colour when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
}

fn default_opacity() -> f64 {
    0.25
}

/// One named expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedItem {
    /// Stable identifier; a valid Rhai name (see
    /// [`crate::identity::sanitize_to_identifier`]) and immutable.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The expression, in Rhai.
    pub expression: String,
    /// The unit the expression is written in.
    #[serde(default = "default_unit")]
    pub unit: Unit,
    /// Hex colour, same validation as [`crate::project::layers::Layer::color`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Whether the viewer shows it by default. Off, because a new expression
    /// is more likely to be wrong than the layers it is built from.
    #[serde(default, skip_serializing_if = "is_false")]
    pub show: bool,
    /// Whether the item appears in the viewer's layer panel at all.
    ///
    /// On by default. An intermediate layer -- one that exists only as an
    /// input to another derived item -- can be unlisted so it does not clutter
    /// the panel; it stays usable in expressions and stays on the /layers
    /// page. Distinct from `show`, which is only the initial drawn state of an
    /// item that *is* listed.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub listed: bool,
    /// Optional range fill (#209).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_to: Option<FillTo>,
    #[serde(default)]
    pub scope: Scope,
    /// Whose picks feed this item, for viewers who cannot already see
    /// everyone's picks (below the operator role).
    #[serde(default)]
    pub audience: Audience,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Whose picks an item is evaluated over, for a viewer who cannot already see
/// everyone's picks.
///
/// This is the axis that decides whether a result is a *personal* readout or a
/// *cross-user* one, and it is deliberately separate from [`Scope`], which
/// decides who can see the item exists at all. The distinction matters because
/// a consensus computed over other people's picks is the one thing a picker
/// must not see mid-experiment: seeing it would tell them what everyone else
/// concluded, which is exactly the bias the study design exists to avoid.
///
/// Note that `OwnPicks` is not a restriction on the *item*, it is a promise
/// about what a given viewer gets: the same project-wide definition yields the
/// full consensus for an operator and a personal result for a picker. That is
/// what lets a consensus be defined and monitored while picking is still open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// Every viewer below operator gets the item over **their own** picks.
    ///
    /// The default, and always safe: it can only show someone a function of
    /// data they already have.
    #[default]
    OwnPicks,
    /// An admin has released the cross-user result to everyone who can see
    /// the item.
    ///
    /// Setting this is the deliberate act of publishing other people's work
    /// in aggregate, which is why it needs the admin role rather than the
    /// operator role that authoring an item needs.
    Released,
}

fn default_unit() -> Unit {
    Unit::Meters
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

fn default_true() -> bool {
    true
}

impl DerivedItem {
    /// Whether `user` may see this item.
    pub fn visible_to(&self, user: &str) -> bool {
        match &self.scope {
            Scope::Project => true,
            Scope::Private { user: owner } => owner == user,
        }
    }

    /// Infer the expression's kind against the layer vocabulary.
    ///
    /// A reference to another derived item is not resolvable from one item
    /// alone; use [`DerivedSet::inferred_kind`] for that.
    pub fn inferred_kind(&self, layers: &LayerSet) -> Result<Kind, DeriveError> {
        let engine = derive::build_engine();
        let ast = derive::compile(&engine, &self.expression)?;
        let layer_ids: Vec<String> = layers.layers.iter().map(|l| l.id.clone()).collect();
        derive::infer_kind(
            &engine,
            &ast,
            &self.expression,
            &layer_ids,
            &BTreeMap::new(),
        )
    }
}

/// The whole set of derived items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedSet {
    #[serde(default = "default_schema")]
    pub schema: String,
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    #[serde(default)]
    pub items: Vec<DerivedItem>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_schema() -> String {
    SCHEMA.to_string()
}

fn default_schema_version() -> String {
    SCHEMA_VERSION.to_string()
}

impl Default for DerivedSet {
    fn default() -> Self {
        DerivedSet {
            schema: default_schema(),
            schema_version: default_schema_version(),
            items: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }
}

impl DerivedSet {
    pub fn get(&self, id: &str) -> Option<&DerivedItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Items `user` may see, in stored order.
    pub fn visible_to(&self, user: &str) -> Vec<&DerivedItem> {
        self.items
            .iter()
            .filter(|item| item.visible_to(user))
            .collect()
    }

    /// Ids of the other items this item's expression references.
    ///
    /// Extracted as whole identifiers rather than by substring, so a layer
    /// called `bed` does not make `bedrock` depend on it.
    pub fn dependencies(&self, id: &str) -> Vec<String> {
        let Some(item) = self.get(id) else {
            return Vec::new();
        };
        let mut deps = Vec::new();
        for token in identifiers(&item.expression) {
            if token != id && self.get(&token).is_some() && !deps.contains(&token) {
                deps.push(token);
            }
        }
        deps
    }

    /// A cycle in the dependency graph, as the loop's path, if one exists.
    ///
    /// The path is closed -- `a → b → a` -- so the error reads as the loop
    /// rather than as a list of nodes.
    pub fn find_cycle(&self) -> Option<Vec<String>> {
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            Unvisited,
            InProgress,
            Done,
        }
        let mut marks: BTreeMap<String, Mark> = self
            .items
            .iter()
            .map(|i| (i.id.clone(), Mark::Unvisited))
            .collect();
        let mut stack: Vec<String> = Vec::new();

        // Iterative DFS would be more code than this graph deserves; derived
        // sets are tens of items at most and the recursion is bounded by the
        // item count.
        fn visit(
            set: &DerivedSet,
            id: &str,
            marks: &mut BTreeMap<String, Mark>,
            stack: &mut Vec<String>,
        ) -> Option<Vec<String>> {
            marks.insert(id.to_string(), Mark::InProgress);
            stack.push(id.to_string());
            for dep in set.dependencies(id) {
                match marks.get(&dep) {
                    Some(Mark::InProgress) => {
                        let start = stack.iter().position(|n| n == &dep).unwrap_or(0);
                        let mut path: Vec<String> = stack[start..].to_vec();
                        path.push(dep);
                        return Some(path);
                    }
                    Some(Mark::Unvisited) => {
                        if let Some(path) = visit(set, &dep, marks, stack) {
                            return Some(path);
                        }
                    }
                    _ => {}
                }
            }
            stack.pop();
            marks.insert(id.to_string(), Mark::Done);
            None
        }

        for item in &self.items {
            if marks.get(&item.id) == Some(&Mark::Unvisited) {
                if let Some(path) = visit(self, &item.id, &mut marks, &mut stack) {
                    return Some(path);
                }
            }
        }
        None
    }

    /// Item ids in an order where every dependency comes first.
    pub fn evaluation_order(&self) -> Result<Vec<String>, DerivedError> {
        if let Some(path) = self.find_cycle() {
            return Err(DerivedError::Cycle { path });
        }
        let mut ordered: Vec<String> = Vec::new();
        let mut visited: BTreeMap<String, bool> = BTreeMap::new();

        fn visit(
            set: &DerivedSet,
            id: &str,
            ordered: &mut Vec<String>,
            visited: &mut BTreeMap<String, bool>,
        ) {
            if visited.get(id).copied().unwrap_or(false) {
                return;
            }
            for dep in set.dependencies(id) {
                visit(set, &dep, ordered, visited);
            }
            visited.insert(id.to_string(), true);
            ordered.push(id.to_string());
        }

        for item in &self.items {
            visit(self, &item.id, &mut ordered, &mut visited);
        }
        Ok(ordered)
    }

    /// Infer an item's kind, resolving references to other derived items.
    pub fn inferred_kind(&self, id: &str, layers: &LayerSet) -> Result<Kind, DerivedError> {
        let order = self.evaluation_order()?;
        let engine = derive::build_engine();
        let layer_ids: Vec<String> = layers.layers.iter().map(|l| l.id.clone()).collect();
        let mut kinds: BTreeMap<String, Kind> = BTreeMap::new();
        for item_id in order {
            let Some(item) = self.get(&item_id) else {
                continue;
            };
            let ast = derive::compile(&engine, &item.expression)?;
            let kind = derive::infer_kind(&engine, &ast, &item.expression, &layer_ids, &kinds)?;
            kinds.insert(item_id.clone(), kind);
        }
        kinds.get(id).copied().ok_or_else(|| {
            DerivedError::Derive(DeriveError::MissingItem {
                name: id.to_string(),
            })
        })
    }

    /// Reject a set that cannot be used unambiguously.
    pub fn validate(&self) -> Result<(), DerivedError> {
        let mut seen: Vec<&str> = Vec::new();
        for item in &self.items {
            if !crate::identity::is_valid_identifier(&item.id) {
                return Err(DerivedError::InvalidId {
                    id: item.id.clone(),
                    reason: "a derived item id must match ^[a-z][a-z0-9_]*$ so it can \
                             be named in an expression"
                        .to_string(),
                });
            }
            if seen.contains(&item.id.as_str()) {
                return Err(DerivedError::DuplicateId(item.id.clone()));
            }
            seen.push(&item.id);
            if let Some(color) = &item.color {
                if !is_hex_color(color) {
                    return Err(DerivedError::InvalidColor {
                        id: item.id.clone(),
                        color: color.clone(),
                    });
                }
            }
            if let Some(fill) = &item.fill_to {
                if fill.target.is_empty() {
                    return Err(DerivedError::Malformed {
                        message: format!(
                            "derived item '{}' has a range fill with no target layer",
                            item.id
                        ),
                    });
                }
            }
        }
        if let Some(path) = self.find_cycle() {
            return Err(DerivedError::Cycle { path });
        }
        Ok(())
    }

    /// Evaluate every item over one radargram's reduced picks.
    ///
    /// The whole dependency chain is evaluated together on one grid, so no
    /// alignment checks are needed and a referenced item is converted into the
    /// referencing expression's unit.
    pub fn evaluate(
        &self,
        reduced: &ReducedPicks,
        geometry: &RadargramGeometry,
    ) -> Result<BTreeMap<String, EvaluatedItem>, DerivedError> {
        let order = self.evaluation_order()?;
        let engine = derive::build_engine();
        let layer_ids: Vec<String> = reduced.layers.keys().cloned().collect();
        let mut results: BTreeMap<String, EvaluatedItem> = BTreeMap::new();

        for id in order {
            let Some(item) = self.get(&id) else {
                continue;
            };
            let ast = derive::compile(&engine, &item.expression)?;
            let item_kinds: BTreeMap<String, Kind> =
                results.iter().map(|(k, v)| (k.clone(), v.kind)).collect();
            let kind =
                derive::infer_kind(&engine, &ast, &item.expression, &layer_ids, &item_kinds)?;
            // A position is a depth, and a depth is not dimensionless. Caught
            // here rather than in `validate`, which has no layer vocabulary
            // and so cannot know an expression's kind.
            if !item.unit.allows(kind) {
                return Err(DerivedError::UnitMismatch {
                    id: item.id.clone(),
                    kind,
                    unit: item.unit,
                });
            }

            let mut values = Vec::with_capacity(reduced.n_positions());
            for position in 0..reduced.n_positions() {
                let mut bound: BTreeMap<String, UserArray> = BTreeMap::new();
                for (layer_id, per_position) in &reduced.layers {
                    let array = per_position[position]
                        .iter()
                        .map(|sample| derive::sample_to_unit(*sample, item.unit, geometry))
                        .collect();
                    bound.insert(layer_id.clone(), UserArray(array));
                }
                let mut deps: BTreeMap<String, f64> = BTreeMap::new();
                for (dep_id, dep) in &results {
                    let value = if dep.kind == Kind::Position {
                        derive::convert_position(
                            dep.values[position],
                            dep.unit,
                            item.unit,
                            geometry,
                        )
                    } else {
                        dep.values[position]
                    };
                    deps.insert(dep_id.clone(), value);
                }
                let value = derive::evaluate_at(
                    &engine,
                    &ast,
                    &item.expression,
                    &bound,
                    &deps,
                    &layer_ids,
                    &item_kinds,
                )?;
                values.push(value);
            }
            results.insert(
                id,
                EvaluatedItem {
                    kind,
                    unit: item.unit,
                    values,
                },
            );
        }
        Ok(results)
    }
}

/// Extract identifier-shaped tokens from an expression.
fn identifiers(expression: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in expression.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_hex_color(value: &str) -> bool {
    let Some(digits) = value.strip_prefix('#') else {
        return false;
    };
    matches!(digits.len(), 3 | 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit())
}

#[derive(Debug)]
pub enum DerivedError {
    /// The declared unit cannot describe the expression's inferred kind.
    UnitMismatch {
        id: String,
        kind: Kind,
        unit: Unit,
    },
    Store(StoreError),
    Malformed {
        message: String,
    },
    DuplicateId(String),
    InvalidId {
        id: String,
        reason: String,
    },
    InvalidColor {
        id: String,
        color: String,
    },
    Cycle {
        path: Vec<String>,
    },
    Derive(DeriveError),
}

impl std::fmt::Display for DerivedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DerivedError::UnitMismatch { id, kind, unit } => write!(
                f,
                "the derived item '{id}' is declared in '{unit}' but its expression \
                 is a {kind}; a position is a depth and cannot be dimensionless. \
                 Use meters, nanoseconds or samples."
            ),
            DerivedError::Store(e) => write!(f, "{e}"),
            DerivedError::Malformed { message } => {
                write!(f, "the derived items are not readable: {message}")
            }
            DerivedError::DuplicateId(id) => {
                write!(f, "the derived item id '{id}' is defined more than once")
            }
            DerivedError::InvalidId { id, reason } => {
                write!(f, "the derived item id '{id}' is not usable: {reason}")
            }
            DerivedError::InvalidColor { id, color } => write!(
                f,
                "derived item '{id}' has colour '{color}'; use a hex colour such as \
                 '#e6194b'"
            ),
            DerivedError::Cycle { path } => {
                write!(f, "the derived items form a cycle: {}", path.join(" → "))
            }
            DerivedError::Derive(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DerivedError {}

impl From<StoreError> for DerivedError {
    fn from(e: StoreError) -> Self {
        DerivedError::Store(e)
    }
}

impl From<DeriveError> for DerivedError {
    fn from(e: DeriveError) -> Self {
        DerivedError::Derive(e)
    }
}

fn path() -> PathBuf {
    PathBuf::from(crate::project::DERIVED_DIR).join(FILE)
}

/// Read the derived set. A project that has never defined one reads as empty.
pub fn read(store: &DocumentStore) -> Result<(DerivedSet, Option<Version>), DerivedError> {
    let Some(stored) = store.read(&path())? else {
        return Ok((DerivedSet::default(), None));
    };
    let set: DerivedSet =
        serde_json::from_str(&stored.text).map_err(|e| DerivedError::Malformed {
            message: e.to_string(),
        })?;
    set.validate()?;
    Ok((set, Some(stored.version)))
}

/// Replace the derived set.
pub fn write(
    store: &DocumentStore,
    set: &DerivedSet,
    expected: &Expectation,
) -> Result<Version, DerivedError> {
    set.validate()?;
    let text = serde_json::to_string_pretty(set).map_err(|e| DerivedError::Malformed {
        message: e.to_string(),
    })?;
    Ok(store.write(&path(), &text, expected)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::layers::Layer;
    use crate::project::Project;

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        (dir, project)
    }

    fn layer(id: &str) -> Layer {
        Layer {
            id: id.to_string(),
            name: id.to_string(),
            color: None,
            description: None,
            allow_overhangs: false,
            reducer: None,
            warn_on_duplicates: true,
            groups: Vec::new(),
            extra: Default::default(),
        }
    }

    fn layers() -> LayerSet {
        LayerSet {
            layers: vec![
                layer("bed"),
                layer("bed_no_temperate"),
                layer("temperate_ice"),
                layer("bed_not_visible"),
            ],
            ..LayerSet::default()
        }
    }

    fn item(id: &str, expression: &str) -> DerivedItem {
        DerivedItem {
            id: id.to_string(),
            name: id.to_string(),
            expression: expression.to_string(),
            unit: Unit::Meters,
            color: None,
            show: false,
            listed: true,
            fill_to: None,
            scope: Scope::Project,
            audience: Audience::OwnPicks,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_position_may_not_be_declared_dimensionless() {
        use crate::interp::derive::Unit;
        // `count` is a scalar, so dimensionless is exactly right for it.
        let mut counting = item("n", "count(bed)");
        counting.unit = Unit::Dimensionless;
        assert!(Unit::Dimensionless.allows(Kind::Scalar));

        // `median(bed)` is a depth, and a depth has a unit.
        let mut positional = item("m", "median(bed)");
        positional.unit = Unit::Dimensionless;
        assert!(!Unit::Dimensionless.allows(Kind::Position));

        // Every other unit takes any kind: an attribute is simply never
        // converted, so declaring a count in metres is odd but not wrong.
        for unit in [Unit::Meters, Unit::Nanoseconds, Unit::Samples] {
            for kind in [Kind::Position, Kind::Length, Kind::Scalar] {
                assert!(unit.allows(kind), "{unit} should accept {kind}");
            }
        }
    }

    fn set(items: Vec<DerivedItem>) -> DerivedSet {
        DerivedSet {
            items,
            ..DerivedSet::default()
        }
    }

    #[test]
    fn a_cycle_is_rejected_with_the_loop_named() {
        let set = set(vec![item("a", "b"), item("b", "a")]);
        let path = set.find_cycle().unwrap();
        assert_eq!(path, vec!["a", "b", "a"]);
        let error = set.validate().unwrap_err();
        assert!(error.to_string().contains("a → b → a"), "{error}");
    }

    #[test]
    fn a_three_cycle_is_rejected() {
        let set = set(vec![item("a", "b"), item("b", "c"), item("c", "a")]);
        let path = set.find_cycle().unwrap();
        assert_eq!(path, vec!["a", "b", "c", "a"]);
    }

    #[test]
    fn a_diamond_evaluates_in_topological_order() {
        let set = set(vec![
            item("d", "median(b + c)"),
            item("b", "median(a)"),
            item("c", "median(a)"),
            item("a", "median(bed)"),
        ]);
        assert!(set.find_cycle().is_none());
        let order = set.evaluation_order().unwrap();
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("a") < pos("c"));
        assert!(pos("b") < pos("d"));
        assert!(pos("c") < pos("d"));
    }

    #[test]
    fn kind_inference_pins_the_expression_rules() {
        let layers = layers();
        assert_eq!(
            item("x", "median(bed)").inferred_kind(&layers).unwrap(),
            Kind::Position
        );
        assert_eq!(
            item("x", "median(bed) - median(temperate_ice)")
                .inferred_kind(&layers)
                .unwrap(),
            Kind::Length
        );
        assert_eq!(
            item("x", "count(bed)").inferred_kind(&layers).unwrap(),
            Kind::Scalar
        );
    }

    #[test]
    fn a_missing_layer_is_reported_by_name() {
        let layers = layers();
        let error = item("x", "median(no_such)")
            .inferred_kind(&layers)
            .unwrap_err();
        assert_eq!(error.to_string(), "references missing layer 'no_such'");
    }

    #[test]
    fn a_private_item_is_invisible_to_another_user() {
        let mut private = item("mine", "median(bed)");
        private.scope = Scope::Private {
            user: "erik".to_string(),
        };
        let set = set(vec![item("shared", "median(bed)"), private]);
        assert!(set.visible_to("erik").iter().any(|i| i.id == "mine"));
        assert!(set.visible_to("someone").iter().all(|i| i.id != "mine"));
        assert!(set.visible_to("someone").iter().any(|i| i.id == "shared"));
    }

    #[test]
    fn ids_must_be_expression_safe_and_unique() {
        let mut set = set(vec![item("not safe", "median(bed)")]);
        assert!(matches!(
            set.validate(),
            Err(DerivedError::InvalidId { .. })
        ));
        set.items[0].id = "safe_id".to_string();
        assert!(set.validate().is_ok());
        set.items.push(item("safe_id", "median(bed)"));
        assert!(matches!(set.validate(), Err(DerivedError::DuplicateId(_))));
    }

    #[test]
    fn derived_set_round_trips_through_the_store() {
        let (_dir, project) = project();
        let mut set = set(vec![item("thickness", "percentile_lower(bed, 49.0)")]);
        set.items[0].fill_to = Some(FillTo {
            target: "bed".to_string(),
            color: Some("#e6194b".to_string()),
            opacity: 0.5,
        });
        let version = write(project.documents(), &set, &Expectation::Absent).unwrap();
        let (read_back, read_version) = read(project.documents()).unwrap();
        assert_eq!(read_back, set);
        assert_eq!(read_version, Some(version));
    }

    #[test]
    fn the_shipped_example_parses_validates_and_types() {
        // The example is user-facing documentation; a broken one is worse
        // than none, so it is parsed and type-checked here.
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/examples/dronbreen-20250327-DAT_0066_A1_1");
        let layers: LayerSet =
            serde_json::from_str(&std::fs::read_to_string(root.join("layers.json")).unwrap())
                .unwrap();
        layers.validate().unwrap();
        let derived: DerivedSet =
            serde_json::from_str(&std::fs::read_to_string(root.join("derived.json")).unwrap())
                .unwrap();
        derived.validate().unwrap();
        assert_eq!(
            derived.inferred_kind("thickness", &layers).unwrap(),
            Kind::Position
        );
        assert_eq!(
            derived
                .inferred_kind("thickness_user_count", &layers)
                .unwrap(),
            Kind::Scalar
        );
    }

    #[test]
    fn a_derived_layer_evaluates_against_reduced_picks() {
        use crate::interp::derive::GridPosition;
        use gprinterp::Document;

        let document: Document = serde_json::from_value(serde_json::json!({
            "key": "line",
            "features": [
                {"type": "Feature",
                 "geometry": {"type": "LineString", "coordinates": [[0.0, 10.0], [2.0, 10.0]]},
                 "properties": {"label": "bed"}},
                {"type": "Feature",
                 "geometry": {"type": "LineString", "coordinates": [[0.0, 20.0], [2.0, 20.0]]},
                 "properties": {"label": "temperate_ice"}}
            ]
        }))
        .unwrap();

        let geometry = RadargramGeometry {
            radargram_id: "line".into(),
            revision_id: "rev".into(),
            distance: (0..3).map(|i| i as f64).collect(),
            twtt: (0..50).map(|i| i as f64 * 0.4).collect(),
            depth: (0..50).map(|i| i as f64 * 0.04).collect(),
            easting: (0..3).map(|i| 400_000.0 + i as f64).collect(),
            northing: (0..3).map(|_| 8_700_000.0).collect(),
            longitude: (0..3).map(|_| 15.0).collect(),
            latitude: (0..3).map(|_| 78.0).collect(),
            antenna_separation_effective_m: Some(0.0),
            twtt_anchor: None,
            crs: "EPSG:32633".into(),
        };
        let grid: Vec<GridPosition> = (0..3)
            .map(|i| GridPosition {
                trace: i as f64,
                distance_m: i as f64,
            })
            .collect();
        let reduced = derive::reduce_picks(
            &[("a".to_string(), document)],
            &layers(),
            &geometry,
            &grid,
            false,
        );

        let set = set(vec![item(
            "thickness",
            "percentile_lower(bed, 49.0) - percentile_lower(temperate_ice, 49.0)",
        )]);
        let results = set.evaluate(&reduced, &geometry).unwrap();
        let thickness = &results["thickness"];
        assert_eq!(thickness.kind, Kind::Length);
        // bed at sample 10 -> 0.4 m, cts at 20 -> 0.8 m; thickness is
        // 0.4 - 0.8 = -0.4 m (bed above cts), exactly as written.
        for value in &thickness.values {
            assert!((value + 0.4).abs() < 1e-9, "{value}");
        }
    }
}
