use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

use ndarray::Array2;

use crate::{formats, gpr};

pub fn load_pe_dt1(
    filepath: &Path,
    height: usize,
    width: usize,
) -> Result<Array2<f32>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(filepath)?;

    const TRACE_HEADER_BYTES: usize = 25 * 4 + 28; // 128

    // Based on one header. Should probably be set from the header itself.
    // Also, it's a bit unclear if it should be halved or not...
    let bits_to_millivolt = 104.12 / i16::MAX as f32;

    let bytes_per_trace = TRACE_HEADER_BYTES + height * 2;
    let expected_len = width * bytes_per_trace;

    if bytes.len() < expected_len {
        return Err(format!(
            "File too short: got {} bytes, expected at least {} bytes",
            bytes.len(),
            expected_len
        )
        .into());
    }

    let mut data: Vec<f32> = Vec::with_capacity(height * width);
    let mut offset: usize = 0;

    for _ in 0..width {
        offset += TRACE_HEADER_BYTES;

        let end = offset + height * 2;
        let slice = &bytes[offset..end];

        for j in 0..height {
            let k = j * 2;
            let v = i16::from_le_bytes([slice[k], slice[k + 1]]);
            data.push(v as f32 * bits_to_millivolt);
        }

        offset = end;
    }

    Ok(Array2::from_shape_vec((width, height), data)?.reversed_axes())
}

pub fn load_pe_hd(
    filepath: &Path,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let content = std::fs::read_to_string(filepath)?;

    // Collect all rows into a hashmap, assuming a "KEY:VALUE" structure.
    let mut data = HashMap::<&str, &str>::new();
    for (key, value) in content.lines().filter_map(|s| s.split_once('=')) {
        data.insert(key.trim(), value.trim());
    }
    let samples: u32 = data
        .get("NUMBER OF PTS/TRC")
        .ok_or("No 'NUMBER OF PTS/TRC' key in metadata")?
        .trim()
        .parse()?;
    let time_window: f32 = data
        .get("TOTAL TIME WINDOW")
        .ok_or("No 'TOTAL TIME WINDOW' key in metadata")?
        .trim()
        .parse()?;

    let frequency = 1000. * (samples as f32) / time_window;

    let dt1_filepath = formats::find_neighbor_case_insensitive(filepath, "dt1")
        .unwrap_or_else(|| filepath.with_extension("dt1"));
    if !dt1_filepath.is_file() {
        return Err(format!("File not found: {dt1_filepath:?}").into());
    };

    let antenna_mhz = match override_antenna_mhz {
        Some(v) => v,
        None => data
            .get("NOMINAL FREQUENCY")
            .ok_or("No 'NOMINAL FREQUENCY' key in metadata")?
            .replace(' ', "")
            .parse()
            .map_err(|e| {
                format!("Could not read frequency from the 'NOMINAL FREQUENCY' field ({e:?}). Try using the antenna MHz override")
            })?
    };

    Ok(gpr::GPRMeta {
        samples,
        frequency,
        frequency_steps: 0,
        time_interval: data
            .get("TRACE INTERVAL (s)")
            .ok_or("No 'TRACE INTERVAL (s)' key in metadata")?
            .replace(' ', "")
            .parse()?,
        antenna_mhz,
        antenna: data
            .get("NOMINAL FREQUENCY")
            .ok_or("No 'NOMINAL FREQUENCY' key in metadata")?
            .replace(' ', "")
            .parse::<String>()?
            + " MHz",
        antenna_separation: match override_antenna_separation {
            Some(v) => v,
            None => data
                .get("ANTENNA SEPARATION")
                .ok_or("No 'ANTENNA SEPARATION' key in metadata")?
                .trim()
                .parse()?,
        },
        time_window,
        last_trace: data
            .get("NUMBER OF TRACES")
            .ok_or("No 'NUMBER OF TRACES' key in metadata")?
            .trim()
            .parse()?,
        data_filepath: dt1_filepath,
        medium_velocity,
    })
}

pub fn load_pe_gp2(
    filepath: &Path,
    projected_crs: Option<&String>,
) -> Result<gpr::GPRLocation, Box<dyn Error>> {
    let content = std::fs::read_to_string(filepath)?;

    let mut date_str: Option<&str> = None;

    // Create a new empty points vec
    let mut coords = Vec::<crate::coords::Coord>::new();
    let mut points: Vec<gpr::CorPoint> = Vec::new();
    // Loop over the lines of the file and parse CorPoints from it
    for line in content.lines() {
        if line.starts_with(";") | line.starts_with("traces") {
            if line.contains("Date=") {
                date_str = Some(line.split_once("=").unwrap().1.split_once(" ").unwrap().0);
            }
            continue;
        };

        let data: Vec<&str> = line.splitn(5, ",").collect();

        let trace_n = (data[0].parse::<i64>()? - 1) as u32; // The ".cor"-files are 1-indexed whereas this is 0-indexed

        if points.last().map(|p| p.trace_n == trace_n) == Some(true) {
            continue;
        }

        let (datetime, coord, altitude) = super::read_gga(data[4], date_str.unwrap())?;

        coords.push(coord);

        // Coordinates are 0 right now. That's fixed right below
        points.push(gpr::CorPoint {
            trace_n,
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

#[cfg(test)]
mod tests {
    use super::{load_pe_gp2, load_pe_hd};

    #[test]
    #[cfg(not(target_os = "windows"))] // Added 2026-02-17 because gdal is hard to install in CI
    fn test_load_pe_hd() {
        // Fake a .rad metadata file
        let temp_dir = tempfile::tempdir().unwrap();
        let rad_path = temp_dir.path().join("hello.hd");
        let rd3_path = rad_path.with_extension("dt1");
        let hd_text = [
            "1234",
            "200MHz_lines - pulseEKKO v1.8.1423",
            "2025-Apr-04",
            "NUMBER OF TRACES   = 9896",
            "NUMBER OF PTS/TRC  = 1625",
            "TIMEZERO AT POINT  = 163.5",
            "TOTAL TIME WINDOW  = 650",
            "STARTING POSITION  = 0",
            "FINAL POSITION     = 9895",
            "STEP SIZE USED     = 1",
            "POSITION UNITS     = m",
            "NOMINAL FREQUENCY  = 200",
            "ANTENNA SEPARATION = 1",
            "PULSER VOLTAGE (V) = 250",
            "NUMBER OF STACKS   = 1024",
            "SURVEY MODE        = Reflection",
            "STACKING TYPE      = F1, P1024, DynaQ OFF",
            "ELEVATION DATA ENTERED : MAX = 704.945 MIN = 625.49",
            "X Y Z POSITIONS ADDED - LatLong",
            "TRIGGER MODE       = Free",
            "DATA TYPE          = I*2",
            "AMPLITUDE WINDOW (mV)= 104.12",
            "TRACE INTERVAL (s) = 0.2",
            "TRACEHEADERDEF_26  = ORIENA",
            "GPR SERIAL#        = 006785670042",
            "RX SERIAL#         = 009030322610",
            "DVL SERIAL#        = 0087-0052-3004",
            "TX SERIAL#         = 002431701007",
        ]
        .join("\r\n");

        std::fs::write(&rad_path, hd_text).unwrap();

        // The rd3 file needs to exist, but it doesn't need to contain anything
        std::fs::write(&rd3_path, "").unwrap();

        let gpr_meta = load_pe_hd(&rad_path, 0.1, None, None).unwrap();

        // Check that the correct values were parsed
        assert_eq!(gpr_meta.samples, 1625);
        assert_eq!(gpr_meta.frequency, 1000. * 1625. / 650.);
        // assert_eq!(gpr_meta.frequency_steps, 20);
        assert_eq!(gpr_meta.time_interval, 0.2);
        assert_eq!(gpr_meta.antenna_mhz, 200.);
        assert_eq!(gpr_meta.antenna_separation, 1.);
        assert_eq!(gpr_meta.time_window, 650.);
        assert_eq!(gpr_meta.last_trace, 9896);
        assert_eq!(gpr_meta.data_filepath, rd3_path);

        // Test overriding the antenna frequency
        let gpr_meta = load_pe_hd(&rad_path, 0.1, Some(300.), Some(1.75)).unwrap();
        assert_eq!(gpr_meta.antenna_mhz, 300.);
        assert_eq!(gpr_meta.antenna_separation, 1.75);
    }

    #[test]
    // EPSG:4326 is not UTM, so this runs `projinfo` and must not overlap the
    // `serial` tests that unset `PATH` (see `coords::tests::test_projinfo_to_wkt`).
    #[serial_test::parallel]
    #[cfg(not(target_os = "windows"))] // Added 2026-02-17 because gdal is hard to install in CI
    fn test_load_pe_gp2() {
        let temp_dir = tempfile::tempdir().unwrap();
        let gp2_path = temp_dir.path().join("hello.gp2");

        let gp2_text = [
            ";GPS@@@",
            ";Ver=1.1.0",
            ";DIP=2009-00152-00",
            ";Date=2025-Apr-04 02:08:52",
            ";----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------",
            "traces,odo_tick,pos(m),time_elapsed(s),GPS",
            "1,0,0.000000,0.028076,\"$GPGGA,130857.30,7719.1908439,N,01522.6497456,E,2,42,0.8,625.490,M,31.466,M,5.2,0123*40\"",
            "1,0,0.000000,0.131520,\"$GPGGA,130857.40,7719.1908439,N,01522.6497254,E,2,42,0.8,625.495,M,31.466,M,3.4,0123*46\"",
            "1,0,0.000000,0.227752,\"$GPGGA,130857.50,7719.1908439,N,01522.6497254,E,2,42,0.8,625.497,M,31.466,M,3.4,0123*45\"",
            "1,0,0.000000,0.331571,\"$GPGGA,130857.60,7719.1908439,N,01522.6497075,E,2,42,0.8,625.501,M,31.466,M,3.6,0123*4B\"",
            "2,0,0.000000,0.427717,\"$GPGGA,130857.70,7719.1908438,N,01522.6497080,E,2,42,0.8,625.502,M,31.466,M,3.6,0123*42\"",
            "2,0,0.000000,0.531579,\"$GPGGA,130857.80,7719.1908438,N,01522.6496916,E,2,42,0.8,625.505,M,31.466,M,3.8,0123*43\"",
            "3,0,0.000000,0.627810,\"$GPGGA,130857.90,7719.1908437,N,01522.6496922,E,2,42,0.8,625.507,M,31.466,M,3.8,0123*48\"",
            "3,0,0.000000,0.746427,\"$GPGGA,130858.00,7719.1908436,N,01522.6496784,E,2,42,0.8,625.509,M,31.466,M,4.0,0123*4C\"",
            "4,0,0.000000,0.827951,\"$GPGGA,130858.10,7719.1908435,N,01522.6496785,E,2,42,0.8,625.510,M,31.466,M,4.0,0123*47\"",
            "4,0,0.000000,0.931560,\"$GPGGA,130858.20,7719.1908434,N,01522.6496653,E,2,42,0.8,625.513,M,31.466,M,4.2,0123*4E\"",
            "5,0,0.000000,1.027760,\"$GPGGA,130858.30,7719.1908435,N,01522.6496658,E,2,42,0.8,625.515,M,31.466,M,4.2,0123*43\"",
            "5,0,0.000000,1.131538,\"$GPGGA,130858.40,7719.1908431,N,01522.6496540,E,2,42,0.8,625.516,M,31.466,M,4.4,0123*4F\"",
            "6,0,0.000000,1.227757,\"$GPGGA,130858.50,7719.1908433,N,01522.6496541,E,2,42,0.8,625.518,M,31.466,M,4.4,0123*43\"",
            "6,0,0.000000,1.331490,\"$GPGGA,130858.60,7719.1908428,N,01522.6496436,E,2,42,0.8,625.519,M,31.466,M,4.6,0123*48\"",
            "7,0,0.000000,1.427735,\"$GPGGA,130858.70,7719.1908427,N,01522.6496441,E,2,42,0.8,625.519,M,31.466,M,4.6,0123*46\"",
            "7,0,0.000000,1.531530,\"$GPGGA,130858.80,7719.1908423,N,01522.6496353,E,2,42,0.8,625.518,M,31.466,M,4.8,0123*46\"",
            "8,0,0.000000,1.627638,\"$GPGGA,130858.90,7719.1908423,N,01522.6496350,E,2,42,0.8,625.519,M,31.466,M,4.8,0123*45\"",
            "8,0,0.000000,1.735229,\"$GPGGA,130859.00,7719.1908420,N,01522.6496265,E,2,42,0.8,625.519,M,31.466,M,5.0,0123*40\"",
            "9,0,0.000000,1.827934,\"$GPGGA,130859.10,7719.1908422,N,01522.6496267,E,2,42,0.8,625.522,M,31.466,M,5.0,0123*49\"",
            "9,0,0.000000,1.931559,\"$GPGGA,130859.20,7719.1908419,N,01522.6496187,E,2,42,0.8,625.521,M,31.466,M,5.2,0123*4E\"",
        ]
        .join("\r\n");
        std::fs::write(&gp2_path, gp2_text).unwrap();

        let locations = load_pe_gp2(&gp2_path, Some(&"EPSG:4326".to_string())).unwrap();

        assert_eq!(locations.cor_points.len(), 9);
        assert!(locations.cor_points.first().unwrap().northing > 77.);
    }
}
