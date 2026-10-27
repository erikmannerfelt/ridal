//! Finding and reading the raw recordings and their positions.

use std::error::Error;
use std::path::{Path, PathBuf};

use crate::formats::{self, FormatKind, ResolvedInput};
use crate::gpr::{GPRLocation, GPRMeta};
use crate::io;

fn has_glob_chars(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.contains('*') || text.contains('?') || text.contains('[')
}

pub fn expand_input_paths(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut expanded = Vec::<PathBuf>::new();
    for input in inputs {
        if has_glob_chars(input) {
            let mut matches = glob::glob(&input.to_string_lossy())
                .map_err(|e| format!("Invalid glob pattern {:?}: {e}", input))?
                .filter_map(Result::ok)
                .collect::<Vec<PathBuf>>();
            matches.sort();
            if matches.is_empty() {
                return Err(format!("Glob pattern matched no files: {:?}", input));
            }
            expanded.extend(matches);
        } else {
            expanded.push(input.clone());
        }
    }
    if expanded.is_empty() {
        return Err("No input files were provided.".to_string());
    }
    Ok(expanded)
}

/// A recording's header and its positions, read from the positions file
/// `resolved` names: the format's own, or a Ridal coordinate CSV for any
/// format (#13). Apply `--cor` with [`formats::with_coordinates`] first.
pub(crate) fn load_meta_and_location(
    resolved: &ResolvedInput,
    medium_velocity: f32,
    crs: Option<&String>,
    dem_path: Option<&PathBuf>,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
    override_date: Option<chrono::NaiveDate>,
) -> Result<(GPRMeta, GPRLocation), Box<dyn Error>> {
    let gpr_meta = load_meta(
        resolved,
        medium_velocity,
        override_antenna_mhz,
        override_antenna_separation,
        override_date,
    )?;

    let positions = &resolved.coordinates;
    if !positions.is_file() {
        return Err(format!(
            "No positions for {}: {} does not exist. Ridal needs a position for \
             every trace. Give another positions file with --cor (cor= in \
             Python): a {} or a Ridal coordinate CSV of trace, time, lon, \
             lat and alt, or put the CSV beside the recording as \
             <stem>{}.",
            resolved.header.display(),
            positions.display(),
            formats::format_info(resolved.kind).files.coordinates,
            formats::COORDS_CSV_SUFFIX,
        )
        .into());
    }
    let mut gpr_location = load_positions(resolved, positions, crs, override_date)?;

    if let Some(dem_path) = dem_path {
        gpr_location.get_dem_elevations(dem_path)?;
    } else if gpr_location
        .cor_points
        .iter()
        .any(|point| !point.altitude.is_finite())
    {
        return Err(format!(
            "{} gives no altitude. Add an alt column, or give a DEM with --dem \
             (dem= in Python) to take the elevations from.",
            positions.display()
        )
        .into());
    }

    Ok((gpr_meta, gpr_location))
}

/// A recording's header alone.
pub(crate) fn load_meta(
    resolved: &ResolvedInput,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
    override_date: Option<chrono::NaiveDate>,
) -> Result<GPRMeta, Box<dyn Error>> {
    if !resolved.header.is_file() {
        return Err(format!("File not found: {:?}", resolved.header).into());
    }
    Ok(match resolved.kind {
        FormatKind::Ramac => io::load_rad(
            &resolved.header,
            medium_velocity,
            override_antenna_mhz,
            override_antenna_separation,
        )?,
        FormatKind::PulseEkko => io::load_pe_hd(
            &resolved.header,
            medium_velocity,
            override_antenna_mhz,
            override_antenna_separation,
        )?,
        FormatKind::Gssi => io::load_gssi_dzt(
            &resolved.header,
            medium_velocity,
            override_antenna_mhz,
            override_antenna_separation,
        )?,
        FormatKind::IceRadar => io::load_iceradar_meta(
            &resolved.header,
            iceradar_line(resolved)?,
            medium_velocity,
            override_antenna_mhz,
            override_antenna_separation,
            override_date,
        )?,
    })
}

/// The line of an IceRadar recording to read: the one [`formats::with_lines`]
/// chose, or the only one. A file of several lines that was not expanded
/// fails with the list of them.
pub(crate) fn iceradar_line(resolved: &ResolvedInput) -> Result<u32, Box<dyn Error>> {
    if let Some(line) = resolved.part.line {
        return Ok(line);
    }
    // Refuses a file of several lines, listing them.
    formats::with_lines(vec![resolved.clone()], None)?;
    Ok(io::iceradar_lines(&resolved.header)?[0].0)
}

/// A whole radargram: the header, the positions and the data, of one
/// recording or of one line of it.
pub(crate) fn load_gpr(
    resolved: &ResolvedInput,
    meta: GPRMeta,
    location: GPRLocation,
    override_date: Option<chrono::NaiveDate>,
) -> Result<crate::gpr::GPR, Box<dyn Error>> {
    let gpr = match resolved.kind {
        FormatKind::IceRadar => {
            let data =
                io::load_iceradar_data(&resolved.header, iceradar_line(resolved)?, override_date)?;
            crate::gpr::GPR::from_meta_loc_and_data(location, meta, data)
        }
        _ => crate::gpr::GPR::from_meta_and_loc(location, meta),
    };
    gpr.map_err(|e| format!("Error loading GPR data from {:?}: {:?}", resolved.data, e).into())
}

/// The positions in `positions`, for the recording `resolved`: a Ridal
/// coordinate CSV, or the positions file of the recording's format.
pub(crate) fn load_positions(
    resolved: &ResolvedInput,
    positions: &Path,
    crs: Option<&String>,
    override_date: Option<chrono::NaiveDate>,
) -> Result<GPRLocation, Box<dyn Error>> {
    // Only these record a time of day without a trustworthy date.
    let dated_by_override = !formats::is_coords_csv(positions)
        && matches!(resolved.kind, FormatKind::IceRadar | FormatKind::PulseEkko);
    if override_date.is_some() && !dated_by_override {
        return Err(format!(
            "--override-date (override_date= in Python) applies to positions read from IceRadar \
             and pulseEKKO files; \
             {} gives its own dates",
            positions.display()
        )
        .into());
    }
    if formats::is_coords_csv(positions) {
        return io::load_coords_csv(positions, crs);
    }
    if resolved.kind == FormatKind::IceRadar && positions == resolved.header {
        return io::load_iceradar_positions(
            &resolved.header,
            iceradar_line(resolved)?,
            override_date,
            crs,
        );
    }
    let native = formats::format_info(resolved.kind).files.coordinates;
    let is_native = positions
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(native.trim_start_matches('.')));
    if !is_native {
        return Err(format!(
            "{} cannot hold the positions of {}: a {} recording's positions \
             are a {native} or a Ridal coordinate CSV (.csv).",
            positions.display(),
            resolved.header.display(),
            formats::format_info(resolved.kind).name,
        )
        .into());
    }
    match resolved.kind {
        FormatKind::Ramac => io::load_cor(positions, crs),
        FormatKind::PulseEkko => io::load_pe_gp2(positions, crs, override_date),
        FormatKind::Gssi => io::load_gssi_dzg(positions, crs),
        FormatKind::IceRadar => Err(format!(
            "{} cannot hold the positions of {}: an IceRadar recording's positions are in \
             the file itself, or in a Ridal coordinate CSV (.csv).",
            positions.display(),
            resolved.header.display(),
        )
        .into()),
    }
}

/// Whether `path` is a NetCDF file, which `info` reads as Ridal output
/// rather than as a raw recording.
pub(crate) fn is_netcdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nc"))
}

pub(crate) fn expand_cli_inputs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::<PathBuf>::new();

    for input in inputs {
        let text = input.to_string_lossy();
        let looks_like_glob = text.contains('*') || text.contains('?') || text.contains('[');

        if looks_like_glob {
            let mut matched_any = false;
            let pattern =
                glob::glob(&text).map_err(|e| format!("Invalid glob pattern '{}': {e}", text))?;
            for entry in pattern {
                let path = entry.map_err(|e| format!("Error expanding glob '{}': {e}", text))?;
                matched_any = true;
                out.push(path);
            }
            if !matched_any {
                return Err(format!("Glob did not match any files: {text}"));
            }
        } else {
            out.push(input.clone());
        }
    }

    if out.is_empty() {
        return Err("No input files were provided".to_string());
    }

    Ok(out)
}
