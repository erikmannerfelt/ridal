//! The project's layer vocabulary: `layers/layers.json`.
//!
//! A layer is what `properties.label` on a gprinterp feature refers to --
//! "bed", "internal reflector", "englacial water". The vocabulary is
//! project-scoped and user-editable, because a fixed server-side list will
//! not survive contact with real interpretation work.
//!
//! # Why a layer has both an id and a name
//!
//! `id` is a slug and is what gets written into `properties.label`, so it is
//! also what appears in the `layer` column of a level 2 export. `name` is
//! free text for display. Separating them means renaming a layer in the GUI
//! -- "bed" to "Bed (picked 2026)" -- is a cosmetic change that does not
//! rewrite, or orphan, a single existing pick.
//!
//! # Deleting
//!
//! Removing a layer from the vocabulary does not touch the interpretations
//! that used it; their features keep their label. Such a label is reported
//! as unknown rather than being dropped, since the picks are real data and
//! the vocabulary is only a description of it.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the write half of the project API is reached through the \
                  server's HTTP routes; a CLI-only build still needs the \
                  types to read and inspect a project"
    )
)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::project::store::{DocumentStore, Expectation, StoreError, Version};

pub const FILE: &str = "layers.json";
pub const SCHEMA: &str = "ridal-layers";
pub const SCHEMA_VERSION: &str = "1";

/// One layer definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    /// Stable slug. Written into gprinterp `properties.label`.
    pub id: String,
    /// Display name. Cosmetic; safe to change at any time.
    pub name: String,
    /// CSS colour used to draw the layer in the viewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Permit lines in this layer to double back in trace.
    ///
    /// Off by default, because a reflector has one depth per position and a
    /// line that overhangs is usually a mis-click. Some layers legitimately
    /// do overhang -- a crevasse wall, a water-body outline -- which is why
    /// it is a property of the layer rather than of Ridal.
    ///
    /// Turning it on has a cost: such a layer cannot be exported at even
    /// spacing along the ground track, because that works by asking the line
    /// for its depth at a position, which is the question an overhang has
    /// two answers to. It exports as its own picked vertices instead. See
    /// [`crate::interp::checks`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_overhangs: bool,
    /// How to collapse several picked values for one user at one position to
    /// one. `None` inherits [`LayerSet::default_reducer`].
    ///
    /// Reducers apply at **evaluation time only**: stored picks are never
    /// rewritten, so changing a reducer is non-destructive and reversible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reducer: Option<Reducer>,
    /// Whether more than one value for this layer at one trace is worth
    /// pointing out.
    ///
    /// Off for a layer that is legitimately multi-valued -- folded englacial
    /// reflectors, say -- where a duplicate is the point rather than a
    /// mistake.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub warn_on_duplicates: bool,
    /// Ids of the exclusivity groups this layer belongs to (#208).
    ///
    /// Membership is defined by [`ExclusivityGroup::members`]; this list is
    /// the same relation seen from the layer, and the two are unioned when
    /// conflicts are computed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Anything a future version adds, preserved on rewrite.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

/// How to collapse several picked values for one user at one position into
/// one, at evaluation time (#207).
///
/// `Shallowest` is the project default: stray picks on multiples and ringing
/// lie *below* the true reflector, so the minimum depth is the defensible
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reducer {
    Shallowest,
    Deepest,
    Median,
    Mean,
}

/// A set of layers that cannot all hold a value for one user at one position
/// (#208).
///
/// Membership is deliberately *not* transitive: in the Svalbard setup `bed`
/// and `bed_no_temperate` are exclusive, and `bed_no_temperate` and
/// `temperate_ice` are exclusive, but `bed` and `temperate_ice` coexist. Two
/// layers conflict iff they share at least one group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExclusivityGroup {
    /// Stable id. A layer may list it in [`Layer::groups`].
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// The layers that are mutually exclusive within this group.
    #[serde(default)]
    pub members: Vec<String>,
    /// Anything a future version adds, preserved on rewrite.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The whole vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSet {
    #[serde(default = "default_schema")]
    pub schema: String,
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// The reducer a layer inherits when it does not set its own (#207).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reducer: Option<Reducer>,
    /// Mutually exclusive layer groups (#208).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<ExclusivityGroup>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_schema() -> String {
    SCHEMA.to_string()
}

fn default_schema_version() -> String {
    SCHEMA_VERSION.to_string()
}

impl Default for LayerSet {
    fn default() -> Self {
        LayerSet {
            schema: default_schema(),
            schema_version: default_schema_version(),
            layers: Vec::new(),
            default_reducer: None,
            groups: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }
}

/// The subset of a [`LayerSet`] the GUI edits and carries back.
///
/// Deserialization-only, and merged onto the stored document rather than
/// replacing it. The GUI never has the envelope -- `default_reducer`, schema
/// fields, anything a future version adds -- and making it reconstruct those
/// would let an edit drop them silently. Both collections are optional so a
/// caller can send only the one it changed.
#[derive(Debug, Deserialize)]
pub struct PartialLayerSet {
    #[serde(default)]
    pub layers: Option<Vec<Layer>>,
    #[serde(default)]
    pub groups: Option<Vec<ExclusivityGroup>>,
}

impl LayerSet {
    pub fn get(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// Whether `label` names a layer that permits overhangs.
    ///
    /// An undefined or missing label answers `false`: opting out of the
    /// guardrail is a deliberate act recorded on a layer, and a label with
    /// no definition has not made it.
    pub fn allows_overhangs(&self, label: Option<&str>) -> bool {
        label
            .and_then(|id| self.get(id))
            .map(|layer| layer.allow_overhangs)
            .unwrap_or(false)
    }

    /// Whether a duplicate value for `label` should be reported (#207).
    ///
    /// An undefined or missing label answers `true`: the guardrail is the
    /// default, and an undefined layer has not opted out of it.
    pub fn warns_on_duplicates(&self, label: Option<&str>) -> bool {
        label
            .and_then(|id| self.get(id))
            .map(|layer| layer.warn_on_duplicates)
            .unwrap_or(true)
    }

    /// The reducer for a layer, resolving layer -> set default -> shallowest.
    pub fn reducer_for(&self, id: &str) -> Reducer {
        self.get(id)
            .and_then(|layer| layer.reducer)
            .or(self.default_reducer)
            .unwrap_or(Reducer::Shallowest)
    }

    /// The exclusivity groups `layer_id` belongs to, from both the layer's own
    /// [`Layer::groups`] list and every group that names it as a member.
    pub fn groups_of(&self, layer_id: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(layer) = self.get(layer_id) {
            out.extend(layer.groups.iter().cloned());
        }
        for group in &self.groups {
            if group.members.iter().any(|member| member == layer_id) {
                out.push(group.id.clone());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Whether two layers are mutually exclusive, i.e. share a group (#208).
    ///
    /// Not transitive by design: sharing a group is the whole test.
    pub fn conflicts(&self, a: &str, b: &str) -> bool {
        let a_groups = self.groups_of(a);
        let b_groups = self.groups_of(b);
        a_groups.iter().any(|group| b_groups.contains(group))
    }

    /// Every pair of defined layers that conflict, in vocabulary order.
    pub fn conflicting_pairs(&self) -> Vec<(String, String)> {
        let ids: Vec<&str> = self.layers.iter().map(|layer| layer.id.as_str()).collect();
        let mut pairs = Vec::new();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                if self.conflicts(ids[i], ids[j]) {
                    pairs.push((ids[i].to_string(), ids[j].to_string()));
                }
            }
        }
        pairs
    }

    /// Groups that name a layer the vocabulary does not define.
    ///
    /// Reported rather than dropped: a group is a statement about layers that
    /// may be added later, and silently removing an unknown member would erase
    /// an edit. This never fails validation for the same reason.
    ///
    /// Surfaced by `ridal project info`; the GUI reports the same fact from
    /// the document it already renders, so it does not need this over HTTP.
    #[cfg_attr(not(feature = "cli"), allow(dead_code))]
    pub fn group_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for group in &self.groups {
            for member in &group.members {
                if self.get(member).is_none() {
                    warnings.push(format!(
                        "exclusivity group '{}' names layer '{member}', which is not defined",
                        group.id
                    ));
                }
            }
        }
        warnings
    }

    /// Ids in this vocabulary that a derived-layer expression cannot name
    /// (#206).
    ///
    /// These are legacy ids -- a layer whose stored id contains `-` predates
    /// expression-safe ids. They must keep working, because picks reference
    /// them, so this is reported as a warning rather than treated as an
    /// error. New ids cannot be created in this shape; see [`write`].
    #[allow(
        dead_code,
        reason = "reported as a read-time warning once the server's layer \
                  routes surface it (P6)"
    )]
    pub fn expression_unsafe_ids(&self) -> Vec<String> {
        self.layers
            .iter()
            .filter(|layer| !crate::identity::is_valid_identifier(&layer.id))
            .map(|layer| layer.id.clone())
            .collect()
    }

    /// Ids referenced by `labels` that this vocabulary does not define.
    pub fn unknown_ids<'a>(&self, labels: impl Iterator<Item = &'a str>) -> Vec<String> {
        let mut unknown: Vec<String> = labels
            .filter(|label| self.get(label).is_none())
            .map(str::to_string)
            .collect();
        unknown.sort();
        unknown.dedup();
        unknown
    }

    /// Reject a set that cannot be used unambiguously.
    ///
    /// Duplicate ids are fatal rather than deduplicated: two definitions of
    /// "bed" means the colour a pick draws in depends on iteration order,
    /// and silently keeping one discards a real edit.
    pub fn validate(&self) -> Result<(), LayerError> {
        let mut seen: Vec<&str> = Vec::new();
        for layer in &self.layers {
            if layer.id.is_empty() {
                return Err(LayerError::InvalidId {
                    id: layer.id.clone(),
                    reason: "a layer id must not be empty".to_string(),
                });
            }
            // Same charset as the identity slugs: layer ids end up in
            // exported CSV columns, GeoJSON properties and URLs.
            if let Some(bad) = layer
                .id
                .chars()
                .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
            {
                return Err(LayerError::InvalidId {
                    id: layer.id.clone(),
                    reason: format!(
                        "'{bad}' is not allowed; use lowercase ASCII letters, digits, \
                         '-' and '_'. The display name is free text -- put the \
                         punctuation there instead."
                    ),
                });
            }
            if let Some(color) = &layer.color {
                if !is_hex_color(color) {
                    return Err(LayerError::InvalidColor {
                        id: layer.id.clone(),
                        color: color.clone(),
                    });
                }
            }
            if seen.contains(&layer.id.as_str()) {
                return Err(LayerError::DuplicateId(layer.id.clone()));
            }
            seen.push(&layer.id);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum LayerError {
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
    /// A *new* layer id that cannot be named from a derived-layer expression
    /// (#206). Legacy ids that already exist are reported via
    /// [`LayerSet::expression_unsafe_ids`] instead, so that picks which
    /// reference them keep working.
    IdNotExpressionSafe {
        id: String,
        suggestion: String,
    },
    /// An existing layer's id was renamed (#206). Ids are immutable because
    /// picks and derived expressions reference them.
    IdChanged {
        name: String,
        from: String,
        to: String,
    },
}

/// Is this `#rgb`, `#rrggbb` or `#rrggbbaa`?
///
/// Colours are written into `style.background` in the browser, where CSS
/// accepts far more than a colour -- `url(http://…)` would make every
/// viewer fetch whatever the author chose. The colour input in the UI
/// constrains the widget, not the API or a hand-edited `layers.json`, so
/// the restriction has to live at the boundary that stores it.
fn is_hex_color(value: &str) -> bool {
    let Some(digits) = value.strip_prefix('#') else {
        return false;
    };
    matches!(digits.len(), 3 | 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit())
}

impl std::fmt::Display for LayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayerError::Store(e) => write!(f, "{e}"),
            LayerError::Malformed { message } => {
                write!(f, "the layer definitions are not readable: {message}")
            }
            LayerError::InvalidColor { id, color } => write!(
                f,
                "layer '{id}' has colour '{color}'; use a hex colour such as \
                 '#e6194b'. Anything else would be handed to CSS, which accepts \
                 more than colours."
            ),
            LayerError::DuplicateId(id) => {
                write!(f, "the layer id '{id}' is defined more than once")
            }
            LayerError::InvalidId { id, reason } => {
                write!(f, "the layer id '{id}' is not usable: {reason}")
            }
            LayerError::IdNotExpressionSafe { id, suggestion } => write!(
                f,
                "the layer id '{id}' cannot be used in an expression; use '{suggestion}'?"
            ),
            LayerError::IdChanged { name, from, to } => write!(
                f,
                "the layer '{name}' cannot change its id from '{from}' to '{to}': ids are \
                 immutable because picks and derived expressions reference them. Rename the \
                 display name instead, or create a new layer."
            ),
        }
    }
}

impl std::error::Error for LayerError {}

impl From<StoreError> for LayerError {
    fn from(e: StoreError) -> Self {
        LayerError::Store(e)
    }
}

fn path() -> PathBuf {
    PathBuf::from(crate::project::LAYERS_DIR).join(FILE)
}

/// Read the vocabulary. A project that has never defined one reads as empty
/// with no version, which is a normal state rather than an error.
pub fn read(store: &DocumentStore) -> Result<(LayerSet, Option<Version>), LayerError> {
    let Some(stored) = store.read(&path())? else {
        return Ok((LayerSet::default(), None));
    };
    let set: LayerSet = serde_json::from_str(&stored.text).map_err(|e| LayerError::Malformed {
        message: e.to_string(),
    })?;
    // Validated on the way out as well as on the way in. `write` is not
    // the only way a document gets here: `layers.json` is meant to be
    // hand-editable, and an invalid colour edited in by hand would
    // otherwise reach `style.background` in the browser, where CSS accepts
    // considerably more than a colour.
    set.validate()?;
    Ok((set, Some(stored.version)))
}

/// Replace the vocabulary.
///
/// Two #206 rules are enforced here rather than in [`LayerSet::validate`],
/// because they need to know what came before:
///
/// - A **new** id must be expression-safe. A legacy id that already exists in
///   the stored document is allowed to stay: picks reference it, and refusing
///   the read would strand them.
/// - An existing layer may not change its **id**. Renaming the display name is
///   the supported way to relabel a layer.
pub fn write(
    store: &DocumentStore,
    set: &LayerSet,
    expected: &Expectation,
) -> Result<Version, LayerError> {
    set.validate()?;

    // The stored set is what "new" and "existing" are relative to. An absent
    // document means every id is new.
    let (existing, _) = read(store)?;
    let existing_ids: Vec<String> = existing.layers.iter().map(|l| l.id.clone()).collect();

    for old in &existing.layers {
        if let Some(new) = set.layers.iter().find(|l| l.name == old.name) {
            if new.id != old.id {
                return Err(LayerError::IdChanged {
                    name: old.name.clone(),
                    from: old.id.clone(),
                    to: new.id.clone(),
                });
            }
        }
    }

    let mut taken = existing_ids.clone();
    for layer in &set.layers {
        if !existing_ids.contains(&layer.id) {
            if !crate::identity::is_valid_identifier(&layer.id) {
                return Err(LayerError::IdNotExpressionSafe {
                    id: layer.id.clone(),
                    suggestion: crate::identity::sanitize_to_identifier(&layer.id, &taken),
                });
            }
            taken.push(layer.id.clone());
        }
    }

    let text = serde_json::to_string_pretty(set).map_err(|e| LayerError::Malformed {
        message: e.to_string(),
    })?;
    Ok(store.write(&path(), &text, expected)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        (dir, project)
    }

    fn layer(id: &str, name: &str) -> Layer {
        Layer {
            id: id.to_string(),
            name: name.to_string(),
            color: Some("#e6194b".to_string()),
            description: None,
            allow_overhangs: false,
            reducer: None,
            warn_on_duplicates: true,
            groups: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn a_colour_must_be_hex_because_css_accepts_more_than_colours() {
        // The value lands in `style.background` in the browser, where
        // `url(http://...)` would make every viewer fetch whatever the
        // author chose. The colour picker in the UI constrains the widget,
        // not the API or a hand-edited layers.json.
        let mut set = LayerSet::default();
        set.layers.push(Layer {
            id: "bed".to_string(),
            name: "Bed".to_string(),
            color: Some("url(http://example.invalid/x.png)".to_string()),
            description: None,
            allow_overhangs: false,
            reducer: None,
            warn_on_duplicates: true,
            groups: Vec::new(),
            extra: Default::default(),
        });
        let err = set.validate().expect_err("must refuse");
        assert!(matches!(err, LayerError::InvalidColor { .. }), "{err}");
        assert!(err.to_string().contains("hex colour"), "{err}");

        for good in ["#fff", "#e6194b", "#e6194bcc", "#ABCDEF"] {
            set.layers[0].color = Some(good.to_string());
            assert!(set.validate().is_ok(), "{good} should be accepted");
        }
        for bad in ["red", "#12", "#1234567", "#ggghhh", "rgb(1,2,3)", ""] {
            set.layers[0].color = Some(bad.to_string());
            assert!(set.validate().is_err(), "{bad:?} should be refused");
        }

        // No colour at all stays legal -- the viewer falls back to its own.
        set.layers[0].color = None;
        assert!(set.validate().is_ok());
    }

    #[test]
    fn a_project_without_layers_reads_as_empty() {
        let (_dir, project) = project();
        let (set, version) = read(project.documents()).unwrap();
        assert!(set.layers.is_empty());
        assert!(version.is_none());
    }

    #[test]
    fn layers_round_trip() {
        let (_dir, project) = project();
        let set = LayerSet {
            layers: vec![layer("bed", "Bed"), layer("internal", "Internal reflector")],
            ..LayerSet::default()
        };
        let version = write(project.documents(), &set, &Expectation::Absent).unwrap();

        let (read_back, read_version) = read(project.documents()).unwrap();
        assert_eq!(read_back.layers, set.layers);
        assert_eq!(read_version, Some(version));
        assert_eq!(read_back.schema, SCHEMA);
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let (_dir, project) = project();
        let set = LayerSet {
            layers: vec![layer("bed", "Bed"), layer("bed", "Bed again")],
            ..LayerSet::default()
        };
        let error = write(project.documents(), &set, &Expectation::Any).unwrap_err();
        assert!(matches!(error, LayerError::DuplicateId(id) if id == "bed"));
    }

    #[test]
    fn ids_are_restricted_but_names_are_free_text() {
        let (_dir, project) = project();
        let bad = LayerSet {
            layers: vec![layer("Bed Layer", "Bed")],
            ..LayerSet::default()
        };
        assert!(matches!(
            write(project.documents(), &bad, &Expectation::Any),
            Err(LayerError::InvalidId { .. })
        ));

        let good = LayerSet {
            layers: vec![layer("bed", "Bed (Drønbreen, picked 2026) — upper")],
            ..LayerSet::default()
        };
        assert!(write(project.documents(), &good, &Expectation::Any).is_ok());
    }

    #[test]
    fn renaming_a_layer_leaves_its_id_alone() {
        // The whole point of the id/name split: picks reference the id.
        let (_dir, project) = project();
        let mut set = LayerSet {
            layers: vec![layer("bed", "Bed")],
            ..LayerSet::default()
        };
        let version = write(project.documents(), &set, &Expectation::Absent).unwrap();

        set.layers[0].name = "Glacier bed".to_string();
        write(project.documents(), &set, &Expectation::Version(version)).unwrap();

        let (read_back, _) = read(project.documents()).unwrap();
        assert_eq!(read_back.layers[0].id, "bed");
        assert_eq!(read_back.layers[0].name, "Glacier bed");
    }

    #[test]
    fn new_ids_must_be_expression_safe_but_legacy_ids_keep_working() {
        let (_dir, project) = project();
        // A legacy id containing '-' is written straight to disk, as an
        // older Ridal would have. It must still read.
        let legacy = serde_json::json!({
            "schema": SCHEMA,
            "schema_version": SCHEMA_VERSION,
            "layers": [{"id": "bed-2", "name": "Bed (2)"}]
        });
        project
            .documents()
            .write(
                &path(),
                &serde_json::to_string_pretty(&legacy).unwrap(),
                &Expectation::Absent,
            )
            .unwrap();

        let (set, version) = read(project.documents()).unwrap();
        assert_eq!(set.expression_unsafe_ids(), vec!["bed-2"]);
        // Rewriting the legacy set unchanged is allowed: the id already
        // exists, so nothing new is being introduced.
        write(
            project.documents(),
            &set,
            &Expectation::Version(version.unwrap()),
        )
        .unwrap();

        // A brand-new layer with an unsafe id is refused, with a suggestion.
        let mut with_new = set.clone();
        with_new.layers.push(layer("foo-bar", "Foo"));
        let error = write(project.documents(), &with_new, &Expectation::Any).unwrap_err();
        assert!(
            matches!(&error, LayerError::IdNotExpressionSafe { id, suggestion } if id == "foo-bar" && suggestion == "foo_bar"),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "the layer id 'foo-bar' cannot be used in an expression; use 'foo_bar'?"
        );

        // The suggested id is accepted.
        let mut renamed = set.clone();
        renamed.layers.push(layer("foo_bar", "Foo"));
        assert!(write(project.documents(), &renamed, &Expectation::Any).is_ok());
    }

    #[test]
    fn a_layer_id_cannot_be_renamed() {
        let (_dir, project) = project();
        let set = LayerSet {
            layers: vec![layer("bed", "Bed")],
            ..LayerSet::default()
        };
        write(project.documents(), &set, &Expectation::Absent).unwrap();

        // Same display name, different id: a rename, and refused, because
        // picks and derived expressions reference the old id.
        let renamed = LayerSet {
            layers: vec![layer("glacier_bed", "Bed")],
            ..LayerSet::default()
        };
        let error = write(project.documents(), &renamed, &Expectation::Any).unwrap_err();
        assert!(
            matches!(&error, LayerError::IdChanged { from, to, .. } if from == "bed" && to == "glacier_bed"),
            "{error}"
        );

        // Changing the display name is the supported operation.
        let relabelled = LayerSet {
            layers: vec![layer("bed", "Glacier bed")],
            ..LayerSet::default()
        };
        assert!(write(project.documents(), &relabelled, &Expectation::Any).is_ok());
    }

    #[test]
    fn reducers_round_trip_and_absent_reads_as_none() {
        let (_dir, project) = project();
        let mut set = LayerSet {
            layers: vec![layer("bed", "Bed")],
            default_reducer: Some(Reducer::Median),
            ..LayerSet::default()
        };
        set.layers[0].reducer = Some(Reducer::Deepest);
        write(project.documents(), &set, &Expectation::Absent).unwrap();

        let (read_back, _) = read(project.documents()).unwrap();
        assert_eq!(read_back.layers[0].reducer, Some(Reducer::Deepest));
        assert_eq!(read_back.default_reducer, Some(Reducer::Median));

        // An absent reducer field reads as `None` (inherit), not a default
        // baked in at deserialization time.
        let stored = serde_json::json!({
            "schema": SCHEMA,
            "schema_version": SCHEMA_VERSION,
            "layers": [{"id": "bed", "name": "Bed"}]
        });
        project
            .documents()
            .write(
                &path(),
                &serde_json::to_string_pretty(&stored).unwrap(),
                &Expectation::Any,
            )
            .unwrap();
        let (plain, _) = read(project.documents()).unwrap();
        assert_eq!(plain.layers[0].reducer, None);
        assert_eq!(plain.default_reducer, None);
    }

    #[test]
    fn reducer_resolution_is_layer_then_set_then_shallowest() {
        let mut set = LayerSet {
            layers: vec![layer("bed", "Bed"), layer("cts", "CTS")],
            default_reducer: Some(Reducer::Mean),
            ..LayerSet::default()
        };
        // Layer override wins over the set default.
        set.layers[0].reducer = Some(Reducer::Deepest);
        assert_eq!(set.reducer_for("bed"), Reducer::Deepest);
        // No layer override: the set default, for a defined layer...
        assert_eq!(set.reducer_for("cts"), Reducer::Mean);
        // ...and for one the vocabulary does not define. The set default is
        // the project default, not a per-layer fallback.
        assert_eq!(set.reducer_for("no_such"), Reducer::Mean);

        // No layer override and no set default: shallowest.
        set.default_reducer = None;
        assert_eq!(set.reducer_for("cts"), Reducer::Shallowest);
        assert_eq!(set.reducer_for("no_such"), Reducer::Shallowest);
    }

    #[test]
    fn duplicate_warnings_default_on_and_can_be_turned_off() {
        let mut set = LayerSet {
            layers: vec![layer("fold", "Folded reflector")],
            ..LayerSet::default()
        };
        // Default is on, for a defined and for an undefined layer.
        assert!(set.layers[0].warn_on_duplicates);
        assert!(set.warns_on_duplicates(Some("fold")));
        assert!(set.warns_on_duplicates(Some("undefined")));
        assert!(set.warns_on_duplicates(None));

        set.layers[0].warn_on_duplicates = false;
        assert!(!set.warns_on_duplicates(Some("fold")));
        // The opt-out is per layer, not global.
        assert!(set.warns_on_duplicates(Some("undefined")));
    }

    fn group(id: &str, members: &[&str]) -> ExclusivityGroup {
        ExclusivityGroup {
            id: id.to_string(),
            name: id.to_string(),
            members: members.iter().map(|m| m.to_string()).collect(),
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn exclusivity_conflicts_are_shared_group_membership() {
        let mut set = LayerSet {
            layers: vec![
                layer("bed", "Bed"),
                layer("bed_no_temperate", "Cold bed"),
                layer("temperate_ice", "Temperate ice"),
            ],
            ..LayerSet::default()
        };
        set.groups = vec![
            group("g1", &["bed", "bed_no_temperate"]),
            group("g2", &["bed_no_temperate", "temperate_ice"]),
        ];

        // Sharing a group is the test, and it is deliberately not transitive.
        assert!(set.conflicts("bed", "bed_no_temperate"));
        assert!(set.conflicts("bed_no_temperate", "temperate_ice"));
        assert!(!set.conflicts("bed", "temperate_ice"));

        assert_eq!(
            set.conflicting_pairs(),
            vec![
                ("bed".to_string(), "bed_no_temperate".to_string()),
                ("bed_no_temperate".to_string(), "temperate_ice".to_string()),
            ]
        );
    }

    #[test]
    fn layer_side_group_membership_counts_too() {
        let mut set = LayerSet {
            layers: vec![layer("bed", "Bed"), layer("bed_no_temperate", "Cold bed")],
            ..LayerSet::default()
        };
        set.layers[0].groups = vec!["g1".to_string()];
        set.groups = vec![group("g1", &["bed_no_temperate"])];
        assert!(set.conflicts("bed", "bed_no_temperate"));
        assert_eq!(set.groups_of("bed"), vec!["g1".to_string()]);
    }

    #[test]
    fn undefined_group_members_are_reported_not_errors() {
        let mut set = LayerSet {
            layers: vec![layer("bed", "Bed")],
            ..LayerSet::default()
        };
        set.groups = vec![group("g1", &["bed", "ghost"])];
        // Not a validation error: a member may be a layer added later, and
        // dropping it would erase a deliberate edit.
        assert!(set.validate().is_ok());
        let warnings = set.group_warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("ghost"), "{warnings:?}");
    }

    #[test]
    fn groups_round_trip_through_layers_json_including_extra() {
        let (_dir, project) = project();
        let stored = serde_json::json!({
            "schema": SCHEMA,
            "schema_version": SCHEMA_VERSION,
            "layers": [{"id": "bed", "name": "Bed"}],
            "default_reducer": "median",
            "groups": [{
                "id": "g1",
                "name": "Mutually exclusive",
                "members": ["bed"],
                "future_group_field": 7
            }],
            "future_top_level": true
        });
        project
            .documents()
            .write(
                &path(),
                &serde_json::to_string_pretty(&stored).unwrap(),
                &Expectation::Absent,
            )
            .unwrap();

        let (set, version) = read(project.documents()).unwrap();
        assert_eq!(set.groups.len(), 1);
        assert_eq!(set.groups[0].id, "g1");
        assert_eq!(set.groups[0].members, vec!["bed".to_string()]);
        assert_eq!(set.default_reducer, Some(Reducer::Median));
        write(
            project.documents(),
            &set,
            &Expectation::Version(version.unwrap()),
        )
        .unwrap();

        let (again, _) = read(project.documents()).unwrap();
        assert_eq!(
            again.groups[0].extra.get("future_group_field"),
            Some(&serde_json::json!(7))
        );
        assert_eq!(
            again.extra.get("future_top_level"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn labels_with_no_definition_are_reported_not_dropped() {
        let set = LayerSet {
            layers: vec![layer("bed", "Bed")],
            ..LayerSet::default()
        };
        let unknown = set.unknown_ids(["bed", "englacial", "englacial", "moraine"].into_iter());
        assert_eq!(unknown, vec!["englacial", "moraine"]);
    }

    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let (_dir, project) = project();
        let stored = serde_json::json!({
            "schema": SCHEMA,
            "schema_version": SCHEMA_VERSION,
            "layers": [{"id": "bed", "name": "Bed", "future_field": 42}],
            "future_top_level": true
        });
        project
            .documents()
            .write(
                &path(),
                &serde_json::to_string_pretty(&stored).unwrap(),
                &Expectation::Absent,
            )
            .unwrap();

        let (set, version) = read(project.documents()).unwrap();
        write(
            project.documents(),
            &set,
            &Expectation::Version(version.unwrap()),
        )
        .unwrap();

        let (again, _) = read(project.documents()).unwrap();
        assert_eq!(
            again.layers[0].extra.get("future_field"),
            Some(&serde_json::json!(42))
        );
        assert_eq!(
            again.extra.get("future_top_level"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn a_concurrent_edit_is_refused() {
        let (_dir, project) = project();
        let set = LayerSet {
            layers: vec![layer("bed", "Bed")],
            ..LayerSet::default()
        };
        let first = write(project.documents(), &set, &Expectation::Absent).unwrap();
        write(
            project.documents(),
            &set,
            &Expectation::Version(first.clone()),
        )
        .unwrap();

        // `first` is now stale only if the content changed; it did not, so
        // rewrite with a genuinely different set to move the version on.
        let changed = LayerSet {
            layers: vec![layer("bed", "Bed"), layer("moraine", "Moraine")],
            ..LayerSet::default()
        };
        write(
            project.documents(),
            &changed,
            &Expectation::Version(first.clone()),
        )
        .unwrap();

        let error = write(project.documents(), &set, &Expectation::Version(first)).unwrap_err();
        assert!(matches!(
            error,
            LayerError::Store(StoreError::Conflict { .. })
        ));
    }
}
