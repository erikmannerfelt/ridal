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
//! | `time_source` | no | `recorded`, `interpolated` or `synthetic` |
//! | `alt_source` | no | `recorded`, `interpolated` or `dem` |
//!
//! The two source columns are what `ridal coords` says about each value it
//! wrote. A `synthetic` time marks the whole radargram's times as made up
//! ([`gpr::TimeSource::Synthetic`]), and a `dem` altitude its elevations as
//! sampled from a DEM ([`gpr::LocationCorrection::InCoordinates`]).
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
const OPTIONAL: [&str; 3] = ["alt", "time_source", "alt_source"];

/// Where a row's time came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeOrigin {
    /// At a fix that was recorded with a time.
    Recorded,
    /// Between or beyond recorded fixes.
    Interpolated,
    /// Made up from a start time and a trace interval.
    Synthetic,
}

/// Where a row's altitude came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltOrigin {
    Recorded,
    Interpolated,
    /// Sampled from a DEM.
    Dem,
}

impl TimeOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            TimeOrigin::Recorded => "recorded",
            TimeOrigin::Interpolated => "interpolated",
            TimeOrigin::Synthetic => "synthetic",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Recorded, Self::Interpolated, Self::Synthetic]
            .into_iter()
            .find(|origin| origin.as_str() == text)
    }
}

impl AltOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            AltOrigin::Recorded => "recorded",
            AltOrigin::Interpolated => "interpolated",
            AltOrigin::Dem => "dem",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Recorded, Self::Interpolated, Self::Dem]
            .into_iter()
            .find(|origin| origin.as_str() == text)
    }
}

/// One row of a coordinate CSV, as [`write_coords_csv`] writes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoordsRow {
    pub trace: u32,
    /// Seconds since the UNIX epoch, UTC.
    pub time_seconds: f64,
    pub time_source: TimeOrigin,
    /// WGS84 degrees.
    pub lon: f64,
    pub lat: f64,
    /// `None` in every row or in none.
    pub alt: Option<(f64, AltOrigin)>,
}

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
    let time_source_col = column("time_source");
    let alt_source_col = column("alt_source");
    if alt_source_col.is_some() && alt_col.is_none() {
        return Err(at(header_line, "alt_source without alt".to_string()).into());
    }
    let mut synthetic_time = false;
    let mut dem_altitude = false;

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

        if let Some(col) = time_source_col {
            let origin = TimeOrigin::parse(fields[col]).ok_or_else(|| {
                at(
                    line,
                    format!(
                        "time_source '{}' is not recorded, interpolated or synthetic",
                        fields[col]
                    ),
                )
            })?;
            synthetic_time |= origin == TimeOrigin::Synthetic;
        }
        if let Some(col) = alt_source_col {
            let origin = AltOrigin::parse(fields[col]).ok_or_else(|| {
                at(
                    line,
                    format!(
                        "alt_source '{}' is not recorded, interpolated or dem",
                        fields[col]
                    ),
                )
            })?;
            dem_altitude |= origin == AltOrigin::Dem;
        }

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
        correction: if dem_altitude {
            gpr::LocationCorrection::InCoordinates {
                path: filepath.to_path_buf(),
            }
        } else {
            gpr::LocationCorrection::None
        },
        time_source: if synthetic_time {
            gpr::TimeSource::Synthetic
        } else {
            gpr::TimeSource::Recorded
        },
        crs,
    })
}

/// Write a coordinate CSV that [`load_coords_csv`] reads back, with
/// `comments` as `#` lines above the header and times to the millisecond.
///
/// Written beside `filepath` and renamed into place, so a file that exists
/// is complete.
///
/// # Errors
/// The file could not be written, a time is outside what a date can hold,
/// or some rows have an altitude and others not.
pub fn write_coords_csv(
    filepath: &Path,
    rows: &[CoordsRow],
    comments: &[String],
) -> Result<(), String> {
    let with_alt = rows.first().is_some_and(|row| row.alt.is_some());
    let mut text = String::new();
    for comment in comments {
        for line in comment.lines() {
            text += &format!("# {line}\n");
        }
    }
    text += if with_alt {
        "trace,time,lon,lat,alt,time_source,alt_source\n"
    } else {
        "trace,time,lon,lat,time_source\n"
    };
    for row in rows {
        // To the millisecond: the times are interpolated in floating point,
        // and nothing that positions a trace is finer.
        let millis = (row.time_seconds * 1000.).round();
        let time = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis as i64)
            .ok_or_else(|| {
                format!(
                    "trace {}: time {} is out of range",
                    row.trace, row.time_seconds
                )
            })?
            .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
        text += &format!("{},{time},{:.9},{:.9}", row.trace, row.lon, row.lat);
        match (with_alt, row.alt) {
            (true, Some((alt, origin))) => {
                text += &format!(",{alt:.3},{},{}", row.time_source.as_str(), origin.as_str())
            }
            (false, None) => text += &format!(",{}", row.time_source.as_str()),
            _ => return Err(format!("trace {}: alt in some rows but not all", row.trace)),
        }
        text += "\n";
    }

    let name = filepath
        .file_name()
        .ok_or_else(|| format!("{} is not a file name", filepath.display()))?
        .to_string_lossy();
    let temporary = filepath.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = std::fs::write(&temporary, text)
        .and_then(|()| std::fs::rename(&temporary, filepath))
        .map_err(|e| format!("could not write {}: {e}", filepath.display()));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
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
    fn what_is_written_reads_back_with_its_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.coords.csv");
        let row = |trace, time_source, alt| CoordsRow {
            trace,
            time_seconds: 1_743_070_220.25 + f64::from(trace),
            time_source,
            lon: 16.648388333,
            lat: 78.101888333,
            alt,
        };
        let comments = vec!["written by a test".to_string()];

        let rows = [
            row(0, TimeOrigin::Recorded, Some((715.2, AltOrigin::Recorded))),
            row(
                1,
                TimeOrigin::Interpolated,
                Some((715.3, AltOrigin::Interpolated)),
            ),
        ];
        write_coords_csv(&path, &rows, &comments).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# written by a test\ntrace,time,lon,lat,alt,"));
        assert!(
            text.contains(
                "\n0,2025-03-27T10:10:20.250Z,16.648388333,78.101888333,715.200,recorded,recorded\n"
            ),
            "{text}"
        );
        let read = load_coords_csv(&path, None).unwrap();
        assert_eq!(read.time_source, gpr::TimeSource::Recorded);
        assert!(matches!(read.correction, gpr::LocationCorrection::None));
        assert_eq!(read.cor_points[1].time_seconds, rows[1].time_seconds);
        assert_eq!(read.cor_points[1].altitude, 715.3);

        let rows = [
            row(0, TimeOrigin::Synthetic, Some((700., AltOrigin::Dem))),
            row(5, TimeOrigin::Synthetic, Some((701., AltOrigin::Dem))),
        ];
        write_coords_csv(&path, &rows, &[]).unwrap();
        let read = load_coords_csv(&path, None).unwrap();
        assert_eq!(read.time_source, gpr::TimeSource::Synthetic);
        assert!(matches!(
            &read.correction,
            gpr::LocationCorrection::InCoordinates { path: p } if *p == path
        ));

        let rows = [row(0, TimeOrigin::Recorded, None)];
        write_coords_csv(&path, &rows, &[]).unwrap();
        let read = load_coords_csv(&path, None).unwrap();
        assert!(read.cor_points[0].altitude.is_nan());
        // Nothing but the file itself is left in the directory.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
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
                format!("{header},alt_source\n{row0},dem\n"),
                "alt_source without alt",
            ),
            (
                format!("{header},time_source\n{row0},guessed\n"),
                "time_source 'guessed' is not",
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
