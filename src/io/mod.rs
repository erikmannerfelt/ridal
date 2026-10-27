//! Functions to handle input and output (I/O) of GPR data files.
//!
//! Each supported input format has its own submodule, so adding another one is
//! a new file rather than another branch in a shared one:
//!
//! - [`mala`]: Malå RAMAC (`.rad` / `.rd3` / `.cor`).
//! - [`pulseekko`]: Sensors & Software pulseEKKO (`.hd` / `.dt1` / `.gp2`).
//! - [`gssi`]: GSSI (`.DZT` / `.DZG`).
//! - [`iceradar`]: Blue System Integration IceRadar (`.h5` / `.hdf5`), with
//!   the header, the data and the positions in one file.
//! - [`coords_csv`]: Ridal's coordinate CSV, which `--cor` reads in place of
//!   any format's own positions file (#13).
//! - [`ridal`]: Ridal's own products -- the processed NetCDF and the track
//!   CSV -- and the recogniser that reads the NetCDF back.
//!
//! What more than one of them needs lives here: [`read_gga`], the NMEA
//! `$GPGGA` sentence parser shared by the GSSI and pulseEKKO coordinate
//! sidecars, and [`project_points`], which turns parsed WGS84 positions into
//! the projected CRS a [`crate::gpr::GPRLocation`] carries.

mod coords_csv;
mod gssi;
mod iceradar;
mod mala;
mod pulseekko;
mod ridal;

pub use coords_csv::{load_coords_csv, write_coords_csv, AltOrigin, CoordsRow, TimeOrigin};
pub use gssi::{load_dzt, load_gssi_dzg, load_gssi_dzt};
pub use iceradar::{
    iceradar_date, iceradar_lines, load_iceradar_data, load_iceradar_meta, load_iceradar_positions,
    IceRadarDate,
};
pub use mala::{load_cor, load_rad, load_rd3};
pub use pulseekko::{load_pe_dt1, load_pe_gp2, load_pe_hd};
// This is a facade: the submodules are private, and `io` itself is private to
// the crate, so a name no caller happens to use -- `RidalNetcdfMetadata` is
// only reached through `RidalNetcdfKind` -- reads as an unused import even
// though keeping the path is the point.
#[allow(unused_imports)]
pub use ridal::{
    export_locations, export_netcdf, inspect_ridal_netcdf, legacy_reason, read_recipe,
    RidalNetcdfKind, RidalNetcdfMetadata,
};

use std::error::Error;

use crate::gpr;

/// A header whose antenna field gives no frequency, which an override
/// supplies (#409). Its own type, so a caller that can ask for the
/// override -- the ingest planner, for each antenna it finds -- knows this
/// case from any other unreadable file without reading the message.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownAntennaFrequency {
    /// The antenna field as the header has it.
    pub antenna: String,
    message: String,
}

impl UnknownAntennaFrequency {
    fn new(antenna: impl Into<String>, message: String) -> Self {
        Self {
            antenna: antenna.into(),
            message,
        }
    }
}

impl std::fmt::Display for UnknownAntennaFrequency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for UnknownAntennaFrequency {}

/// Parse one NMEA `$GPGGA` sentence into an epoch time, a WGS84 coordinate and
/// an elevation.
///
/// Shared because both the GSSI `.DZG` and the pulseEKKO `.gp2` sidecars
/// carry their positions as `$GPGGA` sentences; there is no per-format
/// difference to preserve.
fn read_gga(gga_str: &str, date: &str) -> Result<(f64, crate::coords::Coord, f64), Box<dyn Error>> {
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut date = date.to_string();
    for (i, month) in months.iter().enumerate() {
        date = date.replace(month, &format!("{:02}", (i + 1)));
    }

    let parts: Vec<&str> = gga_str.split(",").collect();

    // A GGA sentence is attacker-controlled for an uploaded file, so every
    // field and every fixed-width split is fallible rather than a panic. `at`
    // names a missing field; `slice` refuses a byte range that is out of
    // bounds or splits a multi-byte character.
    let at = |i: usize| -> Result<&str, Box<dyn Error>> {
        parts
            .get(i)
            .copied()
            .ok_or_else(|| format!("GGA field {i} is missing").into())
    };
    fn slice(s: &str, range: std::ops::Range<usize>) -> Result<&str, Box<dyn Error>> {
        s.get(range.clone())
            .ok_or_else(|| format!("GGA field {s:?} is too short for {range:?}").into())
    }

    let lat_str = at(2)?;
    let mut lat = slice(lat_str, 0..2)?.parse::<f64>()?
        + (slice(lat_str, 2..lat_str.len())?.parse::<f64>()? / 60.);

    if parts.get(3) == Some(&"S") {
        lat *= -1.;
    }

    let lon_str = at(4)?;
    let mut lon = slice(lon_str, 0..3)?.parse::<f64>()?
        + (slice(lon_str, 3..lon_str.len())?.parse::<f64>()? / 60.);

    if parts.get(5) == Some(&"W") {
        lon *= -1.;
    }

    let coord = crate::coords::Coord { x: lon, y: lat };

    let elev = at(9)?.parse::<f64>()?;

    let time_str = at(1)?;
    let hr = slice(time_str, 0..2)?.to_string();
    let min = slice(time_str, 2..4)?.to_string();
    let sec = slice(time_str, 4..time_str.len())?.to_string();

    let datetime =
        chrono::DateTime::parse_from_rfc3339(&format!("{}T{}:{}:{}+00:00", date, hr, min, sec))?
            .timestamp() as f64;

    Ok((datetime, coord, elev))
}

/// Project `wgs84` and write the easting/northing of each position onto the
/// point at the same index, returning the CRS the result is in.
///
/// The three coordinate parsers (`load_cor`, `load_pe_gp2`, `load_gssi_dzg`)
/// all end the same way: pick the caller's CRS or the UTM zone best suited to
/// the first position, project every WGS84 coordinate, and store the result.
/// Keeping it here means that choice is made once.
///
/// `points` and `wgs84` are parallel and non-empty; the callers have already
/// refused an empty parse before this is reached.
fn project_points(
    points: &mut [gpr::CorPoint],
    wgs84: &[crate::coords::Coord],
    projected_crs: Option<&String>,
) -> Result<String, Box<dyn Error>> {
    let projected_crs = match projected_crs {
        Some(s) => s.to_string(),
        None => crate::coords::UtmCrs::optimal_crs(&wgs84[0]).to_epsg_str(),
    };
    for (point, coord) in points.iter_mut().zip(crate::coords::from_wgs84(
        wgs84,
        &crate::coords::Crs::from_user_input(&projected_crs)?,
    )?) {
        point.easting = coord.x;
        point.northing = coord.y;
    }
    Ok(projected_crs)
}

#[cfg(test)]
mod tests {
    use super::read_gga;

    /// A GGA sentence from an uploaded file is untrusted. Short field counts
    /// and short or non-ASCII fixed-width fields used to panic on `.unwrap()`
    /// and on byte-index slicing; each must now be a graceful error.
    #[test]
    fn a_malformed_gga_errors_rather_than_panicking() {
        let date = "2025-Apr-04";
        for gga in [
            "",                                   // no fields at all
            "$GPGGA,130857.30",                   // too few fields for lat/lon/elev
            "$GPGGA,130857.30,7,N,0,E,,,,",       // lat/lon too short for the fixed split
            "$GPGGA,1,7719.19,N,01522.64,E,,,,9", // time field too short
            "$GPGGA,130857.30,7é,N,0é9,E,,,,100", // multi-byte char across a split boundary
        ] {
            // The assertion is simply that the call returns; a panic would
            // fail the test.
            let _ = read_gga(gga, date);
        }
        // A well-formed sentence still parses.
        let ok = read_gga(
            "$GPGGA,130857.30,7719.1908439,N,01522.6497456,E,2,42,0.8,625.490,M",
            date,
        );
        assert!(ok.is_ok(), "{ok:?}");
    }
}
