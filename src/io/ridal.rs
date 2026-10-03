use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::export::ExportAttr;
use crate::gpr;

impl From<ExportAttr> for netcdf::AttributeValue {
    fn from(val: ExportAttr) -> Self {
        match val {
            ExportAttr::String(s) => netcdf::AttributeValue::Str(s),
            ExportAttr::Strings(s) => netcdf::AttributeValue::Strs(s),
            ExportAttr::F64(v) => netcdf::AttributeValue::Double(v),
            ExportAttr::F32(v) => netcdf::AttributeValue::Float(v),
            ExportAttr::U8(v) => netcdf::AttributeValue::Uchar(v),
            ExportAttr::I64(v) => netcdf::AttributeValue::Longlong(v),
        }
    }
}
/// Common functionality for writing NetCDF variables
fn write_nc_variable_common<T>(
    v: &mut netcdf::VariableMut,
    name: &str,
    data: &[T],
    attrs: Option<&BTreeMap<String, ExportAttr>>,
) -> Result<(), String>
where
    T: netcdf::NcTypeDescriptor,
{
    v.put_values(data, ..)
        .map_err(|e| format!("NetCDF export error when adding variable '{name}' data: {e}"))?;

    if let Some(attrs) = attrs {
        for (k, attr) in attrs {
            v.put_attribute(k, attr.to_owned()).map_err(|e| {
                format!("NetCDF export error when setting variable '{name}' attribute '{k}': {e}")
            })?;
        }
    };

    Ok(())
}

/// Add a variable without compression/chunking
fn add_nc_variable<T>(
    file: &mut netcdf::FileMut,
    name: &str,
    dims: &[&str],
    data: &[T],
    attrs: Option<&BTreeMap<String, ExportAttr>>,
) -> Result<(), String>
where
    T: netcdf::NcTypeDescriptor,
{
    let mut v = file
        .add_variable::<T>(name, dims)
        .map_err(|e| format!("NetCDF export error when adding variable '{name}': {e}"))?;

    write_nc_variable_common(&mut v, name, data, attrs)
}

/// Add a 2D variable with compression/chunking
fn add_nc_variable_compressed_2d<T>(
    file: &mut netcdf::FileMut,
    name: &str,
    dims: &[&str],
    data: &[T],
    shape: (usize, usize), // (ny, nx) in the same order as `dims`
    attrs: Option<&BTreeMap<String, ExportAttr>>,
) -> Result<(), String>
where
    T: netcdf::NcTypeDescriptor,
{
    let (ny, nx) = shape;

    let mut v = file
        .add_variable::<T>(name, dims)
        .map_err(|e| format!("NetCDF export error when adding variable '{name}': {e}"))?;

    v.set_compression(5, true)
        .map_err(|e| format!("NetCDF export error when setting '{name}' compression: {e}"))?;

    // 256 matches the web viewer's render chunk size (see #115 / #118), so a
    // render chunk decompresses exactly one HDF5 chunk instead of a fraction
    // of a larger one. Measured on a ~1 GB synthetic radargram: 5x lower
    // latency for a single chunk (8.4 -> 1.7 ms) and 3x for a full viewer
    // sweep (6.9 -> 2.2 s), for no change in file size -- radar amplitudes
    // compress ~1.17x regardless of chunk size, so there is no space/speed
    // tradeoff here to weigh against.
    for chunking in [256_usize, 128, 64, 32, 16, 8] {
        if ny < chunking || nx < chunking {
            continue;
        }
        v.set_chunking(&[chunking, chunking])
            .map_err(|e| format!("NetCDF export error when chunking '{name}': {e}"))?;
        break;
    }

    write_nc_variable_common(&mut v, name, data, attrs)?;

    Ok(())
}

/// Add an attribute to a NetCDF file
fn add_nc_attribute<T>(file: &mut netcdf::FileMut, name: &str, data: T) -> Result<(), String>
where
    T: Into<netcdf::AttributeValue>,
{
    file.add_attribute(name, data)
        .map_err(|e| format!("NetCDF export error when adding '{name}' attribute: {e}"))?;
    Ok(())
}

/// Export a GPR profile and its metadata to a NetCDF (".nc") file.
///
/// It will overwrite any file that already exists with the same filename.
///
/// # Arguments
/// - `gpr`: The GPR object to export
/// - `nc_filepath`: The filepath of the output NetCDF file
///
/// # Errors
/// - If the file already exists and cannot be removed.
/// - If a dimension, attribute or variable could not be created in the NetCDF file
/// - If data could not be written to the file
pub fn export_netcdf(
    ds: &crate::export::ExportDataset<'_>,
    nc_filepath: &Path,
) -> Result<(), String> {
    // Remove existing file (same reason as before)
    if nc_filepath.is_file() {
        std::fs::remove_file(nc_filepath).map_err(|e| {
            format!("NetCDF export error when removing old file with same name: {e}")
        })?;
    }

    // Create new file
    let mut file = netcdf::create(nc_filepath)
        .map_err(|e| format!("NetCDF export error when creating NetCDF file: {e}"))?;

    // ---- Dimensions ----
    for (name, len) in &ds.dims {
        file.add_dimension(name, *len)
            .map_err(|e| format!("NetCDF export error when adding dimension {name}: {e}"))?;
    }

    // ---- Global attributes from dataset ----
    for (k, v) in &ds.attrs {
        add_nc_attribute(&mut file, k, v.to_owned())?;
    }

    // ---- Coordinates (1D) ----
    for (name, var) in &ds.coords {
        match &var.data {
            crate::export::ExportArray::U32Owned1D(v) => {
                add_nc_variable::<u32>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    v,
                    Some(&var.attrs),
                )?;
            }
            crate::export::ExportArray::F32Owned1D(v) => {
                // collect unit attr if present
                add_nc_variable::<f32>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    v,
                    Some(&var.attrs),
                )?;
            }
            crate::export::ExportArray::F64Owned1D(v) => {
                add_nc_variable::<f64>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    v,
                    Some(&var.attrs),
                )?;
            }
            crate::export::ExportArray::U8Scalar(v) => {
                add_nc_variable::<u8>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    &[*v],
                    Some(&var.attrs),
                )?;
            }
            crate::export::ExportArray::F64Scalar(v) => {
                add_nc_variable::<f64>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    &[*v],
                    Some(&var.attrs),
                )?;
            }
            crate::export::ExportArray::F32Borrowed2D(_) => {
                // coords are expected to be 1D; ignore
                continue;
            }
        }
    }

    // ---- Data variables ----
    for (name, var) in &ds.data_vars {
        match &var.data {
            crate::export::ExportArray::F32Borrowed2D(arr2d) => {
                // Flatten and write compressed/chunked 2D
                let ny = ds.dims[var.dims[0].as_str()];
                let nx = ds.dims[var.dims[1].as_str()];
                let flat: Vec<f32> = arr2d.iter().copied().collect();

                add_nc_variable_compressed_2d::<f32>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    &flat,
                    (ny, nx),
                    Some(&var.attrs),
                )?;
            }
            crate::export::ExportArray::U8Scalar(v) => {
                add_nc_variable::<u8>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    &[*v],
                    Some(&var.attrs),
                )?;
            }
            crate::export::ExportArray::F64Scalar(v) => {
                add_nc_variable::<f64>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    &[*v],
                    Some(&var.attrs),
                )?;
            }
            crate::export::ExportArray::F64Owned1D(v) => {
                add_nc_variable::<f64>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    v,
                    Some(&var.attrs),
                )?;
            }
            // data variables are expected to be 2D here; ignore other shapes
            _ => continue,
        }
    }

    Ok(())
}

/// Export a "track" file.
///
/// It has its own associated function because the logic may happen in two different places in the
/// main() function.
///
/// # Arguments
/// - `gpr_locations`: The GPRLocation object to export
/// - `potential_track_path`: The output path of the track file or a directory (if provided)
/// - `output_filepath`: The output filepath to derive a track filepath from in case `potential_track_path` was not provided.
/// - `verbose`: Print progress?
///
/// # Returns
/// The exit code of the function
pub fn export_locations(
    gpr_locations: &gpr::GPRLocation,
    potential_track_path: Option<&PathBuf>,
    output_filepath: &Path,
    verbose: bool,
) -> Result<(), Box<dyn Error>> {
    // Determine the output filepath. If one was given, use that. If none was given, use the
    // parent and file stem + "_track.csv" of the output filepath. If a directory was given,
    // use the directory + the file stem of the output filepath + "_track.csv".
    let track_path: PathBuf = match potential_track_path {
        // Here is in case a filepath or directory was given
        Some(fp) => match fp.is_dir() {
            // In case the filepath points to a directory
            true => fp
                .join(
                    output_filepath
                        .file_stem()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string()
                        + "_track",
                )
                .with_extension("csv"),
            // In case it is not a directory (and thereby assumed to be a normal filepath)
            false => fp.clone(),
        },
        // Here is if no filepath was given
        None => output_filepath
            .with_file_name(
                output_filepath
                    .file_stem()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
                    + "_track",
            )
            .with_extension("csv"),
    };
    if verbose {
        println!("Exporting track to {:?}", track_path);
    };

    Ok(gpr_locations.to_csv(&track_path)?)
}

/// Result of inspecting a `.nc` candidate for Ridal recognition (#123).
///
/// A plain `is_ridal_nc() -> bool` is deliberately avoided: callers need
/// metadata for `Supported` files and need to distinguish an ordinary
/// non-Ridal NetCDF file (`NotRidal`) from an I/O or NetCDF-reading failure,
/// which `inspect_ridal_netcdf` reports as `Err` rather than as a variant
/// here.
///
/// Only consumed by catalog discovery under the `server` feature (#122);
/// the `cfg_attr` below reflects that honestly rather than blanket-allowing
/// dead code for CLI-only builds.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub enum RidalNetcdfKind {
    NotRidal,
    /// Written by a ridal old enough to predate radargram ids (#116): it
    /// has the pre-rename unprefixed `program_version` attribute but none
    /// of the `ridal_*` ones, so it structurally cannot supply a
    /// `RidalNetcdfMetadata`. Kept distinct from `NotRidal` so a caller can
    /// give a specific "reprocess it" answer instead of a generic "not a
    /// ridal file" one (#167). Carries the raw `program_version` string,
    /// which is always present -- it is what this variant is detected by.
    Legacy(String),
    Supported(RidalNetcdfMetadata),
}

/// The reason clause of a "this file needs reprocessing" message, shared by
/// every caller that reports a `RidalNetcdfKind::Legacy` result (upload,
/// replace, and catalog discovery) so the wording stays one decision instead
/// of three (#167). Callers prepend whatever names the file for them, e.g.
/// `format!("{name} was {}", legacy_reason(&version))`.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub fn legacy_reason(version: &str) -> String {
    format!(
        "processed by an old ridal ({version}), which predates radargram ids. \
         Reprocess it with a current `ridal process` first."
    )
}

/// Metadata read from a supported Ridal-produced NetCDF file, without
/// loading the amplitude array.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub struct RidalNetcdfMetadata {
    pub radargram_id: crate::identity::RadargramId,
    pub display_name: Option<crate::identity::DisplayName>,
    pub group_name: Option<crate::identity::GroupName>,
    pub group_id: Option<crate::identity::GroupId>,
    /// Kept as the raw RFC3339 string rather than parsed: the fingerprint in
    /// #117 hashes this exact string, so re-serializing a parsed value could
    /// silently change revision identity by changing formatting.
    pub processing_datetime: String,
    pub ridal_version: String,
    /// `(n_samples, n_traces)`, i.e. `(rows, columns)` of the `data` variable.
    pub shape: (usize, usize),
}

/// Read a single global attribute as a string, or `None` if absent or not a
/// string-valued attribute.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
fn read_global_str_attr(file: &netcdf::File, name: &str) -> Option<String> {
    let attr = file.attribute(name)?;
    match attr.value() {
        Ok(netcdf::AttributeValue::Str(s)) => Some(s),
        _ => None,
    }
}

/// Read a global attribute as a string, trying `primary` first and falling
/// back to `legacy` if absent. Supports files written before the
/// `ridal_*` attribute rename (#116); the value is otherwise identical.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
fn read_global_str(file: &netcdf::File, primary: &str, legacy: &str) -> Option<String> {
    read_global_str_attr(file, primary).or_else(|| read_global_str_attr(file, legacy))
}

/// Inspect `path` for Ridal recognition without loading the amplitude array.
///
/// Recognition requires `ridal_version`, `ridal_processing_datetime` and a
/// valid `ridal_radargram_id` to all be present; anything else is reported
/// as `NotRidal` rather than an error. No legacy-unprefixed-name fallback
/// is needed for these two: `ridal_radargram_id` became a mandatory,
/// always-written attribute in the exact same change that renamed
/// `program_version`/`processing_datetime` to their `ridal_` forms (#116),
/// so a file old enough to have the unprefixed names is always also old
/// enough to lack `ridal_radargram_id` -- and gets rejected on that check
/// regardless. (The group name/id legacy fallback below is a different
/// case: that split landed well after `ridal_radargram_id` was already
/// mandatory, so files genuinely exist with a valid id and only the old
/// unsplit `ridal_group` attribute.)
///
/// Errors are reserved for failures to open or read the file at all, kept
/// distinct from `NotRidal` so catalog discovery (#122) can report them
/// separately rather than silently skipping unreadable candidates.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub fn inspect_ridal_netcdf(path: &Path) -> Result<RidalNetcdfKind, String> {
    let file = netcdf::open(path).map_err(|e| format!("Failed to open {path:?} as NetCDF: {e}"))?;

    let ridal_version = read_global_str_attr(&file, "ridal_version");
    let processing_datetime = read_global_str_attr(&file, "ridal_processing_datetime");
    let (ridal_version, processing_datetime) = match (ridal_version, processing_datetime) {
        (Some(v), Some(d)) => (v, d),
        // No ridal_* attributes at all: either an unrelated NetCDF file, or
        // a ridal file old enough to predate the #116 rename (and, with it,
        // radargram ids). `program_version` is that rename's unprefixed
        // predecessor and was never written by anything else, so its
        // presence -- regardless of what else the file does or doesn't
        // have -- is a reliable "old ridal" signal (#167).
        _ => {
            return Ok(match read_global_str_attr(&file, "program_version") {
                Some(version) => RidalNetcdfKind::Legacy(version),
                None => RidalNetcdfKind::NotRidal,
            })
        }
    };

    let radargram_id = match read_global_str_attr(&file, "ridal_radargram_id") {
        Some(raw) => match crate::identity::RadargramId::new(&raw) {
            Ok(id) => id,
            Err(_) => return Ok(RidalNetcdfKind::NotRidal),
        },
        None => return Ok(RidalNetcdfKind::NotRidal),
    };

    let display_name = read_global_str_attr(&file, "ridal_display_name")
        .and_then(crate::identity::DisplayName::from_input);
    // Legacy fallback: files from before the group name/id split (#116
    // extended one level up) wrote a single "ridal_group" attribute that
    // was itself a validated slug, used directly as both display heading
    // and id. Reading it as the *name* here and deriving the id from it
    // below reproduces that value unchanged for such files, since
    // sanitizing an already-valid slug is a no-op.
    let group_name = read_global_str(&file, "ridal_group_name", "ridal_group")
        .and_then(crate::identity::GroupName::from_input);
    let group_id = read_global_str_attr(&file, "ridal_group_id")
        .and_then(|raw| crate::identity::GroupId::new(&raw).ok())
        .or_else(|| {
            group_name
                .as_ref()
                .and_then(|name| crate::identity::GroupId::from_fallback(name.as_str()).ok())
        });

    let Some(data_var) = file.variable("data") else {
        return Ok(RidalNetcdfKind::NotRidal);
    };
    let dims = data_var.dimensions();
    if dims.len() != 2 {
        return Ok(RidalNetcdfKind::NotRidal);
    }
    let shape = (dims[0].len(), dims[1].len());

    Ok(RidalNetcdfKind::Supported(RidalNetcdfMetadata {
        radargram_id,
        display_name,
        group_name,
        group_id,
        processing_datetime,
        ridal_version,
        shape,
    }))
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, str::FromStr};

    use crate::gpr;

    /// CI turns HDF5 file locking off (#129) as a second guard behind
    /// `tools::spawn_tool`, which is the fix on Unix. The workflow arranges
    /// it with an environment variable -- a mechanism nothing in the code
    /// points at, and therefore one that would quietly stop being true.
    ///
    /// Asserted rather than assumed. The first attempt at this fix put the
    /// variable in `.cargo/config.toml`, which is gitignored for the local
    /// gprinterp override; it worked locally, was never committed, and CI
    /// never saw it. This test is what said so, immediately, instead of the
    /// flake simply continuing.
    ///
    /// Only under CI, because that is where the setting lives. A developer
    /// on Linux or macOS does not need it.
    #[test]
    fn hdf5_file_locking_is_disabled_for_the_test_run() {
        if std::env::var("CI").is_err() {
            return;
        }
        assert_eq!(
            std::env::var("HDF5_USE_FILE_LOCKING").ok().as_deref(),
            Some("FALSE"),
            "CI must set HDF5_USE_FILE_LOCKING=FALSE (see the `env:` block in \
             .github/workflows/rust.yml); without it the NetCDF tests \
             intermittently fail to reopen a file they just wrote (#129)."
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // Added 2026-02-17 because gdal is hard to install in CI
    fn test_export_locations() {
        use super::export_locations;
        // Two positions in EPSG:4326, so this tests the export alone and does
        // not depend on any one format's parser: the first is the northern
        // hemisphere, the second the southern, which is what makes the sign
        // of the northing worth asserting.
        let locations = gpr::GPRLocation {
            cor_points: vec![
                gpr::CorPoint {
                    trace_n: 0,
                    time_seconds: 0.0,
                    easting: 16.0,
                    northing: 78.0,
                    altitude: 100.0,
                },
                gpr::CorPoint {
                    trace_n: 9,
                    time_seconds: 0.0,
                    easting: -16.0,
                    northing: -78.0,
                    altitude: 100.0,
                },
            ],
            correction: gpr::LocationCorrection::None,
            crs: "EPSG:4326".to_string(),
        };

        let temp_dir = tempfile::tempdir().unwrap();
        let out_dir = temp_dir.path().to_path_buf();
        let out_path = out_dir.join("track.csv");

        // The GPR filepath will be used in case no explicit filepath was given
        let dummy_gpr_output_path = out_dir.join("gpr.nc");
        let expected_default_path = out_dir.join("gpr_track.csv");

        for alternative in [
            Some(&out_path), // In case of a target filepath
            Some(&out_dir),  // In case of a target directory
            None,            // In case of a default name beside the GPR file
        ] {
            export_locations(&locations, alternative, &dummy_gpr_output_path, false).unwrap();

            let expected_path = match alternative {
                Some(p) if p == &out_path => &out_path,
                _ => &expected_default_path,
            };
            assert!(expected_path.is_file());

            let content = std::fs::read_to_string(expected_path)
                .unwrap()
                .split("\n")
                .map(|s| s.to_string())
                .collect::<Vec<String>>();

            assert_eq!(content[0], "trace_n,easting,northing,altitude");

            let line0: Vec<&str> = content[1].split(",").collect();

            assert_eq!(line0[0], "0");
            assert_eq!(line0[1], "16");
            assert_eq!(line0[2], "78");
            assert_eq!(line0[3], "100");

            let line1: Vec<&str> = content[2].split(",").collect();
            assert_eq!(line1[2], "-78");

            std::fs::remove_file(expected_path).unwrap();
        }
    }

    #[test]
    // #[ignore] // Added 2026-03-13 because it randomly fails sometimes. Unclear why
    // 2026-08-26: the "randomly fails" was very likely netcdf-c/HDF5 not being
    // thread-safe for concurrent open/create across tests -- see the
    // `#[serial_test::serial(netcdf)]` added here and on inspect_ridal_netcdf's
    // tests below, which introduced enough concurrent netcdf::create/open calls
    // to make the same underlying race reproduce on every run instead of
    // occasionally. The retry once kept here as a second line of defense
    // went once #324 fixed the subprocess half of that race.
    #[serial_test::serial(netcdf)]
    fn test_save_netcdf() {
        let mut gpr = crate::gpr::tests::make_dummy_gpr(100, 10, Some(1.));

        let mut gpr2 = crate::gpr::tests::make_dummy_gpr(100, 10, Some(1.));
        gpr2.metadata.data_filepath = PathBuf::from_str("other_filepath.rd3").unwrap();

        gpr.merge(&gpr2).unwrap();
        gpr.process("subset(0 50)").unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let nc_path = temp_dir.path().join("data.nc");

        gpr.export(&nc_path).unwrap();

        assert!(nc_path.is_file());

        let out = netcdf::open(&nc_path)
            .map_err(|e| format!("Error reading NetCDF: {e:?}"))
            .unwrap();

        let expected_attrs = vec![
            (
                "processing_steps",
                netcdf::AttributeValue::Strs(vec![
                    "subset(min_trace=0, max_trace=50, min_sample=0, max_sample=-1)".to_string(),
                ]),
            ),
            (
                "processing_log",
                netcdf::AttributeValue::Str(
                    "merge (duration: 0.00s):\tMerged \"other_filepath.rd3\"\nsubset (duration: 0.00s):\tSubset data from [10, 200] to (0:10, 0:50)"
                        .to_string(),
                ),
            ),
            ("total_distance", netcdf::AttributeValue::Double(49.)),
            (
                "original_filepaths",
                netcdf::AttributeValue::Strs(vec![
                    "filepath.rd3".to_string(),
                    "other_filepath.rd3".to_string(),
                ]),
            ),
        ];

        let grid_mapping = out.variable("projected_crs").unwrap();
        assert_eq!(
            grid_mapping
                .attribute("grid_mapping_name")
                .unwrap()
                .value()
                .unwrap(),
            netcdf::AttributeValue::Str("transverse_mercator".into())
        );
        assert_eq!(
            grid_mapping
                .attribute("false_easting")
                .unwrap()
                .value()
                .unwrap(),
            netcdf::AttributeValue::Double(500000.0)
        );

        // Load the data and check that it's identical
        let mut data = ndarray::Array2::<f32>::zeros((gpr.height(), gpr.width()));
        out.variable("data")
            .unwrap()
            .get_into(data.view_mut(), ..)
            .unwrap();
        assert_eq!((data - gpr.data).mapv(|v| v.abs()).sum(), 0.);

        for (key, expected) in expected_attrs {
            assert_eq!(
                out.attribute(key)
                    .ok_or(format!("Cannot find attribute {key}"))
                    .unwrap()
                    .value()
                    .unwrap(),
                expected
            );
        }
    }

    fn export_dummy_ridal_nc(path: &std::path::Path) {
        let gpr = crate::gpr::tests::make_dummy_gpr(20, 10, Some(1.));
        gpr.export(path).unwrap();
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn test_inspect_ridal_netcdf_supported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("supported.nc");
        export_dummy_ridal_nc(&path);

        match super::inspect_ridal_netcdf(&path).unwrap() {
            super::RidalNetcdfKind::Supported(meta) => {
                assert_eq!(meta.radargram_id.as_str(), "test-radargram");
                assert_eq!(meta.display_name, None);
                assert_eq!(meta.group_name, None);
                assert_eq!(meta.group_id, None);
                assert_eq!(meta.shape, (10, 20)); // (n_samples, n_traces)
                assert!(meta.ridal_version.contains("ridal version"));
            }
            other => panic!("expected Supported, got {other:?}"),
        }
    }

    // A user-supplied non-ASCII display or group name must survive the
    // write/read round trip byte for byte (#256). Ridal writes it as UTF-8
    // in an `NC_CHAR` attribute; its own reader must return exactly what
    // went in, even though readers that trust the HDF5 `H5T_CSET_ASCII`
    // flag (xarray with `h5netcdf`, `h5dump`) will garble it. Ridal must
    // neither transliterate nor reject it.
    #[test]
    #[serial_test::serial(netcdf)]
    fn test_unicode_display_and_group_names_survive_the_netcdf_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unicode.nc");

        let mut gpr = crate::gpr::tests::make_dummy_gpr(20, 10, Some(1.));
        gpr.identity.display_name =
            crate::identity::DisplayName::from_input("Drønbreen line 2 \u{1f6f7}");
        gpr.identity.group_name = crate::identity::GroupName::from_input("Ålesund / Ærø");
        gpr.export(&path).unwrap();

        match super::inspect_ridal_netcdf(&path).unwrap() {
            super::RidalNetcdfKind::Supported(meta) => {
                assert_eq!(
                    meta.display_name.map(|name| name.to_string()),
                    Some("Drønbreen line 2 \u{1f6f7}".to_string()),
                    "the display name must be written and read back unchanged"
                );
                assert_eq!(
                    meta.group_name.map(|name| name.to_string()),
                    Some("Ålesund / Ærø".to_string()),
                    "the group name must be written and read back unchanged"
                );
            }
            other => panic!("expected Supported, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn test_inspect_ridal_netcdf_unrelated_file_is_not_ridal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unrelated.nc");
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_dimension("x", 3).unwrap();
            let mut var = file.add_variable::<f32>("temperature", &["x"]).unwrap();
            var.put_values(&[1.0f32, 2.0, 3.0], ..).unwrap();
        }

        assert_eq!(
            super::inspect_ridal_netcdf(&path).unwrap(),
            super::RidalNetcdfKind::NotRidal
        );
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn test_inspect_ridal_netcdf_recognizes_unprefixed_legacy_attrs_as_legacy() {
        // Unprefixed processing_datetime/program_version, with no
        // ridal_processing_datetime/ridal_version at all: recognized as an
        // old ridal file rather than an arbitrary NetCDF file (#167), even
        // though a `ridal_radargram_id` is (artificially) present here too.
        // A real file never has both -- ridal_radargram_id became
        // mandatory in the exact same change that introduced the ridal_*
        // rename (#116) -- but the check only looks at the version
        // attributes, so this combination still resolves to `Legacy`
        // rather than `Supported`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.nc");
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_dimension("y", 2).unwrap();
            file.add_dimension("x", 2).unwrap();
            let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
            var.put_values(&[0.0f32, 0., 0., 0.], ..).unwrap();
            file.add_attribute("processing_datetime", "2020-01-01T00:00:00Z")
                .unwrap();
            file.add_attribute("program_version", "ridal version 0.1.0 by test")
                .unwrap();
            file.add_attribute("ridal_radargram_id", "legacy-radargram")
                .unwrap();
        }

        assert_eq!(
            super::inspect_ridal_netcdf(&path).unwrap(),
            super::RidalNetcdfKind::Legacy("ridal version 0.1.0 by test".to_string())
        );
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn test_inspect_ridal_netcdf_missing_radargram_id_is_not_ridal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_id.nc");
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_dimension("y", 2).unwrap();
            file.add_dimension("x", 2).unwrap();
            let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
            var.put_values(&[0.0f32, 0., 0., 0.], ..).unwrap();
            file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
                .unwrap();
            file.add_attribute("ridal_version", "ridal version 0.1.0 by test")
                .unwrap();
            // Deliberately no ridal_radargram_id.
        }

        assert_eq!(
            super::inspect_ridal_netcdf(&path).unwrap(),
            super::RidalNetcdfKind::NotRidal
        );
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn test_inspect_ridal_netcdf_malformed_id_is_not_ridal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_id.nc");
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_dimension("y", 2).unwrap();
            file.add_dimension("x", 2).unwrap();
            let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
            var.put_values(&[0.0f32, 0., 0., 0.], ..).unwrap();
            file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
                .unwrap();
            file.add_attribute("ridal_version", "ridal version 0.1.0 by test")
                .unwrap();
            file.add_attribute("ridal_radargram_id", "Not A Valid ID!")
                .unwrap();
        }

        assert_eq!(
            super::inspect_ridal_netcdf(&path).unwrap(),
            super::RidalNetcdfKind::NotRidal
        );
    }

    #[test]
    fn test_legacy_reason_names_the_version() {
        assert_eq!(
            super::legacy_reason("ridal version 0.5.1 by test"),
            "processed by an old ridal (ridal version 0.5.1 by test), which predates \
             radargram ids. Reprocess it with a current `ridal process` first."
        );
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn test_inspect_ridal_netcdf_unreadable_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.nc");
        std::fs::write(&path, b"this is not a netcdf file").unwrap();

        let result = super::inspect_ridal_netcdf(&path);
        assert!(result.is_err(), "expected an error, got {result:?}");
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn test_inspect_ridal_netcdf_nonexistent_file_is_an_error() {
        let result = super::inspect_ridal_netcdf(std::path::Path::new("/no/such/file.nc"));
        assert!(result.is_err());
    }
}
