/// Functions to handle input and output (I/O) of GPR data files.
use ndarray::Array2;
use rayon::prelude::*;
use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::{gpr, tools};

/// Load and parse a Malå metadata file (.rad)
///
/// # Arguments
/// - `filepath`: The filepath of the input metadata file
/// - `medium_velocity`: The velocity of the portrayed medium to assign the GPR data
/// - `override_antenna_mhz`: Optional antenna frequency override (will not read from metadata).
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
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let bytes = std::fs::read(Path::new(filepath))?; // read as raw bytes
    let content = String::from_utf8_lossy(&bytes); // &str with invalid bytes replaced

    // Collect all rows into a hashmap, assuming a "KEY:VALUE" structure.
    let data: HashMap<&str, &str> = content.lines().filter_map(|s| s.split_once(':')).collect();

    let rd3_filepath = filepath.with_extension("rd3");
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
        antenna_separation: data
            .get("ANTENNA SEPARATION")
            .ok_or("No 'ANTENNA SEPARATION' key in metadata")?
            .trim()
            .parse()?,
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
            time_str = "0".to_string() + &time_str;
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

    let projected_crs = match projected_crs {
        Some(s) => s.to_string(),
        None => crate::coords::UtmCrs::optimal_crs(&coords[0]).to_epsg_str(),
    };
    for (i, coord) in crate::coords::from_wgs84(
        &coords,
        &crate::coords::Crs::from_user_input(&projected_crs)?,
    )?
    .iter()
    .enumerate()
    {
        points[i].easting = coord.x;
        points[i].northing = coord.y;
    }

    if !points.is_empty() {
        Ok(gpr::GPRLocation {
            cor_points: points,
            correction: gpr::LocationCorrection::None,
            crs: projected_crs.to_string(),
        })
    } else {
        Err(format!("Could not parse location data from: {:?}", filepath).into())
    }
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
    for byte_pair in bytes.chunks_exact(2) {
        let value = i16::from_le_bytes([byte_pair[0], byte_pair[1]]);
        data.push(value as f32 * bits_to_millivolt);
    }

    let width: usize = data.len() / height;

    Ok(ndarray::Array2::from_shape_vec((width, height), data)?.reversed_axes())
}

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

    let dt1_filepath = filepath.with_extension("dt1");
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
        antenna_separation: data
            .get("ANTENNA SEPARATION")
            .ok_or("No 'ANTENNA SEPARATION' key in metadata")?
            .trim()
            .parse()?,
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

        let (datetime, coord, altitude) = read_gga(data[4], date_str.unwrap())?;

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

    let projected_crs = match projected_crs {
        Some(s) => s.to_string(),
        None => crate::coords::UtmCrs::optimal_crs(&coords[0]).to_epsg_str(),
    };
    for (i, coord) in crate::coords::from_wgs84(
        &coords,
        &crate::coords::Crs::from_user_input(&projected_crs)?,
    )?
    .iter()
    .enumerate()
    {
        points[i].easting = coord.x;
        points[i].northing = coord.y;
    }

    if !points.is_empty() {
        Ok(gpr::GPRLocation {
            cor_points: points,
            correction: gpr::LocationCorrection::None,
            crs: projected_crs.to_string(),
        })
    } else {
        Err(format!("Could not parse location data from: {:?}", filepath).into())
    }
}

/// Common functionality for writing NetCDF variables
fn write_nc_variable_common<T>(
    v: &mut netcdf::VariableMut,
    name: &str,
    data: &[T],
    unit: Option<&str>,
) -> Result<(), String>
where
    T: netcdf::NcTypeDescriptor,
{
    v.put_values(data, ..)
        .map_err(|e| format!("NetCDF export error when adding variable '{name}' data: {e}"))?;

    if let Some(unit) = unit {
        v.put_attribute("unit", unit)
            .map_err(|e| format!("NetCDF export error when setting variable '{name}' unit: {e}"))?;
    }

    Ok(())
}

/// Add a variable without compression/chunking
fn add_nc_variable<T>(
    file: &mut netcdf::FileMut,
    name: &str,
    dims: &[&str],
    data: &[T],
    unit: Option<&str>,
) -> Result<(), String>
where
    T: netcdf::NcTypeDescriptor,
{
    let mut v = file
        .add_variable::<T>(name, dims)
        .map_err(|e| format!("NetCDF export error when adding variable '{name}': {e}"))?;

    write_nc_variable_common(&mut v, name, data, unit)
}

/// Add a 2D variable with compression/chunking
fn add_nc_variable_compressed_2d<T>(
    file: &mut netcdf::FileMut,
    name: &str,
    dims: &[&str],
    data: &[T],
    shape: (usize, usize), // (ny, nx) in the same order as `dims`
    unit: Option<&str>,
    extra_attrs: &[(&str, &str)], // e.g. [("coordinates", "distance twtt")]
) -> Result<(), String>
where
    T: netcdf::NcTypeDescriptor,
{
    let (ny, nx) = shape;

    let mut v = file
        .add_variable::<T>(name, dims)
        .map_err(|e| format!("NetCDF export error when adding variable '{name}': {e}"))?;

    v.set_compression(5, true)
        .map_err(|e| format!("NetCDF export error when setting '{name}' compression: {e}"))?;

    for chunking in [1024_usize, 512, 256, 128, 64, 32, 16, 8] {
        if ny < chunking || nx < chunking {
            continue;
        }
        v.set_chunking(&[chunking, chunking])
            .map_err(|e| format!("NetCDF export error when chunking '{name}': {e}"))?;
        break;
    }

    write_nc_variable_common(&mut v, name, data, unit)?;

    for (attr_name, attr_val) in extra_attrs {
        v.put_attribute(attr_name, *attr_val).map_err(|e| {
            format!(
                "NetCDF export error when setting variable '{name}' attribute '{attr_name}': {e}"
            )
        })?;
    }

    Ok(())
}

/// Add an attribute to a NetCDF file
fn add_nc_attribute<T>(file: &mut netcdf::FileMut, name: &str, data: T) -> Result<(), String>
where
    T: Into<netcdf::AttributeValue>,
{
    file.add_attribute(name, data)
        .map_err(|e| format!("NetCDF export error when adding '{name}' attribute: {e}"))?;
    Ok(())
}

/// Export a GPR profile and its metadata to a NetCDF (".nc") file.
///
/// It will overwrite any file that already exists with the same filename.
///
/// # Arguments
/// - `gpr`: The GPR object to export
/// - `nc_filepath`: The filepath of the output NetCDF file
///
/// # Errors
/// - If the file already exists and cannot be removed.
/// - If a dimension, attribute or variable could not be created in the NetCDF file
/// - If data could not be written to the file
pub fn export_netcdf(
    ds: &crate::export::ExportDataset<'_>,
    nc_filepath: &Path,
) -> Result<(), String> {
    // Remove existing file (same reason as before)
    if nc_filepath.is_file() {
        std::fs::remove_file(nc_filepath).map_err(|e| {
            format!("NetCDF export error when removing old file with same name: {e}")
        })?;
    }

    // Create new file
    let mut file = netcdf::create(nc_filepath)
        .map_err(|e| format!("NetCDF export error when creating NetCDF file: {e}"))?;

    // ---- Dimensions ----
    for (name, len) in &ds.dims {
        file.add_dimension(name, *len)
            .map_err(|e| format!("NetCDF export error when adding dimension {name}: {e}"))?;
    }

    // ---- Global attributes from dataset ----
    for (k, v) in &ds.attrs {
        match v {
            crate::export::ExportAttr::String(s) => add_nc_attribute(&mut file, k, s.as_str())?,
            crate::export::ExportAttr::Strings(vs) => add_nc_attribute(&mut file, k, vs.clone())?,
            crate::export::ExportAttr::F64(x) => add_nc_attribute(&mut file, k, *x)?,
            crate::export::ExportAttr::F32(x) => add_nc_attribute(&mut file, k, *x)?,
            crate::export::ExportAttr::I64(x) => add_nc_attribute(&mut file, k, *x)?,
            crate::export::ExportAttr::U8(x) => add_nc_attribute(&mut file, k, *x)?,
        }
    }

    // ---- Coordinates (1D) ----
    for (name, var) in &ds.coords {
        match &var.data {
            crate::export::ExportArray::U32Owned1D(v) => {
                add_nc_variable::<u32>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    v,
                    None,
                )?;
            }
            crate::export::ExportArray::F32Owned1D(v) => {
                // collect unit attr if present
                let unit = var.attrs.get("unit").map(|s| s.as_str());
                add_nc_variable::<f32>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    v,
                    unit,
                )?;
            }
            crate::export::ExportArray::F64Owned1D(v) => {
                let unit = var.attrs.get("unit").map(|s| s.as_str());
                add_nc_variable::<f64>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    v,
                    unit,
                )?;
            }
            crate::export::ExportArray::F32Borrowed2D(_) => {
                // coords are expected to be 1D; ignore
                continue;
            }
        }
    }

    // ---- Data variables ----
    for (name, var) in &ds.data_vars {
        match &var.data {
            crate::export::ExportArray::F32Borrowed2D(arr2d) => {
                // Flatten and write compressed/chunked 2D
                let ny = ds.dims[var.dims[0].as_str()];
                let nx = ds.dims[var.dims[1].as_str()];
                let flat: Vec<f32> = arr2d.iter().copied().collect();

                // unit & extra attributes (coordinates)
                let unit = var.attrs.get("unit").map(|s| s.as_str());
                let mut extras: Vec<(&str, &str)> = Vec::new();
                if let Some(coord_str) = var.attrs.get("coordinates") {
                    extras.push(("coordinates", coord_str.as_str()));
                }

                add_nc_variable_compressed_2d::<f32>(
                    &mut file,
                    name,
                    &var.dims.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    &flat,
                    (ny, nx),
                    unit,
                    &extras,
                )?;
            }
            // data variables are expected to be 2D here; ignore other shapes
            _ => continue,
        }
    }

    Ok(())
}

/// Render an image of the processed GPR data.
///
/// # Arguments
/// - `gpr`: The GPR data to render
/// - `filepath`: The output filepath of the image
///
/// # Errors
/// - The file could not be written.
/// - The extension is not understood.
pub fn render_jpg(gpr: &gpr::GPR, filepath: &Path) -> Result<(), Box<dyn Error>> {
    for (dim, value) in [("wide", gpr.width()), ("tall", gpr.height())] {
        if value >= 65535 {
            return Err(
                format!("Radargram too {dim} ({value}, max 65535) to generate a JPG",).into(),
            );
        }
    }
    let data_to_render = match &gpr.topo_data {
        Some(d) => d,
        None => &gpr.data,
    };

    let data = data_to_render.iter().collect::<Vec<&f32>>();

    // Get quick and dirty quantiles by only looking at a 10th of the data
    let q = tools::quantiles(&data, &[0.01, 0.99], Some(10));
    let mut minval = q[0];
    let maxval = q[1];

    // If unphase has been run, there are no (valid) negative numbers, so it should instead start at 0
    let unphase_run = gpr.log.iter().any(|s| s.contains("unphase"));
    if unphase_run {
        minval = &0.;
    };

    //let logit99 = (0.99_f32 / (1.0_f32 - 0.99_f32)).log(std::f32::consts::E);

    // Render the pixels into a grayscale image
    let pixels: Vec<u8> = data
        .into_par_iter()
        .map(|f| {
            (255.0 * {
                let mut val_norm = ((f - minval) / (maxval - minval)).clamp(0.0, 1.0);
                if unphase_run {
                    val_norm = 0.5 * val_norm + 0.5;
                };

                //0.5 + (val_norm / (1.0_f32 - val_norm)).log(std::f32::consts::E) / logit99
                val_norm
            }) as u8
        })
        .collect();

    image::save_buffer(
        filepath,
        &pixels,
        data_to_render.shape()[1] as u32,
        data_to_render.shape()[0] as u32,
        image::ColorType::L8,
    )?;

    Ok(())
}

/// Export a "track" file.
///
/// It has its own associated function because the logic may happen in two different places in the
/// main() function.
///
/// # Arguments
/// - `gpr_locations`: The GPRLocation object to export
/// - `potential_track_path`: The output path of the track file or a directory (if provided)
/// - `output_filepath`: The output filepath to derive a track filepath from in case `potential_track_path` was not provided.
/// - `verbose`: Print progress?
///
/// # Returns
/// The exit code of the function
pub fn export_locations(
    gpr_locations: &gpr::GPRLocation,
    potential_track_path: Option<&PathBuf>,
    output_filepath: &Path,
    verbose: bool,
) -> Result<(), Box<dyn Error>> {
    // Determine the output filepath. If one was given, use that. If none was given, use the
    // parent and file stem + "_track.csv" of the output filepath. If a directory was given,
    // use the directory + the file stem of the output filepath + "_track.csv".
    let track_path: PathBuf = match potential_track_path {
        // Here is in case a filepath or directory was given
        Some(fp) => match fp.is_dir() {
            // In case the filepath points to a directory
            true => fp
                .join(
                    output_filepath
                        .file_stem()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string()
                        + "_track",
                )
                .with_extension("csv"),
            // In case it is not a directory (and thereby assumed to be a normal filepath)
            false => fp.clone(),
        },
        // Here is if no filepath was given
        None => output_filepath
            .with_file_name(
                output_filepath
                    .file_stem()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
                    + "_track",
            )
            .with_extension("csv"),
    };
    if verbose {
        println!("Exporting track to {:?}", track_path);
    };

    Ok(gpr_locations.to_csv(&track_path)?)
}

#[cfg(test)]
mod tests {

    use std::{path::PathBuf, str::FromStr};

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

        let gpr_meta = load_rad(&rad_path, 0.1, None).unwrap();

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
        let gpr_meta = load_rad(&rad_path, 0.1, Some(200.)).unwrap();
        assert_eq!(gpr_meta.antenna_mhz, 200.);
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
        let gpr_meta_fail = load_rad(&rad_path, 0.1, None);
        assert!(gpr_meta_fail.is_err());

        let err_msg = gpr_meta_fail.unwrap_err().to_string();
        assert!(
            err_msg.contains("frequency from the antenna field"),
            "Got:     {err_msg:?}\nExpected 'Could not read frequency from the antenna field'",
        );
        assert!(load_rad(&rad_path, 0.1, None).is_err());

        let gpr_meta = load_rad(&rad_path, 0.1, Some(100.)).unwrap();
        assert_eq!(gpr_meta.antenna_mhz, 100.);
    }

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

        let gpr_meta = crate::io::load_pe_hd(&rad_path, 0.1, None).unwrap();

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
        let gpr_meta = crate::io::load_pe_hd(&rad_path, 0.1, Some(300.)).unwrap();
        assert_eq!(gpr_meta.antenna_mhz, 300.);
    }

    #[test]
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

        let locations = crate::io::load_pe_gp2(&gp2_path, Some(&"EPSG:4326".to_string())).unwrap();

        assert_eq!(locations.cor_points.len(), 9);
        assert!(locations.cor_points.first().unwrap().northing > 77.);
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // Added 2026-02-17 because gdal is hard to install in CI
    fn test_export_locations() {
        use super::export_locations;
        let temp_dir = tempfile::tempdir().unwrap();
        let cor_path = temp_dir.path().join("hello.cor");

        std::fs::write(&cor_path, fake_cor_text()).unwrap();

        // Load it and "convert" (or rather don't convert) the CRS to WGS84
        let locations = load_cor(&cor_path, Some(&"EPSG:4326".to_string())).unwrap();

        let out_dir = temp_dir.path().to_path_buf();
        let out_path = out_dir.join("track.csv");

        // The GPR filepath will be used in case no explicit filepath was given
        let dummy_gpr_output_path = out_dir.join("gpr.nc");
        let expected_default_path = out_dir.join("gpr_track.csv");

        for alternative in [
            Some(&out_path), // In case of a target filepath
            Some(&out_dir),  // In case of a target directory
            None,            // In case of a default name beside the GPR file
        ] {
            export_locations(&locations, alternative, &dummy_gpr_output_path, false).unwrap();

            let expected_path = match alternative {
                Some(p) if p == &out_path => &out_path,
                _ => &expected_default_path,
            };
            assert!(expected_path.is_file());

            let content = std::fs::read_to_string(expected_path)
                .unwrap()
                .split("\n")
                .map(|s| s.to_string())
                .collect::<Vec<String>>();

            assert_eq!(content[0], "trace_n,easting,northing,altitude");

            let line0: Vec<&str> = content[1].split(",").collect();

            // The cor file says 1 but ridal is zero-indexed, hence 0
            assert_eq!(line0[0], "0");
            assert_eq!(line0[1], "16");
            assert_eq!(line0[2], "78");
            assert_eq!(line0[3], "100");

            let line1: Vec<&str> = content[2].split(",").collect();
            assert_eq!(line1[2], "-78");

            std::fs::remove_file(expected_path).unwrap();
        }
    }

    #[test]
    // #[ignore] // Added 2026-03-13 because it randomly fails sometimes. Unclear why
    #[test_retry::retry]
    fn test_save_netcdf() {
        let mut gpr = crate::gpr::tests::make_dummy_gpr(100, 10, Some(1.));

        let mut gpr2 = crate::gpr::tests::make_dummy_gpr(100, 10, Some(1.));
        gpr2.metadata.data_filepath = PathBuf::from_str("other_filepath.rd3").unwrap();

        gpr.merge(&gpr2).unwrap();
        gpr.process("subset(0 50)").unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let nc_path = temp_dir.path().join("data.nc");

        gpr.export(&nc_path).unwrap();

        assert!(nc_path.is_file());

        let out = netcdf::open(&nc_path)
            .map_err(|e| format!("Error reading NetCDF: {e:?}"))
            .unwrap();

        let expected_attrs = vec![
            (
                "processing_steps",
                netcdf::AttributeValue::Strs(vec!["subset(0 50)".to_string()]),
            ),
            (
                "processing_log",
                netcdf::AttributeValue::Str(
                    "merge (duration: 0.00s):\tMerged \"other_filepath.rd3\"\nsubset (duration: 0.00s):\tSubset data from [10, 200] to (0:10, 0:50)"
                        .to_string(),
                ),
            ),
            ("total_distance", netcdf::AttributeValue::Double(49.)),
            (
                "original_filepaths",
                netcdf::AttributeValue::Strs(vec![
                    "filepath.rd3".to_string(),
                    "other_filepath.rd3".to_string(),
                ]),
            ),
        ];

        // Load the data and check that it's identical
        let mut data = ndarray::Array2::<f32>::zeros((gpr.height(), gpr.width()));
        out.variable("data")
            .unwrap()
            .get_into(.., data.view_mut())
            .unwrap();
        assert_eq!((data - gpr.data).mapv(|v| v.abs()).sum(), 0.);

        for (key, expected) in expected_attrs {
            assert_eq!(
                out.attribute(key)
                    .ok_or(format!("Cannot find attribute {key}"))
                    .unwrap()
                    .value()
                    .unwrap(),
                expected
            );
        }
    }
}
