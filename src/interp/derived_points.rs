//! Building the wide derived-point product (#205).
//!
//! A derived item is one value per position. A derived **layer** is a depth,
//! so it is written in all three vertical units (`<id>_m`, `<id>_ns`,
//! `<id>_samples`); a derived **attribute** is a number written in its own
//! unit (`<id>_m`, `<id>_ns`, `<id>_samples`, or the bare id when
//! dimensionless).
//!
//! Every visible item is a property of **one point per grid position**, not a
//! separate point per item. That is what puts a thickness and a cts depth on
//! the same row, with the attributes that summarise them beside them, instead
//! of each derived line being exported as though it were a picked layer.
//!
//! This module is pure: the derived evaluation lives in
//! [`crate::project::derived`] and the grid in [`crate::interp::level2`], so
//! the assembly can be tested against synthetic inputs with no I/O.

use std::collections::BTreeMap;

use crate::interp::derive::{convert_position, EvaluatedItem, Kind, Unit};
use crate::interp::level2::{self, RadargramGeometry};
use crate::project::derived::DerivedSet;

/// One property a derived item contributes.
///
/// Carried on the export for provenance: the property name alone
/// (`thickness_user_std_m`) does not say which item, unit or kind produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedItemInfo {
    pub id: String,
    pub name: String,
    pub unit: Unit,
    pub kind: Kind,
    /// The property names this item contributes, in output order.
    pub properties: Vec<String>,
}

/// One grid position with every visible derived item attached.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedPoint {
    pub trace: f64,
    pub distance_m: f64,
    pub easting: f64,
    pub northing: f64,
    pub longitude: f64,
    pub latitude: f64,
    /// Property name -> value. `None` where the item is absent here, which a
    /// NaN reads as in both serializers.
    pub values: BTreeMap<String, Option<f64>>,
}

/// A radargram's derived items as one wide table of points.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedPointsExport {
    pub points: Vec<DerivedPoint>,
    pub radargram_id: String,
    pub revision_id: String,
    pub crs: String,
    /// The spacing actually used, in metres. `None` for a per-trace export.
    pub spacing_m: Option<f64>,
    pub antenna_separation_effective_m: Option<f64>,
    pub twtt_anchor: Option<String>,
    /// The author recorded on every point: the caller this was evaluated for.
    pub user: String,
    /// The items present, in stored (draw) order.
    pub items: Vec<DerivedItemInfo>,
}

/// Assemble the wide point product from evaluated items.
///
/// Only items `viewer` may see are included, and -- unless `include_unlisted` --
/// only those marked `listed`, matching the viewer panel and the picked
/// download's unlisted handling.
pub fn build(
    set: &DerivedSet,
    results: &BTreeMap<String, EvaluatedItem>,
    geometry: &RadargramGeometry,
    grid: &[f64],
    spacing_m: Option<f64>,
    viewer: &str,
    include_unlisted: bool,
) -> DerivedPointsExport {
    let visible: Vec<&crate::project::derived::DerivedItem> = set
        .visible_to(viewer)
        .into_iter()
        .filter(|item| include_unlisted || item.listed)
        .filter(|item| results.contains_key(&item.id))
        .collect();

    let items: Vec<DerivedItemInfo> = visible
        .iter()
        .map(|item| {
            let result = &results[&item.id];
            DerivedItemInfo {
                id: item.id.clone(),
                name: item.name.clone(),
                unit: result.unit,
                kind: result.kind,
                properties: property_names(&item.id, result.kind, result.unit),
            }
        })
        .collect();

    let points: Vec<DerivedPoint> = grid
        .iter()
        .enumerate()
        .map(|(position, trace)| {
            let mut values = BTreeMap::new();
            for (item, info) in visible.iter().zip(&items) {
                let result = &results[&item.id];
                let value = result.values.get(position).copied().unwrap_or(f64::NAN);
                match result.kind {
                    // A layer is a position, so it is meaningful in every
                    // vertical unit. Converting a NaN keeps it absent.
                    Kind::Layer => {
                        for (property, target) in info.properties.iter().zip([
                            Unit::Meters,
                            Unit::Nanoseconds,
                            Unit::Samples,
                        ]) {
                            values.insert(
                                property.clone(),
                                finite(convert_position(value, result.unit, target, geometry)),
                            );
                        }
                    }
                    // An attribute is a number in its own unit: a standard
                    // deviation in metres is not a depth, and converting it
                    // through the sample axis would give it a spatial
                    // meaning it does not have.
                    Kind::Attribute => {
                        values.insert(info.properties[0].clone(), finite(value));
                    }
                }
            }
            DerivedPoint {
                trace: *trace,
                distance_m: level2::interpolate_index(&geometry.distance, *trace),
                easting: level2::interpolate_index(&geometry.easting, *trace),
                northing: level2::interpolate_index(&geometry.northing, *trace),
                longitude: level2::interpolate_index(&geometry.longitude, *trace),
                latitude: level2::interpolate_index(&geometry.latitude, *trace),
                values,
            }
        })
        .collect();

    DerivedPointsExport {
        points,
        radargram_id: geometry.radargram_id.clone(),
        revision_id: geometry.revision_id.clone(),
        crs: geometry.crs.clone(),
        spacing_m,
        antenna_separation_effective_m: geometry.antenna_separation_effective_m,
        twtt_anchor: geometry.twtt_anchor.clone(),
        user: viewer.to_string(),
        items,
    }
}

/// The output property names one item contributes, in order.
pub fn property_names(id: &str, kind: Kind, unit: Unit) -> Vec<String> {
    match kind {
        Kind::Layer => ["m", "ns", "samples"]
            .into_iter()
            .map(|suffix| format!("{id}_{suffix}"))
            .collect(),
        Kind::Attribute => match unit {
            Unit::Dimensionless => vec![id.to_string()],
            Unit::Meters => vec![format!("{id}_m")],
            Unit::Nanoseconds => vec![format!("{id}_ns")],
            Unit::Samples => vec![format!("{id}_samples")],
        },
    }
}

/// A finite value, or `None` for NaN and infinity.
fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::derived::{Audience, DerivedItem, FillTo, Scope};
    use std::collections::BTreeMap;

    fn geometry() -> RadargramGeometry {
        let n = 5;
        RadargramGeometry {
            radargram_id: "test-line".into(),
            revision_id: "revabc123".into(),
            distance: (0..n).map(|i| i as f64 * 10.0).collect(),
            twtt: (0..4).map(|i| i as f64 * 5.0).collect(),
            depth: (0..4).map(|i| i as f64 * 1.0).collect(),
            easting: (0..n).map(|i| 400_000.0 + i as f64).collect(),
            northing: (0..n).map(|_| 8_700_000.0).collect(),
            longitude: (0..n).map(|i| 15.0 + i as f64 * 1e-5).collect(),
            latitude: (0..n).map(|_| 78.0).collect(),
            antenna_separation_effective_m: Some(0.0),
            twtt_anchor: Some("twtt_normal_incidence".into()),
            crs: "EPSG:32633".into(),
        }
    }

    fn item(id: &str, unit: Unit, listed: bool) -> DerivedItem {
        DerivedItem {
            id: id.into(),
            name: id.into(),
            expression: "median(bed)".into(),
            unit,
            color: None,
            show: false,
            listed,
            fill_to: None::<FillTo>,
            scope: Scope::Project,
            audience: Audience::OwnPicks,
            extra: Default::default(),
        }
    }

    fn result(kind: Kind, unit: Unit, values: Vec<f64>) -> EvaluatedItem {
        EvaluatedItem { kind, unit, values }
    }

    #[test]
    fn a_layer_is_one_point_per_position_in_every_vertical_unit() {
        let set = DerivedSet {
            items: vec![item("thickness", Unit::Meters, true)],
            ..DerivedSet::default()
        };
        let mut results = BTreeMap::new();
        // Sample 2.0 -> depth 2 m, twtt 10 ns, sample 2.
        results.insert(
            "thickness".to_string(),
            result(Kind::Layer, Unit::Meters, vec![2.0; 5]),
        );

        let export = build(
            &set,
            &results,
            &geometry(),
            &[0.0, 1.0, 2.0],
            None,
            "me",
            false,
        );

        assert_eq!(export.points.len(), 3);
        assert_eq!(
            export.items[0].properties,
            ["thickness_m", "thickness_ns", "thickness_samples"]
        );
        // All three units describe the same position, on the same point.
        assert_eq!(export.points[1].values["thickness_m"], Some(2.0));
        assert_eq!(export.points[1].values["thickness_ns"], Some(10.0));
        assert_eq!(export.points[1].values["thickness_samples"], Some(2.0));
        // Base geography is interpolated at the grid trace.
        assert_eq!(export.points[1].distance_m, 10.0);
        assert_eq!(export.points[1].easting, 400_001.0);
    }

    #[test]
    fn two_layers_and_an_attribute_share_one_point() {
        let set = DerivedSet {
            items: vec![
                item("thickness", Unit::Meters, true),
                item("cts", Unit::Meters, true),
                item("thickness_user_count", Unit::Dimensionless, true),
            ],
            ..DerivedSet::default()
        };
        let mut results = BTreeMap::new();
        results.insert(
            "thickness".to_string(),
            result(Kind::Layer, Unit::Meters, vec![2.0, f64::NAN]),
        );
        results.insert(
            "cts".to_string(),
            result(Kind::Layer, Unit::Meters, vec![1.0, 1.5]),
        );
        results.insert(
            "thickness_user_count".to_string(),
            result(Kind::Attribute, Unit::Dimensionless, vec![3.0, 4.0]),
        );

        let export = build(&set, &results, &geometry(), &[0.0, 1.0], None, "me", false);

        let first = &export.points[0];
        assert_eq!(first.values["thickness_m"], Some(2.0));
        assert_eq!(first.values["cts_m"], Some(1.0));
        assert_eq!(first.values["thickness_user_count"], Some(3.0));

        // A gap is absent, not a fabricated depth; the other items remain.
        let second = &export.points[1];
        assert_eq!(second.values["thickness_m"], None);
        assert_eq!(second.values["cts_m"], Some(1.5));
    }

    #[test]
    fn an_attribute_carries_its_own_unit_and_a_dimensionless_one_is_bare() {
        assert_eq!(
            property_names("spread", Kind::Attribute, Unit::Meters),
            ["spread_m"]
        );
        assert_eq!(
            property_names("spread", Kind::Attribute, Unit::Nanoseconds),
            ["spread_ns"]
        );
        assert_eq!(
            property_names("count", Kind::Attribute, Unit::Dimensionless),
            ["count"]
        );
    }

    #[test]
    fn unlisted_items_are_left_out_unless_asked_for() {
        let set = DerivedSet {
            items: vec![
                item("shown", Unit::Meters, true),
                item("hidden", Unit::Meters, false),
            ],
            ..DerivedSet::default()
        };
        let mut results = BTreeMap::new();
        results.insert(
            "shown".to_string(),
            result(Kind::Layer, Unit::Meters, vec![1.0]),
        );
        results.insert(
            "hidden".to_string(),
            result(Kind::Layer, Unit::Meters, vec![2.0]),
        );

        let default = build(&set, &results, &geometry(), &[0.0], None, "me", false);
        assert_eq!(default.items.len(), 1);
        assert!(!default.points[0].values.contains_key("hidden_m"));

        let all = build(&set, &results, &geometry(), &[0.0], None, "me", true);
        assert_eq!(all.items.len(), 2);
        assert_eq!(all.points[0].values["hidden_m"], Some(2.0));
    }
}
