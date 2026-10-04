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
    /// How many samples the revision has. Read from the travel-time axis
    /// rather than a dimension, so it is the length of the thing an
    /// interpretation's `sample` coordinate actually indexes.
    pub n_samples: usize,
}

/// A processed file's identity and axis declarations: everything a
/// replace's consequence report reads from the incoming file (#331).
///
/// The body of `POST /api/v1/datasets/{radargram_id}/replace/preflight`,
/// and what `ridal._preflight_body` reads from a local file for the Python
/// client (#328), so one type is both ends of the request. A type of its
/// own rather than [`AxisDeclarations`] itself, so the API does not change
/// when that internal type does.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct RevisionDeclarations {
    /// The file's radargram id. It must be the radargram in the path.
    pub radargram_id: String,
    /// The file's `ridal_processing_datetime` attribute, verbatim. With the
    /// radargram id it gives the revision id.
    pub processing_datetime: String,
    /// The file's `time` variable: acquisition time per trace, in seconds
    /// since the Unix epoch.
    pub time: Vec<f64>,
    /// The `anchor_name` attribute of the file's `twtt` variable, or `null`
    /// when it has none.
    #[cfg_attr(feature = "server", schema(required = true))]
    pub twtt_anchor: Option<String>,
    /// The file's `twtt_crop` variable: one value, one per trace, or empty
    /// when the file has none.
    pub twtt_crop: Vec<f64>,
    /// The file's `twtt_time_zero` variable, in the same way.
    pub twtt_time_zero: Vec<f64>,
    /// The spacing of the file's `twtt` variable in nanoseconds (its second
    /// value minus its first), or 0 when it has fewer than two.
    pub dt_ns: f64,
    /// The length of the file's `twtt` variable.
    pub n_samples: usize,
}

impl RevisionDeclarations {
    /// Read them from a processed file, which must be a current Ridal file:
    /// a legacy one has no radargram id to send.
    #[allow(
        dead_code,
        reason = "called by `ridal._preflight_body` in lib.rs, which the bin target \
                  does not build, and by tests"
    )]
    pub fn read(path: &Path) -> Result<Self, String> {
        let meta = match crate::io::inspect_ridal_netcdf(path)? {
            crate::io::RidalNetcdfKind::Supported(meta) => meta,
            crate::io::RidalNetcdfKind::Legacy(version) => {
                return Err(format!(
                    "{} was {}",
                    path.display(),
                    crate::io::legacy_reason(&version)
                ))
            }
            crate::io::RidalNetcdfKind::NotRidal => {
                return Err(format!(
                    "{} is a NetCDF file but not one Ridal processed.",
                    path.display()
                ))
            }
        };
        let declared = read_axis_declarations(path)?;
        Ok(Self {
            radargram_id: meta.radargram_id.to_string(),
            processing_datetime: meta.processing_datetime,
            time: declared.time,
            twtt_anchor: declared.twtt_anchor,
            twtt_crop: declared.twtt_crop,
            twtt_time_zero: declared.twtt_time_zero,
            dt_ns: declared.dt_ns,
            n_samples: declared.n_samples,
        })
    }

    #[cfg_attr(
        not(feature = "server"),
        allow(dead_code, reason = "called by the preflight route")
    )]
    pub fn into_axis_declarations(self) -> AxisDeclarations {
        AxisDeclarations {
            time: self.time,
            twtt_anchor: self.twtt_anchor,
            twtt_crop: self.twtt_crop,
            twtt_time_zero: self.twtt_time_zero,
            dt_ns: self.dt_ns,
            processing_datetime: Some(self.processing_datetime),
            n_samples: self.n_samples,
        }
    }
}

/// Read the axis declarations, or as much of them as the file carries.
///
/// Lenient about content, unlike [`read_geometry`]: this describes what a
/// radargram can offer, and a radargram that can offer nothing is a fact to
/// record rather than an error to raise. Its picks are still perfectly good
/// picks — they simply cannot be carried across a reprocess, which is the
/// state every interpretation was in before #146.
///
/// Not lenient about the file itself. One that cannot be opened is an
/// error, not a radargram that declares nothing: answering with defaults
/// made a transient failure (#129) look like a file without axes, and every
/// caller had to tell the two apart again, or did not.
pub fn read_axis_declarations(path: &Path) -> Result<AxisDeclarations, String> {
    let file = netcdf::open(path).map_err(|e| format!("could not open {path:?}: {e}"))?;
    let twtt = read_f64_variable(&file, "twtt").unwrap_or_default();
    Ok(AxisDeclarations {
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
        n_samples: twtt.len(),
    })
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

#[cfg(test)]
mod tests {
    #[test]
    #[serial_test::serial(netcdf)]
    fn revision_declarations_need_a_current_ridal_file() {
        // A legacy file has no radargram id to send, and an unrelated one no
        // identity at all; each says which it is rather than sending nothing.
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("old.nc");
        netcdf::create(&legacy)
            .unwrap()
            .add_attribute("program_version", "ridal version 0.3.0")
            .unwrap();
        let error = super::RevisionDeclarations::read(&legacy).unwrap_err();
        assert!(error.contains("Reprocess it"), "{error}");

        let other = dir.path().join("other.nc");
        netcdf::create(&other)
            .unwrap()
            .add_dimension("x", 1)
            .unwrap();
        let error = super::RevisionDeclarations::read(&other).unwrap_err();
        assert!(error.contains("not one Ridal processed"), "{error}");
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_file_that_cannot_be_opened_is_an_error_not_an_empty_declaration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.nc");
        std::fs::write(&path, b"not a netcdf file").unwrap();
        assert!(super::read_axis_declarations(&path).is_err());
        assert!(super::read_axis_declarations(&dir.path().join("absent.nc")).is_err());
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_readable_file_without_axes_still_declares_nothing() {
        // The leniency that stays: a file that opens but carries no axis
        // variables is a radargram that cannot offer a mapping, not an
        // error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bare.nc");
        netcdf::create(&path)
            .unwrap()
            .add_dimension("x", 3)
            .unwrap();
        let declared = super::read_axis_declarations(&path).unwrap();
        assert!(declared.time.is_empty());
        assert_eq!(declared.twtt_anchor, None);
        assert_eq!(declared.dt_ns, 0.0);
        assert_eq!(declared.n_samples, 0);
        assert_eq!(declared.processing_datetime, None);
    }
}
