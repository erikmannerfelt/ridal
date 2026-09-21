//! Serializing a level 2 export to GeoJSON or CSV.
//!
//! # Coordinate reference systems
//!
//! GeoJSON output is WGS84 by default, which is what RFC 7946 mandates. The
//! `crs` member of the older 2008 draft is *not* emitted: support for it is
//! inconsistent, and a reader that ignores it places the whole dataset near
//! the equator rather than failing -- a silent, badly wrong result for a
//! data product.
//!
//! Projected coordinates are therefore carried as `easting`/`northing`
//! properties on every point, alongside the `crs` string, which is lossless
//! and readable by everything. `--crs` additionally reprojects the geometry
//! itself for callers who want that; it is opt-in precisely because the
//! result is no longer portable GeoJSON.

use std::fmt::Write as _;

use crate::coords;
use crate::interp::derived_points::DerivedPointsExport;
use crate::interp::level2::{Level2Export, Level2Point};

/// Which coordinates the output geometry is expressed in.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputCrs {
    /// WGS84 longitude/latitude. RFC 7946 conformant.
    Wgs84,
    /// A user-specified CRS. The geometry is reprojected and the CRS is
    /// recorded in the file's properties, but the result is not portable
    /// GeoJSON.
    Named(String),
}

impl OutputCrs {
    fn label(&self, native: &str) -> String {
        match self {
            OutputCrs::Wgs84 => "EPSG:4326".to_string(),
            OutputCrs::Named(name) if name == "native" => native.to_string(),
            OutputCrs::Named(name) => name.clone(),
        }
    }
}

/// A point's WGS84 longitude/latitude and native easting/northing, in that
/// order. The two coordinate pairs every point product carries, kept as one
/// tuple so reprojection is written once rather than per product.
type Coordinates = (f64, f64, f64, f64);

/// The geometry coordinate for each point, in the requested CRS.
fn project_positions(
    native_crs: &str,
    crs: &OutputCrs,
    points: &[Coordinates],
) -> Result<Vec<(f64, f64)>, String> {
    match crs {
        OutputCrs::Wgs84 => Ok(points
            .iter()
            .map(|(longitude, latitude, _, _)| (*longitude, *latitude))
            .collect()),
        OutputCrs::Named(name) => {
            let target = if name == "native" {
                native_crs
            } else {
                name.as_str()
            };
            // Asking for the radargram's own CRS returns the stored values
            // untouched. Round-tripping them through WGS84 instead would be
            // a lossy no-op: measured at ~0.13 m of drift on a Drønbreen
            // line, which is a real displacement for a bed pick and comes
            // from nothing but the reprojection itself.
            if target == native_crs {
                return Ok(points
                    .iter()
                    .map(|(_, _, easting, northing)| (*easting, *northing))
                    .collect());
            }
            // Any other CRS has to go via WGS84, since that is the only
            // transform pair `coords` exposes.
            let target_crs = coords::Crs::from_user_input(target)?;
            let wgs84: Vec<coords::Coord> = points
                .iter()
                .map(|(longitude, latitude, _, _)| coords::Coord {
                    x: *longitude,
                    y: *latitude,
                })
                .collect();
            Ok(coords::from_wgs84(&wgs84, &target_crs)?
                .into_iter()
                .map(|c| (c.x, c.y))
                .collect())
        }
    }
}

/// Serialize as a GeoJSON FeatureCollection of points.
/// Serialize one or more exports as a single GeoJSON FeatureCollection.
///
/// A slice rather than one export because a group download is exactly a
/// concatenation: every point already names its own radargram and revision,
/// so merging them needs no reconciliation. A single-radargram download
/// passes a slice of one and comes out identical to before.
pub fn to_geojson(exports: &[Level2Export], crs: &OutputCrs) -> Result<String, String> {
    // Only resolved when the output actually depends on it. WGS84 does not,
    // so a group whose radargrams sit in different UTM zones still exports
    // fine in the portable format -- refusing that would be refusing the
    // one thing that always works.
    let native = match crs {
        OutputCrs::Wgs84 => String::new(),
        OutputCrs::Named(_) => native_crs(exports)?,
    };

    let mut features = Vec::new();
    for export in exports {
        let coordinates: Vec<Coordinates> = export
            .points
            .iter()
            .map(|p| (p.longitude, p.latitude, p.easting, p.northing))
            .collect();
        let positions = project_positions(&native, crs, &coordinates)?;
        for (point, (x, y)) in export.points.iter().zip(&positions) {
            features.push(serde_json::json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [x, y]},
                "properties": properties(point, export),
            }));
        }
    }

    let sources: Vec<serde_json::Value> = exports
        .iter()
        .map(|export| {
            serde_json::json!({
                "radargram_id": export.radargram_id,
                "revision_id": export.revision_id,
                "crs": export.crs,
                "spacing_m": export.spacing_m,
                "points": export.points.len(),
            })
        })
        .collect();

    let collection = serde_json::json!({
        "type": "FeatureCollection",
        // Not a GeoJSON member: provenance for the whole file, so a
        // downstream user can tell which radargrams and which processing
        // runs these depths came from without opening a level 1 document.
        // One entry per source, because a merged file has several.
        "ridal": {
            "product_level": 2,
            "output_crs": crs.label(&native),
            "ridal_version": crate::PROGRAM_VERSION,
            "sources": sources,
        },
        "features": features,
    });

    serde_json::to_string_pretty(&collection).map_err(|e| format!("Failed to serialize: {e}"))
}

/// The projected CRS shared by every export, for `--crs native`.
///
/// Refused rather than guessed when they disagree: "native" names one CRS,
/// and a file whose coordinates silently came from two of them would be
/// wrong in a way nothing downstream could detect. WGS84 output is
/// unaffected, which is why this only matters for the native case.
fn native_crs(exports: &[Level2Export]) -> Result<String, String> {
    single_native_crs(exports.iter().map(|e| e.crs.as_str()))
}

/// The same for the derived product, whose sources carry a CRS too.
fn native_crs_derived(exports: &[DerivedPointsExport]) -> Result<String, String> {
    single_native_crs(exports.iter().map(|e| e.crs.as_str()))
}

fn single_native_crs<'a>(crss: impl Iterator<Item = &'a str>) -> Result<String, String> {
    let mut distinct: Vec<&str> = crss.collect();
    distinct.sort_unstable();
    distinct.dedup();
    match distinct.as_slice() {
        [] => Ok(String::new()),
        [one] => Ok((*one).to_string()),
        many => Err(format!(
            "These radargrams do not share a projected CRS ({}), so there is no single \
             native CRS to write them in. Download in WGS84 instead.",
            many.join(", ")
        )),
    }
}

fn properties(point: &Level2Point, export: &Level2Export) -> serde_json::Value {
    serde_json::json!({
        "radargram_id": point.radargram_id,
        "revision_id": point.revision_id,
        "layer": point.layer,
        "line_index": point.line_index,
        "point_index": point.point_index,
        "feature_id": point.feature_id,
        "trace": point.trace,
        "sample": point.sample,
        "distance_m": point.distance_m,
        "twtt_ns": point.twtt_ns,
        "depth_m": point.depth_m,
        "easting": point.easting,
        "northing": point.northing,
        "longitude": point.longitude,
        "latitude": point.latitude,
        "crs": export.crs,
        "antenna_separation_effective_m": export.antenna_separation_effective_m,
        "twtt_anchor": export.twtt_anchor,
        "user": point.user,
    })
}

/// Column order for the CSV output. Also the documented field order.
const CSV_HEADER: &str = "radargram_id,revision_id,layer,line_index,point_index,feature_id,\
                          trace,sample,distance_m,twtt_ns,depth_m,easting,northing,longitude,\
                          latitude,crs,antenna_separation_effective_m,twtt_anchor,user";

/// Serialize as CSV.
///
/// Always in native easting/northing plus WGS84 longitude/latitude, both as
/// their own columns, so the CRS question does not arise: a CSV has no
/// geometry to reproject.
pub fn to_csv(exports: &[Level2Export]) -> String {
    let total: usize = exports.iter().map(|e| e.points.len()).sum();
    let mut out = String::with_capacity(CSV_HEADER.len() + total * 128);
    out.push_str(CSV_HEADER);
    out.push('\n');
    // One header, then every export's rows. Each row names its own
    // radargram, so a merged file needs no separator and no second header.
    for export in exports {
        for point in &export.points {
            let _ = writeln!(
                out,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                csv_escape(&point.radargram_id),
                csv_escape(&point.revision_id),
                csv_escape(&point.layer),
                point.line_index,
                point.point_index,
                csv_escape(point.feature_id.as_deref().unwrap_or("")),
                point.trace,
                point.sample,
                point.distance_m,
                point.twtt_ns,
                point.depth_m,
                point.easting,
                point.northing,
                point.longitude,
                point.latitude,
                csv_escape(&export.crs),
                // Empty rather than a stand-in value: the older file this
                // came from does not record it, and any number written here
                // would be an assertion Ridal cannot make.
                export
                    .antenna_separation_effective_m
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                csv_escape(export.twtt_anchor.as_deref().unwrap_or("")),
                csv_escape(&point.user),
            );
        }
    }
    out
}

/// Base columns of the wide derived CSV, before the per-item properties.
const DERIVED_CSV_BASE: &str = "radargram_id,revision_id,trace,distance_m,easting,northing,\
                                longitude,latitude,crs,antenna_separation_effective_m,\
                                twtt_anchor,user";

/// Serialize derived points as one GeoJSON FeatureCollection of wide points.
///
/// One feature per grid position; every visible derived item is a property,
/// its name carrying its unit (`thickness_m`, `thickness_ns`, `count`).
/// Provenance lives in the `ridal` block, one entry per source, each listing
/// the items it contributed.
pub fn to_geojson_derived(
    exports: &[DerivedPointsExport],
    crs: &OutputCrs,
) -> Result<String, String> {
    let native = match crs {
        OutputCrs::Wgs84 => String::new(),
        OutputCrs::Named(_) => native_crs_derived(exports)?,
    };

    let mut features = Vec::new();
    for export in exports {
        let coordinates: Vec<Coordinates> = export
            .points
            .iter()
            .map(|p| (p.longitude, p.latitude, p.easting, p.northing))
            .collect();
        let positions = project_positions(&native, crs, &coordinates)?;
        for (point, (x, y)) in export.points.iter().zip(&positions) {
            features.push(serde_json::json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [x, y]},
                "properties": derived_properties(point, export),
            }));
        }
    }

    let sources: Vec<serde_json::Value> = exports
        .iter()
        .map(|export| {
            let items: Vec<serde_json::Value> =
                export.items.iter().map(derived_item_json).collect();
            serde_json::json!({
                "radargram_id": export.radargram_id,
                "revision_id": export.revision_id,
                "crs": export.crs,
                "spacing_m": export.spacing_m,
                "points": export.points.len(),
                "items": items,
            })
        })
        .collect();

    let collection = serde_json::json!({
        "type": "FeatureCollection",
        "ridal": {
            "product_level": 2,
            "output_crs": crs.label(&native),
            "ridal_version": crate::PROGRAM_VERSION,
            "sources": sources,
        },
        "features": features,
    });

    serde_json::to_string_pretty(&collection).map_err(|e| format!("Failed to serialize: {e}"))
}

fn derived_item_json(item: &crate::interp::derived_points::DerivedItemInfo) -> serde_json::Value {
    serde_json::json!({
        "id": item.id,
        "name": item.name,
        "unit": item.unit,
        "kind": item.kind,
        "properties": item.properties,
    })
}

fn derived_properties(
    point: &crate::interp::derived_points::DerivedPoint,
    export: &DerivedPointsExport,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    // Items first, so a base field always wins a name collision: a
    // dimensionless attribute is named by its bare id, and an id equal to
    // `trace` or `distance_m` must not be able to displace the coordinate
    // every consumer depends on.
    for (name, value) in &point.values {
        object.insert(
            name.clone(),
            match value {
                Some(value) => serde_json::json!(value),
                None => serde_json::Value::Null,
            },
        );
    }
    for (name, value) in [
        ("radargram_id", serde_json::json!(export.radargram_id)),
        ("revision_id", serde_json::json!(export.revision_id)),
        ("trace", serde_json::json!(point.trace)),
        ("distance_m", serde_json::json!(point.distance_m)),
        ("easting", serde_json::json!(point.easting)),
        ("northing", serde_json::json!(point.northing)),
        ("longitude", serde_json::json!(point.longitude)),
        ("latitude", serde_json::json!(point.latitude)),
        ("crs", serde_json::json!(export.crs)),
        (
            "antenna_separation_effective_m",
            serde_json::json!(export.antenna_separation_effective_m),
        ),
        ("twtt_anchor", serde_json::json!(export.twtt_anchor)),
        ("user", serde_json::json!(export.user)),
    ] {
        object.insert(name.to_string(), value);
    }
    serde_json::Value::Object(object)
}

/// Serialize derived points as one wide CSV.
///
/// Columns are the base fields followed by every item property in the file.
/// A merged file spans several radargrams whose item sets may differ, so the
/// header is the union in first-appearance order and a missing value is
/// empty -- the shape `expected_consensus.csv` uses.
pub fn to_csv_derived(exports: &[DerivedPointsExport]) -> String {
    let mut columns: Vec<&str> = Vec::new();
    for export in exports {
        for item in &export.items {
            for property in &item.properties {
                if !columns.contains(&property.as_str()) {
                    columns.push(property.as_str());
                }
            }
        }
    }

    let total: usize = exports.iter().map(|e| e.points.len()).sum();
    let mut out = String::with_capacity(DERIVED_CSV_BASE.len() + total * (32 + columns.len() * 16));
    out.push_str(DERIVED_CSV_BASE);
    for column in &columns {
        out.push(',');
        out.push_str(column);
    }
    out.push('\n');

    for export in exports {
        for point in &export.points {
            let _ = write!(
                out,
                "{},{},{},{},{},{},{},{},{},{},{},{}",
                csv_escape(&export.radargram_id),
                csv_escape(&export.revision_id),
                point.trace,
                point.distance_m,
                point.easting,
                point.northing,
                point.longitude,
                point.latitude,
                csv_escape(&export.crs),
                export
                    .antenna_separation_effective_m
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                csv_escape(export.twtt_anchor.as_deref().unwrap_or("")),
                csv_escape(&export.user),
            );
            for column in &columns {
                out.push(',');
                if let Some(Some(value)) = point.values.get(*column) {
                    let _ = write!(out, "{value}");
                }
            }
            out.push('\n');
        }
    }
    out
}

/// Quote a field if it contains a delimiter, quote or newline.
///
/// Layer names are user-supplied and will eventually arrive from a GUI text
/// box, so "bed, upper" is a realistic value rather than a hypothetical one.
fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::derive::{Kind, Unit};
    use crate::interp::derived_points::{DerivedItemInfo, DerivedPoint};
    use crate::interp::level2::Level2Point;

    fn export() -> Level2Export {
        Level2Export {
            points: vec![Level2Point {
                radargram_id: "test-line".into(),
                revision_id: "revabc123".into(),
                layer: "bed".into(),
                line_index: 0,
                point_index: 3,
                feature_id: Some("f-0001".into()),
                trace: 30.0,
                sample: 12.5,
                distance_m: 30.0,
                twtt_ns: 5.0,
                depth_m: 0.5,
                easting: 400_030.0,
                northing: 8_700_000.0,
                longitude: 15.0003,
                latitude: 78.0,
                user: "default".into(),
            }],
            radargram_id: "test-line".into(),
            revision_id: "revabc123".into(),
            crs: "EPSG:32633".into(),
            spacing_m: Some(10.0),
            antenna_separation_effective_m: Some(0.0),
            twtt_anchor: Some("twtt_normal_incidence".into()),
        }
    }

    #[test]
    fn geojson_is_wgs84_and_carries_projected_coordinates_as_properties() {
        let text = to_geojson(&[export()], &OutputCrs::Wgs84).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();

        let geometry = &value["features"][0]["geometry"];
        assert_eq!(geometry["type"], "Point");
        assert_eq!(geometry["coordinates"][0], 15.0003);
        assert_eq!(geometry["coordinates"][1], 78.0);

        let properties = &value["features"][0]["properties"];
        assert_eq!(properties["easting"], 400_030.0);
        assert_eq!(properties["northing"], 8_700_000.0);
        assert_eq!(properties["crs"], "EPSG:32633");
        assert_eq!(properties["layer"], "bed");
        assert_eq!(properties["line_index"], 0);
        assert_eq!(properties["depth_m"], 0.5);
        assert_eq!(properties["user"], "default");
    }

    #[test]
    fn geojson_omits_the_legacy_crs_member() {
        // RFC 7946 removed it, and a reader that ignores it silently places
        // the data at the equator instead of failing.
        let text = to_geojson(&[export()], &OutputCrs::Wgs84).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(value.get("crs").is_none());
    }

    #[test]
    fn geojson_records_the_source_revision_for_provenance() {
        let text = to_geojson(&[export()], &OutputCrs::Wgs84).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        // One entry per source, because a merged file has several.
        assert_eq!(value["ridal"]["sources"][0]["radargram_id"], "test-line");
        assert_eq!(value["ridal"]["sources"][0]["revision_id"], "revabc123");
        assert_eq!(value["ridal"]["sources"][0]["spacing_m"], 10.0);
        assert_eq!(value["ridal"]["product_level"], 2);
    }

    #[test]
    fn requesting_the_native_crs_returns_the_stored_values_untouched() {
        // No reprojection round trip, so no drift. Measured at ~0.13 m on a
        // Drønbreen line before this was special-cased, which would move a
        // bed pick by more than its own along-track spacing.
        for name in ["native", "EPSG:32633"] {
            let text = to_geojson(&[export()], &OutputCrs::Named(name.into())).unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            let coordinates = &value["features"][0]["geometry"]["coordinates"];
            assert_eq!(coordinates[0], 400_030.0, "{name}");
            assert_eq!(coordinates[1], 8_700_000.0, "{name}");
            assert_eq!(value["ridal"]["output_crs"], "EPSG:32633", "{name}");
        }
    }

    #[test]
    fn csv_has_a_header_and_one_row_per_point() {
        let text = to_csv(&[export()]);
        let mut lines = text.lines();
        assert_eq!(lines.next().unwrap(), CSV_HEADER);
        let row = lines.next().unwrap();
        // Radargram first, so a merged file sorts and filters by it.
        assert!(
            row.starts_with("test-line,revabc123,bed,0,3,f-0001,30,12.5,30,5,0.5,"),
            "{row}"
        );
        assert!(
            row.ends_with("EPSG:32633,0,twtt_normal_incidence,default"),
            "{row}"
        );
        assert!(lines.next().is_none());
    }

    #[test]
    fn a_row_says_what_its_travel_time_refers_to() {
        // Both columns are needed to answer it. An effective separation of
        // zero could equally mean "corrected" or "acquired with coincident
        // antennas", and the anchor name is what tells them apart -- so a
        // reader does not have to know Ridal's inference rule.
        let text = to_csv(&[export()]);
        let header: Vec<&str> = CSV_HEADER.split(',').collect();
        let row: Vec<&str> = text.lines().nth(1).unwrap().split(',').collect();
        let field = |name: &str| row[header.iter().position(|h| *h == name).unwrap()];

        assert_eq!(field("antenna_separation_effective_m"), "0");
        assert_eq!(field("twtt_anchor"), "twtt_normal_incidence");
    }

    #[test]
    fn an_older_radargram_leaves_the_new_columns_empty_rather_than_guessing() {
        // A radargram processed before Ridal recorded these does not say,
        // and any number written here would be an assertion Ridal cannot
        // make. Its picks still export.
        let mut silent = export();
        silent.antenna_separation_effective_m = None;
        silent.twtt_anchor = None;

        let text = to_csv(&[silent.clone()]);
        let row = text.lines().nth(1).unwrap();
        assert!(row.ends_with("EPSG:32633,,,default"), "{row}");

        let value: serde_json::Value =
            serde_json::from_str(&to_geojson(&[silent], &OutputCrs::Wgs84).unwrap()).unwrap();
        let properties = &value["features"][0]["properties"];
        assert!(properties["antenna_separation_effective_m"].is_null());
        assert!(properties["twtt_anchor"].is_null());
    }

    #[test]
    fn merging_two_exports_keeps_each_point_attributed() {
        let mut second = export();
        second.radargram_id = "other-line".into();
        second.points[0].radargram_id = "other-line".into();
        second.points[0].layer = "internal".into();

        let text = to_csv(&[export(), second.clone()]);
        let lines: Vec<&str> = text.lines().collect();
        // One header for the whole file, then a row per point.
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[1].starts_with("test-line,"), "{text}");
        assert!(lines[2].starts_with("other-line,"), "{text}");

        let value: serde_json::Value =
            serde_json::from_str(&to_geojson(&[export(), second], &OutputCrs::Wgs84).unwrap())
                .unwrap();
        assert_eq!(value["features"].as_array().unwrap().len(), 2);
        assert_eq!(
            value["features"][1]["properties"]["radargram_id"],
            "other-line"
        );
        assert_eq!(value["ridal"]["sources"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_native_crs_download_is_refused_when_the_sources_disagree() {
        // "native" names one CRS. A file mixing two would be wrong in a way
        // nothing downstream could detect.
        let mut second = export();
        second.crs = "EPSG:32634".into();
        let error =
            to_geojson(&[export(), second], &OutputCrs::Named("native".into())).unwrap_err();
        assert!(error.contains("EPSG:32633"), "{error}");
        assert!(error.contains("EPSG:32634"), "{error}");

        // WGS84 is unaffected: it does not depend on the source CRS.
        let mut second = export();
        second.crs = "EPSG:32634".into();
        assert!(to_geojson(&[export(), second], &OutputCrs::Wgs84).is_ok());
    }

    #[test]
    fn csv_quotes_layer_names_containing_a_comma() {
        let mut export = export();
        export.points[0].layer = "bed, upper".into();
        let text = to_csv(&[export]);
        assert!(
            text.lines()
                .nth(1)
                .unwrap()
                .starts_with("test-line,revabc123,\"bed, upper\","),
            "{text}"
        );
    }

    // -- Wide derived product ------------------------------------------------

    fn derived_point(trace: f64, values: &[(&str, Option<f64>)]) -> DerivedPoint {
        DerivedPoint {
            trace,
            distance_m: trace * 5.0,
            easting: 400_000.0 + trace,
            northing: 8_700_000.0,
            longitude: 15.0 + trace * 1e-5,
            latitude: 78.0,
            values: values
                .iter()
                .map(|(name, value)| (name.to_string(), *value))
                .collect(),
        }
    }

    fn derived_export() -> DerivedPointsExport {
        DerivedPointsExport {
            points: vec![
                derived_point(
                    0.0,
                    &[
                        ("thickness_m", Some(2.0)),
                        ("thickness_ns", Some(10.0)),
                        ("thickness_samples", Some(2.0)),
                        ("cts_m", None),
                    ],
                ),
                derived_point(
                    1.0,
                    &[
                        ("thickness_m", Some(2.5)),
                        ("thickness_ns", Some(12.5)),
                        ("thickness_samples", Some(2.5)),
                        ("cts_m", Some(1.5)),
                    ],
                ),
            ],
            radargram_id: "test-line".into(),
            revision_id: "revabc123".into(),
            crs: "EPSG:32633".into(),
            spacing_m: Some(5.0),
            antenna_separation_effective_m: Some(0.0),
            twtt_anchor: Some("twtt_normal_incidence".into()),
            user: "default".into(),
            items: vec![
                DerivedItemInfo {
                    id: "thickness".into(),
                    name: "Thickness".into(),
                    unit: Unit::Meters,
                    kind: Kind::Layer,
                    properties: vec![
                        "thickness_m".into(),
                        "thickness_ns".into(),
                        "thickness_samples".into(),
                    ],
                },
                DerivedItemInfo {
                    id: "cts".into(),
                    name: "CTS".into(),
                    unit: Unit::Meters,
                    kind: Kind::Layer,
                    properties: vec!["cts_m".into(), "cts_ns".into(), "cts_samples".into()],
                },
            ],
        }
    }

    #[test]
    fn derived_geojson_is_one_feature_per_position_with_every_item() {
        let text = to_geojson_derived(&[derived_export()], &OutputCrs::Wgs84).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();

        let features = value["features"].as_array().unwrap();
        assert_eq!(features.len(), 2, "one point per grid position");

        let first = &features[0]["properties"];
        assert_eq!(first["thickness_m"], 2.0);
        assert_eq!(first["thickness_ns"], 10.0);
        assert_eq!(first["cts_m"], serde_json::Value::Null);
        assert_eq!(first["distance_m"], 0.0);
        assert_eq!(first["trace"], 0.0);

        // Provenance names the items the source carried.
        let items = &value["ridal"]["sources"][0]["items"];
        assert_eq!(items[0]["id"], "thickness");
        assert_eq!(items[0]["properties"][0], "thickness_m");
        assert_eq!(value["ridal"]["sources"][0]["spacing_m"], 5.0);
    }

    #[test]
    fn derived_csv_is_wide_and_a_missing_value_is_empty() {
        let text = to_csv_derived(&[derived_export()]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "radargram_id,revision_id,trace,distance_m,easting,northing,longitude,latitude,\
             crs,antenna_separation_effective_m,twtt_anchor,user,thickness_m,thickness_ns,\
             thickness_samples,cts_m,cts_ns,cts_samples"
        );
        assert_eq!(lines.len(), 3, "{text}");
        // The first point has no cts value: its three columns are empty.
        assert!(lines[1].ends_with("2,10,2,,,"), "{}", lines[1]);
        assert!(lines[2].ends_with("2.5,12.5,2.5,1.5,,"), "{}", lines[2]);
    }

    #[test]
    fn a_derived_item_cannot_displace_a_base_field() {
        // A dimensionless attribute is named by its bare id, so an id like
        // `distance_m` is reachable. The coordinate every consumer depends
        // on must win.
        let mut export = derived_export();
        export.points = vec![derived_point(0.0, &[("distance_m", Some(99.0))])];
        let text = to_geojson_derived(&[export], &OutputCrs::Wgs84).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["features"][0]["properties"]["distance_m"], 0.0);
    }

    #[test]
    fn a_merged_derived_csv_unions_the_item_columns() {
        // Two radargrams may carry different items; the header is the union
        // in first-appearance order and a missing value is empty.
        let mut second = derived_export();
        second.radargram_id = "other-line".into();
        second.points = vec![derived_point(0.0, &[("cts_m", Some(1.0))])];
        second.items = vec![DerivedItemInfo {
            id: "cts".into(),
            name: "CTS".into(),
            unit: Unit::Meters,
            kind: Kind::Layer,
            properties: vec!["cts_m".into()],
        }];

        let text = to_csv_derived(&[derived_export(), second]);
        let lines: Vec<&str> = text.lines().collect();
        // thickness_* come from the first export, cts_m is already present;
        // the second adds no new column.
        assert!(lines[0].contains("thickness_m"), "{}", lines[0]);
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[3].starts_with("other-line,"), "{}", lines[3]);
    }
}
