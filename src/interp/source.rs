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

/// Everything needed to describe a radargram's axes to a gprinterp
/// document, read from the file it was processed into.
///
/// Separate from [`RadargramGeometry`] because they answer different
/// questions: that one is "where is this pick, in metres and nanoseconds",
/// this one is "what would let somebody put this pick on a different
/// version of the same radargram". A level 2 export needs the first; a save
/// needs the second.
#[derive(Debug, Clone, Default)]
pub struct AxisDeclarations {
    /// Per-trace acquisition time, epoch seconds.
    pub time: Vec<f64>,
    /// Which gprinterp `y` anchor the travel-time axis is (#144). `None`
    /// for a radargram processed before that landed.
    pub twtt_anchor: Option<String>,
    /// Recording-clock position of sample 0, and of time zero. Scalar in
    /// the file reads as one value; per-trace as one each.
    pub twtt_crop: Vec<f64>,
    pub twtt_time_zero: Vec<f64>,
    /// Sample interval, nanoseconds.
    pub dt_ns: f64,
    /// The file's own `ridal_processing_datetime`, so a caller can check
    /// that these axes belong to the revision it believes it is
    /// describing. The catalog's snapshot and this read happen at
    /// different moments, and a file reprocessed in place between them
    /// would otherwise pair one revision's mapping with another's id.
    pub processing_datetime: Option<String>,
}

/// Read the axis declarations, or as much of them as the file carries.
///
/// Lenient throughout, unlike [`read_geometry`]: this describes what a
/// radargram can offer, and a radargram that can offer nothing is a fact to
/// record rather than an error to raise. Its picks are still perfectly good
/// picks — they simply cannot be carried across a reprocess, which is the
/// state every interpretation was in before #146.
pub fn read_axis_declarations(path: &Path) -> AxisDeclarations {
    let Ok(file) = netcdf::open(path) else {
        return AxisDeclarations::default();
    };
    let twtt = read_f64_variable(&file, "twtt").unwrap_or_default();
    AxisDeclarations {
        time: read_f64_variable(&file, "time").unwrap_or_default(),
        processing_datetime: read_str_attr(&file, "ridal_processing_datetime"),
        twtt_anchor: file
            .variable("twtt")
            .and_then(|var| read_str_attr_of(&var, "anchor_name")),
        twtt_crop: read_f64_variable(&file, "twtt_crop").unwrap_or_default(),
        twtt_time_zero: read_f64_variable(&file, "twtt_time_zero").unwrap_or_default(),
        // From the axis itself rather than from the metadata, so it is the
        // spacing the file actually has after whatever resampling ran.
        dt_ns: match twtt.as_slice() {
            [first, second, ..] => second - first,
            _ => 0.0,
        },
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
