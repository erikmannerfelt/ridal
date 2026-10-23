//! Ridal's coordinate CSV (#13): positions for a recording from any source,
//! read in place of its own positions file.
//!
//! One header line names the columns, in any order:
//!
//! | column | required | meaning |
//! |---|---|---|
//! | `trace` | yes | the trace index, 0-based as everywhere else in Ridal |
//! | `time` | yes | ISO 8601 with a UTC offset, such as `2025-03-27T10:15:02.5Z` |
//! | `lon`, `lat` | yes | WGS84 degrees |
//! | `alt` | no | elevation (m), passed through as given |
//!
//! Rows may be sparse: traces between two rows are interpolated, as between
//! the fixes of a `.cor`. Blank lines and lines starting with `#` are
//! skipped. Unlike the instruments' own files, nothing that fails to parse
//! is skipped: this file is written on purpose, so a bad row is a mistake
//! to point at, not noise to step over.

use std::error::Error;
use std::path::Path;

use crate::gpr;

/// The columns a coordinate CSV may have, required ones first.
const REQUIRED: [&str; 4] = ["trace", "time", "lon", "lat"];
const OPTIONAL: [&str; 1] = ["alt"];

/// Load a Ridal coordinate CSV.
///
/// Without an `alt` column every altitude is NaN, for a DEM to fill.
///
/// # Arguments
/// - `filepath`: The path to the file to read.
/// - `projected_crs`: The CRS to project the positions into, or `None` for
///   the WGS84 UTM zone of the first one.
///
/// # Errors
/// - The file could not be read.
/// - The header lacks a required column, or names an unknown or repeated one.
/// - A row has the wrong number of fields or a value that does not parse:
///   a negative or non-integer trace, a time without an offset, a position
///   off the globe, or a non-finite altitude.
/// - The traces do not strictly increase, or the times decrease.
/// - `projected_crs` is not understood.
pub fn load_coords_csv(
    filepath: &Path,
    projected_crs: Option<&String>,
) -> Result<gpr::GPRLocation, Box<dyn Error>> {
    let content = std::fs::read_to_string(filepath)?;
    let at = |line: usize, message: String| format!("{}:{}: {message}", filepath.display(), line);

    let mut lines = content
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'));

    let (header_line, header) = lines
        .next()
        .ok_or_else(|| format!("{} is empty", filepath.display()))?;
    let names: Vec<&str> = header.split(',').map(str::trim).collect();
    for (i, name) in names.iter().enumerate() {
        if !REQUIRED.contains(name) && !OPTIONAL.contains(name) {
            return Err(at(
                header_line,
                format!(
                    "unknown column '{name}'; the columns are {} and optionally {}",
                    REQUIRED.join(", "),
                    OPTIONAL.join(", ")
                ),
            )
            .into());
        }
        if names[..i].contains(name) {
            return Err(at(header_line, format!("column '{name}' is given twice")).into());
        }
    }
    let column = |name: &str| names.iter().position(|n| *n == name);
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|name| column(name).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(at(
            header_line,
            format!("missing column(s) {}", missing.join(", ")),
        )
        .into());
    }
    let [trace_col, time_col, lon_col, lat_col] = REQUIRED.map(|name| column(name).unwrap());
    let alt_col = column("alt");

    let mut coords = Vec::<crate::coords::Coord>::new();
    let mut points = Vec::<gpr::CorPoint>::new();
    for (line, row) in lines {
        let fields: Vec<&str> = row.split(',').map(str::trim).collect();
        if fields.len() != names.len() {
            return Err(at(
                line,
                format!(
                    "{} fields where the header has {}",
                    fields.len(),
                    names.len()
                ),
            )
            .into());
        }
        let number = |col: usize, name: &str| {
            fields[col]
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| at(line, format!("{name} '{}' is not a number", fields[col])))
        };

        let trace_n: u32 = fields[trace_col].parse().map_err(|_| {
            at(
                line,
                format!("trace '{}' is not a 0-based trace index", fields[trace_col]),
            )
        })?;
        let time = chrono::DateTime::parse_from_rfc3339(fields[time_col]).map_err(|_| {
            at(
                line,
                format!(
                    "time '{}' is not ISO 8601 with a UTC offset, \
                     such as 2025-03-27T10:15:02Z or 2025-03-27T12:15:02+02:00",
                    fields[time_col]
                ),
            )
        })?;
        let time_seconds =
            time.timestamp() as f64 + f64::from(time.timestamp_subsec_nanos()) * 1e-9;
        let longitude = number(lon_col, "lon")?;
        let latitude = number(lat_col, "lat")?;
        if !(-180. ..=180.).contains(&longitude) || !(-90. ..=90.).contains(&latitude) {
            return Err(at(
                line,
                format!("lon {longitude}, lat {latitude} is not a WGS84 position in degrees"),
            )
            .into());
        }
        let altitude = match alt_col {
            Some(col) => number(col, "alt")?,
            None => f64::NAN,
        };

        if let Some(previous) = points.last() {
            if trace_n <= previous.trace_n {
                return Err(at(
                    line,
                    format!(
                        "trace {trace_n} follows trace {}; traces must increase",
                        previous.trace_n
                    ),
                )
                .into());
            }
            if time_seconds < previous.time_seconds {
                return Err(at(
                    line,
                    format!("time {} is earlier than the row before", fields[time_col]),
                )
                .into());
            }
        }

        coords.push(crate::coords::Coord {
            x: longitude,
            y: latitude,
        });
        // Projected right below, all at once.
        points.push(gpr::CorPoint {
            trace_n,
            time_seconds,
            easting: 0.,
            northing: 0.,
            altitude,
        });
    }

    if points.is_empty() {
        return Err(format!("{} has a header but no rows", filepath.display()).into());
    }

    let crs = super::project_points(&mut points, &coords, projected_crs)?;

    Ok(gpr::GPRLocation {
        cor_points: points,
        correction: gpr::LocationCorrection::None,
        crs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> Result<gpr::GPRLocation, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.coords.csv");
        std::fs::write(&path, text).unwrap();
        load_coords_csv(&path, Some(&"EPSG:32633".to_string())).map_err(|e| e.to_string())
    }

    #[test]
    fn reads_columns_in_any_order_with_comments_and_fractional_seconds() {
        let location = load(
            "# from a PPK track\n\
             lat,lon,alt,time,trace\n\
             78.5,16.0,512.5,2025-03-27T10:00:00Z,0\n\
             \n\
             78.5001,16.0,513.0,2025-03-27T12:00:01.5+02:00,10\n",
        )
        .unwrap();
        assert_eq!(location.crs, "EPSG:32633");
        let [first, second] = location.cor_points[..] else {
            panic!("expected two points, got {:?}", location.cor_points);
        };
        assert_eq!((first.trace_n, second.trace_n), (0, 10));
        assert_eq!(first.altitude, 512.5);
        // The offset is honoured: 12:00:01.5+02:00 is 10:00:01.5 UTC.
        assert_eq!(second.time_seconds - first.time_seconds, 1.5);
        // Projected into UTM 33N, 0.0001° of latitude north (about 11 m).
        assert!(first.easting > 400_000. && first.easting < 600_000.);
        assert!((second.northing - first.northing - 11.).abs() < 0.5);
    }

    #[test]
    fn without_alt_every_altitude_is_nan() {
        let location = load("trace,time,lon,lat\n0,2025-03-27T10:00:00Z,16,78.5\n").unwrap();
        assert!(location.cor_points[0].altitude.is_nan());
    }

    #[test]
    fn bad_files_are_refused_with_the_line_and_the_reason() {
        let header = "trace,time,lon,lat";
        let row0 = "0,2025-03-27T10:00:00Z,16,78.5";
        for (text, expected) in [
            (String::new(), "is empty"),
            (format!("{header}\n"), "no rows"),
            ("trace,time,lon\n".to_string(), "missing column(s) lat"),
            (format!("{header},speed\n"), "unknown column 'speed'"),
            (format!("{header},lon\n"), "'lon' is given twice"),
            (
                format!("{header}\n0,2025-03-27T10:00:00Z,16\n"),
                ":2: 3 fields",
            ),
            (
                format!("{header}\n-1,2025-03-27T10:00:00Z,16,78.5\n"),
                "not a 0-based trace index",
            ),
            (
                format!("{header}\n0,2025-03-27 10:00:00,16,78.5\n"),
                "with a UTC offset",
            ),
            (
                format!("{header}\n0,2025-03-27T10:00:00Z,east,78.5\n"),
                "lon 'east'",
            ),
            (
                format!("{header}\n0,2025-03-27T10:00:00Z,16,95\n"),
                "not a WGS84 position",
            ),
            (
                format!("{header},alt\n0,2025-03-27T10:00:00Z,16,78.5,NaN\n"),
                "alt 'NaN' is not a number",
            ),
            (
                format!("{header}\n{row0}\n{row0}\n"),
                ":3: trace 0 follows trace 0",
            ),
            (
                format!("{header}\n{row0}\n1,2025-03-27T09:59:59Z,16,78.5\n"),
                "earlier than the row before",
            ),
        ] {
            let error = load(&text).unwrap_err();
            assert!(
                error.contains(expected),
                "{text:?}: expected {expected:?} in {error:?}"
            );
        }
    }
}
