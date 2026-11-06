//! UAV GPR recorded by SPH Engineering's SkyHub: a Radsys zGPR (such as the
//! Zond Aero LF, `zAeroLF`) written as SEG-Y (#561).
//!
//! The SEG-Y is revision 0 but **little-endian**, with int32 samples. Its
//! textual header is ASCII, not EBCDIC, and names SkyHub and the zGPR,
//! which is how a SkyHub file is told from any other `.sgy`. The binary
//! header gives the sample interval in picoseconds and the sample count.
//! Each trace header gives:
//!
//! - bytes 1-4 (`tracl`): the trace number within the whole recording,
//!   from 0, and the key into the positions files. Byte 21 is not: it is a
//!   constant in some files.
//! - byte 41: the altitude relative to take-off, as float32.
//! - bytes 73/77: longitude and latitude as float32 in NMEA `DDMM.mmmm`,
//!   whatever `scalco` and `counit` say, so to about 1 m.
//! - bytes 157-165: year, day of year, hour, minute and second, UTC.
//!
//! Sample 0 is a constant marker, not data, and the reader drops it as a
//! one-sample crop. The direct wave saturates at `0x7FFF0000`.
//!
//! A recording comes in one of two layouts:
//!
//! - As SkyHub writes it: `<stamp>-gpr.sgy` with `<stamp>-position.csv`,
//!   one row per GNSS or trace event, keyed to the traces by its
//!   `zGPR:Trace` column. Its RTK columns are absolute; its standalone
//!   `Altitude` is relative to take-off. `Elapsed` is Unix time in ms.
//! - Cut into parts by other software: `<stem>_001.sgy`, `<stem>_002.sgy`,
//!   ... with one `<stem>.txt` of trace, latitude, longitude and altitude
//!   relative to take-off for the whole recording, so a part's traces do
//!   not start at 0.
//!
//! Without either, the trace headers' own positions are read.
//!
//! What the files do not record: the antenna frequency (the controller
//! takes interchangeable dipoles, so it is never inferred) and the antenna
//! separation. The first must be given; the separation is 0 m without one.
//! The `Pulse delay` of the textual header is a unitless hardware setting,
//! calibrated per antenna and unit, and is only reported.

use std::error::Error;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use ndarray::Array2;

use crate::gpr;

/// The textual header, in bytes.
const TEXT_BYTES: usize = 3200;
/// The textual and binary headers together, before the first trace.
const FILE_HEADER_BYTES: usize = 3600;
/// A trace's header, in bytes.
const TRACE_HEADER_BYTES: usize = 240;
/// SEG-Y's data sample format code for 4-byte two's complement integers.
const FORMAT_INT32: u16 = 2;
/// The value the zGPR saturates at. Anything this far from zero, of either
/// sign, is clipped.
const SATURATED: i32 = 0x7FFF_0000;

/// What a SkyHub SEG-Y's file headers say.
#[derive(Debug, Clone, PartialEq)]
struct FileHeader {
    /// The textual header's first line, naming the software that wrote it.
    written_by: String,
    /// The `Key: value` lines of the textual header, in order.
    settings: Vec<(String, String)>,
    samples: usize,
    /// The sample interval, in picoseconds.
    interval_ps: u16,
    /// The whole traces in the file. A trace cut short at the end, as an
    /// interrupted recording leaves, is not counted.
    traces: usize,
}

impl FileHeader {
    fn trace_bytes(&self) -> usize {
        TRACE_HEADER_BYTES + 4 * self.samples
    }

    /// The value of the textual header's `key` line.
    fn setting(&self, key: &str) -> Option<&str> {
        self.settings
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// Read the file headers of `path`, refusing anything but a SkyHub SEG-Y.
fn file_header(path: &Path) -> Result<FileHeader, Box<dyn Error>> {
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len() as usize;
    let mut bytes = vec![0u8; FILE_HEADER_BYTES.min(length)];
    file.read_exact(&mut bytes)?;
    parse_file_header(&bytes, length, path)
}

/// [`file_header`] of `bytes`, at least the first 3600 bytes of a file of
/// `length` bytes.
fn parse_file_header(
    bytes: &[u8],
    length: usize,
    path: &Path,
) -> Result<FileHeader, Box<dyn Error>> {
    let not_skyhub = |why: String| -> Box<dyn Error> {
        format!(
            "{} is not a SEG-Y file written by SPH Engineering's SkyHub, the only SEG-Y Ridal \
             reads: {why}",
            path.display()
        )
        .into()
    };
    if bytes.len() < FILE_HEADER_BYTES {
        return Err(not_skyhub(format!(
            "it is {length} bytes, too short for a SEG-Y file header"
        )));
    }
    let text = &bytes[..TEXT_BYTES];
    if !text.iter().all(|b| b.is_ascii()) {
        return Err(not_skyhub(
            "its textual header is not ASCII (an EBCDIC header is the standard's, which \
             SkyHub does not write)"
                .to_string(),
        ));
    }
    let text = String::from_utf8_lossy(text).replace('\0', "");
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    // "This SEG-Y file has been generated by UgCS SkyHub v4.2.1.3219"
    let first = lines.first().copied().unwrap_or_default();
    let written_by = first
        .split_once("generated by ")
        .map_or(first, |(_, by)| by)
        .to_string();
    let lower = text.to_ascii_lowercase();
    if !lower.contains("skyhub") || !lower.contains("zgpr") {
        return Err(not_skyhub(format!(
            "its textual header does not name SkyHub and a zGPR (it starts {:?})",
            first.chars().take(80).collect::<String>()
        )));
    }
    let settings = lines
        .iter()
        .filter_map(|line| line.split_once(": "))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();

    let u16_at = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    let interval_ps = u16_at(3216);
    let samples = usize::from(u16_at(3220));
    let format = u16_at(3224);
    if format != FORMAT_INT32 {
        return Err(not_skyhub(format!(
            "its samples are in format {format} (little-endian), not 2, the 32-bit integers \
             SkyHub writes"
        )));
    }
    if samples == 0 || interval_ps == 0 {
        return Err(not_skyhub(format!(
            "its binary header gives {samples} samples {interval_ps} ps apart"
        )));
    }
    let trace_bytes = TRACE_HEADER_BYTES + 4 * samples;
    let traces = (length - FILE_HEADER_BYTES) / trace_bytes;
    if traces == 0 {
        return Err(format!("{} holds no whole trace", path.display()).into());
    }
    Ok(FileHeader {
        written_by,
        settings,
        samples,
        interval_ps,
        traces,
    })
}

/// What a trace header says, of what the reader uses.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TraceHeader {
    /// The trace number within the whole recording.
    number: i32,
    /// Degrees, from the NMEA `DDMM.mmmm` the header holds.
    longitude: f64,
    latitude: f64,
    /// Metres above take-off.
    altitude: f64,
    /// Seconds since the epoch, to the second, if the date is valid.
    seconds: Option<i64>,
}

/// Degrees of an NMEA `DDMM.mmmm` value, its sign that of the hemisphere.
fn nmea_degrees(value: f64) -> f64 {
    let magnitude = value.abs();
    let degrees = (magnitude / 100.).trunc();
    value.signum() * (degrees + (magnitude - 100. * degrees) / 60.)
}

fn parse_trace_header(bytes: &[u8]) -> TraceHeader {
    let i32_at = |byte: usize| i32::from_le_bytes(bytes[byte - 1..byte + 3].try_into().unwrap());
    let i16_at = |byte: usize| i16::from_le_bytes([bytes[byte - 1], bytes[byte]]);
    let f32_at = |byte: usize| f32::from_le_bytes(bytes[byte - 1..byte + 3].try_into().unwrap());
    let seconds = chrono::NaiveDate::from_yo_opt(i32::from(i16_at(157)), i16_at(159) as u32)
        .and_then(|date| {
            date.and_hms_opt(i16_at(161) as u32, i16_at(163) as u32, i16_at(165) as u32)
        })
        .map(|time| time.and_utc().timestamp());
    TraceHeader {
        number: i32_at(1),
        longitude: nmea_degrees(f64::from(f32_at(73))),
        latitude: nmea_degrees(f64::from(f32_at(77))),
        altitude: f64::from(f32_at(41)),
        seconds,
    }
}

/// Every trace header of a file.
fn trace_headers(path: &Path, header: &FileHeader) -> Result<Vec<TraceHeader>, Box<dyn Error>> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    reader.seek(std::io::SeekFrom::Start(FILE_HEADER_BYTES as u64))?;
    let skip = (header.trace_bytes() - TRACE_HEADER_BYTES) as i64;
    let mut bytes = [0u8; TRACE_HEADER_BYTES];
    let mut out = Vec::with_capacity(header.traces);
    for _ in 0..header.traces {
        reader.read_exact(&mut bytes)?;
        out.push(parse_trace_header(&bytes));
        reader.seek_relative(skip)?;
    }
    Ok(out)
}

/// The trace header of trace `index` (from 0) alone.
fn trace_header(
    path: &Path,
    header: &FileHeader,
    index: usize,
) -> Result<TraceHeader, Box<dyn Error>> {
    let mut file = std::fs::File::open(path)?;
    file.seek(std::io::SeekFrom::Start(
        (FILE_HEADER_BYTES + index * header.trace_bytes()) as u64,
    ))?;
    let mut bytes = [0u8; TRACE_HEADER_BYTES];
    file.read_exact(&mut bytes)?;
    Ok(parse_trace_header(&bytes))
}

/// The file beside `sgy` that holds its positions, if there is one: the
/// `<stamp>-position.csv` of a `<stamp>-gpr.sgy`, or the `<stem>.txt` of a
/// part `<stem>_NNN.sgy`.
pub fn skyhub_positions_beside(sgy: &Path) -> Option<PathBuf> {
    let dir = sgy.parent().unwrap_or_else(|| Path::new("."));
    let name = sgy.file_name()?.to_str()?;
    skyhub_positions_file_name(name)
        .and_then(|candidate| crate::formats::find_named(dir, &candidate))
}

/// The name of the positions file of a SEG-Y named `name`, by the two
/// layouts' rules, whether or not it exists.
pub fn skyhub_positions_file_name(name: &str) -> Option<String> {
    let stem = name.get(..name.len().checked_sub(4)?)?;
    if !name[stem.len()..].eq_ignore_ascii_case(".sgy") {
        return None;
    }
    if let Some(stamp) = stem
        .len()
        .checked_sub(4)
        .filter(|&i| stem.is_char_boundary(i) && stem[i..].eq_ignore_ascii_case("-gpr"))
        .map(|i| &stem[..i])
    {
        return Some(format!("{stamp}-position.csv"));
    }
    let (base, part) = stem.rsplit_once('_')?;
    (!base.is_empty() && !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{base}.txt"))
}

/// The SEG-Y a `<stamp>-position.csv` belongs to, which it stands for as a
/// `.cor` does for its `.rad`.
pub fn skyhub_recording_of_positions(csv: &Path) -> Option<PathBuf> {
    let name = csv.file_name()?.to_str()?;
    let split = name.len().checked_sub("-position.csv".len())?;
    if split == 0
        || !name.is_char_boundary(split)
        || !name[split..].eq_ignore_ascii_case("-position.csv")
    {
        return None;
    }
    Some(csv.with_file_name(format!("{}-gpr.sgy", &name[..split])))
}

/// Whether `path` is named as a SkyHub positions file, `<stamp>-position.csv`,
/// which is not a Ridal coordinate CSV.
pub fn is_skyhub_position_csv(path: &Path) -> bool {
    skyhub_recording_of_positions(path).is_some()
}

/// Where a recording's positions came from, and so what its altitudes
/// are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkyHubPositions {
    /// The RTK columns of the `-position.csv`: absolute altitudes of the
    /// aircraft, in a vertical datum the files do not name.
    Rtk,
    /// The standalone columns of the `-position.csv`, used when no row has
    /// RTK: altitudes relative to take-off.
    Standalone,
    /// The `.txt` of a recording cut into parts: altitudes relative to
    /// take-off.
    CutText,
    /// The trace headers' own, to about 1 m: altitudes relative to take-off.
    TraceHeaders,
}

impl SkyHubPositions {
    /// Whether the altitudes are relative to take-off rather than absolute.
    pub fn relative_altitude(self) -> bool {
        self != SkyHubPositions::Rtk
    }

    /// The source, for warnings.
    pub fn name(self) -> &'static str {
        match self {
            SkyHubPositions::Rtk => "RTK positions",
            SkyHubPositions::Standalone => "standalone GNSS positions",
            SkyHubPositions::CutText => "positions file",
            SkyHubPositions::TraceHeaders => "trace headers",
        }
    }

    /// The source and its altitudes in words, for `info` and the log.
    pub fn describe(self) -> &'static str {
        match self {
            SkyHubPositions::Rtk => "RTK positions (absolute altitude of the aircraft)",
            SkyHubPositions::Standalone => {
                "standalone GNSS positions, without RTK (altitude relative to take-off)"
            }
            SkyHubPositions::CutText => "positions file (altitude relative to take-off)",
            SkyHubPositions::TraceHeaders => {
                "positions in the trace headers, to about 1 m (altitude relative to take-off)"
            }
        }
    }
}

/// A position known at `at` (a time or a trace number), in WGS84 degrees
/// and metres.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fix {
    at: f64,
    longitude: f64,
    latitude: f64,
    altitude: f64,
}

/// The value of `known` (sorted by `.0`) at `x`, linear between them. Beyond
/// the ends it holds the end value, or with `extrapolate`, continues the
/// end segment.
fn interpolate(known: &[(f64, f64)], x: f64, extrapolate: bool) -> f64 {
    let last = known.len() - 1;
    let i = known.partition_point(|(k, _)| *k <= x);
    let (a, b) = match i {
        0 if !extrapolate || last == 0 => return known[0].1,
        0 => (known[0], known[1]),
        i if i > last && (!extrapolate || last == 0) => return known[last].1,
        i if i > last => (known[last - 1], known[last]),
        i => (known[i - 1], known[i]),
    };
    if b.0 == a.0 {
        return a.1;
    }
    a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0)
}

/// The fixes that each start a run of identical positions: a GNSS that
/// updates more slowly than its values are logged repeats them, and the
/// position is interpolated between updates rather than stepped.
fn first_of_runs(fixes: impl IntoIterator<Item = Fix>) -> Vec<Fix> {
    let mut out: Vec<Fix> = Vec::new();
    for fix in fixes {
        let same = out.last().is_some_and(|last| {
            (last.longitude, last.latitude, last.altitude)
                == (fix.longitude, fix.latitude, fix.altitude)
        });
        if !same && out.last().is_none_or(|last| fix.at > last.at) {
            out.push(fix);
        }
    }
    out
}

/// Each of `at`, positioned by interpolating `fixes` (sorted by `at`).
fn positions_at(fixes: &[Fix], at: &[f64]) -> Vec<(f64, f64, f64)> {
    let column = |f: fn(&Fix) -> f64| fixes.iter().map(|x| (x.at, f(x))).collect::<Vec<_>>();
    let (lon, lat, alt) = (
        column(|f| f.longitude),
        column(|f| f.latitude),
        column(|f| f.altitude),
    );
    at.iter()
        .map(|&x| {
            (
                interpolate(&lon, x, false),
                interpolate(&lat, x, false),
                interpolate(&alt, x, false),
            )
        })
        .collect()
}

/// Each trace's time in seconds, from the trace headers' whole seconds: the
/// first trace of each new second is that second, and the traces between
/// are interpolated.
fn header_times(headers: &[TraceHeader], path: &Path) -> Result<Vec<f64>, Box<dyn Error>> {
    let mut known: Vec<(f64, f64)> = Vec::new();
    for (i, header) in headers.iter().enumerate() {
        let Some(seconds) = header.seconds else {
            continue;
        };
        let seconds = seconds as f64;
        if known.last().is_none_or(|(_, last)| seconds > *last) {
            known.push((i as f64, seconds));
        }
    }
    if known.is_empty() {
        return Err(format!("No trace header of {} gives a valid date", path.display()).into());
    }
    Ok((0..headers.len())
        .map(|i| interpolate(&known, i as f64, true))
        .collect())
}

/// The columns of a SkyHub `-position.csv` that the reader uses.
struct PositionCsv {
    /// Each row's `Elapsed` (Unix ms) and its trace, if it has one.
    rows: Vec<(f64, Option<i64>)>,
    rtk: Vec<Fix>,
    standalone: Vec<Fix>,
}

fn read_position_csv(path: &Path) -> Result<PositionCsv, Box<dyn Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let names = lines
        .next()
        .ok_or_else(|| format!("{} is empty", path.display()))?
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>();
    let column = |name: &str| -> Result<usize, Box<dyn Error>> {
        names.iter().position(|n| *n == name).ok_or_else(|| {
            format!(
                "{} has no {name:?} column, which a SkyHub positions file has",
                path.display()
            )
            .into()
        })
    };
    let elapsed = column("Elapsed")?;
    let trace = column("zGPR:Trace")?;
    let status = column("RTK Status")?;
    let standalone_columns = [
        column("Longitude")?,
        column("Latitude")?,
        column("Altitude")?,
    ];
    let rtk_columns = [
        column("Longitude RTK")?,
        column("Latitude RTK")?,
        column("Altitude RTK")?,
    ];
    let mut out = PositionCsv {
        rows: Vec::new(),
        rtk: Vec::new(),
        standalone: Vec::new(),
    };
    for (number, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
        let field = |i: usize| fields.get(i).copied().unwrap_or("");
        let Ok(ms) = field(elapsed).parse::<f64>() else {
            return Err(format!(
                "Row {} of {} has no valid Elapsed time: {:?}",
                number + 2,
                path.display(),
                field(elapsed)
            )
            .into());
        };
        let trace_number = match field(trace) {
            "" => None,
            text => Some(text.parse::<i64>().map_err(|_| {
                format!(
                    "Row {} of {} has an invalid zGPR:Trace {text:?}",
                    number + 2,
                    path.display()
                )
            })?),
        };
        out.rows.push((ms, trace_number));
        let fix = |[lon, lat, alt]: [usize; 3]| -> Option<Fix> {
            let parse = |i: usize| field(i).parse::<f64>().ok().filter(|v| v.is_finite());
            let (longitude, latitude, altitude) = (parse(lon)?, parse(lat)?, parse(alt)?);
            // An unset fix is logged as zeros.
            (longitude != 0. || latitude != 0.).then_some(Fix {
                at: ms / 1000.,
                longitude,
                latitude,
                altitude,
            })
        };
        if field(status).eq_ignore_ascii_case("ON") {
            out.rtk.extend(fix(rtk_columns));
        }
        out.standalone.extend(fix(standalone_columns));
    }
    Ok(out)
}

/// Each trace's time in seconds, from the `Elapsed` of its row in the
/// `-position.csv`, matched by trace number. A trace without a row gets a
/// time interpolated from its neighbours'.
fn csv_times(
    rows: &[(f64, Option<i64>)],
    headers: &[TraceHeader],
    path: &Path,
) -> Result<Vec<f64>, Box<dyn Error>> {
    let mut known = rows
        .iter()
        .filter_map(|(ms, trace)| trace.map(|t| (t as f64, ms / 1000.)))
        .collect::<Vec<_>>();
    known.sort_by(|a, b| a.0.total_cmp(&b.0));
    known.dedup_by(|b, a| a.0 == b.0);
    if known.is_empty() {
        return Err(format!("No row of {} names a trace (zGPR:Trace)", path.display()).into());
    }
    Ok(headers
        .iter()
        .map(|h| interpolate(&known, f64::from(h.number), true))
        .collect())
}

/// Read the `.txt` of a recording cut into parts: two header lines, then
/// trace, latitude, longitude and altitude, tab-separated.
fn read_cut_text(path: &Path) -> Result<Vec<Fix>, Box<dyn Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut fixes = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        // The header lines, `Coordinate Units:Deg` and the column names,
        // start with a word.
        if fields.first().is_none_or(|f| f.parse::<f64>().is_err()) {
            continue;
        }
        let parse = |i: usize| -> Result<f64, Box<dyn Error>> {
            fields
                .get(i)
                .and_then(|f| f.parse::<f64>().ok())
                .ok_or_else(|| {
                    format!(
                        "Line {} of {} is not trace, latitude, longitude and altitude: {line:?}",
                        number + 1,
                        path.display()
                    )
                    .into()
                })
        };
        fixes.push(Fix {
            at: parse(0)?,
            latitude: parse(1)?,
            longitude: parse(2)?,
            altitude: parse(3)?,
        });
    }
    fixes.sort_by(|a, b| a.at.total_cmp(&b.at));
    Ok(fixes)
}

/// Which source the positions file `positions` of `sgy` is.
fn source_of(sgy: &Path, positions: &Path) -> Result<SkyHubPositions, Box<dyn Error>> {
    if positions == sgy {
        return Ok(SkyHubPositions::TraceHeaders);
    }
    if is_skyhub_position_csv(positions) {
        let csv = read_position_csv(positions)?;
        return Ok(match csv.rtk.is_empty() {
            false => SkyHubPositions::Rtk,
            true => SkyHubPositions::Standalone,
        });
    }
    let is_txt = positions
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
    if is_txt {
        return Ok(SkyHubPositions::CutText);
    }
    Err(format!(
        "{} cannot hold the positions of {}: a SkyHub recording's positions are its \
         <stamp>-position.csv, the <stem>.txt of a recording cut into parts, its own trace \
         headers, or a Ridal coordinate CSV (<stem>.coords.csv).",
        positions.display(),
        sgy.display()
    )
    .into())
}

/// The positions of a SkyHub recording `sgy`, from `positions`: its
/// `-position.csv` (RTK where any row has it), the `.txt` of a recording
/// cut into parts, or `sgy` itself for the trace headers'. One point per
/// trace, and which source it was.
///
/// # Errors
/// A file that is not a SkyHub SEG-Y, or positions that cannot be read or
/// give no position.
pub fn load_skyhub_positions(
    sgy: &Path,
    positions: &Path,
    projected_crs: Option<&String>,
) -> Result<(gpr::GPRLocation, SkyHubPositions), Box<dyn Error>> {
    let header = file_header(sgy)?;
    let headers = trace_headers(sgy, &header)?;
    let source = source_of(sgy, positions)?;
    let none_in = |what: &str| -> Box<dyn Error> {
        format!("{} gives no {what} position", positions.display()).into()
    };
    let (times, located) = match source {
        SkyHubPositions::Rtk | SkyHubPositions::Standalone => {
            let csv = read_position_csv(positions)?;
            let times = csv_times(&csv.rows, &headers, positions)?;
            let fixes = first_of_runs(match source {
                SkyHubPositions::Rtk => csv.rtk,
                _ => csv.standalone,
            });
            if fixes.is_empty() {
                return Err(none_in("GNSS"));
            }
            let located = positions_at(&fixes, &times);
            (times, located)
        }
        SkyHubPositions::CutText => {
            let fixes = first_of_runs(read_cut_text(positions)?);
            if fixes.is_empty() {
                return Err(none_in("trace"));
            }
            let numbers = headers
                .iter()
                .map(|h| f64::from(h.number))
                .collect::<Vec<_>>();
            (header_times(&headers, sgy)?, positions_at(&fixes, &numbers))
        }
        SkyHubPositions::TraceHeaders => {
            let fixes = first_of_runs(
                headers
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| h.longitude != 0. || h.latitude != 0.)
                    .map(|(i, h)| Fix {
                        at: i as f64,
                        longitude: h.longitude,
                        latitude: h.latitude,
                        altitude: h.altitude,
                    }),
            );
            if fixes.is_empty() {
                return Err(none_in("trace header"));
            }
            let indices = (0..headers.len()).map(|i| i as f64).collect::<Vec<_>>();
            (header_times(&headers, sgy)?, positions_at(&fixes, &indices))
        }
    };
    let wgs84 = located
        .iter()
        .map(|&(x, y, _)| crate::coords::Coord { x, y })
        .collect::<Vec<_>>();
    let mut points = times
        .iter()
        .zip(&located)
        .enumerate()
        .map(|(i, (&time_seconds, &(_, _, altitude)))| gpr::CorPoint {
            trace_n: i as u32,
            time_seconds,
            easting: 0.,
            northing: 0.,
            altitude,
        })
        .collect::<Vec<_>>();
    let crs = super::project_points(&mut points, &wgs84, projected_crs)?;
    Ok((
        gpr::GPRLocation {
            cor_points: points,
            correction: gpr::LocationCorrection::None,
            time_source: gpr::TimeSource::Recorded,
            crs,
        },
        source,
    ))
}

/// The header of a SkyHub SEG-Y.
///
/// # Errors
/// A file that is not a SkyHub SEG-Y, or no antenna frequency given.
pub fn load_skyhub_meta(
    path: &Path,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let header = file_header(path)?;
    let antenna = header.setting("Unit Type").unwrap_or("zGPR").to_string();
    let antenna_mhz = override_antenna_mhz.ok_or_else(|| {
        super::UnknownAntennaFrequency::new(
            antenna.as_str(),
            format!(
                "SkyHub files do not record the antenna frequency, and the {antenna} takes \
                 antennas of several. Give it with --override antenna_mhz=MHZ (overrides= in \
                 Python), or as antenna_mhz in a <stem>.ridal.toml beside the file"
            ),
        )
    })?;
    // Whole seconds over the recording, which is enough for an average.
    let first = trace_header(path, &header, 0)?.seconds;
    let last = trace_header(path, &header, header.traces - 1)?.seconds;
    let time_interval = match (first, last) {
        (Some(first), Some(last)) if header.traces > 1 && last > first => {
            (last - first) as f32 / (header.traces - 1) as f32
        }
        _ => 0.,
    };
    let frequency = 1e6 / f32::from(header.interval_ps);
    Ok(gpr::GPRMeta {
        samples: header.samples as u32,
        frequency,
        frequency_steps: 0,
        time_interval,
        antenna,
        antenna_mhz,
        antenna_separation: override_antenna_separation.unwrap_or(0.),
        time_window: 1000. * header.samples as f32 / frequency,
        last_trace: header.traces as u32,
        data_filepath: path.to_path_buf(),
        medium_velocity,
    })
}

/// The samples of every trace of a SkyHub SEG-Y, samples by traces, as
/// recorded: sample 0 is the marker, which the caller crops.
pub fn load_skyhub_data(path: &Path) -> Result<Array2<f32>, Box<dyn Error>> {
    let header = file_header(path)?;
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    reader.seek(std::io::SeekFrom::Start(FILE_HEADER_BYTES as u64))?;
    let mut trace = vec![0u8; header.trace_bytes()];
    let mut data = Vec::with_capacity(header.samples * header.traces);
    for _ in 0..header.traces {
        reader.read_exact(&mut trace)?;
        data.extend(
            trace[TRACE_HEADER_BYTES..]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&bytes| i32::from_le_bytes(bytes) as f32),
        );
    }
    Ok(Array2::from_shape_vec((header.traces, header.samples), data)?.reversed_axes())
}

/// The share of the samples (sample 0 left out) at the zGPR's saturation,
/// from 0 to 1.
fn clipped_fraction(path: &Path) -> Result<f64, Box<dyn Error>> {
    let header = file_header(path)?;
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    reader.seek(std::io::SeekFrom::Start(FILE_HEADER_BYTES as u64))?;
    let mut trace = vec![0u8; header.trace_bytes()];
    let mut clipped = 0usize;
    for _ in 0..header.traces {
        reader.read_exact(&mut trace)?;
        clipped += trace[TRACE_HEADER_BYTES + 4..]
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|&&bytes| i32::from_le_bytes(bytes).unsigned_abs() >= SATURATED as u32)
            .count();
    }
    Ok(clipped as f64 / (header.traces * (header.samples - 1)).max(1) as f64)
}

/// Which source the positions of `sgy` in `positions` are, without reading
/// them, for a recording whose positions are its own (not a coordinate
/// CSV).
pub fn skyhub_positions_source(
    sgy: &Path,
    positions: &Path,
) -> Result<SkyHubPositions, Box<dyn Error>> {
    source_of(sgy, positions)
}

/// What `ridal info` reports of a SkyHub recording beyond its metadata:
/// the textual header's settings, the software that wrote it, where its
/// positions are from (`None` for a coordinate CSV) and the share of
/// clipped samples.
pub fn skyhub_settings(
    sgy: &Path,
    positions: Option<&Path>,
) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let header = file_header(sgy)?;
    let mut out = vec![("Written by".to_string(), header.written_by.clone())];
    for key in [
        "Control Unit",
        "Unit Type",
        "Time range",
        "Pulse delay",
        "Sounding medium",
    ] {
        if let Some(value) = header.setting(key) {
            out.push((key.to_string(), value.to_string()));
        }
    }
    if let Some(positions) = positions {
        let source = source_of(sgy, positions)?;
        out.push(("Positions".to_string(), source.describe().to_string()));
    }
    out.push((
        "Clipped samples".to_string(),
        format!("{:.2} %", 100. * clipped_fraction(sgy)?),
    ));
    Ok(out)
}

#[cfg(test)]
pub(crate) mod test_sgy {
    //! Synthetic SkyHub SEG-Y files, with the quirks of real ones.

    use std::path::Path;

    /// One trace to write: its number in the recording, its time, its
    /// header position (degrees) and altitude, and its samples (the marker
    /// first).
    #[derive(Debug, Clone)]
    pub struct Trace {
        pub number: i32,
        /// Year, day of year, hour, minute, second.
        pub time: (i16, i16, i16, i16, i16),
        pub longitude: f64,
        pub latitude: f64,
        pub altitude: f32,
        pub samples: Vec<i32>,
    }

    /// Degrees as the NMEA `DDMM.mmmm` the headers hold.
    fn nmea(degrees: f64) -> f32 {
        let whole = degrees.abs().trunc();
        (degrees.signum() * (whole * 100. + (degrees.abs() - whole) * 60.)) as f32
    }

    /// A SkyHub SEG-Y of `traces`, with `textual` as its textual header
    /// and `interval_ps` between samples.
    pub fn sgy(textual: &str, interval_ps: u16, traces: &[Trace]) -> Vec<u8> {
        let samples = traces[0].samples.len();
        let mut text = textual.replace('\n', "\r\n").into_bytes();
        text.resize(super::TEXT_BYTES, 0);
        let mut binary = vec![0u8; 400];
        binary[16..18].copy_from_slice(&interval_ps.to_le_bytes());
        binary[20..22].copy_from_slice(&(samples as u16).to_le_bytes());
        binary[24..26].copy_from_slice(&2u16.to_le_bytes());
        let mut out = [text, binary].concat();
        for trace in traces {
            let mut header = vec![0u8; super::TRACE_HEADER_BYTES];
            let mut put = |byte: usize, bytes: &[u8]| {
                header[byte - 1..byte - 1 + bytes.len()].copy_from_slice(bytes)
            };
            put(1, &trace.number.to_le_bytes());
            put(21, &10001i32.to_le_bytes());
            put(41, &trace.altitude.to_le_bytes());
            put(71, &(-1000i16).to_le_bytes());
            put(73, &nmea(trace.longitude).to_le_bytes());
            put(77, &nmea(trace.latitude).to_le_bytes());
            put(89, &2i16.to_le_bytes());
            put(115, &(samples as i16).to_le_bytes());
            put(117, &(interval_ps as i16).to_le_bytes());
            let (y, d, h, m, s) = trace.time;
            for (byte, value) in [(157, y), (159, d), (161, h), (163, m), (165, s)] {
                put(byte, &value.to_le_bytes());
            }
            out.extend(header);
            for sample in &trace.samples {
                out.extend(sample.to_le_bytes());
            }
        }
        out
    }

    /// A textual header as SkyHub writes it.
    pub fn textual() -> String {
        "This SEG-Y file has been generated by UgCS SkyHub v4.2.1.3219\n\
         Copyright (c) 2018-2021 SPH Engineering\n\n\nE=9\n\n\
         Georadar's settings information:\n\n\
         Date: 01.03.2024\nTime: 12:00:00\n\n\
         Control Unit: RadSys zGPR\nUnit Type: zAeroLF\n\n\
         Samples per trace: 6\nTime range: 1.5 ns\nSounding medium: 9\nPulse delay: 300\n"
            .to_string()
    }

    /// `count` traces of 6 samples, numbered from `first`, 1 s apart from
    /// 12:00:00 UTC on 2024-03-01 (day 61), moving north.
    pub fn traces(first: i32, count: usize) -> Vec<Trace> {
        (0..count)
            .map(|i| Trace {
                number: first + i as i32,
                time: (2024, 61, 12, 0, i as i16),
                longitude: 10.0,
                latitude: 60.0 + 0.0001 * i as f64,
                altitude: 5. + i as f32,
                samples: [65024, super::SATURATED, -3, 10 * i as i32, 0, 7].to_vec(),
            })
            .collect()
    }

    /// The simple raw layout, `<dir>/2024-03-01-12-00-00-gpr.sgy`, without a
    /// positions file, for tests outside this module.
    pub fn write_simple(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("2024-03-01-12-00-00-gpr.sgy");
        std::fs::write(&path, sgy(&textual(), 250, &traces(0, 4))).unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use super::test_sgy::{sgy, textual, traces};
    use super::*;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A `-position.csv` for traces 0-3 of [`traces`]: GNSS rows without a
    /// trace between them, a position repeated over two rows, and no row
    /// for trace 2. `status` is the RTK status of every row.
    fn position_csv(status: &str) -> String {
        let mut out = String::from(
            "Elapsed,Date,Time,Pitch,Roll,Heading,Latitude,Longitude,Altitude,Next WP,Velocity,\
             RTK Status,Latitude RTK,Longitude RTK,Altitude RTK,ALT:Altitude,ALT:ID,\
             OBS:Sectors1,OBS:Sectors2,OBS:Sectors3,OBS:ID,zGPR:Trace\n",
        );
        let start = 1_709_294_400_000_i64; // 2024-03-01T12:00:00Z
                                           // (ms after start, trace, latitude): the GNSS moves 0.001 degrees
                                           // a second, and repeats its position until it updates.
        let rows: [(i64, Option<i32>, f64); 6] = [
            (0, Some(0), 50.0),
            (500, None, 50.0),
            (1000, Some(1), 50.001),
            (1500, None, 50.0015),
            (3000, Some(3), 50.003),
            (3500, None, 50.003),
        ];
        for (ms, trace, lat) in rows {
            let trace = trace.map(|t| t.to_string()).unwrap_or_default();
            out.push_str(&format!(
                "{},2024/03/01,12:00:00.000,0,0,0,{},12.0,{},,0,{status},{},12.0,{},5.0,1,,,,,{trace}\n",
                start + ms,
                lat + 1.,
                10.,
                lat,
                1000. + (lat - 50.) * 1000.,
            ));
        }
        out
    }

    #[test]
    fn a_skyhub_header_is_read_and_any_other_segy_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "a-gpr.sgy",
            &sgy(&textual(), 250, &traces(0, 3)),
        );
        let header = file_header(&path).unwrap();
        assert_eq!(header.samples, 6);
        assert_eq!(header.interval_ps, 250);
        assert_eq!(header.traces, 3);
        assert_eq!(header.setting("unit type"), Some("zAeroLF"));
        assert_eq!(header.setting("Pulse delay"), Some("300"));
        assert_eq!(header.written_by, "UgCS SkyHub v4.2.1.3219");

        // A trace cut short at the end is not read.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend([0u8; 100]);
        let cut = write(dir.path(), "cut-gpr.sgy", &bytes);
        assert_eq!(file_header(&cut).unwrap().traces, 3);

        let other = textual().replace("SkyHub", "Something");
        let path = write(dir.path(), "b.sgy", &sgy(&other, 250, &traces(0, 3)));
        let error = file_header(&path).unwrap_err().to_string();
        assert!(error.contains("does not name SkyHub"), "{error}");
        assert!(error.contains("This SEG-Y file has been"), "{error}");

        let mut ebcdic = sgy(&textual(), 250, &traces(0, 3));
        ebcdic[0] = 0xC3;
        let path = write(dir.path(), "c.sgy", &ebcdic);
        let error = file_header(&path).unwrap_err().to_string();
        assert!(error.contains("not ASCII"), "{error}");

        let mut float = sgy(&textual(), 250, &traces(0, 3));
        float[3224] = 5;
        let path = write(dir.path(), "d.sgy", &float);
        let error = file_header(&path).unwrap_err().to_string();
        assert!(error.contains("format 5"), "{error}");

        let path = write(dir.path(), "e.sgy", b"short");
        assert!(file_header(&path).is_err());
    }

    #[test]
    fn nmea_positions_are_degrees_and_minutes() {
        assert!((nmea_degrees(5012.3456) - (50. + 12.3456 / 60.)).abs() < 1e-12);
        assert!((nmea_degrees(-650.0) - -(6. + 50. / 60.)).abs() < 1e-12);
    }

    #[test]
    fn positions_files_are_named_by_the_layouts_rules() {
        assert_eq!(
            skyhub_positions_file_name("2024-03-01-10-00-00-gpr.sgy").as_deref(),
            Some("2024-03-01-10-00-00-position.csv")
        );
        assert_eq!(
            skyhub_positions_file_name("2024-03-02-09-00-00-gpr-0_003.SGY").as_deref(),
            Some("2024-03-02-09-00-00-gpr-0.txt")
        );
        for name in ["line.sgy", "_001.sgy", "a_b.sgy", "a-gpr.dzt"] {
            assert_eq!(skyhub_positions_file_name(name), None, "{name}");
        }
        assert_eq!(
            skyhub_recording_of_positions(Path::new("d/x-position.csv")),
            Some(PathBuf::from("d/x-gpr.sgy"))
        );
        assert!(!is_skyhub_position_csv(Path::new("x.coords.csv")));
        assert!(!is_skyhub_position_csv(Path::new("-position.csv")));
    }

    #[test]
    fn interpolation_holds_or_extrapolates_beyond_the_ends() {
        let known = [(0., 0.), (2., 4.), (4., 4.)];
        assert_eq!(interpolate(&known, 1., false), 2.);
        assert_eq!(interpolate(&known, 3., false), 4.);
        assert_eq!(interpolate(&known, -1., false), 0.);
        assert_eq!(interpolate(&known, -1., true), -2.);
        assert_eq!(interpolate(&known, 6., false), 4.);
        assert_eq!(interpolate(&[(1., 3.)], 9., true), 3.);
    }

    #[test]
    fn the_data_are_read_whole_with_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "a-gpr.sgy",
            &sgy(&textual(), 250, &traces(0, 3)),
        );
        let data = load_skyhub_data(&path).unwrap();
        assert_eq!(data.shape(), &[6, 3]);
        assert_eq!(data[[0, 0]], 65024.);
        assert_eq!(data[[1, 1]], SATURATED as f32);
        assert_eq!(data[[3, 2]], 20.);
        // One clipped sample of the five after the marker, in every trace.
        assert!((clipped_fraction(&path).unwrap() - 0.2).abs() < 1e-12);
    }

    #[test]
    fn the_header_needs_the_antenna_frequency() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "a-gpr.sgy",
            &sgy(&textual(), 250, &traces(0, 5)),
        );
        let error = load_skyhub_meta(&path, 0.168, None, None).unwrap_err();
        let unknown = error
            .downcast_ref::<crate::io::UnknownAntennaFrequency>()
            .unwrap();
        assert_eq!(unknown.antenna, "zAeroLF");
        assert!(error.to_string().contains("--override antenna_mhz=MHZ"));

        let meta = load_skyhub_meta(&path, 0.168, Some(75.), None).unwrap();
        assert_eq!(meta.samples, 6);
        assert_eq!(meta.frequency, 4000.);
        assert_eq!(meta.time_window, 1.5);
        assert_eq!(meta.last_trace, 5);
        assert_eq!(meta.antenna_separation, 0.);
        assert_eq!(meta.time_interval, 1.);
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // PROJ, as for the other position readers.
    fn rtk_positions_are_matched_by_trace_and_interpolated_in_time() {
        let dir = tempfile::tempdir().unwrap();
        let sgy_path = write(
            dir.path(),
            "a-gpr.sgy",
            &sgy(&textual(), 250, &traces(0, 4)),
        );
        let csv = write(dir.path(), "a-position.csv", position_csv("ON").as_bytes());
        assert_eq!(skyhub_positions_beside(&sgy_path), Some(csv.clone()));
        let crs = "EPSG:4326".to_string();
        let (location, source) = load_skyhub_positions(&sgy_path, &csv, Some(&crs)).unwrap();
        assert_eq!(source, SkyHubPositions::Rtk);
        let start = 1_709_294_400.;
        let times = location
            .cor_points
            .iter()
            .map(|p| p.time_seconds - start)
            .collect::<Vec<_>>();
        // Trace 2 has no row: halfway between its neighbours.
        assert_eq!(times, [0., 1., 2., 3.]);
        // The repeated position at 0.5 s is not a fix, so 0.5 s on is
        // interpolated from 0 to 1 s rather than held.
        let lats = location
            .cor_points
            .iter()
            .map(|p| p.northing)
            .collect::<Vec<_>>();
        for (lat, expected) in lats.iter().zip([50.0, 50.001, 50.002, 50.003]) {
            assert!((lat - expected).abs() < 1e-9, "{lats:?}");
        }
        let alts = location
            .cor_points
            .iter()
            .map(|p| p.altitude)
            .collect::<Vec<_>>();
        for (alt, expected) in alts.iter().zip([1000., 1001., 1002., 1003.]) {
            assert!((alt - expected).abs() < 1e-6, "{alts:?}");
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn without_rtk_the_standalone_positions_are_used() {
        let dir = tempfile::tempdir().unwrap();
        let sgy_path = write(
            dir.path(),
            "a-gpr.sgy",
            &sgy(&textual(), 250, &traces(0, 4)),
        );
        let csv = write(dir.path(), "a-position.csv", position_csv("").as_bytes());
        let crs = "EPSG:4326".to_string();
        let (location, source) = load_skyhub_positions(&sgy_path, &csv, Some(&crs)).unwrap();
        assert_eq!(source, SkyHubPositions::Standalone);
        assert!(source.relative_altitude());
        assert!((location.cor_points[0].northing - 51.0).abs() < 1e-9);
        assert_eq!(location.cor_points[0].altitude, 10.);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn a_cut_part_is_positioned_by_its_trace_numbers() {
        let dir = tempfile::tempdir().unwrap();
        // A part of traces 100-103 of a recording of 0-110, from a writer
        // whose marker sample is 0.
        let mut part = traces(100, 4);
        for trace in &mut part {
            trace.samples[0] = 0;
        }
        let textual = textual().replace("UgCS SkyHub v4.2.1.3219", "SkyHub v4.4.1.3542");
        let sgy_path = write(dir.path(), "rec-gpr-0_002.sgy", &sgy(&textual, 1250, &part));
        let mut txt =
            String::from("Coordinate Units:Deg\nTrace\tLatitude\tLongitude\tAltitude, m\n");
        for trace in 0..=110 {
            // Trace 101 is missing.
            if trace != 101 {
                txt.push_str(&format!(
                    "{trace}\t{:.10}\t12.0000000000\t{}\n",
                    50. + 0.0001 * f64::from(trace),
                    f64::from(trace) / 10.
                ));
            }
        }
        let txt_path = write(dir.path(), "rec-gpr-0.txt", txt.as_bytes());
        assert_eq!(skyhub_positions_beside(&sgy_path), Some(txt_path.clone()));
        let crs = "EPSG:4326".to_string();
        let (location, source) = load_skyhub_positions(&sgy_path, &txt_path, Some(&crs)).unwrap();
        assert_eq!(source, SkyHubPositions::CutText);
        let lats = location
            .cor_points
            .iter()
            .map(|p| (p.trace_n, p.northing, p.altitude))
            .collect::<Vec<_>>();
        for ((trace_n, lat, alt), number) in lats.iter().zip(100..) {
            assert_eq!(*trace_n, number as u32 - 100);
            assert!(
                (lat - (50. + 0.0001 * f64::from(number))).abs() < 1e-9,
                "{lats:?}"
            );
            assert!((alt - f64::from(number) / 10.).abs() < 1e-9, "{lats:?}");
        }
        let start = chrono::NaiveDate::from_ymd_opt(2024, 3, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64;
        assert_eq!(location.cor_points[3].time_seconds - start, 3.);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn without_a_positions_file_the_trace_headers_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let sgy_path = write(
            dir.path(),
            "a-gpr.sgy",
            &sgy(&textual(), 250, &traces(0, 4)),
        );
        assert_eq!(skyhub_positions_beside(&sgy_path), None);
        let crs = "EPSG:4326".to_string();
        let (location, source) = load_skyhub_positions(&sgy_path, &sgy_path, Some(&crs)).unwrap();
        assert_eq!(source, SkyHubPositions::TraceHeaders);
        // Float32 DDMM.mmmm: to about a metre, so about 1e-5 degrees.
        assert!((location.cor_points[3].northing - 60.0003).abs() < 1e-5);
        assert!((location.cor_points[3].easting - 10.0).abs() < 1e-5);
        assert_eq!(location.cor_points[3].altitude, 8.);

        let other = write(dir.path(), "a.cor", b"");
        let error = load_skyhub_positions(&sgy_path, &other, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot hold the positions"), "{error}");
    }

    /// Process `inputs` quietly into UTM 32N.
    fn run(inputs: &[PathBuf]) -> Result<crate::gpr::GPR, String> {
        let params = crate::processing::RunParams {
            filepaths: inputs.to_vec(),
            output_path: None,
            track_path: None,
            render_path: None,
            radargram_id: Some("test".into()),
            display_name: None,
            options: crate::processing::ProcessingOptions {
                quiet: true,
                crs: Some("EPSG:32632".into()),
                ..Default::default()
            },
        };
        crate::processing::build_processed_gpr(params)
            .map(|(gpr, _)| gpr)
            .map_err(|e| e.to_string())
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn parts_are_processed_without_their_marker_and_say_where_they_are_from() {
        let dir = tempfile::tempdir().unwrap();
        let mut txt =
            String::from("Coordinate Units:Deg\nTrace\tLatitude\tLongitude\tAltitude, m\n");
        for trace in 0..20 {
            txt.push_str(&format!(
                "{trace}\t{}\t10.0\t{trace}\n",
                60.0 + 0.0001 * f64::from(trace)
            ));
        }
        write(dir.path(), "rec-gpr-0.txt", txt.as_bytes());
        let first = write(
            dir.path(),
            "rec-gpr-0_001.sgy",
            &sgy(&textual(), 250, &traces(2, 3)),
        );
        let second = write(
            dir.path(),
            "rec-gpr-0_002.sgy",
            &sgy(&textual(), 250, &traces(8, 4)),
        );
        let inputs = [first, second];

        let error = run(&inputs).err().unwrap();
        assert!(error.contains("--override antenna_mhz=MHZ"), "{error}");
        for stem in ["rec-gpr-0_001", "rec-gpr-0_002"] {
            std::fs::write(
                dir.path().join(format!("{stem}.ridal.toml")),
                "[overrides]\nantenna_mhz = 75\n",
            )
            .unwrap();
        }
        let gpr = run(&inputs).unwrap();
        // The marker sample is a crop of one sample of 0.25 ns.
        assert_eq!(gpr.data.shape(), &[5, 7]);
        assert_eq!(gpr.data[[0, 0]], SATURATED as f32);
        assert_eq!(gpr.metadata.time_window, 1.25);
        assert!(gpr.twtt_crop_ns().iter().all(|&ns| ns == 0.25));
        assert_eq!(gpr.metadata.antenna_mhz, 75.);
        // Positions by the traces' numbers in the whole recording.
        let altitudes = gpr
            .location
            .cor_points
            .iter()
            .map(|p| p.altitude)
            .collect::<Vec<_>>();
        assert_eq!(altitudes, [2., 3., 4., 8., 9., 10., 11.]);
        // Each part's source is in the log, the second's before its merge.
        let names = gpr
            .log
            .iter()
            .map(|line| crate::gpr::processing_log::parse(line).name.unwrap_or(""))
            .collect::<Vec<_>>();
        assert_eq!(names, ["read", "read", "merge"]);
        assert!(gpr.log[1].contains("rec-gpr-0_002.sgy"), "{}", gpr.log[1]);
        assert!(
            gpr.log[1].contains("relative to take-off"),
            "{}",
            gpr.log[1]
        );
    }

    #[test]
    fn the_aircraft_s_altitude_is_warned_about_without_a_dem() {
        let dir = tempfile::tempdir().unwrap();
        let sgy_path = write(
            dir.path(),
            "a-gpr.sgy",
            &sgy(&textual(), 250, &traces(0, 4)),
        );
        let warnings = |options: crate::processing::ProcessingOptions| {
            let resolved = crate::formats::resolve_input(&sgy_path).unwrap();
            crate::processing::assumptions(&[resolved], &options)
                .unwrap()
                .into_iter()
                .filter(|w| w.contains("aircraft"))
                .collect::<Vec<_>>()
        };
        let bare = warnings(Default::default());
        assert_eq!(bare.len(), 1, "{bare:?}");
        assert!(bare[0].contains("from its trace headers"), "{}", bare[0]);
        assert!(bare[0].contains("altitude_offset_m=M"), "{}", bare[0]);

        let offset = warnings(crate::processing::ProcessingOptions {
            overrides: crate::sidecar::Overrides {
                altitude_offset_m: Some(1000.),
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(offset.len(), 1, "{offset:?}");
        assert!(!offset[0].contains("altitude_offset_m"), "{}", offset[0]);

        let dem = warnings(crate::processing::ProcessingOptions {
            dem_path: Some("dem.tif".into()),
            ..Default::default()
        });
        assert!(dem.is_empty(), "{dem:?}");

        // RTK altitudes are absolute, but still the aircraft's.
        let csv = position_csv("ON");
        write(dir.path(), "a-position.csv", csv.as_bytes());
        let rtk = warnings(Default::default());
        assert!(rtk[0].contains("from its RTK positions"), "{}", rtk[0]);
        assert!(!rtk[0].contains("altitude_offset_m"), "{}", rtk[0]);
    }

    #[test]
    fn info_reports_the_settings_and_the_positions_source() {
        let dir = tempfile::tempdir().unwrap();
        let sgy_path = write(
            dir.path(),
            "a-gpr.sgy",
            &sgy(&textual(), 250, &traces(0, 4)),
        );
        let settings = skyhub_settings(&sgy_path, Some(&sgy_path)).unwrap();
        let get = |key: &str| {
            settings
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("Unit Type"), Some("zAeroLF"));
        assert_eq!(get("Pulse delay"), Some("300"));
        assert_eq!(get("Clipped samples"), Some("20.00 %"));
        assert!(get("Positions").unwrap().contains("trace headers"));
        assert!(skyhub_settings(&sgy_path, None)
            .unwrap()
            .iter()
            .all(|(k, _)| k != "Positions"));
    }
}
