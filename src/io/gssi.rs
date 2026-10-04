use std::error::Error;
use std::path::Path;

use ndarray::Array2;

use crate::gpr;

fn gssi_date_to_iso(date: &str) -> Result<String, Box<dyn Error>> {
    if date.len() != 6 {
        return Err(format!("Invalid GSSI date: {date}").into());
    }

    let day = &date[0..2];
    let month = &date[2..4];
    let year = &date[4..6];
    let year = format!("20{year}");
    Ok(format!("{year}-{month}-{day}"))
}

type GssiHeader = (u16, u16, u16, f32, f32, f32, f32, u16, String);

fn read_gssi_header(bytes: &[u8]) -> Result<GssiHeader, Box<dyn Error>> {
    if bytes.len() < 128 {
        return Err("GSSI header is too short".into());
    }

    let u16_at = |offset: usize| u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
    let i16_at = |offset: usize| i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
    let f32_at = |offset: usize| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());

    let data = u16_at(2);
    let nsamp = u16_at(4);
    let bits = u16_at(6);
    let _zero = i16_at(8);
    let sps = f32_at(10);
    let spm = f32_at(14);
    let _mpm = f32_at(18);
    let position = f32_at(22);
    let range = f32_at(26);
    let _npass = u16_at(30);
    let _create = &bytes[32..36];
    let _modify = &bytes[36..40];
    let _rgain = u16_at(40);
    let _nrgain = u16_at(42);
    let _text = u16_at(44);
    let _ntext = u16_at(46);
    let _proc = u16_at(48);
    let _nproc = u16_at(50);
    let nchan = u16_at(52);
    let antname = String::from_utf8_lossy(&bytes[98..112])
        .trim_matches('\0')
        .trim()
        .to_string();

    Ok((data, nsamp, bits, sps, spm, position, range, nchan, antname))
}

fn gssi_data_offset(bytes: &[u8], bits: u16, data: u16, nchan: u16) -> usize {
    let bytes_per_sample = (bits as usize / 8).max(1);
    let offset = if data < 1024 {
        1024 * data as usize
    } else {
        1024 * nchan as usize
    };
    if offset < bytes.len() {
        offset
    } else {
        32768 * bytes_per_sample
    }
}

fn gssi_bits_to_millivolt(bits: u16) -> f32 {
    // RGPR's readDZT.R uses bits2volt(Vmax = 50, nbits = bits), i.e.
    // abs(Vmax - Vmin) / 2^nbits with Vmin = -Vmax, then ridal exports mV.
    100000.0 / 2f32.powi(bits as i32)
}

fn gssi_antenna_mhz(antname: &str) -> Option<f32> {
    // RGPR readDZT.R antenna map.
    match antname.trim() {
        "3200" | "3200MLF" => None,
        "500MHz" => Some(500.),
        "3207" | "3207AP" => Some(100.),
        "5106" | "5106A" | "50200HS" => Some(200.),
        "50300" => Some(300.),
        "350" | "350HS" => Some(350.),
        "50270" | "50270S" => Some(270.),
        "50400" | "50400S" => Some(400.),
        "800" | "D50800" => Some(800.),
        "3101" | "3101A" => Some(900.),
        "51600" | "51600S" => Some(1600.),
        "62000" | "62000-003" => Some(2000.),
        "62300" | "62300XT" => Some(2300.),
        "52600" | "52600S" => Some(2600.),
        other => other
            .split_whitespace()
            .next()
            .and_then(|token| token.trim_end_matches("MHz").parse::<f32>().ok()),
    }
}

pub fn load_gssi_dzt(
    filepath: &Path,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let bytes = std::fs::read(filepath)?;
    let (data, nsamp, bits, sps, _spm, position, range, nchan, antenna) = read_gssi_header(&bytes)?;
    let time_window = range - position;
    let frequency = 1000. * (nsamp as f32) / time_window;
    let antenna_mhz = match override_antenna_mhz {
        Some(v) => v,
        None => gssi_antenna_mhz(&antenna).ok_or_else(|| {
            format!(
                "Could not read frequency from the GSSI antenna field ({antenna:?}). Try using the antenna MHz override"
            )
        })?,
    };

    let data_offset = gssi_data_offset(&bytes, bits, data, nchan);
    if data_offset >= bytes.len() {
        return Err(format!("File too short: no data found in {:?}", filepath).into());
    }
    let bytes_per_sample = (bits as usize / 8).max(1);
    let samples_per_trace = nsamp as usize;
    let bytes_per_trace = samples_per_trace * bytes_per_sample;
    if bytes_per_trace == 0 {
        return Err("Invalid GSSI header: zero sample size".into());
    }
    let trace_bytes = bytes.len() - data_offset;
    let last_trace = (trace_bytes / bytes_per_trace) as u32;

    Ok(gpr::GPRMeta {
        samples: nsamp as u32,
        frequency,
        frequency_steps: 0,
        time_interval: 1.0 / sps,
        antenna_mhz,
        antenna,
        antenna_separation: override_antenna_separation.unwrap_or(0.0),
        time_window,
        last_trace,
        data_filepath: filepath.to_path_buf(),
        medium_velocity,
    })
}

pub fn load_dzt(filepath: &Path, height: usize) -> Result<Array2<f32>, Box<dyn Error>> {
    let bytes = std::fs::read(filepath)?;
    let (data, nsamp, bits, _sps, _spm, _position, _range, nchan, _antenna) =
        read_gssi_header(&bytes)?;
    let data_offset = gssi_data_offset(&bytes, bits, data, nchan);
    if data_offset >= bytes.len() {
        return Err(format!("File too short: no data found in {:?}", filepath).into());
    }

    let bytes_per_sample = (bits as usize / 8).max(1);
    let samples_per_trace = height.max(nsamp as usize);
    let bytes_per_trace = samples_per_trace * bytes_per_sample;
    let payload = &bytes[data_offset..];
    if payload.len() < bytes_per_trace {
        return Err(format!("File too short: {:?}", filepath).into());
    }

    let mut data: Vec<f32> = Vec::with_capacity(payload.len() / bytes_per_sample);
    let scale = gssi_bits_to_millivolt(bits);
    match bits {
        8 => {
            for byte in payload.iter() {
                data.push((*byte as i16 - 128) as f32 * scale);
            }
        }
        16 => {
            for chunk in payload.as_chunks::<2>().0 {
                let value = u16::from_le_bytes(*chunk) as i32 - 32768;
                data.push(value as f32 * scale);
            }
        }
        32 => {
            for chunk in payload.as_chunks::<4>().0 {
                let value = i32::from_le_bytes(*chunk);
                data.push(value as f32 * scale);
            }
        }
        other => return Err(format!("Unsupported GSSI sample width: {other} bits").into()),
    }

    let width = data.len() / height;
    Ok(Array2::from_shape_vec((width, height), data)?.reversed_axes())
}

pub fn load_gssi_dzg(
    filepath: &Path,
    projected_crs: Option<&String>,
) -> Result<gpr::GPRLocation, Box<dyn Error>> {
    let content = std::fs::read_to_string(filepath)?;
    let mut date_str: Option<String> = None;
    let mut current_scan: Option<u32> = None;
    let mut coords = Vec::<crate::coords::Coord>::new();
    let mut points: Vec<gpr::CorPoint> = Vec::new();

    for line in content.lines() {
        let parts: Vec<&str> = line.split(',').collect();
        match parts.first().copied() {
            Some("$GSSIS") => {
                current_scan = parts.get(1).and_then(|s| s.parse::<u32>().ok());
            }
            Some("$GPRMC") => {
                if let Some(date) = parts.get(9) {
                    date_str = Some(gssi_date_to_iso(date)?);
                }
            }
            Some("$GPGGA") => {
                let Some(scan) = current_scan else {
                    continue;
                };
                let Some(date) = &date_str else {
                    continue;
                };
                let (datetime, coord, altitude) = super::read_gga(line, date)?;
                coords.push(coord);
                points.push(gpr::CorPoint {
                    trace_n: scan,
                    time_seconds: datetime,
                    easting: 0.,
                    northing: 0.,
                    altitude,
                });
                current_scan = None;
            }
            _ => {}
        }
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
    use super::{gssi_antenna_mhz, load_dzt, load_gssi_dzg, load_gssi_dzt};

    fn make_gssi_dzt(bytes_per_sample: usize) -> Vec<u8> {
        let samples = 4usize;
        let traces = 2usize;
        let data_offset = 128 * 1024;
        let mut bytes = vec![0u8; data_offset + samples * traces * bytes_per_sample];
        bytes[2..4].copy_from_slice(&128u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&(samples as u16).to_le_bytes());
        bytes[6..8].copy_from_slice(&((bytes_per_sample * 8) as u16).to_le_bytes());
        bytes[10..14].copy_from_slice(&12f32.to_le_bytes());
        bytes[14..18].copy_from_slice(&4f32.to_le_bytes());
        bytes[22..26].copy_from_slice(&(-60f32).to_le_bytes());
        bytes[26..30].copy_from_slice(&600f32.to_le_bytes());
        bytes[52..54].copy_from_slice(&1u16.to_le_bytes());
        let ant = b"5106";
        bytes[98..98 + ant.len()].copy_from_slice(ant);

        let mut offset = data_offset;
        for trace in 0..traces {
            for sample in 0..samples {
                let value = (trace * 10 + sample + 1) as i32;
                bytes[offset..offset + bytes_per_sample]
                    .copy_from_slice(&value.to_le_bytes()[..bytes_per_sample]);
                offset += bytes_per_sample;
            }
        }

        bytes
    }

    #[test]
    fn test_gssi_antenna_map() {
        assert_eq!(gssi_antenna_mhz("5106"), Some(200.));
        assert_eq!(gssi_antenna_mhz("3101A"), Some(900.));
        assert_eq!(gssi_antenna_mhz("50300"), Some(300.));
    }

    #[test]
    fn test_load_gssi_dzt() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dzt_path = temp_dir.path().join("track.DZT");
        std::fs::write(&dzt_path, make_gssi_dzt(4)).unwrap();

        let meta = load_gssi_dzt(&dzt_path, 0.1, Some(200.), Some(1.5)).unwrap();
        assert_eq!(meta.samples, 4);
        assert_eq!(meta.last_trace, 2);
        assert_eq!(meta.time_interval, 1.0 / 12.0);
        assert_eq!(meta.antenna_mhz, 200.);
        assert_eq!(meta.antenna, "5106");
        assert_eq!(meta.antenna_separation, 1.5);

        let data = load_dzt(&dzt_path, 4).unwrap();
        assert_eq!(data.shape(), &[4, 2]);
        assert!(data[[0, 0]] > 0.0);
        assert!(data[[0, 1]] > 0.0);
        assert!(data[[0, 0]] < 1.0);
        assert!(data[[0, 1]] < 1.0);
    }

    #[test]
    // EPSG:4326 is not UTM, so this runs `projinfo` and must not overlap the
    // `serial` tests that unset `PATH` (see `coords::tests::test_projinfo_to_wkt`).
    #[serial_test::parallel]
    fn test_load_gssi_dzg() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dzg_path = temp_dir.path().join("track.DZG");
        let dzg_text = [
            "$GSSIS,0,-1",
            "$GPRMC,140541,A,7901.4763,N,01332.0122,E,0.0,357.5,140424,8.8,E,A*16",
            "$GPGGA,140541,7901.4763,N,01332.0122,E,1,12,0.7,753.0,M,35.3,M,,*4C",
            "$GSSIS,3,-1",
            "$GPRMC,140543,A,7901.4764,N,01332.0123,E,0.0,357.5,140424,8.8,E,A*15",
            "$GPGGA,140543,7901.4764,N,01332.0123,E,1,12,0.7,752.9,M,35.3,M,,*4F",
        ]
        .join("\n");
        std::fs::write(&dzg_path, dzg_text).unwrap();

        let locations = load_gssi_dzg(&dzg_path, Some(&"EPSG:4326".to_string())).unwrap();
        assert_eq!(locations.cor_points.len(), 2);
        assert_eq!(locations.cor_points[0].trace_n, 0);
        assert_eq!(locations.cor_points[1].trace_n, 3);
        assert!((locations.cor_points[0].altitude - 753.0).abs() < 1e-6);
    }
}
