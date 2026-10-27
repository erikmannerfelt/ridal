//! Positions for a recording from partial information: `ridal coords` (#13).
//!
//! `ridal process` reads a coordinate CSV and infers nothing. Everything a
//! recording's positions are inferred from lives here instead, and ends up
//! in a CSV that can be read, edited and passed back:
//!
//! - **Positions** come from `--positions` (a CSV of trace numbers and
//!   positions), from `--start`/`--end` (the first and last trace), or from
//!   the recording's own positions file, and are interpolated by trace in
//!   between. For a wheel-triggered recording that is exact; for a
//!   time-triggered one it assumes a steady speed.
//! - **Times** come from the given positions if they have them, else from
//!   the recording's own positions file, else they are made up as a start
//!   time plus a trace interval and marked `synthetic`.
//! - **Altitudes** come from `--dem` before anything else, then from the
//!   given positions, then from the recording's own.
//!
//! Every row says where its time and altitude came from, and the CSV starts
//! with comments that say the same for the whole file.

use std::path::{Path, PathBuf};

use crate::coords::{self, Coord};
use crate::formats;
use crate::gpr::{CorPoint, GPRLocation, LocationCorrection, TimeSource};
use crate::io::{AltOrigin, CoordsRow, TimeOrigin};

/// A position given on the command line: `lon,lat[,alt]` in WGS84, or
/// `x,y[,alt]` in `--src-crs`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub a: f64,
    pub b: f64,
    pub alt: Option<f64>,
}

impl std::str::FromStr for Point {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let values = text
            .split(',')
            .map(|v| v.trim().parse::<f64>().ok().filter(|v| v.is_finite()))
            .collect::<Option<Vec<f64>>>()
            .ok_or_else(|| format!("'{text}' is not two or three numbers"))?;
        match values[..] {
            [a, b] => Ok(Point { a, b, alt: None }),
            [a, b, alt] => Ok(Point {
                a,
                b,
                alt: Some(alt),
            }),
            _ => Err(format!(
                "'{text}' is not lon,lat[,alt] (or x,y[,alt] with --src-crs)"
            )),
        }
    }
}

/// What to build the positions of one recording from.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Any one of the recording's files.
    pub recording: PathBuf,
    /// A CSV of `trace` and `lon,lat` (or `x,y` with `src_crs`), and
    /// optionally `alt` and `time`.
    pub positions: Option<PathBuf>,
    /// The first and last trace's positions, as an alternative to
    /// `positions`.
    pub endpoints: Option<(Point, Point)>,
    /// A GNSS track: `time` and `lon,lat` (or `x,y` with `src_crs`), and
    /// optionally `alt`, matched to the traces by time, as an alternative to
    /// `positions` and `endpoints`.
    pub track: Option<PathBuf>,
    /// Seconds to add to the recording's clock to get the track's.
    pub time_offset: Option<f64>,
    /// The CRS of `x,y` positions. Positions given as `lon,lat` are WGS84.
    pub src_crs: Option<String>,
    /// When the first trace was recorded, for times made up from it.
    pub start_time: Option<String>,
    /// Seconds between traces, for times made up from `start_time`.
    pub time_interval: Option<f64>,
    /// A DEM to take every altitude from.
    pub dem: Option<PathBuf>,
}

/// The positions of one recording, ready for [`crate::io::write_coords_csv`].
#[derive(Debug, Clone)]
pub struct Built {
    pub rows: Vec<CoordsRow>,
    /// What the rows came from, for the top of the file.
    pub comments: Vec<String>,
    /// What a person should know: times made up, an interval guessed.
    pub warnings: Vec<String>,
}

/// One given position, in WGS84: a row of `--positions` (with a trace)
/// or of `--track` (with a time and no trace), or a start or end point.
#[derive(Debug, Clone, Copy)]
struct Given {
    trace: Option<u32>,
    position: Coord,
    alt: Option<f64>,
    time: Option<f64>,
}

/// Build one row per trace of `request.recording`.
///
/// # Errors
/// The recording cannot be read; nothing gives positions; a given position
/// file is malformed or names a trace past the last; times are both given
/// and asked to be made up; or the DEM gives no elevation for some trace.
pub fn build(request: &Request) -> Result<Built, String> {
    let resolved = formats::resolve_input(&request.recording)?;
    let header_name = file_name(&resolved.header);
    // Nothing here depends on the antenna, so one whose frequency the
    // header does not state should not stop it.
    let meta = crate::processing::input::load_meta(&resolved, 0.168, Some(0.), None, None)
        .map_err(|e| format!("{}: {e}", resolved.header.display()))?;
    let n_traces = meta.last_trace;
    if n_traces == 0 {
        return Err(format!("{} has no traces", resolved.header.display()));
    }
    let mut warnings = Vec::new();
    let mut comments = vec![format!(
        "Positions for {header_name}, written by ridal coords {}.",
        env!("CARGO_PKG_VERSION")
    )];

    let alternatives = [
        request.positions.is_some(),
        request.endpoints.is_some(),
        request.track.is_some(),
    ];
    if alternatives.iter().filter(|&&given| given).count() > 1 {
        return Err("give one of positions, start and end points, or a track".into());
    }
    if request.time_offset.is_some() && request.track.is_none() {
        return Err("a time offset only applies to a track".into());
    }
    let given = match (&request.positions, &request.endpoints) {
        (Some(_), Some(_)) => unreachable!("refused above"),
        (Some(path), None) => {
            comments.push(format!(
                "Positions from {}, interpolated by trace between rows.",
                file_name(path)
            ));
            let rows = read_points(path, request.src_crs.as_deref(), &mut warnings)?;
            if rows.iter().any(|row| row.trace.is_none()) {
                return Err(format!(
                    "{} has no trace column; give a track of times and positions with --track",
                    path.display()
                ));
            }
            Some(rows)
        }
        (None, Some((start, end))) => {
            comments.push(
                "Positions interpolated by trace between the given start and end points."
                    .to_string(),
            );
            Some(endpoints(
                *start,
                *end,
                n_traces,
                request.src_crs.as_deref(),
            )?)
        }
        (None, None) => None,
    };
    if let Some(given) = &given {
        if let Some(past) = given
            .iter()
            .filter_map(|g| g.trace)
            .find(|&t| t >= n_traces)
        {
            return Err(format!(
                "trace {past} is past the last trace of {header_name}, {}",
                n_traces - 1
            ));
        }
    }

    // The recording's own positions, read in the CRS the given ones are
    // projected into, so the two interpolate alike.
    let own_path = formats::own_positions(&resolved);
    let working_crs = given
        .as_ref()
        .map(|given| coords::UtmCrs::optimal_crs(&given[0].position).to_epsg_str());
    let own = if own_path.is_file() {
        match crate::processing::input::load_positions(
            &resolved,
            &own_path,
            working_crs.as_ref(),
            None,
        ) {
            Ok(own) => Some(own),
            Err(e) if given.is_some() || request.track.is_some() => {
                warnings.push(format!(
                    "{} could not be read ({e}), so its times and altitudes were not used",
                    own_path.display()
                ));
                None
            }
            Err(e) => return Err(format!("{}: {e}", own_path.display())),
        }
    } else {
        None
    };
    if given.is_none() && request.track.is_none() {
        if own.is_none() {
            return Err(format!(
                "{header_name} has no positions of its own ({} does not exist): \
                 give positions, start and end points, or a track",
                own_path.display()
            ));
        }
        comments.push(format!(
            "Positions from {}, interpolated by trace between fixes.",
            file_name(&own_path)
        ));
    }

    let given_location = match (&given, &working_crs) {
        (Some(given), Some(crs)) => Some(project(given, crs)?),
        _ => None,
    };
    let fill = |location: &GPRLocation| {
        location
            .range_fill(0, n_traces, meta.time_interval)
            .cor_points
    };
    let traces_of = |location: &GPRLocation| {
        location
            .cor_points
            .iter()
            .map(|p| p.trace_n)
            .collect::<std::collections::HashSet<u32>>()
    };
    // Times.
    let given_times = given
        .as_ref()
        .is_some_and(|given| given.iter().all(|g| g.time.is_some()));
    let made_up = request.start_time.is_some() || request.time_interval.is_some();
    if given_times && made_up {
        return Err(
            "the given positions have times, so a start time or interval would be ignored".into(),
        );
    }
    let mut times: Vec<(f64, TimeOrigin)> =
        if let (true, Some(location)) = (given_times, &given_location) {
            comments.push("Times from the given positions.".to_string());
            origins(&fill(location), &traces_of(location), |p| p.time_seconds)
        } else if let (false, Some(own)) = (made_up, &own) {
            comments.push(format!("Times from {}.", file_name(&own_path)));
            origins(&fill(own), &traces_of(own), |p| p.time_seconds)
        } else {
            let (start, interval) = synthetic_clock(request, meta.time_interval, &mut warnings)?;
            comments.push(format!(
                "Times are synthetic: {} plus {interval} s per trace. They order the traces \
             but are not when they were recorded.",
                seconds_to_text(start)
            ));
            (0..n_traces)
                .map(|i| (start + f64::from(i) * interval, TimeOrigin::Synthetic))
                .collect()
        };
    if let Some(offset) = request.time_offset {
        comments.push(format!(
            "Times are the recording's clock plus {offset} s, as the track's."
        ));
        for (time, _) in &mut times {
            *time += offset;
        }
    }

    // Positions.
    let mut track_alts = None;
    let wgs84 = if let Some(track_path) = &request.track {
        let track = read_points(track_path, request.src_crs.as_deref(), &mut warnings)?;
        if track
            .iter()
            .any(|fix| fix.trace.is_some() || fix.time.is_none())
        {
            return Err(format!(
                "{} is not a track: it needs times and no trace column \
                 (give positions by trace with --positions)",
                track_path.display()
            ));
        }
        comments.push(format!(
            "Positions from the track {}, interpolated in time to each trace.",
            file_name(track_path)
        ));
        let matched =
            match_track(&track, &times).map_err(|e| format!("{}: {e}", track_path.display()))?;
        if let Some(warning) = matched.warning {
            warnings.push(warning);
        }
        track_alts = matched.alts;
        matched.positions
    } else {
        let (dense, crs) = match (&given_location, &own) {
            (Some(given), _) => (fill(given), given.crs.clone()),
            (None, Some(own)) => (fill(own), own.crs.clone()),
            (None, None) => unreachable!("refused above"),
        };
        coords::to_wgs84(
            &dense
                .iter()
                .map(|p| Coord {
                    x: p.easting,
                    y: p.northing,
                })
                .collect::<Vec<_>>(),
            &coords::Crs::from_user_input(&crs)?,
        )?
    };

    // Altitudes.
    let given_alts = given
        .as_ref()
        .is_some_and(|given| given.iter().all(|g| g.alt.is_some()));
    let alts: Option<Vec<(f64, AltOrigin)>> = if let Some(dem) = &request.dem {
        let sample =
            crate::dem::sample_dem(dem, &wgs84).map_err(|e| format!("{}: {e}", dem.display()))?;
        let missing = (0..wgs84.len())
            .filter(|&i| !sample.elevations.get(i).is_some_and(|e| e.is_finite()))
            .count();
        if missing > 0 {
            return Err(format!(
                "{} gives no elevation for {missing} of {} traces",
                dem.display(),
                wgs84.len()
            ));
        }
        comments.push(format!(
            "Altitudes from the DEM {}{}.",
            file_name(dem),
            match sample.resampling {
                crate::dem::Resampling::Bilinear => "",
                crate::dem::Resampling::Nearest => " (nearest cell, GDAL before 3.10)",
            }
        ));
        Some(
            sample
                .elevations
                .iter()
                .map(|&e| (f64::from(e), AltOrigin::Dem))
                .collect(),
        )
    } else if let Some(track_alts) = track_alts {
        comments.push("Altitudes from the track.".to_string());
        Some(track_alts)
    } else if let (true, Some(location)) = (given_alts, &given_location) {
        comments.push("Altitudes from the given positions.".to_string());
        Some(alt_origins(&fill(location), &traces_of(location)))
    } else if let Some(own) = &own {
        comments.push(format!("Altitudes from {}.", file_name(&own_path)));
        Some(alt_origins(&fill(own), &traces_of(own)))
    } else {
        warnings.push(
            "nothing gives altitudes, so the CSV has none; give --dem to ridal process".to_string(),
        );
        None
    };

    let rows = (0..n_traces as usize)
        .map(|i| CoordsRow {
            trace: i as u32,
            time_seconds: times[i].0,
            time_source: times[i].1,
            lon: wgs84[i].x,
            lat: wgs84[i].y,
            alt: alts.as_ref().map(|alts| alts[i]),
        })
        .collect();
    Ok(Built {
        rows,
        comments,
        warnings,
    })
}

/// Where a recording's CSV goes: `output` if given, else
/// `<stem>.coords.csv` in `output_dir`, or beside the recording's header,
/// where `ridal process` finds it.
pub fn output_path(
    recording: &Path,
    output: Option<&Path>,
    output_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(output) = output {
        return Ok(output.to_path_buf());
    }
    let resolved = formats::resolve_input(recording)?;
    let stem = resolved
        .header
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{} has no usable name", resolved.header.display()))?;
    let name = format!("{stem}{}", formats::COORDS_CSV_SUFFIX);
    Ok(match output_dir {
        Some(dir) => dir.join(name),
        None => resolved.header.with_file_name(name),
    })
}

/// [`build`] the CSV for `request` and write it to `output`.
///
/// # Errors
/// What [`build`] refuses, an `output` that exists unless `overwrite`, and
/// a file that cannot be written.
pub fn write(request: &Request, output: &Path, overwrite: bool) -> Result<Built, String> {
    if output.exists() && !overwrite {
        return Err(format!(
            "{} exists already; give --overwrite (overwrite=True in Python) to replace it",
            output.display()
        ));
    }
    let built = build(request)?;
    crate::io::write_coords_csv(output, &built.rows, &built.comments)?;
    Ok(built)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn seconds_to_text(seconds: f64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds.floor() as i64, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| seconds.to_string())
}

/// A time as ISO 8601. One without an offset is taken as UTC, with a
/// warning, since that is what a GNSS log usually means.
fn parse_time(text: &str, warnings: &mut Vec<String>) -> Result<f64, String> {
    let seconds = |t: chrono::DateTime<chrono::Utc>| {
        t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) * 1e-9
    };
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(seconds(time.with_timezone(&chrono::Utc)));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, format) {
            let warning = "times without a UTC offset were taken as UTC".to_string();
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
            return Ok(seconds(naive.and_utc()));
        }
    }
    Err(format!("'{text}' is not an ISO 8601 time"))
}

/// The start time and trace interval to make times up from.
fn synthetic_clock(
    request: &Request,
    header_interval: f32,
    warnings: &mut Vec<String>,
) -> Result<(f64, f64), String> {
    let start = match &request.start_time {
        Some(text) => parse_time(text, warnings)?,
        None => {
            warnings.push(
                "no start time is known, so the times start at 1970-01-01T00:00:00Z; \
                 give --start-time if the date is known"
                    .to_string(),
            );
            0.
        }
    };
    let interval = match request.time_interval {
        Some(interval) if interval > 0. && interval.is_finite() => interval,
        Some(interval) => return Err(format!("a time interval of {interval} s is not positive")),
        None if header_interval > 0. && header_interval.is_finite() => {
            warnings.push(format!(
                "the trace interval is the header's {header_interval} s, which is wrong \
                 for a distance-triggered recording; give --time-interval if it is"
            ));
            // The header's decimal, not the f32 nearest it, so that the
            // times are 0.2 s apart rather than 0.2000000030 s.
            header_interval
                .to_string()
                .parse()
                .unwrap_or(f64::from(header_interval))
        }
        None => {
            warnings.push(
                "the header gives no trace interval, so the times are 1 s apart; \
                 give --time-interval"
                    .to_string(),
            );
            1.
        }
    };
    Ok((start, interval))
}

/// Each trace's value, `recorded` where a row or fix is and `interpolated`
/// elsewhere.
fn origins(
    dense: &[CorPoint],
    at: &std::collections::HashSet<u32>,
    value: impl Fn(&CorPoint) -> f64,
) -> Vec<(f64, TimeOrigin)> {
    dense
        .iter()
        .map(|p| {
            let origin = if at.contains(&p.trace_n) {
                TimeOrigin::Recorded
            } else {
                TimeOrigin::Interpolated
            };
            (value(p), origin)
        })
        .collect()
}

fn alt_origins(dense: &[CorPoint], at: &std::collections::HashSet<u32>) -> Vec<(f64, AltOrigin)> {
    origins(dense, at, |p| p.altitude)
        .into_iter()
        .map(|(alt, origin)| {
            let origin = match origin {
                TimeOrigin::Recorded => AltOrigin::Recorded,
                _ => AltOrigin::Interpolated,
            };
            (alt, origin)
        })
        .collect()
}

/// Given positions as a sparse location in `crs`, for [`GPRLocation::range_fill`].
fn project(given: &[Given], crs: &str) -> Result<GPRLocation, String> {
    let projected = coords::from_wgs84(
        &given.iter().map(|g| g.position).collect::<Vec<_>>(),
        &coords::Crs::from_user_input(crs)?,
    )?;
    Ok(GPRLocation {
        cor_points: given
            .iter()
            .zip(projected)
            .map(|(g, xy)| CorPoint {
                trace_n: g.trace.expect("given positions have traces"),
                time_seconds: g.time.unwrap_or(0.),
                easting: xy.x,
                northing: xy.y,
                altitude: g.alt.unwrap_or(f64::NAN),
            })
            .collect(),
        correction: LocationCorrection::None,
        crs: crs.to_string(),
        time_source: TimeSource::Recorded,
    })
}

/// Positions given in `src_crs`, or in WGS84 without one, in WGS84.
fn to_wgs84(points: Vec<Coord>, src_crs: Option<&str>) -> Result<Vec<Coord>, String> {
    let Some(src_crs) = src_crs else {
        if let Some(off) = points
            .iter()
            .find(|p| !(-180. ..=180.).contains(&p.x) || !(-90. ..=90.).contains(&p.y))
        {
            return Err(format!(
                "lon {}, lat {} is not a WGS84 position in degrees; \
                 give --src-crs for projected coordinates",
                off.x, off.y
            ));
        }
        return Ok(points);
    };
    coords::to_wgs84(&points, &coords::Crs::from_user_input(src_crs)?)
}

fn endpoints(
    start: Point,
    end: Point,
    n_traces: u32,
    src_crs: Option<&str>,
) -> Result<Vec<Given>, String> {
    if n_traces < 2 {
        return Err("a recording of one trace has no separate start and end".into());
    }
    if start.alt.is_some() != end.alt.is_some() {
        return Err("give an altitude for both the start and the end, or for neither".into());
    }
    let wgs84 = to_wgs84(
        vec![
            Coord {
                x: start.a,
                y: start.b,
            },
            Coord { x: end.a, y: end.b },
        ],
        src_crs,
    )?;
    Ok([(0, start), (n_traces - 1, end)]
        .into_iter()
        .zip(wgs84)
        .map(|((trace, point), position)| Given {
            trace: Some(trace),
            position,
            alt: point.alt,
            time: None,
        })
        .collect())
}

/// Read a CSV of positions: `lon,lat` (or `x,y` with `src_crs`), and
/// optionally `trace`, `alt` and `time`. Other columns are ignored, so a GNSS
/// receiver's export can be given as it is.
///
/// Rows with traces must have them increasing, and rows with times must not
/// go back in time.
fn read_points(
    path: &Path,
    src_crs: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Given>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let at = |line: usize, message: String| format!("{}:{line}: {message}", path.display());
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'));
    let (header_line, header) = lines
        .next()
        .ok_or_else(|| format!("{} is empty", path.display()))?;
    let names: Vec<String> = header
        .split(',')
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    let column = |name: &str| names.iter().position(|n| n == name);
    let pair = match (
        src_crs,
        column("lon"),
        column("lat"),
        column("x"),
        column("y"),
    ) {
        (None, Some(lon), Some(lat), None, None) => (lon, lat),
        (Some(_), None, None, Some(x), Some(y)) => (x, y),
        (Some(_), Some(_), Some(_), _, _) => {
            return Err(at(
                header_line,
                "lon and lat are WGS84; name the columns x and y for --src-crs".into(),
            ))
        }
        (None, _, _, Some(_), Some(_)) => {
            return Err(at(header_line, "x and y need --src-crs".into()))
        }
        _ => {
            return Err(at(
                header_line,
                "the columns need lon,lat (or x,y with --src-crs)".into(),
            ))
        }
    };
    let (trace_col, alt_col, time_col) = (column("trace"), column("alt"), column("time"));

    let mut rows: Vec<(Option<u32>, Option<f64>, Option<f64>)> = Vec::new();
    let mut points = Vec::new();
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
            ));
        }
        let number = |col: usize, name: &str| {
            fields[col]
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| at(line, format!("{name} '{}' is not a number", fields[col])))
        };
        let trace = trace_col
            .map(|col| {
                fields[col].parse::<u32>().map_err(|_| {
                    at(
                        line,
                        format!("trace '{}' is not a 0-based trace index", fields[col]),
                    )
                })
            })
            .transpose()?;
        let alt = alt_col.map(|col| number(col, "alt")).transpose()?;
        let time = time_col
            .map(|col| parse_time(fields[col], warnings).map_err(|e| at(line, e)))
            .transpose()?;
        if let Some(&(previous_trace, _, previous_time)) = rows.last() {
            if let (Some(trace), Some(previous)) = (trace, previous_trace) {
                if trace <= previous {
                    return Err(at(
                        line,
                        format!("trace {trace} follows trace {previous}; traces must increase"),
                    ));
                }
            }
            if let (Some(time), Some(previous)) = (time, previous_time) {
                if time < previous {
                    return Err(at(line, "the time is earlier than the row before".into()));
                }
            }
        }
        points.push(Coord {
            x: number(pair.0, &names[pair.0])?,
            y: number(pair.1, &names[pair.1])?,
        });
        rows.push((trace, alt, time));
    }
    if rows.is_empty() {
        return Err(format!("{} has a header but no rows", path.display()));
    }
    let wgs84 = to_wgs84(points, src_crs).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(rows
        .into_iter()
        .zip(wgs84)
        .map(|((trace, alt, time), position)| Given {
            trace,
            position,
            alt,
            time,
        })
        .collect())
}

/// Positions, and altitudes if the track has them, at each trace's time.
struct Matched {
    positions: Vec<Coord>,
    alts: Option<Vec<(f64, AltOrigin)>>,
    warning: Option<String>,
}

/// Interpolate `track` linearly in time at each of `times`.
///
/// Longitude and latitude are interpolated as they are: fixes are seconds
/// and metres apart, where a straight line in degrees and one on the map
/// differ by far less than a fix's own error. A trace before the track's
/// first fix or after its last is held there, with a warning.
///
/// # Errors
/// No trace's time is within the track, which almost always means the two
/// clocks differ (a time zone, say), which `--time-offset` corrects.
fn match_track(track: &[Given], times: &[(f64, TimeOrigin)]) -> Result<Matched, String> {
    let fix_times: Vec<f64> = track
        .iter()
        .map(|fix| fix.time.unwrap_or(f64::NAN))
        .collect();
    let (first, last) = (fix_times[0], fix_times[fix_times.len() - 1]);
    let (before, after) = (
        times.iter().filter(|(t, _)| *t < first).count(),
        times.iter().filter(|(t, _)| *t > last).count(),
    );
    if before + after == times.len() {
        return Err(format!(
            "no trace is within the track's time, {} to {}, while the traces run from {} \
             to {}; give --time-offset if the clocks differ",
            seconds_to_text(first),
            seconds_to_text(last),
            seconds_to_text(times[0].0),
            seconds_to_text(times[times.len() - 1].0),
        ));
    }
    let with_alt = track.iter().all(|fix| fix.alt.is_some());
    let mut positions = Vec::with_capacity(times.len());
    let mut alts = Vec::with_capacity(times.len());
    for &(time, _) in times {
        // The fix at or after `time`, and the one before it.
        let next = fix_times
            .partition_point(|&t| t < time)
            .min(track.len() - 1);
        let previous = next.saturating_sub(1);
        let (a, b) = (track[previous], track[next]);
        let span = fix_times[next] - fix_times[previous];
        let along = if span > 0. {
            ((time - fix_times[previous]) / span).clamp(0., 1.)
        } else {
            1.
        };
        let lerp = |x: f64, y: f64| x + (y - x) * along;
        positions.push(Coord {
            x: lerp(a.position.x, b.position.x),
            y: lerp(a.position.y, b.position.y),
        });
        if with_alt {
            let exact = fix_times[next] == time;
            alts.push((
                lerp(a.alt.unwrap_or(f64::NAN), b.alt.unwrap_or(f64::NAN)),
                if exact {
                    AltOrigin::Recorded
                } else {
                    AltOrigin::Interpolated
                },
            ));
        }
    }
    let warning = (before + after > 0).then(|| {
        format!(
            "{before} traces before the track's first fix and {after} after its last were \
             held at its ends"
        )
    });
    Ok(Matched {
        positions,
        alts: with_alt.then_some(alts),
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = "dronbreen-20250327-DAT_0066_A1";
    /// Traces in the fixture, by its header's `LAST TRACE`.
    const TRACES: usize = 3548;

    /// The fixture's header and data, and its `.cor` if `with_cor`, in a
    /// directory of their own.
    fn recording(dir: &Path, with_cor: bool) -> PathBuf {
        let exts: &[&str] = if with_cor {
            &["rad", "rd3", "cor"]
        } else {
            &["rad", "rd3"]
        };
        for ext in exts {
            std::fs::copy(
                format!("assets/mala/{LINE}.{ext}"),
                dir.join(format!("{LINE}.{ext}")),
            )
            .unwrap();
        }
        dir.join(format!("{LINE}.rad"))
    }

    fn request(recording: PathBuf) -> Request {
        Request {
            recording,
            ..Request::default()
        }
    }

    /// Two points on the Mettebreen test DEM.
    fn on_the_dem() -> (Point, Point) {
        (
            Point {
                a: 17.2,
                b: 77.81,
                alt: None,
            },
            Point {
                a: 17.25,
                b: 77.82,
                alt: None,
            },
        )
    }

    #[test]
    fn points_parse_with_and_without_altitude() {
        assert_eq!(
            "16.5,78.1".parse::<Point>().unwrap(),
            Point {
                a: 16.5,
                b: 78.1,
                alt: None
            }
        );
        assert_eq!("-16.5, 78.1, 700".parse::<Point>().unwrap().alt, Some(700.));
        assert!("16.5".parse::<Point>().is_err());
        assert!("16.5,north".parse::<Point>().is_err());
    }

    #[test]
    fn the_recordings_own_positions_round_trip_through_the_csv() {
        // Template mode: what ridal process reads from the written CSV is
        // what it reads from the `.cor`, to the millisecond and the
        // millimetre.
        let dir = tempfile::tempdir().unwrap();
        let rad = recording(dir.path(), true);
        let built = build(&request(rad.clone())).unwrap();
        assert_eq!(built.rows.len(), TRACES);
        assert!(built.warnings.is_empty(), "{:?}", built.warnings);
        assert_eq!(built.rows[0].time_source, TimeOrigin::Recorded);
        assert_eq!(built.rows[1].time_source, TimeOrigin::Interpolated);

        let csv = dir.path().join("out.coords.csv");
        crate::io::write_coords_csv(&csv, &built.rows, &built.comments).unwrap();
        let resolved = formats::resolve_input(&rad).unwrap();
        let from_cor = crate::processing::input::load_positions(
            &resolved,
            &formats::own_positions(&resolved),
            None,
            None,
        )
        .unwrap()
        .range_fill(0, TRACES as u32, 0.2);
        let from_csv =
            crate::io::load_coords_csv(&csv, crate::io::CsvPart::Whole, Some(&from_cor.crs))
                .unwrap();
        assert_eq!(from_csv.time_source, TimeSource::Recorded);
        assert_eq!(from_csv.cor_points.len(), TRACES);
        for (a, b) in from_csv.cor_points.iter().zip(&from_cor.cor_points) {
            assert_eq!(a.trace_n, b.trace_n);
            assert!((a.time_seconds - b.time_seconds).abs() <= 0.0005);
            assert!((a.easting - b.easting).abs() < 1e-3);
            assert!((a.northing - b.northing).abs() < 1e-3);
            assert!((a.altitude - b.altitude).abs() <= 0.0005);
        }
    }

    #[test]
    fn start_and_end_alone_give_synthetic_times_and_say_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = request(recording(dir.path(), false));
        let (start, end) = on_the_dem();
        request.endpoints = Some((start, end));
        let built = build(&request).unwrap();

        assert_eq!(built.rows.len(), TRACES);
        assert!(built.rows.iter().all(|row| row.alt.is_none()));
        assert!(built
            .rows
            .iter()
            .all(|row| row.time_source == TimeOrigin::Synthetic));
        let (first, last) = (built.rows[0], built.rows[TRACES - 1]);
        assert!((first.lon - 17.2).abs() < 1e-9 && (first.lat - 77.81).abs() < 1e-9);
        assert!((last.lon - 17.25).abs() < 1e-9 && (last.lat - 77.82).abs() < 1e-9);
        // From the epoch, at the header's 0.2 s.
        assert_eq!(first.time_seconds, 0.);
        assert!((built.rows[10].time_seconds - 2.).abs() < 1e-9);
        let warnings = built.warnings.join("\n");
        for expected in ["1970-01-01", "header's 0.2 s", "nothing gives altitudes"] {
            assert!(warnings.contains(expected), "{expected}: {warnings}");
        }
    }

    #[test]
    fn a_dem_gives_every_altitude_and_the_csv_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = request(recording(dir.path(), true));
        let (start, end) = on_the_dem();
        // An altitude given with the points loses to the DEM.
        request.endpoints = Some((
            Point {
                alt: Some(1.),
                ..start
            },
            Point {
                alt: Some(2.),
                ..end
            },
        ));
        request.start_time = Some("2025-03-27T14:00:00+02:00".to_string());
        request.time_interval = Some(0.5);
        request.dem = Some(PathBuf::from("assets/test_dem_dtm20_mettebreen.tif"));
        let built = build(&request).unwrap();

        assert!(built.warnings.is_empty(), "{:?}", built.warnings);
        assert!(built.rows.iter().all(|row| matches!(
            row.alt,
            Some((alt, AltOrigin::Dem)) if alt > 100. && alt < 1500.
        )));
        // The start time replaces the `.cor`'s, with its offset honoured.
        let noon = chrono::DateTime::parse_from_rfc3339("2025-03-27T12:00:00Z")
            .unwrap()
            .timestamp() as f64;
        assert_eq!(built.rows[0].time_seconds, noon);
        assert_eq!(built.rows[4].time_seconds, noon + 2.);
        assert!(built.comments.iter().any(|c| c.contains("test_dem_dtm20")));

        let csv = dir.path().join("out.coords.csv");
        crate::io::write_coords_csv(&csv, &built.rows, &built.comments).unwrap();
        let read = crate::io::load_coords_csv(&csv, crate::io::CsvPart::Whole, None).unwrap();
        assert_eq!(read.time_source, TimeSource::Synthetic);
        assert!(matches!(
            read.correction,
            LocationCorrection::InCoordinates { .. }
        ));
    }

    #[test]
    fn projected_positions_with_naive_times_are_interpolated_between_rows() {
        let dir = tempfile::tempdir().unwrap();
        let positions = dir.path().join("gnss.csv");
        std::fs::write(
            &positions,
            "trace,x,y,alt,time\n\
             0,550000,8640000,500,2025-03-27 12:00:00\n\
             100,550100,8640000,510,2025-03-27 12:00:10\n\
             3547,550200,8640100,520,2025-03-27T12:10:00\n",
        )
        .unwrap();
        let mut request = request(recording(dir.path(), true));
        request.positions = Some(positions);
        request.src_crs = Some("EPSG:32633".to_string());
        let built = build(&request).unwrap();

        assert_eq!(
            built.warnings,
            ["times without a UTC offset were taken as UTC"]
        );
        let row = |i: usize| built.rows[i];
        assert_eq!(row(100).time_source, TimeOrigin::Recorded);
        assert_eq!(row(50).time_source, TimeOrigin::Interpolated);
        assert_eq!(row(50).alt, Some((505., AltOrigin::Interpolated)));
        assert!((row(50).time_seconds - row(0).time_seconds - 5.).abs() < 1e-6);
        // Halfway along a 100 m step east, back in UTM 33N.
        let back = coords::from_wgs84(
            &[Coord {
                x: row(50).lon,
                y: row(50).lat,
            }],
            &coords::Crs::from_user_input("EPSG:32633").unwrap(),
        )
        .unwrap();
        assert!((back[0].x - 550050.).abs() < 0.01 && (back[0].y - 8640000.).abs() < 0.01);
    }

    /// The fixture's `.cor` fixes as a GNSS track, its clock shifted by
    /// `shift` seconds, keeping fixes `keep` of every one.
    fn track_of_cor(dir: &Path, shift: f64, keep: impl Fn(usize) -> bool) -> PathBuf {
        let mut csv = String::from("quality,time,lat,lon,alt\n");
        let cor = std::fs::read_to_string(format!("assets/mala/{LINE}.cor")).unwrap();
        for (i, line) in cor.lines().enumerate().filter(|(i, _)| keep(*i)) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let time = chrono::DateTime::parse_from_rfc3339(&format!("{}T{:0>8}Z", f[1], f[2]))
                .unwrap()
                .timestamp() as f64
                + shift;
            csv += &format!("{i},{},{},{},{}\n", seconds_to_text(time), f[3], f[5], f[7]);
        }
        let path = dir.join("track.csv");
        std::fs::write(&path, csv).unwrap();
        path
    }

    #[test]
    fn a_track_of_the_recordings_own_fixes_gives_its_own_positions() {
        let dir = tempfile::tempdir().unwrap();
        let rad = recording(dir.path(), true);
        let own = build(&request(rad.clone())).unwrap();
        let mut from_track = request(rad);
        from_track.track = Some(track_of_cor(dir.path(), 0., |_| true));
        let matched = build(&from_track).unwrap();

        // The `.cor` ends five traces before the recording does (#350).
        assert_eq!(
            matched.warnings,
            ["0 traces before the track's first fix and 5 after its last were held at its ends"]
        );
        assert!(matched
            .comments
            .iter()
            .any(|c| c.contains("the track track.csv")));
        for (a, b) in matched.rows.iter().zip(&own.rows) {
            assert_eq!(
                (a.trace, a.time_seconds, a.time_source),
                (b.trace, b.time_seconds, b.time_source)
            );
            // Interpolated in degrees by time rather than in UTM by trace.
            assert!((a.lon - b.lon).abs() < 1e-8 && (a.lat - b.lat).abs() < 1e-8);
            let (alt_a, alt_b) = (a.alt.unwrap().0, b.alt.unwrap().0);
            assert!((alt_a - alt_b).abs() < 1e-6, "{alt_a} {alt_b}");
        }
    }

    #[test]
    fn a_track_on_another_clock_needs_the_offset_and_one_too_short_warns() {
        let dir = tempfile::tempdir().unwrap();
        let rad = recording(dir.path(), true);
        let mut shifted = request(rad.clone());
        shifted.track = Some(track_of_cor(dir.path(), 7200., |_| true));
        let error = build(&shifted).unwrap_err();
        assert!(error.contains("give --time-offset"), "{error}");

        shifted.time_offset = Some(7200.);
        let built = build(&shifted).unwrap();
        let own = build(&request(rad.clone())).unwrap();
        assert_eq!(built.rows[0].time_seconds, own.rows[0].time_seconds + 7200.);
        assert!((built.rows[0].lat - own.rows[0].lat).abs() < 1e-8);
        assert!(built.comments.iter().any(|c| c.contains("plus 7200 s")));

        // Only the first half of the fixes: the rest are held at its end.
        let mut short = request(rad);
        short.track = Some(track_of_cor(dir.path(), 0., |i| i < 300));
        let built = build(&short).unwrap();
        let warning = built.warnings.join("\n");
        assert!(
            warning.contains("0 traces before the track's first fix and"),
            "{warning}"
        );
        let last = built.rows[TRACES - 1];
        assert_eq!(
            (last.lon, last.lat),
            (built.rows[TRACES - 2].lon, built.rows[TRACES - 2].lat)
        );
    }

    #[test]
    fn what_cannot_be_right_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let rad = recording(dir.path(), false);
        let csv = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let past = csv("past.csv", "trace,lon,lat\n0,16,78\n3548,16.1,78\n");
        let untraced = csv("untraced.csv", "lon,lat\n16,78\n");
        let timed = csv(
            "timed.csv",
            "trace,lon,lat,time\n0,16,78,2025-03-27T12:00:00Z\n",
        );
        let lonlat = csv("lonlat.csv", "trace,lon,lat\n0,16,78\n");
        let backwards = csv(
            "backwards.csv",
            "trace,lon,lat,time\n0,16,78,2025-03-27T12:00:01Z\n1,16,78,2025-03-27T12:00:00Z\n",
        );

        let refused = |change: &dyn Fn(&mut Request)| {
            let mut request = request(rad.clone());
            change(&mut request);
            build(&request).unwrap_err()
        };
        for (error, expected) in [
            (refused(&|_| {}), "has no positions of its own"),
            (
                refused(&|r| r.positions = Some(past.clone())),
                "trace 3548 is past the last trace",
            ),
            (
                refused(&|r| {
                    r.positions = Some(timed.clone());
                    r.start_time = Some("2025-03-27T12:00:00Z".into());
                }),
                "have times, so a start time",
            ),
            (
                refused(&|r| {
                    r.positions = Some(lonlat.clone());
                    r.src_crs = Some("EPSG:32633".into());
                }),
                "name the columns x and y",
            ),
            (
                refused(&|r| r.positions = Some(backwards.clone())),
                "earlier than the row before",
            ),
            (
                refused(&|r| r.positions = Some(untraced.clone())),
                "has no trace column",
            ),
            (
                refused(&|r| r.track = Some(lonlat.clone())),
                "is not a track",
            ),
            (
                refused(&|r| {
                    r.endpoints = Some(on_the_dem());
                    r.time_offset = Some(1.);
                }),
                "only applies to a track",
            ),
            (
                refused(&|r| {
                    r.endpoints = Some(on_the_dem());
                    r.track = Some(timed.clone());
                }),
                "give one of positions",
            ),
            (
                refused(&|r| r.time_interval = Some(-1.)),
                "has no positions of its own",
            ),
            (
                refused(&|r| {
                    r.endpoints = Some(on_the_dem());
                    r.time_interval = Some(0.);
                }),
                "is not positive",
            ),
        ] {
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }
}
