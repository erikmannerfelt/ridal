//! Functions to handle input and output (I/O) of GPR data files.
//!
//! Each supported input format has its own submodule, so adding another one is
//! a new file rather than another branch in a shared one:
//!
//! - [`mala`]: Malå RAMAC (`.rad` / `.rd3` / `.cor`).
//! - [`pulseekko`]: Sensors & Software pulseEKKO (`.hd` / `.dt1` / `.gp2`).
//! - [`gssi`]: GSSI (`.DZT` / `.DZG`).
//! - [`ridal`]: Ridal's own products -- the processed NetCDF and the track
//!   CSV -- and the recogniser that reads the NetCDF back.
//!
//! What more than one of them needs lives here: [`read_gga`], the NMEA
//! `$GPGGA` sentence parser shared by the GSSI and pulseEKKO coordinate
//! sidecars, and [`project_points`], which turns parsed WGS84 positions into
//! the projected CRS a [`crate::gpr::GPRLocation`] carries.

mod gssi;
mod mala;
mod pulseekko;
mod ridal;

pub use gssi::{load_dzt, load_gssi_dzg, load_gssi_dzt};
pub use mala::{load_cor, load_rad, load_rd3};
pub use pulseekko::{load_pe_dt1, load_pe_gp2, load_pe_hd};
// This is a facade: the submodules are private, and `io` itself is private to
// the crate, so a name no caller happens to use -- `RidalNetcdfMetadata` is
// only reached through `RidalNetcdfKind` -- reads as an unused import even
// though keeping the path is the point.
#[allow(unused_imports)]
pub use ridal::{
    export_locations, export_netcdf, inspect_ridal_netcdf, legacy_reason, RidalNetcdfKind,
    RidalNetcdfMetadata,
};

use std::error::Error;

use crate::gpr;

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

    let lat_str = parts.get(2).unwrap();
    let mut lat = lat_str[..2].parse::<f64>()? + (lat_str[2..].parse::<f64>()? / 60.);

    if parts.get(3) == Some(&"S") {
        lat *= -1.;
    }

    let lon_str = parts.get(4).unwrap();
    let mut lon = lon_str[..3].parse::<f64>()? + (lon_str[3..].parse::<f64>()? / 60.);

    if parts.get(5) == Some(&"W") {
        lon *= -1.;
    }

    let coord = crate::coords::Coord { x: lon, y: lat };

    let elev = parts.get(9).unwrap().parse::<f64>()?;

    let time_str = parts.get(1).unwrap();
    let hr = time_str[..2].to_string();
    let min = time_str[2..4].to_string();
    let sec = time_str[4..].to_string();

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
