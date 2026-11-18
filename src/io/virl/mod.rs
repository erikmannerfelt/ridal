//! VIRL radar files, as written by the VIRL radars of Vasilenko and the
//! Universidad Politécnica de Madrid. Two versions are read:
//!
//! - [`dsr`]: the VIRL-7's `.DSR`, one file of records that each hold a
//!   trace and the GPS's last `$GPGGA` sentence.
//! - [`hdr`]: the VIRL-6's `<stem>.hdr` and `<stem>.bin`, a text table of
//!   positions with a row per trace, and the traces.
//!
//! Both are one format, `virl`, told apart by the header's extension. In
//! both, the GPS's last fix stands for every trace until the next, so
//! several traces share one; a position is kept at the first trace of each,
//! and the traces between are interpolated.
//!
//! What neither records: the sampling frequency, the date (a fix has only
//! the time of day), the antenna frequency and the separation. The first
//! three must be given, in the recording's sidecar or as arguments; the
//! separation is 0 m without one.

use std::error::Error;
use std::path::Path;

use ndarray::Array2;

use crate::gpr;

mod dsr;
mod hdr;

#[cfg(test)]
pub(crate) use dsr::test_dsr;
#[cfg(test)]
pub(crate) use hdr::test_hdr;
pub use hdr::{is_virl6_header, virl6_bin_beside};

/// What a recording of either version holds, but its traces.
struct Recording {
    samples: usize,
    traces: usize,
    /// The first trace of each fix, or `None` for a recording made without
    /// a GPS, which has no positions at all.
    fixes: Option<Vec<Fix>>,
}

/// One trace with a new fix: the trace, from 0, the fix's time of day in
/// seconds, its WGS84 position and its altitude.
struct Fix {
    trace: usize,
    seconds_of_day: f64,
    coord: crate::coords::Coord,
    altitude: f64,
}

/// Read the recording whose header is `path`: a `.hdr` is a VIRL-6's,
/// anything else a VIRL-7's `.DSR`.
fn read(path: &Path) -> Result<Recording, Box<dyn Error>> {
    if is_hdr(path) {
        hdr::read(path)
    } else {
        dsr::read(path)
    }
}

fn is_hdr(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("hdr"))
}

/// The error for a recording made without a GPS.
fn no_positions(path: &Path) -> Box<dyn Error> {
    format!(
        "{} records no positions: its table has no lat, lon and time columns, as when the GPS \
         was not connected. Give them in a <stem>.coords.csv beside it, or with --cor (cor= in \
         Python)",
        path.display()
    )
    .into()
}

/// A fix as read: its trace, its time in seconds since the epoch, its
/// WGS84 position and its altitude.
struct TimedFix {
    trace: u32,
    time: f64,
    coord: crate::coords::Coord,
    altitude: f64,
}

/// Each fix's time of day on `date`, moving to the next day when the time
/// of day goes back by more than 12 hours. A fix not later than the one
/// before is left out, since the times between fixes are interpolated.
fn timed_fixes(fixes: &[Fix], date: chrono::NaiveDate) -> Vec<TimedFix> {
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc()
        .timestamp() as f64;
    let mut out: Vec<TimedFix> = Vec::new();
    let mut previous_of_day: Option<f64> = None;
    let mut days = 0.;
    for fix in fixes {
        let of_day = fix.seconds_of_day;
        if previous_of_day.is_some_and(|previous| of_day < previous - 43200.) {
            days += 86400.;
        }
        previous_of_day = Some(of_day);
        let time = midnight + of_day + days;
        if out.last().is_some_and(|last| time <= last.time) {
            continue;
        }
        out.push(TimedFix {
            trace: fix.trace as u32,
            time,
            coord: fix.coord,
            altitude: fix.altitude,
        });
    }
    out
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

/// The header of a VIRL recording, which is the size of its traces and
/// the values given for what it does not record.
///
/// # Errors
/// A file that is not a VIRL file, or no sampling frequency or antenna
/// frequency given.
pub fn load_virl_meta(
    path: &Path,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
    sampling_mhz: Option<f32>,
    date: Option<chrono::NaiveDate>,
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let recording = read(path)?;
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
    let time_interval = match (date, &recording.fixes) {
        (Some(date), Some(fixes)) => time_interval(&timed_fixes(fixes, date)),
        _ => 0.,
    };
    Ok(gpr::GPRMeta {
        samples: recording.samples as u32,
        frequency,
        frequency_steps: 0,
        time_interval,
        antenna,
        antenna_mhz,
        antenna_separation: override_antenna_separation.unwrap_or(0.),
        time_window: 1000. * recording.samples as f32 / frequency,
        last_trace: recording.traces as u32,
        data_filepath: path.to_path_buf(),
        medium_velocity,
    })
}

/// The positions of a VIRL recording, at the first trace of each fix.
///
/// # Errors
/// No date given, a recording made without a GPS, or no usable fix.
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
    let fixes = read(path)?.fixes.ok_or_else(|| no_positions(path))?;
    let timed = timed_fixes(&fixes, date);
    if timed.is_empty() {
        return Err(format!("{} has no usable GPS fix", path.display()).into());
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

/// How many usable fixes a VIRL recording has, and how many traces, for a
/// warning when one fix stands for them all.
///
/// # Errors
/// A recording made without a GPS, whose positions cannot be read at all.
pub fn virl_fix_count(path: &Path) -> Result<(usize, usize), Box<dyn Error>> {
    let recording = read(path)?;
    let fixes = recording.fixes.ok_or_else(|| no_positions(path))?;
    Ok((fixes.len(), recording.traces))
}

/// The traces of the VIRL recording whose header is `path`, samples by
/// traces.
pub fn load_virl_data(path: &Path) -> Result<Array2<f32>, Box<dyn Error>> {
    if is_hdr(path) {
        hdr::load_data(path)
    } else {
        dsr::load_data(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(trace: usize, seconds_of_day: f64) -> Fix {
        Fix {
            trace,
            seconds_of_day,
            coord: crate::coords::Coord { x: 18., y: 78. },
            altitude: 0.,
        }
    }

    #[test]
    fn fixes_are_dated_and_cross_midnight() {
        let date = chrono::NaiveDate::from_ymd_opt(2014, 4, 5).unwrap();
        let midnight = date
            .succ_opt()
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64;
        let fixes = [
            fix(0, 86398.),
            fix(2, 86399.),
            // Not later than the one before: left out.
            fix(3, 86399.),
            fix(5, 3.),
        ];
        let timed = timed_fixes(&fixes, date);
        assert_eq!(
            timed.iter().map(|f| (f.trace, f.time)).collect::<Vec<_>>(),
            [(0, midnight - 2.), (2, midnight - 1.), (5, midnight + 3.)]
        );
        // 0.5 and 1.33 s per trace; the upper median.
        assert_eq!(time_interval(&timed), 4. / 3.);
        assert_eq!(time_interval(&timed[..1]), 0.);
    }
}
