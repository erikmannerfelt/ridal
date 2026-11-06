//! VIRL radar `.DSR` files, as written by the VIRL-7 of Vasilenko and the
//! Universidad Politécnica de Madrid.
//!
//! A file is one recording and has no file header: it is a run of records
//! of one size, one per trace. Each record is a 112-byte ASCII header,
//! padded with spaces, followed by the trace as little-endian `u16`
//! samples:
//!
//! ```text
//! #00001,2,00001,1,$GPGGA,175915,7808.5257,N,01805.3193,E,1,09,0.8,89.9,M,32.4,M,,*76
//! ```
//!
//! The fields before the GGA sentence are the trace number from 1, two
//! values that never change, and a counter that wraps at 32 767. When it
//! has wrapped it is one character longer and overwrites the comma before
//! `$GPGGA`, so the sentence is found by its `$`, not by counting commas.
//!
//! The GGA sentence is the last fix the GPS gave, so several traces share
//! one; a position is kept at the first trace of each, and the traces
//! between are interpolated. A sentence whose checksum is wrong, as a
//! dropped comma makes it, is skipped.
//!
//! What the files do not record: the sampling frequency, the date (GGA has
//! only the time of day), the antenna frequency and the separation. The
//! first three must be given, in the recording's sidecar or as arguments;
//! the separation is 0 m without one.

use std::error::Error;
use std::path::Path;

use ndarray::Array2;

use crate::gpr;

/// The length of a record's ASCII header, in bytes.
const HEADER_BYTES: usize = 112;

/// The samples are a 14-bit ADC's, stored as offset binary: this is 0 V.
const ADC_ZERO: f32 = 8192.;

/// The layout of a `.DSR` file: how long each record is, and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    samples: usize,
    traces: usize,
}

impl Layout {
    fn record_bytes(self) -> usize {
        HEADER_BYTES + 2 * self.samples
    }
}

/// The layout of `bytes`, a whole `.DSR` file. The record size is where
/// the second record's header starts, `#00002,`, which a trace's samples
/// cannot spell by chance at a size that also divides the file.
fn layout(bytes: &[u8], path: &Path) -> Result<Layout, Box<dyn Error>> {
    let not_virl = |why: &str| -> Box<dyn Error> {
        format!("{} is not a VIRL .DSR file: {why}", path.display()).into()
    };
    if !bytes.starts_with(b"#00001,") {
        return Err(not_virl("it does not start with the header of trace 1"));
    }
    if bytes.len() <= HEADER_BYTES {
        return Err(not_virl("it is too short to hold one trace"));
    }
    let second = bytes[HEADER_BYTES..]
        .windows(7)
        .enumerate()
        .filter(|(_, w)| *w == b"#00002,")
        .map(|(i, _)| HEADER_BYTES + i)
        .find(|&size| (size - HEADER_BYTES).is_multiple_of(2) && bytes.len().is_multiple_of(size));
    // One trace has no second header to find.
    let record_bytes = second.unwrap_or(bytes.len());
    if !(record_bytes - HEADER_BYTES).is_multiple_of(2) {
        return Err(not_virl(
            "its trace is not a whole number of 16-bit samples",
        ));
    }
    Ok(Layout {
        samples: (record_bytes - HEADER_BYTES) / 2,
        traces: bytes.len() / record_bytes,
    })
}

/// The ASCII header of record `index`.
fn record_header(bytes: &[u8], layout: Layout, index: usize) -> &[u8] {
    let start = index * layout.record_bytes();
    &bytes[start..start + HEADER_BYTES]
}

/// The GGA sentence of a record's header, up to and including its
/// checksum, if its checksum is right.
fn checked_gga(header: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(header).ok()?;
    let start = text.find("$GPGGA")?;
    let (body, rest) = text[start + 1..].split_once('*')?;
    let given = u8::from_str_radix(rest.get(..2)?, 16).ok()?;
    let computed = body.bytes().fold(0u8, |sum, b| sum ^ b);
    (given == computed).then(|| &text[start..start + 1 + body.len()])
}

/// One trace with a new fix: the trace, from 0, and its sentence.
struct Fix<'a> {
    trace: usize,
    gga: &'a str,
}

/// The first trace of each GGA sentence in the file, skipping those whose
/// checksum is wrong.
fn fixes(bytes: &[u8], layout: Layout) -> Vec<Fix<'_>> {
    let mut out: Vec<Fix> = Vec::new();
    for trace in 0..layout.traces {
        let Some(gga) = checked_gga(record_header(bytes, layout, trace)) else {
            continue;
        };
        if out.last().is_none_or(|last| last.gga != gga) {
            out.push(Fix { trace, gga });
        }
    }
    out
}

/// A fix as read: its trace, its time in seconds since the epoch, its
/// WGS84 position and its altitude.
struct TimedFix {
    trace: u32,
    time: f64,
    coord: crate::coords::Coord,
    altitude: f64,
}

/// Each fix's trace and time, with the positions to project: from the time of day in each sentence on `date`,
/// moving to the next day when the time of day goes back by more than 12
/// hours. A fix not later than the one before is left out, since the
/// times between fixes are interpolated.
fn timed_fixes(fixes: &[Fix], date: chrono::NaiveDate) -> Result<Vec<TimedFix>, Box<dyn Error>> {
    let date_text = date.format("%Y-%m-%d").to_string();
    let mut out: Vec<TimedFix> = Vec::new();
    let mut previous_of_day: Option<f64> = None;
    let mut days = 0.;
    for fix in fixes {
        let (time, coord, altitude) = super::read_gga(fix.gga, &date_text)?;
        let of_day = time.rem_euclid(86400.);
        if previous_of_day.is_some_and(|previous| of_day < previous - 43200.) {
            days += 86400.;
        }
        previous_of_day = Some(of_day);
        let time = time + days;
        if out.last().is_some_and(|last| time <= last.time) {
            continue;
        }
        out.push(TimedFix {
            trace: fix.trace as u32,
            time,
            coord,
            altitude,
        });
    }
    Ok(out)
}

/// Seconds per trace: the median of the intervals between fixes, or 0
/// without two of them.
fn time_interval(fixes: &[TimedFix]) -> f32 {
    let mut rates = fixes
        .windows(2)
        .map(|w| (w[1].time - w[0].time) / f64::from(w[1].trace - w[0].trace))
        .collect::<Vec<_>>();
    if rates.is_empty() {
        return 0.;
    }
    rates.sort_by(f64::total_cmp);
    rates[rates.len() / 2] as f32
}

/// The header of a `.DSR` file, which is the layout of its records and
/// the values given for what it does not record.
///
/// # Errors
/// A file that is not a `.DSR` file, or no sampling frequency or antenna
/// frequency given.
pub fn load_virl_meta(
    path: &Path,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
    sampling_mhz: Option<f32>,
    date: Option<chrono::NaiveDate>,
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    let layout = layout(&bytes, path)?;
    let antenna = "VIRL".to_string();
    let frequency = sampling_mhz.ok_or_else(|| {
        super::UnknownSamplingFrequency::new(
            antenna.as_str(),
            format!(
                "VIRL files do not record the sampling frequency. Give it as sampling_mhz in a \
                 <stem>.ridal.toml beside {}, with ridal override {} --set sampling_mhz=MHZ",
                path.display(),
                path.display()
            ),
        )
    })?;
    let antenna_mhz = override_antenna_mhz.ok_or_else(|| {
        super::UnknownAntennaFrequency::new(
            antenna.as_str(),
            "VIRL files do not record the antenna frequency. Give it with \
             --override antenna_mhz=MHZ (overrides= in Python), or as antenna_mhz in a \
             <stem>.ridal.toml beside the file"
                .to_string(),
        )
    })?;
    // Without a date the header can still be read, as `ridal override
    // --template` and the ingest planner do; the positions cannot.
    let time_interval = match date {
        Some(date) => time_interval(&timed_fixes(&fixes(&bytes, layout), date)?),
        None => 0.,
    };
    Ok(gpr::GPRMeta {
        samples: layout.samples as u32,
        frequency,
        frequency_steps: 0,
        time_interval,
        antenna,
        antenna_mhz,
        antenna_separation: override_antenna_separation.unwrap_or(0.),
        time_window: 1000. * layout.samples as f32 / frequency,
        last_trace: layout.traces as u32,
        data_filepath: path.to_path_buf(),
        medium_velocity,
    })
}

/// The positions of a `.DSR` file, at the first trace of each fix.
///
/// # Errors
/// No date given, or no fix with a correct checksum.
pub fn load_virl_positions(
    path: &Path,
    date: Option<chrono::NaiveDate>,
    projected_crs: Option<&String>,
) -> Result<gpr::GPRLocation, Box<dyn Error>> {
    let date = date.ok_or_else(|| {
        super::UnknownDate::new(format!(
            "{}'s positions record only the time of day. Give the UTC date as date in a \
             <stem>.ridal.toml beside it (ridal override {} --set date=YYYY-MM-DD), or with \
             --override date=YYYY-MM-DD (overrides= in Python)",
            path.display(),
            path.display()
        ))
    })?;
    let bytes = std::fs::read(path)?;
    let layout = layout(&bytes, path)?;
    let timed = timed_fixes(&fixes(&bytes, layout), date)?;
    if timed.is_empty() {
        return Err(format!("{} has no GPS fix with a correct checksum", path.display()).into());
    }
    let wgs84 = timed.iter().map(|f| f.coord).collect::<Vec<_>>();
    let mut points = timed
        .iter()
        .map(|fix| gpr::CorPoint {
            trace_n: fix.trace,
            time_seconds: fix.time,
            easting: 0.,
            northing: 0.,
            altitude: fix.altitude,
        })
        .collect::<Vec<_>>();
    let crs = super::project_points(&mut points, &wgs84, projected_crs)?;
    Ok(gpr::GPRLocation {
        cor_points: points,
        correction: gpr::LocationCorrection::None,
        time_source: gpr::TimeSource::Recorded,
        crs,
    })
}

/// How many fixes with a correct checksum a `.DSR` file has, and how many
/// traces, for a warning when one fix stands for them all.
pub fn virl_fix_count(path: &Path) -> Result<(usize, usize), Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    let layout = layout(&bytes, path)?;
    Ok((fixes(&bytes, layout).len(), layout.traces))
}

/// The traces of a `.DSR` file, samples by traces, in ADC units about 0.
pub fn load_virl_data(path: &Path) -> Result<Array2<f32>, Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    let layout = layout(&bytes, path)?;
    let mut data = Vec::with_capacity(layout.samples * layout.traces);
    for trace in 0..layout.traces {
        let start = trace * layout.record_bytes() + HEADER_BYTES;
        data.extend(
            bytes[start..start + 2 * layout.samples]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| f32::from(u16::from_le_bytes(pair)) - ADC_ZERO),
        );
    }
    Ok(Array2::from_shape_vec((layout.traces, layout.samples), data)?.reversed_axes())
}

#[cfg(test)]
pub(crate) mod test_dsr {
    //! Synthetic `.DSR` files.

    /// A GGA sentence at `hhmmss`, with its checksum.
    pub fn gga(hhmmss: &str, lat_minutes: f64) -> String {
        let body =
            format!("GPGGA,{hhmmss},78{lat_minutes:07.4},N,01805.3193,E,1,09,0.8,89.9,M,32.4,M,,");
        let sum = body.bytes().fold(0u8, |s, b| s ^ b);
        format!("${body}*{sum:02X}")
    }

    /// A record: trace `n` (from 1) with `sentence`, and samples that tell
    /// where they came from.
    pub fn record(n: usize, counter: &str, sentence: &str, samples: usize) -> Vec<u8> {
        let mut header = format!("#{n:05},2,{counter},1,{sentence}").into_bytes();
        header.resize(super::HEADER_BYTES, b' ');
        for sample in 0..samples {
            header.extend_from_slice(&(8192 + (n * 10 + sample) as u16).to_le_bytes());
        }
        header
    }

    /// A file of four traces of 6 samples, a fix a second from 12:00:00
    /// UTC, for tests outside this module.
    pub fn simple() -> Vec<u8> {
        (1..=4)
            .flat_map(|n| {
                let sentence = gga(&format!("12000{}", n - 1), 8.5 + 0.1 * n as f64);
                record(n, &format!("{:05}", 2 * n), &sentence, 6)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::test_dsr::{gga, record};
    use super::*;

    const SAMPLES: usize = 6;

    /// Six traces: two per fix, the counter wrapped from the fourth, and
    /// the fifth's sentence with a dropped comma, so a wrong checksum.
    fn file() -> Vec<u8> {
        let a = gga("235958", 8.5);
        let b = gga("235959", 8.6);
        let broken = gga("000001", 8.7).replacen(",1,09,", ",109,", 1);
        let c = gga("000003", 8.8);
        [
            record(1, "32765", &a, SAMPLES),
            record(2, "32767", &a, SAMPLES),
            record(3, "-32767", &b, SAMPLES),
            record(4, "-32765", &b, SAMPLES),
            record(5, "-32763", &broken, SAMPLES),
            record(6, "-32761", &c, SAMPLES),
        ]
        .concat()
    }

    fn write(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("030_0001.DSR");
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn the_layout_is_found_from_the_second_header() {
        let (_dir, path) = write(&file());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            layout(&bytes, &path).unwrap(),
            Layout {
                samples: SAMPLES,
                traces: 6
            }
        );
        // One trace alone is its whole file.
        let one = record(1, "00001", &gga("120000", 8.5), SAMPLES);
        assert_eq!(layout(&one, &path).unwrap().traces, 1);
        for bad in [b"GPR!".to_vec(), b"#00001,".to_vec()] {
            assert!(layout(&bad, &path).is_err());
        }
    }

    #[test]
    fn a_wrong_checksum_is_skipped_and_a_wrapped_counter_is_read() {
        let bytes = file();
        let layout = layout(&bytes, Path::new("x.DSR")).unwrap();
        let found = fixes(&bytes, layout);
        // The wrapped counter took the comma before $GPGGA; still found.
        assert_eq!(found.iter().map(|f| f.trace).collect::<Vec<_>>(), [0, 2, 5]);
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // PROJ, as for the other position readers.
    fn positions_are_dated_and_cross_midnight() {
        let (_dir, path) = write(&file());
        let error = load_virl_positions(&path, None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--set date=YYYY-MM-DD"), "{error}");

        let date = chrono::NaiveDate::from_ymd_opt(2014, 4, 5).unwrap();
        let location = load_virl_positions(&path, Some(date), None).unwrap();
        let times = location
            .cor_points
            .iter()
            .map(|p| (p.trace_n, p.time_seconds))
            .collect::<Vec<_>>();
        let midnight = date
            .succ_opt()
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64;
        assert_eq!(
            times,
            [(0, midnight - 2.), (2, midnight - 1.), (5, midnight + 3.)]
        );
        assert_eq!(location.crs, "EPSG:32633");
    }

    #[test]
    fn the_header_needs_the_sampling_and_antenna_frequencies() {
        let (_dir, path) = write(&file());
        let date = chrono::NaiveDate::from_ymd_opt(2014, 4, 5);
        let error = load_virl_meta(&path, 0.168, Some(25.), None, None, date)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--set sampling_mhz=MHZ"), "{error}");
        let error = load_virl_meta(&path, 0.168, None, None, Some(200.), date)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--override antenna_mhz=MHZ"), "{error}");

        let meta = load_virl_meta(&path, 0.168, Some(25.), None, Some(200.), date).unwrap();
        assert_eq!(meta.samples, SAMPLES as u32);
        assert_eq!(meta.last_trace, 6);
        assert_eq!(meta.time_window, 30.);
        assert_eq!(meta.antenna_separation, 0.);
        // Fixes at traces 0, 2 and 5, 1, 1 and 4 s apart: 0.5, 1.33 s per trace.
        assert_eq!(meta.time_interval, 4. / 3.);
        // Without a date, the rest is still read.
        let undated = load_virl_meta(&path, 0.168, Some(25.), None, Some(200.), None).unwrap();
        assert_eq!(undated.time_interval, 0.);
    }

    /// Process `path` quietly into UTM 33N.
    fn run(path: &Path) -> Result<crate::gpr::GPR, String> {
        let params = crate::processing::RunParams {
            filepaths: vec![path.to_path_buf()],
            output_path: None,
            track_path: None,
            render_path: None,
            radargram_id: Some("test".into()),
            display_name: None,
            options: crate::processing::ProcessingOptions {
                quiet: true,
                crs: Some("EPSG:32633".into()),
                ..Default::default()
            },
        };
        crate::processing::build_processed_gpr(params)
            .map(|(gpr, _)| gpr)
            .map_err(|e| e.to_string())
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn a_recording_is_processed_with_what_its_sidecar_gives() {
        let (dir, path) = write(&file());
        let sidecar = dir.path().join("030_0001.ridal.toml");
        let error = run(&path).err().unwrap();
        assert!(error.contains("sampling_mhz"), "{error}");
        std::fs::write(
            &sidecar,
            "[overrides]\nsampling_mhz = 200\nantenna_mhz = 25\n",
        )
        .unwrap();
        let error = run(&path).err().unwrap();
        assert!(error.contains("--set date=YYYY-MM-DD"), "{error}");

        std::fs::write(
            &sidecar,
            "[overrides]\nsampling_mhz = 200\nantenna_mhz = 25\ndate = 2014-04-05\n\
             antenna_separation_m = 11.8\n",
        )
        .unwrap();
        let gpr = run(&path).unwrap();
        assert_eq!(gpr.data.shape(), &[SAMPLES, 6]);
        assert_eq!(gpr.metadata.time_window, 30.);
        assert_eq!(gpr.metadata.antenna_separation, 11.8);
        assert_eq!(gpr.location.cor_points.len(), 6);
        assert_eq!(
            gpr.inputs.as_ref().unwrap().bundles[0].format,
            "virl".to_string()
        );
    }

    #[test]
    fn one_fix_for_every_trace_is_warned_about() {
        let sentence = gga("120000", 8.5);
        let one_fix = (1..=4)
            .map(|n| record(n, "00001", &sentence, SAMPLES))
            .collect::<Vec<_>>()
            .concat();
        for (bytes, warned) in [(one_fix, true), (file(), false)] {
            let (_dir, path) = write(&bytes);
            let resolved = crate::formats::resolve_input(&path).unwrap();
            let warnings = crate::processing::assumptions(
                &[resolved],
                &crate::processing::ProcessingOptions {
                    overrides: crate::sidecar::Overrides {
                        antenna_separation_m: Some(0.),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                warnings
                    .iter()
                    .any(|w| w.contains("gave one fix for its 4 traces")),
                warned,
                "{warnings:?}"
            );
        }
    }

    #[test]
    fn the_data_are_about_the_adc_s_zero() {
        let (_dir, path) = write(&file());
        let data = load_virl_data(&path).unwrap();
        assert_eq!(data.shape(), &[SAMPLES, 6]);
        // Trace n (from 1), sample s: 10 n + s above the ADC's zero.
        assert_eq!(data[[0, 0]], 10.);
        assert_eq!(data[[3, 5]], 63.);
    }
}
