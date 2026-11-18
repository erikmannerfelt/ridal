//! The VIRL-6's recordings: a `<stem>.hdr` and a `<stem>.bin`.
//!
//! The `.hdr` is a text table, a line of column names and then a row per
//! trace:
//!
//! ```text
//!   trn summ  odom   elev      zon     lat          lon       time
//!     0   2      0    811.4      0   79.832433    24.001567   110628
//! ```
//!
//! `lat` and `lon` are WGS84 degrees, `elev` metres, and `time` the time of
//! day of the GPS's last fix as `hhmmss`, so several rows share one. `trn`
//! is not the trace's number: a file's first row can carry the count of the
//! file before, so a trace is its row. `summ` (2 in every file seen,
//! probably the stacking) and `odom` (0, an odometer without a wheel) are
//! not read. `zon` is 0 in every file seen; any other value might mean that
//! `lat` and `lon` are projected, and is refused. A recording made without
//! a GPS has only `trn summ odom`, and its positions must be given.
//!
//! The `.bin` has no header: it is the traces one after another, each its
//! samples as little-endian `f32`, so the number of samples is the file's
//! size over the table's rows.
//!
//! There is no public description of the format; this is what the files of
//! one survey (Austfonna, 2008) show.

use std::error::Error;
use std::path::{Path, PathBuf};

use ndarray::Array2;

use super::{Fix, Recording};

/// The columns a VIRL-6 table starts with, which tell it from any other
/// `.hdr`.
const FIRST_COLUMNS: [&str; 3] = ["trn", "summ", "odom"];

/// Whether `path` is a VIRL-6 `.hdr`: its first line names the columns
/// such a table starts with. Other software (ENVI) also writes `.hdr`
/// files.
pub fn is_virl6_header(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut first = String::new();
    // A first line is short; a binary file without a newline is not read
    // whole.
    use std::io::{BufRead, Read};
    if std::io::BufReader::new(file.take(4096))
        .read_line(&mut first)
        .is_err()
    {
        return false;
    }
    first
        .split_whitespace()
        .take(FIRST_COLUMNS.len())
        .eq(FIRST_COLUMNS)
}

/// The `.bin` of the recording whose `.hdr` is `header`, beside it,
/// whatever the case of its extension, whether or not it exists.
pub fn virl6_bin_beside(header: &Path) -> PathBuf {
    crate::formats::find_neighbor_case_insensitive(header, "bin")
        .unwrap_or_else(|| header.with_extension("bin"))
}

/// The table of a `.hdr`: how many rows it has, and its fixes, unless it
/// has no positions.
struct Table {
    rows: usize,
    fixes: Option<Vec<Fix>>,
}

fn read_table(path: &Path) -> Result<Table, Box<dyn Error>> {
    let not_virl = |why: String| -> Box<dyn Error> {
        format!("{} is not a VIRL-6 .hdr file: {why}", path.display()).into()
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| not_virl(format!("it could not be read as text ({e})")))?;
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty());
    let columns = lines
        .next()
        .map(|(_, line)| line.split_whitespace().collect::<Vec<_>>())
        .unwrap_or_default();
    if !columns.starts_with(&FIRST_COLUMNS) {
        return Err(not_virl(format!(
            "its first line does not start with the columns {}",
            FIRST_COLUMNS.join(" ")
        )));
    }
    let column = |name: &str| columns.iter().position(|c| *c == name);
    let position_columns = [column("lat"), column("lon"), column("time")];
    let position_columns = match position_columns {
        [Some(lat), Some(lon), Some(time)] => Some((lat, lon, time)),
        [None, None, None] => None,
        _ => {
            return Err(not_virl(
                "it has some of the columns lat, lon and time, but not all".to_string(),
            ));
        }
    };
    let elev = column("elev");
    let zone = column("zon");

    let mut rows = 0;
    let mut fixes: Vec<Fix> = Vec::new();
    let mut last_position: Option<Vec<&str>> = None;
    for (index, line) in lines {
        let values = line.split_whitespace().collect::<Vec<_>>();
        let at_line = |why: String| -> Box<dyn Error> {
            format!("{}, line {}: {why}", path.display(), index + 1).into()
        };
        if values.len() != columns.len() {
            return Err(at_line(format!(
                "{} values for {} columns",
                values.len(),
                columns.len()
            )));
        }
        let trace = rows;
        rows += 1;
        if let Some(zone) = zone.filter(|&zone| values[zone] != "0") {
            return Err(at_line(format!(
                "zon is {}, where every file Ridal knows has 0; the positions may then be \
                 projected rather than in degrees, which Ridal does not read",
                values[zone]
            )));
        }
        let Some((lat, lon, time)) = position_columns else {
            continue;
        };
        // A new fix where the position or its time changes.
        let position = [Some(lat), Some(lon), Some(time), elev]
            .into_iter()
            .flatten()
            .map(|i| values[i])
            .collect::<Vec<_>>();
        if last_position.as_ref() == Some(&position) {
            continue;
        }
        let number = |i: usize, name: &str| -> Result<f64, Box<dyn Error>> {
            values[i]
                .parse::<f64>()
                .map_err(|_| at_line(format!("{name} {:?} is not a number", values[i])))
        };
        let altitude = match elev {
            Some(elev) => number(elev, "elev")?,
            None => f64::NAN,
        };
        fixes.push(Fix {
            trace,
            seconds_of_day: seconds_of_day(values[time]).map_err(at_line)?,
            coord: crate::coords::Coord {
                x: number(lon, "lon")?,
                y: number(lat, "lat")?,
            },
            altitude,
        });
        last_position = Some(position);
    }
    if rows == 0 {
        return Err(not_virl("it has no rows".to_string()));
    }
    Ok(Table {
        rows,
        fixes: position_columns.map(|_| fixes),
    })
}

/// The seconds since midnight of a time of day written `hhmmss`, with or
/// without the leading zero of the hour, and with any decimals of a second.
///
/// ```text
/// 110628 -> 39988
///  90628 -> 32788
/// ```
fn seconds_of_day(text: &str) -> Result<f64, String> {
    let invalid = || format!("time {text:?} is not a time of day as hhmmss");
    let value = text.parse::<f64>().map_err(|_| invalid())?;
    if !(0. ..240000.).contains(&value) {
        return Err(invalid());
    }
    let whole = value.trunc() as u32;
    let (hours, minutes, seconds) = (whole / 10000, whole / 100 % 100, whole % 100);
    if minutes >= 60 || seconds >= 60 {
        return Err(invalid());
    }
    Ok(f64::from(hours * 3600 + minutes * 60 + seconds) + value.fract())
}

/// The number of samples a trace has: the `.bin`'s size over the table's
/// rows, each sample four bytes.
fn samples(bin: &Path, bytes: u64, rows: usize) -> Result<usize, Box<dyn Error>> {
    let trace_bytes = bytes / rows as u64;
    if trace_bytes == 0 || !bytes.is_multiple_of(rows as u64) || !trace_bytes.is_multiple_of(4) {
        return Err(format!(
            "{} has {bytes} bytes, which is not {rows} traces (the rows of its .hdr) of 4-byte \
             samples",
            bin.display()
        )
        .into());
    }
    Ok((trace_bytes / 4) as usize)
}

/// The `.bin` of `header`, which must exist.
fn existing_bin(header: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let bin = virl6_bin_beside(header);
    if !bin.is_file() {
        return Err(format!(
            "{} does not exist: a VIRL-6 recording is its .hdr and its .bin, side by side",
            bin.display()
        )
        .into());
    }
    Ok(bin)
}

/// The table of the `.hdr` `path` and the size of its traces.
pub(super) fn read(path: &Path) -> Result<Recording, Box<dyn Error>> {
    let table = read_table(path)?;
    let bin = existing_bin(path)?;
    let samples = samples(&bin, std::fs::metadata(&bin)?.len(), table.rows)?;
    Ok(Recording {
        samples,
        traces: table.rows,
        fixes: table.fixes,
    })
}

/// The traces of the recording whose `.hdr` is `path`, samples by traces,
/// as they are stored.
pub(super) fn load_data(path: &Path) -> Result<Array2<f32>, Box<dyn Error>> {
    let traces = read_table(path)?.rows;
    let bin = existing_bin(path)?;
    let bytes = std::fs::read(&bin)?;
    let samples = samples(&bin, bytes.len() as u64, traces)?;
    let data = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&quad| f32::from_le_bytes(quad))
        .collect::<Vec<_>>();
    Ok(Array2::from_shape_vec((traces, samples), data)?.reversed_axes())
}

#[cfg(test)]
pub(crate) mod test_hdr {
    //! Synthetic VIRL-6 recordings.

    use std::path::{Path, PathBuf};

    /// The `.hdr` and `.bin` of `stem` in `dir`, with `rows` of the table
    /// as given after its column line, and traces of `samples` samples that
    /// tell where they came from: trace t (from 0), sample s is 10 t + s.
    pub fn write(dir: &Path, stem: &str, columns: &str, rows: &[&str], samples: usize) -> PathBuf {
        let mut text = format!("{columns}\r\n");
        for row in rows {
            text.push_str(row);
            text.push_str("\r\n");
        }
        let hdr = dir.join(format!("{stem}.hdr"));
        std::fs::write(&hdr, text).unwrap();
        let bin = (0..rows.len())
            .flat_map(|t| (0..samples).map(move |s| (10 * t + s) as f32))
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        std::fs::write(dir.join(format!("{stem}.bin")), bin).unwrap();
        hdr
    }

    pub const COLUMNS: &str = "  trn summ  odom   elev      zon     lat          lon       time  ";

    /// A row of trace `trn` at `hhmmss`, its latitude from `lat_step`.
    pub fn row(trn: usize, hhmmss: &str, lat_step: usize) -> String {
        format!(
            "{trn:5}   2      0    811.4      0   {:.6}    24.001567   {hhmmss}",
            79.83 + 0.0001 * lat_step as f64
        )
    }

    /// A recording of four traces of 6 samples, two fixes a second apart
    /// from 11:06:28 UTC, for tests outside this module.
    pub fn simple(dir: &Path) -> PathBuf {
        let rows = [
            row(0, "110628", 0),
            row(1, "110628", 0),
            row(2, "110629", 1),
            row(3, "110629", 1),
        ];
        write(
            dir,
            "414_0000",
            COLUMNS,
            &rows.iter().map(String::as_str).collect::<Vec<_>>(),
            6,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_hdr::{row, write, COLUMNS};
    use super::*;
    use crate::io::{load_virl_data, load_virl_meta, load_virl_positions, virl_fix_count};

    const SAMPLES: usize = 5;

    /// Six traces over three fixes, the first row's `trn` the count of a
    /// file before, as the VIRL-6 writes it, and the last fix across
    /// midnight without the hour's leading zero.
    fn recording(dir: &Path) -> PathBuf {
        let rows = [
            row(1437, "235958", 0),
            row(1, "235958", 0),
            row(2, "235958", 0),
            row(3, "235959", 1),
            row(4, "1", 2),
            row(5, "1", 2),
        ];
        write(
            dir,
            "414_0001",
            COLUMNS,
            &rows.iter().map(String::as_str).collect::<Vec<_>>(),
            SAMPLES,
        )
    }

    #[test]
    fn a_virl6_header_is_told_from_another_hdr() {
        let dir = tempfile::tempdir().unwrap();
        let hdr = recording(dir.path());
        assert!(is_virl6_header(&hdr));
        let envi = dir.path().join("image.hdr");
        std::fs::write(&envi, "ENVI\nsamples = 100\n").unwrap();
        assert!(!is_virl6_header(&envi));
        assert!(!is_virl6_header(&dir.path().join("absent.hdr")));
        assert_eq!(virl6_bin_beside(&hdr), dir.path().join("414_0001.bin"));
    }

    #[test]
    fn the_time_of_day_is_read_with_or_without_the_leading_zero() {
        assert_eq!(seconds_of_day("110628"), Ok(39988.));
        assert_eq!(seconds_of_day("90628"), Ok(32788.));
        assert_eq!(seconds_of_day("1"), Ok(1.));
        assert_eq!(seconds_of_day("110628.5"), Ok(39988.5));
        for bad in ["116028", "110678", "250000", "-1", "noon"] {
            assert!(seconds_of_day(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_fix_is_kept_at_its_first_row_and_trn_is_not_the_trace() {
        let dir = tempfile::tempdir().unwrap();
        let hdr = recording(dir.path());
        let recording = read(&hdr).unwrap();
        assert_eq!((recording.traces, recording.samples), (6, SAMPLES));
        let fixes = recording.fixes.unwrap();
        assert_eq!(
            fixes
                .iter()
                .map(|f| (f.trace, f.seconds_of_day))
                .collect::<Vec<_>>(),
            [(0, 86398.), (3, 86399.), (4, 1.)]
        );
        assert_eq!(fixes[1].coord.y, 79.8301);
        assert_eq!(fixes[1].coord.x, 24.001567);
        assert_eq!(fixes[1].altitude, 811.4);
        assert_eq!(virl_fix_count(&hdr).unwrap(), (3, 6));
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // PROJ, as for the other position readers.
    fn positions_are_dated_and_cross_midnight() {
        let dir = tempfile::tempdir().unwrap();
        let hdr = recording(dir.path());
        let error = load_virl_positions(&hdr, None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--set date=YYYY-MM-DD"), "{error}");

        let date = chrono::NaiveDate::from_ymd_opt(2008, 5, 3).unwrap();
        let location = load_virl_positions(&hdr, Some(date), None).unwrap();
        let midnight = date
            .succ_opt()
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64;
        assert_eq!(
            location
                .cor_points
                .iter()
                .map(|p| (p.trace_n, p.time_seconds))
                .collect::<Vec<_>>(),
            [(0, midnight - 2.), (3, midnight - 1.), (4, midnight + 1.)]
        );
        assert_eq!(location.crs, "EPSG:32635");
        assert!(location.cor_points.iter().all(|p| p.altitude == 811.4));
    }

    #[test]
    fn the_header_is_the_tables_rows_and_the_bins_size() {
        let dir = tempfile::tempdir().unwrap();
        let hdr = recording(dir.path());
        let date = chrono::NaiveDate::from_ymd_opt(2008, 5, 3);
        let error = load_virl_meta(&hdr, 0.168, Some(20.), None, None, date)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--set sampling_mhz=MHZ"), "{error}");

        let meta = load_virl_meta(&hdr, 0.168, Some(20.), None, Some(250.), date).unwrap();
        assert_eq!(meta.samples, SAMPLES as u32);
        assert_eq!(meta.last_trace, 6);
        assert_eq!(meta.time_window, 20.);
        // Fixes at traces 0, 3 and 4, 1 and 2 s apart: 1/3 and 2 s per trace.
        assert_eq!(meta.time_interval, 2.);

        // A .bin that is not whole traces, and none at all.
        let bin = dir.path().join("414_0001.bin");
        let mut bytes = std::fs::read(&bin).unwrap();
        bytes.pop();
        std::fs::write(&bin, &bytes).unwrap();
        let error = load_virl_meta(&hdr, 0.168, Some(20.), None, Some(250.), date)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not 6 traces"), "{error}");
        std::fs::remove_file(&bin).unwrap();
        let error = load_virl_meta(&hdr, 0.168, Some(20.), None, Some(250.), date)
            .unwrap_err()
            .to_string();
        assert!(error.contains("its .hdr and its .bin"), "{error}");
    }

    #[test]
    fn a_recording_without_a_gps_reads_but_has_no_positions() {
        let dir = tempfile::tempdir().unwrap();
        let hdr = write(
            dir.path(),
            "414_0006",
            "  trn summ  odom   ",
            &["    0   2      0", "    1   2      0"],
            SAMPLES,
        );
        let meta = load_virl_meta(&hdr, 0.168, Some(20.), None, Some(250.), None).unwrap();
        assert_eq!(meta.last_trace, 2);
        for error in [
            virl_fix_count(&hdr).unwrap_err().to_string(),
            load_virl_positions(&hdr, chrono::NaiveDate::from_ymd_opt(2008, 5, 3), None)
                .unwrap_err()
                .to_string(),
        ] {
            assert!(error.contains("records no positions"), "{error}");
            assert!(error.contains(".coords.csv"), "{error}");
        }
    }

    #[test]
    fn a_table_it_cannot_read_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cases: [(&str, &[&str], &str); 4] = [
            (COLUMNS, &["0 2 0 811.4 33 79.83 24.0 110628"], "zon is 33"),
            (
                COLUMNS,
                &["0 2 0 811.4 0 79.83 24.0"],
                "7 values for 8 columns",
            ),
            (
                "trn summ odom lat lon",
                &["0 2 0 79.83 24.0"],
                "but not all",
            ),
            (
                COLUMNS,
                &["0 2 0 811.4 0 north 24.0 110628"],
                "lat \"north\"",
            ),
        ];
        for (columns, rows, expected) in cases {
            let hdr = write(dir.path(), "bad", columns, rows, SAMPLES);
            let error = read(&hdr).err().unwrap().to_string();
            assert!(error.contains(expected), "{error}");
        }
        let hdr = write(dir.path(), "empty", COLUMNS, &[], SAMPLES);
        let error = read(&hdr).err().unwrap().to_string();
        assert!(error.contains("no rows"), "{error}");
    }

    #[test]
    fn the_data_are_the_stored_floats() {
        let dir = tempfile::tempdir().unwrap();
        let hdr = recording(dir.path());
        let data = load_virl_data(&hdr).unwrap();
        assert_eq!(data.shape(), &[SAMPLES, 6]);
        // Trace t (from 0), sample s: 10 t + s.
        assert_eq!(data[[0, 0]], 0.);
        assert_eq!(data[[4, 5]], 54.);
    }
}
