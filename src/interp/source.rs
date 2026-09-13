//! Building a [`RadargramGeometry`] from a processed Ridal NetCDF.
//!
//! This is the I/O boundary for level 2 export: everything the derivation in
//! [`super::level2`] needs is read here, once, so that module stays pure and
//! testable without files.

use std::path::Path;

use crate::identity::{RadargramId, RevisionId};
use crate::interp::level2::RadargramGeometry;

/// Read the coordinate variables and identity attributes needed to derive a
/// level 2 product.
///
/// Every variable read here is written unconditionally by `export.rs`, so a
/// missing one means the file was not produced by Ridal (or predates the
/// attribute) and is reported rather than defaulted -- unlike the web
/// viewer's `/axes` endpoint, which degrades to a partial readout. A level 2
/// point with a silently absent depth or position would be a data error, not
/// a degraded display.
pub fn read_geometry(path: &Path) -> Result<RadargramGeometry, String> {
    let file = netcdf::open(path).map_err(|e| format!("Failed to open {path:?} as NetCDF: {e}"))?;

    let radargram_id = read_str_attr(&file, "ridal_radargram_id").ok_or_else(|| {
        format!(
            "{path:?} has no 'ridal_radargram_id' attribute, so it is not a \
             processed Ridal radargram"
        )
    })?;
    let radargram_id = RadargramId::new(&radargram_id)
        .map_err(|e| format!("{path:?} has an invalid radargram id: {e}"))?;
    let processing_datetime =
        read_str_attr(&file, "ridal_processing_datetime").ok_or_else(|| {
            format!(
                "{path:?} has no 'ridal_processing_datetime' attribute, so its revision is unknown"
            )
        })?;
    let revision_id = RevisionId::fingerprint_v1(&radargram_id, &processing_datetime);

    let crs =
        read_str_attr(&file, "crs").ok_or_else(|| format!("{path:?} has no 'crs' attribute"))?;

    Ok(RadargramGeometry {
        radargram_id: radargram_id.as_str().to_string(),
        revision_id: revision_id.as_str().to_string(),
        distance: read_f64_variable(&file, "distance")?,
        twtt: read_f64_variable(&file, "twtt")?,
        depth: read_f64_variable(&file, "depth")?,
        easting: read_f64_variable(&file, "easting")?,
        northing: read_f64_variable(&file, "northing")?,
        longitude: read_f64_variable(&file, "longitude")?,
        latitude: read_f64_variable(&file, "latitude")?,
        crs,
        // Optional, unlike everything above: radargrams processed before
        // Ridal recorded these exist, and their picks are still exportable.
        // A missing value is reported as missing rather than guessed --
        // there is no safe default, since both "uncorrected" and "corrected"
        // are wrong half the time.
        antenna_separation_effective_m: read_f64_attr(&file, "antenna_separation_effective"),
        twtt_anchor: file
            .variable("twtt")
            .and_then(|var| read_str_attr_of(&var, "anchor_name")),
    })
}

/// Read a numeric global attribute, widening to `f64`.
///
/// Ridal writes this one as `f32`; the match is explicit rather than a
/// blanket numeric cast so a future type change is a compile-time question
/// rather than a silently absent value.
fn read_f64_attr(file: &netcdf::File, name: &str) -> Option<f64> {
    match file.attribute(name)?.value().ok()? {
        netcdf::AttributeValue::Float(v) => Some(v as f64),
        netcdf::AttributeValue::Double(v) => Some(v),
        _ => None,
    }
}

fn read_str_attr_of(var: &netcdf::Variable, name: &str) -> Option<String> {
    match var.attribute(name)?.value().ok()? {
        netcdf::AttributeValue::Str(value) => Some(value),
        _ => None,
    }
}

/// Read a numeric variable, widening to `f64`.
///
/// `twtt` and `depth` are stored as `f32` and the positional variables as
/// `f64`; the netcdf crate converts on read, so both work here.
fn read_f64_variable(file: &netcdf::File, name: &str) -> Result<Vec<f64>, String> {
    let var = file
        .variable(name)
        .ok_or_else(|| format!("Missing variable '{name}'"))?;
    var.get_values::<f64, _>(..)
        .map_err(|e| format!("Failed to read variable '{name}': {e}"))
}

fn read_str_attr(file: &netcdf::File, name: &str) -> Option<String> {
    match file.attribute(name)?.value().ok()? {
        netcdf::AttributeValue::Str(value) => Some(value),
        _ => None,
    }
}
