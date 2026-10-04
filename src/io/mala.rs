use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

use ndarray::Array2;

use crate::formats;
use crate::gpr;

/// Load and parse a Malå metadata file (.rad)
///
/// # Arguments
/// - `filepath`: The filepath of the input metadata file
/// - `medium_velocity`: The velocity of the portrayed medium to assign the GPR data
/// - `override_antenna_mhz`: Optional antenna frequency override (will not read from metadata).
/// - `override_antenna_separation`: Optional antenna separation override (will not read from metadata).
///
/// # Returns
/// A gpr::GPRMeta instance.
///
/// # Errors
/// - The file could not be read
/// - The contents could not be parsed correctly
/// - The associated ".rd3" file does not exist.
pub fn load_rad(
    filepath: &Path,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let bytes = std::fs::read(Path::new(filepath))?; // read as raw bytes
    let content = String::from_utf8_lossy(&bytes); // &str with invalid bytes replaced

    // Collect all rows into a hashmap, assuming a "KEY:VALUE" structure.
    let data: HashMap<&str, &str> = content.lines().filter_map(|s| s.split_once(':')).collect();

    let rd3_filepath = formats::find_neighbor_case_insensitive(filepath, "rd3")
        .unwrap_or_else(|| filepath.with_extension("rd3"));
    if !rd3_filepath.is_file() {
        return Err(format!("File not found: {rd3_filepath:?}").into());
    };

    // Extract and parse all required metadata into a new GPRMeta object.
    let antenna = data
        .get("ANTENNAS")
        .ok_or("No 'ANTENNAS' key in metadata")?
        .trim()
        .to_string();

    let antenna_mhz = match override_antenna_mhz {
        Some(v) => v,
        None => antenna.split("MHz").collect::<Vec<&str>>()[0]
            .trim()
            .parse::<f32>()
            .map_err(|e| {
                format!("Could not read frequency from the antenna field ({e:?}). Try using the antenna MHz override")
            })?
    };

    Ok(gpr::GPRMeta {
        samples: data
            .get("SAMPLES")
            .ok_or("No 'SAMPLES' key in metadata")?
            .trim()
            .parse()?,
        frequency: data
            .get("FREQUENCY")
            .ok_or("No 'FREQUENCY' key in metadata")?
            .trim()
            .parse()?,
        frequency_steps: data
            .get("FREQUENCY STEPS")
            .ok_or("No 'FREQUENCY STEPS' key in metadata")?
            .trim()
            .parse()?,
        time_interval: data
            .get("TIME INTERVAL")
            .ok_or("No 'TIME INTERVAL' key in metadata")?
            .replace(' ', "")
            .parse()?,
        antenna_mhz,
        antenna,
        antenna_separation: match override_antenna_separation {
            Some(v) => v,
            None => data
                .get("ANTENNA SEPARATION")
                .ok_or("No 'ANTENNA SEPARATION' key in metadata")?
                .trim()
                .parse()?,
        },
        time_window: data
            .get("TIMEWINDOW")
            .ok_or("No 'TIMEWINDOW' key in metadata")?
            .trim()
            .parse()?,
        last_trace: data
            .get("LAST TRACE")
            .ok_or("No 'LAST TRACE' key in metadata")?
            .trim()
            .parse()?,
        data_filepath: rd3_filepath,
        medium_velocity,
    })
}

/// Load and parse a Malå ".cor" location file
///
/// # Arguments
/// - `filepath`: The path to the file to read.
/// - `projected_crs`: Any projected CRS understood by PROJ to project the coordinates into
///
/// # Returns
/// The parsed location points in a GPRLocation object.
///
/// # Errors
/// - The file could not be found/read
/// - `projected_crs` is not understood by PROJ
/// - The contents of the file could not be parsed.
pub fn load_cor(
    filepath: &Path,
    projected_crs: Option<&String>,
) -> Result<gpr::GPRLocation, Box<dyn Error>> {
    let content = std::fs::read_to_string(filepath)?;

    // Create a new empty points vec
    let mut coords = Vec::<crate::coords::Coord>::new();
    let mut points: Vec<gpr::CorPoint> = Vec::new();
    // Loop over the lines of the file and parse CorPoints from it
    for line in content.lines() {
        // Split the line into ten separate columns.
        let data: Vec<&str> = line.split_whitespace().collect();

        // If the line could not be split in ten columns, it is probably wrong.
        if data.len() < 10 {
            continue;
        };

        let Ok(mut latitude) = data[3].parse::<f64>() else {
            continue;
        };
        let Ok(mut longitude) = data[5].parse::<f64>() else {
            continue;
        };

        // Invert the sign of the latitude if it's on the southern hemisphere
        if data[4].trim() == "S" {
            latitude *= -1.;
        };

        // Invert the sign of the longitude if it's west of the prime meridian
        if data[6].trim() == "W" {
            longitude *= -1.;
        };

        // Ugly fix for 9:00:00 -> 09:00:00
        let mut time_str = data[2].to_string();
        if time_str.len() == 7 {
            time_str = "0".to_string() + time_str.as_str();
        }
        // Parse the date and time columns into datetime, then convert to seconds after UNIX epoch.
        // In some odd cases, the time information is wrong. Those lines should b eskipped
        let Ok(datetime_obj) =
            chrono::DateTime::parse_from_rfc3339(&format!("{}T{}+00:00", data[1], time_str))
        else {
            continue;
        };
        let datetime = datetime_obj.timestamp() as f64;

        let Ok(altitude) = data[7].parse::<f64>() else {
            continue;
        };

        // The ".cor"-files are 1-indexed whereas this is 0-indexed
        let Ok(trace_n) = data[0].parse::<i64>().map(|v| v - 1) else {
            continue;
        };

        // If the trace number in the corfile is 0, then this will overflow
        if trace_n < 0 {
            continue;
        };

        coords.push(crate::coords::Coord {
            x: longitude,
            y: latitude,
        });

        // Coordinates are 0 right now. That's fixed right below
        points.push(gpr::CorPoint {
            trace_n: trace_n as u32,
            time_seconds: datetime,
            easting: 0.,
            northing: 0.,
            altitude,
        });
    }

    if points.is_empty() {
        return Err(format!("Could not parse location data from: {:?}", filepath).into());
    }

    let crs = super::project_points(&mut points, &coords, projected_crs)?;

    Ok(gpr::GPRLocation {
        cor_points: points,
        correction: gpr::LocationCorrection::None,
        crs,
    })
}

/// Load a Malå data (.rd3) file
///
/// # Arguments
/// - `filepath`: The path of the file to read.
/// - `height`: The expected height of the data. The width is parsed automatically.
///
/// # Returns
/// A 2D array of 32 bit floating point values in the shape (height, width).
///
/// # Errors
/// - The file cannot be read
/// - The length does not work with the expected shape
pub fn load_rd3(filepath: &Path, height: usize) -> Result<Array2<f32>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(filepath)?;

    let mut data: Vec<f32> = Vec::new();

    // It's 50V (50000mV) in RGPR https://github.com/emanuelhuber/RGPR/blob/d78ff7745c83488111f9e63047680a30da8f825d/R/readMala.R#L8
    let bits_to_millivolt = 50000. / i16::MAX as f32;

    // The values are read as 16 bit little endian signed integers, and are converted to millivolts
    for byte_pair in bytes.as_chunks::<2>().0 {
        let value = i16::from_le_bytes(*byte_pair);
        data.push(value as f32 * bits_to_millivolt);
    }

    let width: usize = data.len() / height;

    Ok(ndarray::Array2::from_shape_vec((width, height), data)?.reversed_axes())
}

#[cfg(test)]
mod tests {
    use super::{load_cor, load_rad};

    /// Fake some data. One point is in the northern hemisphere and one is in the southern
    fn fake_cor_text() -> String {
        [
            "1\t2022-01-01\t00:00:01\t78.0\tN\t16.0\tE\t100.0\tM\t1",
            "10\t2022-01-01\t9:01:00\t78.0\tS\t16.0\tW\t100.0\tM\t1",
            "0\t2022-01-01\t00:01:00\t78.0\tS\t16.0\tW\t100.0\tM\t1", // Trace starts at 0 (bad)
            "11\t2022-01", // This simulates an unfinished line that should be skipped
            "000000\tN\t17.433201666667\tE\t332.20\tM\t2.00", // Another bad line that should be skipped
            "9673\t2011-05-07\t18:95\t79.89\tN\t23.88\tE\t722.1317\tM\t0.62", // Bad time
            "14897\t2010-05-05\t1.:00:\t79.793\tN\t23.32\tE\t692.8199\tM\t0.58", // Another bad time
            "21584\t2010-05-05\t12:04:58   79.78905884333\tN 23.23301804333 E M 2        0.58.0592", // Bad elevation and mixed whitespace/tab
        ]
        .join("\r\n")
    }

    #[test]
    // EPSG:4326 is not UTM, so this runs `projinfo` and must not overlap the
    // `serial` tests that unset `PATH` (see `coords::tests::test_projinfo_to_wkt`).
    #[serial_test::parallel]
    #[cfg(not(target_os = "windows"))] // Added 2026-02-17 because gdal is hard to install in CI
    fn test_load_cor() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cor_path = temp_dir.path().join("hello.cor");

        std::fs::write(&cor_path, fake_cor_text()).unwrap();

        // Load it and "convert" (or rather don't convert) the CRS to WGS84
        let locations = load_cor(&cor_path, Some(&"EPSG:4326".to_string())).unwrap();

        println!("{locations:?}");
        assert_eq!(locations.cor_points.len(), 2);

        // Check that the trace number is now zero based, and that the other fields were read
        // correctly
        assert_eq!(locations.cor_points[0].trace_n, 0);
        assert_eq!(locations.cor_points[0].easting, 16.0);
        assert_eq!(locations.cor_points[0].northing, 78.0);
        assert_eq!(locations.cor_points[0].altitude, 100.0);
        assert_eq!(
            locations.cor_points[0].time_seconds,
            chrono::DateTime::parse_from_rfc3339("2022-01-01T00:00:01+00:00")
                .unwrap()
                .timestamp() as f64
        );

        // Check that the second point has inverted signs (since it's 78*S, 16*W)
        assert_eq!(locations.cor_points[1].easting, -16.0);
        assert_eq!(locations.cor_points[1].northing, -78.0);

        // Load the data again but convert it to WGS84 UTM Zone 33N
        let locations = load_cor(&cor_path, Some(&"EPSG:32633".to_string())).unwrap();

        // Check that the coordinates are within reason
        assert!(
            (locations.cor_points[0].easting > 500_000_f64)
                & (locations.cor_points[0].easting < 600_000_f64)
        );
        assert!(
            (locations.cor_points[0].northing > 8_000_000_f64)
                & (locations.cor_points[0].easting < 9_000_000_f64)
        );
        assert!(
            (locations.cor_points[1].northing < 0_f64)
                & (locations.cor_points[1].northing > -9_000_000_f64)
        );
    }

    #[test]
    fn test_load_rad() {
        // Fake a .rad metadata file
        let temp_dir = tempfile::tempdir().unwrap();
        let rad_path = temp_dir.path().join("hello.rad");
        let rd3_path = rad_path.with_extension("rd3");
        let rad_text = [
            "SAMPLES:2024",
            "FREQUENCY:                 1000.",
            "FREQUENCY STEPS: 20",
            "TIME INTERVAL: 0.1",
            "ANTENNAS: 100 MHz unshielded",
            "ANTENNA SEPARATION: 0.5",
            "TIMEWINDOW:2000",
            "LAST TRACE: 40",
        ]
        .join("\r\n");

        std::fs::write(&rad_path, rad_text).unwrap();

        // The rd3 file needs to exist, but it doesn't need to contain anything
        std::fs::write(&rd3_path, "").unwrap();

        let gpr_meta = load_rad(&rad_path, 0.1, None, None).unwrap();

        // Check that the correct values were parsed
        assert_eq!(gpr_meta.samples, 2024);
        assert_eq!(gpr_meta.frequency, 1000.);
        assert_eq!(gpr_meta.frequency_steps, 20);
        assert_eq!(gpr_meta.time_interval, 0.1);
        assert_eq!(gpr_meta.antenna_mhz, 100.);
        assert_eq!(gpr_meta.antenna_separation, 0.5);
        assert_eq!(gpr_meta.time_window, 2000.);
        assert_eq!(gpr_meta.last_trace, 40);
        assert_eq!(gpr_meta.data_filepath, rd3_path);

        // Test overriding the antenna frequency
        let gpr_meta = load_rad(&rad_path, 0.1, Some(200.), Some(1.25)).unwrap();
        assert_eq!(gpr_meta.antenna_mhz, 200.);
        assert_eq!(gpr_meta.antenna_separation, 1.25);
    }

    #[test]
    fn test_load_rad_bad_antenna_mhz() {
        // Fake a .rad metadata file
        let temp_dir = tempfile::tempdir().unwrap();
        let rad_path = temp_dir.path().join("hello.rad");
        let rd3_path = rad_path.with_extension("rd3");
        let rad_text = [
            "SAMPLES:2024",
            "FREQUENCY:                 1000.",
            "FREQUENCY STEPS: 20",
            "TIME INTERVAL: 0.1",
            "ANTENNAS: onehundredmegaherzz unshielded",
            "ANTENNA SEPARATION: 0.5",
            "TIMEWINDOW:2000",
            "LAST TRACE: 40",
        ]
        .join("\r\n");

        std::fs::write(&rad_path, rad_text).unwrap();

        // The rd3 file needs to exist, but it doesn't need to contain anything
        std::fs::write(&rd3_path, "").unwrap();

        // This should return an error
        let gpr_meta_fail = load_rad(&rad_path, 0.1, None, None);
        assert!(gpr_meta_fail.is_err());

        let err_msg = gpr_meta_fail.unwrap_err().to_string();
        assert!(
            err_msg.contains("frequency from the antenna field"),
            "Got:     {err_msg:?}\nExpected 'Could not read frequency from the antenna field'",
        );
        assert!(load_rad(&rad_path, 0.1, None, None).is_err());

        let gpr_meta = load_rad(&rad_path, 0.1, Some(100.), Some(2.5)).unwrap();
        assert_eq!(gpr_meta.antenna_mhz, 100.);
        assert_eq!(gpr_meta.antenna_separation, 2.5);
    }
}
