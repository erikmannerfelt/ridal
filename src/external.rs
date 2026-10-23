//! A radargram built from arrays instead of read from raw files.
//!
//! This is `ridal.write_netcdf` in Python: data processed in another tool
//! (ImpDAR, a script) given the same NetCDF layout `ridal process` writes,
//! through the same export, so the layout has one owner. Nothing here is
//! specific to any tool; `ridal.impdar` maps ImpDAR's objects onto it.
//!
//! The file records what the caller says about the vertical axis and
//! nothing more. In particular a time zero that has never been located is
//! written as the `0` sentinel, never guessed, so the travel-time and
//! recording-clock anchors that carrying picks relies on mean what they
//! mean for any other file.

use std::path::PathBuf;

use ndarray::Array2;

use crate::gpr::{CorPoint, GPRLocation, GPRMeta, LocationCorrection, RidalIdentity, GPR};

/// How many offending trace indices an error lists before eliding.
const LISTED_INDICES: usize = 10;

/// Trace positions, in exactly one coordinate system.
#[derive(Debug, Clone)]
pub enum Position {
    /// WGS84 longitude and latitude (degrees). Projected into `crs`, or
    /// into the best UTM zone for the first trace, as raw readers do.
    LonLat {
        longitude: Vec<f64>,
        latitude: Vec<f64>,
        crs: Option<String>,
    },
    /// Easting and northing (m) in `crs`.
    Projected {
        x: Vec<f64>,
        y: Vec<f64>,
        crs: String,
    },
}

/// Where each trace's elevation comes from.
#[derive(Debug, Clone)]
pub enum Elevation {
    /// One elevation per trace (m).
    Values(Vec<f64>),
    /// A DEM to sample at each trace, as `ridal process --dem` does.
    Dem(PathBuf),
}

/// Everything `ridal.write_netcdf` takes, before validation.
#[derive(Debug, Clone)]
pub struct Arrays {
    /// Amplitudes, `(samples, traces)`.
    pub data: Array2<f32>,
    /// Time of each sample (ns), evenly spaced: travel time from time zero
    /// when `time_zero_ns` is given, time since the instrument started
    /// sampling when it is not.
    pub twtt_ns: Vec<f64>,
    /// Where time zero sits on the recording clock (ns), one value or one
    /// per trace; `None` when it has never been located.
    pub time_zero_ns: Option<Vec<f64>>,
    /// Time of each trace, seconds since the Unix epoch.
    pub trace_time: Vec<f64>,
    pub position: Position,
    pub elevation: Elevation,
    /// The antenna's name; defaults to its frequency.
    pub antenna: Option<String>,
    /// Antenna centre frequency (MHz).
    pub antenna_mhz: f32,
    /// Transmitter-receiver separation at acquisition (m).
    pub antenna_separation: f32,
    /// The separation the data has not been corrected for (m): the
    /// acquisition separation, or `0` after an NMO-type correction.
    pub antenna_separation_effective: Option<f32>,
    /// Radar wave velocity in the medium (m/ns).
    pub medium_velocity: f32,
    /// Free-text provenance, one line each, ahead of Ridal's own line.
    pub log: Vec<String>,
    /// The file the data was made from, for `original_filepaths`.
    pub source_file: Option<String>,
}

/// Why [`build`] refused, split by whose problem it is.
#[derive(Debug)]
pub enum BuildError {
    /// An argument is wrong; the message says which and why.
    Argument(String),
    /// The DEM could not be sampled.
    Dem(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Argument(e) | BuildError::Dem(e) => f.write_str(e),
        }
    }
}

/// Build a radargram from `arrays`, ready to export.
///
/// Every argument is checked before anything is computed, and an error
/// says which argument and, for per-trace values, which traces.
///
/// # Errors
/// An argument is the wrong length, not finite, out of range, or
/// inconsistent with another; a coordinate system cannot be read; or the
/// DEM cannot be sampled.
pub fn build(arrays: Arrays, identity: RidalIdentity) -> Result<GPR, BuildError> {
    let elevation = arrays.elevation.clone();
    let mut gpr = build_from(arrays).map_err(BuildError::Argument)?;
    if let Elevation::Dem(path) = &elevation {
        gpr.location
            .get_dem_elevations(path)
            .map_err(BuildError::Dem)?;
    }
    gpr.identity = identity;
    Ok(gpr)
}

/// [`build`], short of sampling a DEM.
fn build_from(arrays: Arrays) -> Result<GPR, String> {
    let Arrays {
        data,
        twtt_ns,
        time_zero_ns,
        trace_time,
        position,
        elevation,
        antenna,
        antenna_mhz,
        antenna_separation,
        antenna_separation_effective,
        medium_velocity,
        log,
        source_file,
    } = arrays;

    let (n_samples, n_traces) = data.dim();
    if n_samples < 2 || n_traces < 1 {
        return Err(format!(
            "data must have at least 2 samples and 1 trace; its shape is \
             ({n_samples}, {n_traces}) as (samples, traces)"
        ));
    }

    let dt_ns = sample_interval(&twtt_ns, n_samples)?;
    let (crop_ns, time_zero_ns) = vertical_offsets(&twtt_ns, time_zero_ns.as_deref(), n_traces)?;

    check_per_trace("trace_time", &trace_time, n_traces)?;

    if !(antenna_mhz.is_finite() && antenna_mhz > 0.) {
        return Err(format!(
            "antenna_mhz must be positive and finite, not {antenna_mhz}"
        ));
    }
    if !(antenna_separation.is_finite() && antenna_separation >= 0.) {
        return Err(format!(
            "antenna_separation must be zero or positive and finite, not {antenna_separation}"
        ));
    }
    // Ridal knows two states of the geometry, not a continuum: still to be
    // corrected for, or corrected (`GPR::twtt_anchor_name`). A value in
    // between would be exported and then read as uncorrected.
    let antenna_separation_effective = antenna_separation_effective.unwrap_or(antenna_separation);
    if antenna_separation_effective != antenna_separation && antenna_separation_effective != 0. {
        return Err(format!(
            "antenna_separation_effective must be antenna_separation ({antenna_separation}, \
             not yet corrected for) or 0 (corrected, e.g. by NMO), not \
             {antenna_separation_effective}"
        ));
    }
    if !(medium_velocity.is_finite() && medium_velocity > 0. && medium_velocity <= 0.3) {
        return Err(format!(
            "medium_velocity must be in (0, 0.3] m/ns, not {medium_velocity}"
        ));
    }

    let (eastings, northings, crs) = project(position, n_traces)?;
    let altitudes = match &elevation {
        Elevation::Values(values) => {
            check_per_trace("elevation", values, n_traces)?;
            values.clone()
        }
        // Sampled by `build`, once the location exists.
        Elevation::Dem(_) => vec![0.; n_traces],
    };

    let cor_points = (0..n_traces)
        .map(|i| CorPoint {
            trace_n: i as u32,
            time_seconds: trace_time[i],
            easting: eastings[i],
            northing: northings[i],
            altitude: altitudes[i],
        })
        .collect();
    let location = GPRLocation {
        cor_points,
        correction: LocationCorrection::None,
        time_source: crate::gpr::TimeSource::Recorded,
        crs,
    };

    let time_interval = if n_traces > 1 {
        ((trace_time[n_traces - 1] - trace_time[0]) / (n_traces - 1) as f64) as f32
    } else {
        // Exported only when finite: one trace has no interval.
        f32::NAN
    };
    let metadata = GPRMeta {
        samples: n_samples as u32,
        frequency: (1000. / dt_ns) as f32,
        frequency_steps: 0,
        time_interval,
        antenna: antenna.unwrap_or_else(|| format!("{antenna_mhz} MHz")),
        antenna_mhz,
        antenna_separation,
        time_window: (dt_ns * n_samples as f64) as f32,
        last_trace: n_traces as u32,
        data_filepath: PathBuf::from(source_file.unwrap_or_default()),
        medium_velocity,
    };

    let mut log = log;
    log.push(format!(
        "write_netcdf:\tBuilt from arrays given to ridal.write_netcdf: {n_samples} samples \
         x {n_traces} traces at {dt_ns} ns, time zero {}",
        if time_zero_ns.iter().all(|&zero| zero == 0.) {
            "not located".to_string()
        } else {
            format!("at {} ns on the recording clock", time_zero_ns[0])
        }
    ));

    Ok(GPR::from_external(
        data,
        location,
        metadata,
        antenna_separation_effective,
        crop_ns,
        time_zero_ns,
        log,
    ))
}

/// The spacing of `twtt_ns` (ns), checked to be even and increasing.
fn sample_interval(twtt_ns: &[f64], n_samples: usize) -> Result<f64, String> {
    if twtt_ns.len() != n_samples {
        return Err(format!(
            "twtt_ns has {} values, but data has {n_samples} samples",
            twtt_ns.len()
        ));
    }
    let bad = non_finite(twtt_ns);
    if !bad.is_empty() {
        return Err(format!(
            "twtt_ns is not finite at samples {}",
            list_indices(&bad)
        ));
    }
    let first = twtt_ns[0];
    let dt = (twtt_ns[n_samples - 1] - first) / (n_samples - 1) as f64;
    // Every value is finite (checked above), so `dt` is too.
    if dt <= 0. {
        return Err("twtt_ns must increase".into());
    }
    // A thousandth of a sample covers what float arithmetic leaves on an
    // axis built as `start + i * dt` (or by `arange`), and nothing that is
    // really uneven: the exported axis is regular, so anything else would
    // be written at times it does not have.
    let uneven: Vec<usize> = (0..n_samples)
        .filter(|&i| (twtt_ns[i] - (first + i as f64 * dt)).abs() > dt * 1e-3)
        .collect();
    if !uneven.is_empty() {
        return Err(format!(
            "twtt_ns must be evenly spaced ({dt} ns on average), but is not at samples {}",
            list_indices(&uneven)
        ));
    }
    Ok(dt)
}

/// `crop_ns` and `time_zero_ns` per trace, as `GPR` stores them, from the
/// caller's travel-time axis and time zero.
///
/// The crop is derived rather than taken, so `twtt[0] == crop - time_zero`
/// holds by construction: that equation is how every reader turns the
/// pair into the travel time of sample 0.
fn vertical_offsets(
    twtt_ns: &[f64],
    time_zero_ns: Option<&[f64]>,
    n_traces: usize,
) -> Result<(Vec<f32>, Vec<f32>), String> {
    let first = twtt_ns[0];
    let Some(time_zero) = time_zero_ns else {
        // Never located: the axis is the recording clock itself, so its
        // first value is where the first sample sits on it.
        if first < 0. {
            return Err(format!(
                "twtt_ns starts at {first} ns, before the recording started, but \
                 time_zero_ns is None. Pass where time zero is if twtt_ns is \
                 travel time from it."
            ));
        }
        return Ok((vec![first as f32; n_traces], vec![0.; n_traces]));
    };

    let time_zero = match time_zero.len() {
        1 => vec![time_zero[0]; n_traces],
        n if n == n_traces => time_zero.to_vec(),
        n => {
            return Err(format!(
                "time_zero_ns has {n} values; give one, or one per trace ({n_traces})"
            ))
        }
    };
    // Zero is the sentinel for "never located" (`GPR::twtt_time_zero_ns`),
    // so a located time zero exactly there could not be told apart from
    // an unknown one. Negative is before the recording started.
    let bad: Vec<usize> = (0..n_traces)
        .filter(|&i| !(time_zero[i].is_finite() && time_zero[i] > 0.))
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "time_zero_ns must be positive and finite (0 means never located; pass None \
             for that), but is not at traces {}",
            list_indices(&bad)
        ));
    }
    let crop: Vec<f64> = time_zero.iter().map(|zero| first + zero).collect();
    let before_start: Vec<usize> = (0..n_traces).filter(|&i| crop[i] < 0.).collect();
    if !before_start.is_empty() {
        return Err(format!(
            "twtt_ns starts at {first} ns, which with time_zero_ns puts the first \
             sample before the recording started at traces {}",
            list_indices(&before_start)
        ));
    }
    Ok((
        crop.iter().map(|&v| v as f32).collect(),
        time_zero.iter().map(|&v| v as f32).collect(),
    ))
}

/// Eastings, northings and the CRS they are in.
fn project(position: Position, n_traces: usize) -> Result<(Vec<f64>, Vec<f64>, String), String> {
    match position {
        Position::LonLat {
            longitude,
            latitude,
            crs,
        } => {
            check_per_trace("longitude", &longitude, n_traces)?;
            check_per_trace("latitude", &latitude, n_traces)?;
            let outside: Vec<usize> = (0..n_traces)
                .filter(|&i| longitude[i].abs() > 180. || latitude[i].abs() > 90.)
                .collect();
            if !outside.is_empty() {
                return Err(format!(
                    "longitude/latitude are outside [-180, 180]/[-90, 90] at traces {}. \
                     Projected coordinates go in x and y, with crs.",
                    list_indices(&outside)
                ));
            }
            let wgs84: Vec<crate::coords::Coord> = longitude
                .iter()
                .zip(&latitude)
                .map(|(&x, &y)| crate::coords::Coord { x, y })
                .collect();
            let crs = match crs {
                Some(crs) => crs,
                None => crate::coords::UtmCrs::optimal_crs(&wgs84[0]).to_epsg_str(),
            };
            let projected =
                crate::coords::from_wgs84(&wgs84, &crate::coords::Crs::from_user_input(&crs)?)?;
            Ok((
                projected.iter().map(|c| c.x).collect(),
                projected.iter().map(|c| c.y).collect(),
                crs,
            ))
        }
        Position::Projected { x, y, crs } => {
            check_per_trace("x", &x, n_traces)?;
            check_per_trace("y", &y, n_traces)?;
            // Read here so a CRS that cannot be used fails now, not at the
            // first thing that converts to longitude and latitude.
            crate::coords::Crs::from_user_input(&crs)?;
            Ok((x, y, crs))
        }
    }
}

/// One finite value per trace.
fn check_per_trace(name: &str, values: &[f64], n_traces: usize) -> Result<(), String> {
    if values.len() != n_traces {
        return Err(format!(
            "{name} has {} values, but data has {n_traces} traces",
            values.len()
        ));
    }
    let bad = non_finite(values);
    if !bad.is_empty() {
        return Err(format!(
            "{name} is not finite at traces {}. Every trace needs one; fill gaps \
             (e.g. by interpolation) before writing.",
            list_indices(&bad)
        ));
    }
    Ok(())
}

fn non_finite(values: &[f64]) -> Vec<usize> {
    (0..values.len())
        .filter(|&i| !values[i].is_finite())
        .collect()
}

/// `0, 4, 7` or `0, 1, ... (25 in total)`.
fn list_indices(indices: &[usize]) -> String {
    let shown = indices
        .iter()
        .take(LISTED_INDICES)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if indices.len() > LISTED_INDICES {
        format!("{shown}, ... ({} in total)", indices.len())
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N_SAMPLES: usize = 50;
    const N_TRACES: usize = 20;
    const DT: f64 = 0.8;

    /// A small radargram along a line near Drønbreen, time zero located.
    fn arrays() -> Arrays {
        Arrays {
            data: Array2::from_shape_fn((N_SAMPLES, N_TRACES), |(i, j)| (i * j) as f32),
            twtt_ns: (0..N_SAMPLES).map(|i| i as f64 * DT).collect(),
            time_zero_ns: Some(vec![40.]),
            trace_time: (0..N_TRACES).map(|j| 1.7e9 + j as f64).collect(),
            position: Position::LonLat {
                longitude: (0..N_TRACES).map(|j| 16.9 + j as f64 * 1e-5).collect(),
                latitude: vec![78.2; N_TRACES],
                crs: None,
            },
            elevation: Elevation::Values(vec![500.; N_TRACES]),
            antenna: None,
            antenna_mhz: 100.,
            antenna_separation: 1.,
            antenna_separation_effective: None,
            medium_velocity: 0.168,
            log: vec!["Elsewhere: bandpass".into()],
            source_file: Some("line.mat".into()),
        }
    }

    fn identity() -> RidalIdentity {
        RidalIdentity {
            radargram_id: Some(crate::identity::RadargramId::new("line-1").unwrap()),
            ..Default::default()
        }
    }

    fn error(arrays: Arrays) -> String {
        match build(arrays, identity()) {
            Err(BuildError::Argument(e)) => e,
            Err(BuildError::Dem(e)) => panic!("expected an argument error, got {e}"),
            Ok(_) => panic!("expected an error"),
        }
    }

    fn export(arrays: Arrays) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line-1.nc");
        build(arrays, identity()).unwrap().export(&path).unwrap();
        (dir, path)
    }

    #[test]
    fn the_exported_axis_is_the_one_given() {
        let mut given = arrays();
        // A margin above time zero: travel times start below zero.
        given.twtt_ns = (0..N_SAMPLES).map(|i| -4.0 + i as f64 * DT).collect();
        let (_dir, path) = export(given.clone());

        let geometry = crate::interp::source::read_geometry(&path).unwrap();
        for (written, expected) in geometry.twtt.iter().zip(&given.twtt_ns) {
            assert!((written - expected).abs() < 1e-4, "{written} != {expected}");
        }
        let declared = crate::interp::source::read_axis_declarations(&path).unwrap();
        assert_eq!(declared.twtt_time_zero, vec![40.]);
        assert_eq!(declared.twtt_crop, vec![36.]);
        assert_eq!(declared.time, given.trace_time);
    }

    #[test]
    fn a_located_time_zero_offers_both_anchors() {
        let (_dir, path) = export(arrays());
        let declared = crate::interp::source::read_axis_declarations(&path).unwrap();
        let axes = crate::interp::anchors::axes_from_declarations(&declared);
        assert!(axes.is_usable());
        let names: Vec<&str> = axes
            .y
            .as_ref()
            .unwrap()
            .anchor
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["twtt", "recording_time"]);
        // The recording clock starts where the first sample sits on it.
        assert_eq!(axes.y.unwrap().anchor[1].t0, Some(40.));
    }

    #[test]
    fn an_unknown_time_zero_is_the_sentinel_and_offers_only_the_recording_clock() {
        let mut given = arrays();
        given.time_zero_ns = None;
        given.twtt_ns = (0..N_SAMPLES).map(|i| 12.0 + i as f64 * DT).collect();
        let (_dir, path) = export(given);
        let declared = crate::interp::source::read_axis_declarations(&path).unwrap();
        assert_eq!(declared.twtt_time_zero, vec![0.]);
        assert_eq!(declared.twtt_crop, vec![12.]);
        let axes = crate::interp::anchors::axes_from_declarations(&declared);
        let names: Vec<&str> = axes
            .y
            .as_ref()
            .unwrap()
            .anchor
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["recording_time"]);
    }

    /// An external NMO correction records none of the parameters Ridal's
    /// own correction does, so the recording clock cannot be reconstructed
    /// and is not offered; picks still carry by normal-incidence travel
    /// time between two such files.
    #[test]
    fn an_external_nmo_offers_normal_incidence_travel_time_only() {
        let mut given = arrays();
        given.antenna_separation_effective = Some(0.);
        let (_dir, path) = export(given);
        let declared = crate::interp::source::read_axis_declarations(&path).unwrap();
        let axes = crate::interp::anchors::axes_from_declarations(&declared);
        let names: Vec<&str> = axes
            .y
            .as_ref()
            .unwrap()
            .anchor
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["twtt_normal_incidence"]);
    }

    #[test]
    fn the_file_is_a_revision_without_a_recipe() {
        let mut given = arrays();
        given.medium_velocity = 0.17;
        let (_dir, path) = export(given);
        let crate::io::RidalNetcdfKind::Supported(meta) =
            crate::io::inspect_ridal_netcdf(&path).unwrap()
        else {
            panic!("not read as a Ridal file");
        };
        assert_eq!(meta.radargram_id.to_string(), "line-1");
        assert_eq!(meta.recipe_hash, None);
        assert_eq!(meta.shape, (N_SAMPLES, N_TRACES));
        let file = netcdf::open(&path).unwrap();
        let log = match file.attribute("processing_log").unwrap().value().unwrap() {
            netcdf::AttributeValue::Str(s) => s,
            other => panic!("{other:?}"),
        };
        assert!(
            log.starts_with("Elsewhere: bandpass\nwrite_netcdf:"),
            "{log}"
        );
        let velocity = match file.attribute("medium_velocity").unwrap().value().unwrap() {
            netcdf::AttributeValue::Float(v) => v,
            other => panic!("{other:?}"),
        };
        assert_eq!(velocity, 0.17);
    }

    #[test]
    fn projected_coordinates_are_kept_as_given() {
        let mut given = arrays();
        given.position = Position::Projected {
            x: (0..N_TRACES).map(|j| 500_000. + j as f64).collect(),
            y: vec![8_680_000.; N_TRACES],
            crs: "EPSG:32633".into(),
        };
        let gpr = build(given, identity()).unwrap();
        assert_eq!(gpr.location.crs, "EPSG:32633");
        assert_eq!(gpr.location.cor_points[3].easting, 500_003.);
    }

    #[test]
    fn longitude_and_latitude_are_projected_to_the_best_utm_zone() {
        let gpr = build(arrays(), identity()).unwrap();
        assert_eq!(gpr.location.crs, "EPSG:32633");
        let first = &gpr.location.cor_points[0];
        // `cs2cs EPSG:4326 EPSG:32633` puts 78.2 N, 16.9 E at 543367.10, 8681393.78.
        assert!(
            (first.easting - 543_367.10).abs() < 0.01,
            "{}",
            first.easting
        );
        assert!(
            (first.northing - 8_681_393.78).abs() < 0.01,
            "{}",
            first.northing
        );
    }

    #[test]
    fn bad_traces_are_named() {
        let mut given = arrays();
        given.position = Position::LonLat {
            longitude: (0..N_TRACES)
                .map(|j| if j == 3 || j == 7 { f64::NAN } else { 16.9 })
                .collect(),
            latitude: vec![78.2; N_TRACES],
            crs: None,
        };
        assert!(error(given).contains("longitude is not finite at traces 3, 7"));
    }

    #[test]
    fn many_bad_traces_are_elided() {
        let mut given = arrays();
        given.elevation = Elevation::Values(vec![f64::NAN; N_TRACES]);
        assert!(error(given).contains("0, 1, 2, 3, 4, 5, 6, 7, 8, 9, ... (20 in total)"));
    }

    #[test]
    fn projected_coordinates_in_lon_lat_are_refused() {
        let mut given = arrays();
        given.position = Position::LonLat {
            longitude: vec![500_000.; N_TRACES],
            latitude: vec![8_680_000.; N_TRACES],
            crs: None,
        };
        assert!(error(given).contains("Projected coordinates go in x and y"));
    }

    #[test]
    fn an_uneven_axis_is_refused() {
        let mut given = arrays();
        given.twtt_ns[10] += DT / 2.;
        assert!(error(given).contains("evenly spaced"));
    }

    #[test]
    fn lengths_must_match_the_data() {
        let mut given = arrays();
        given.trace_time.pop();
        assert!(error(given).contains("trace_time has 19 values, but data has 20 traces"));
        let mut given = arrays();
        given.twtt_ns.pop();
        assert!(error(given).contains("twtt_ns has 49 values"));
        let mut given = arrays();
        given.time_zero_ns = Some(vec![40.; 3]);
        assert!(error(given).contains("time_zero_ns has 3 values"));
    }

    #[test]
    fn a_time_zero_of_zero_is_refused_as_ambiguous() {
        let mut given = arrays();
        given.time_zero_ns = Some(vec![0.]);
        assert!(error(given).contains("0 means never located"));
    }

    #[test]
    fn a_first_sample_before_the_recording_is_refused() {
        let mut given = arrays();
        given.twtt_ns = (0..N_SAMPLES).map(|i| -50.0 + i as f64 * DT).collect();
        assert!(error(given).contains("before the recording started"));
        let mut given = arrays();
        given.time_zero_ns = None;
        given.twtt_ns = (0..N_SAMPLES).map(|i| -1.0 + i as f64 * DT).collect();
        assert!(error(given).contains("before the recording started"));
    }

    #[test]
    fn a_partial_separation_correction_is_refused() {
        let mut given = arrays();
        given.antenna_separation_effective = Some(0.5);
        assert!(error(given).contains("antenna_separation_effective must be"));
    }

    #[test]
    fn per_trace_time_zero_is_kept_per_trace() {
        let mut given = arrays();
        given.time_zero_ns = Some((0..N_TRACES).map(|j| 40. + j as f64 * 0.1).collect());
        let gpr = build(given, identity()).unwrap();
        assert_eq!(gpr.twtt_time_zero_uniform_ns(), None);
        assert!((gpr.twtt_crop_ns()[5] - 40.5).abs() < 1e-5);
    }
}
