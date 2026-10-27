//! Blue System Integration IceRadar (`.h5` / `.hdf5`).
//!
//! An IceRadar file is HDF5 holding one or more lines, each a sequence of
//! single-trace groups: `/line_L/location_T/datacapture_0/echogram_0`, in
//! volts, with the digitizer settings and the GPS fix of that trace as
//! LabVIEW cluster XML in string attributes. The positions are in the same
//! file, so one file is header, data and positions at once.
//!
//! There is no public specification. What follows was worked out from one
//! file of each known layout, and every inference is marked as such:
//!
//! - **v1** ("software version one", `.h5`): attributes named with spaces
//!   (`Digitizer-MetaData_xml`, `GPS Cluster- MetaData_xml`). The date is
//!   only in each location group's HDF5 comment, which NetCDF cannot read,
//!   as `location_N[delimiter][delimiter]M/D/YYYY h:mm:ss AM` on the PC
//!   clock.
//! - **v2** ("software version two", `.hdf5`): CamelCase names
//!   (`DigitizerMetaData_xml`, `GPSData_xml`), root attributes naming the
//!   acquisition software, and a `CreationTimestamp` on each line group.
//!
//! The GPS fields are a `$GPGGA` sentence split into named fields, without
//! the hemisphere letters. v1 calls the longitude `Long_ W` for data from
//! eight degrees east, so the name is not a hemisphere: positions are read
//! as north and east. The time of day is UTC; the date is not recorded by
//! the GPS at all (see [`IceRadarDate`]).
//!
//! The autosave beside a v2 file (`<name>.hdf5.x1`) is a copy of the first
//! part of it and is not read.

use std::error::Error;
use std::ffi::CString;
use std::path::Path;

use ndarray::Array2;

use crate::gpr;

/// The two known attribute layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    V1,
    V2,
}

/// The attribute and XML field names of a [`Layout`].
struct Names {
    digitizer: &'static str,
    gps: &'static str,
    time: &'static str,
    lat: &'static str,
    lon: &'static str,
    alt: &'static str,
    fix_quality: &'static str,
    fix_valid: &'static str,
    message_ok: &'static str,
    sample_rate: &'static str,
    initial_x: &'static str,
    x_increment: &'static str,
}

impl Layout {
    fn names(self) -> Names {
        match self {
            Layout::V1 => Names {
                digitizer: "Digitizer-MetaData_xml",
                gps: "GPS Cluster- MetaData_xml",
                time: "GPS_timestamp_UTC",
                lat: "Lat_N",
                lon: "Long_ W",
                alt: "Alt_asl_m",
                fix_quality: "Fix_Quality",
                fix_valid: "GPS Fix valid",
                message_ok: "GPS Message ok",
                // With the leading space, as the file has it.
                sample_rate: " Sample Rate",
                initial_x: "relativeInitialX",
                x_increment: "xIncrement",
            },
            Layout::V2 => Names {
                digitizer: "DigitizerMetaData_xml",
                gps: "GPSData_xml",
                time: "GPSTimestamp_UTC",
                lat: "Lat",
                lon: "Long",
                alt: "Alt_ASL_m",
                fix_quality: "FixQuality",
                fix_valid: "GPSFixValid",
                message_ok: "GPSMessageOk",
                sample_rate: "SampleRate",
                initial_x: "RelativeInitialX",
                x_increment: "XIncrement",
            },
        }
    }
}

/// Where the date of an IceRadar line came from. The GPS records only the
/// time of day, so the date is the PC clock's unless the user gives one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceRadarDate {
    /// `--override-date`.
    Given(chrono::NaiveDate),
    /// The acquisition PC's clock, which is local time and may be wrong by
    /// hours, since nothing sets it from the GPS. A line recorded near
    /// midnight UTC can get the wrong day from it.
    PcClock(chrono::NaiveDate),
}

impl IceRadarDate {
    pub fn date(self) -> chrono::NaiveDate {
        match self {
            IceRadarDate::Given(date) | IceRadarDate::PcClock(date) => date,
        }
    }
}

/// One GPS fix, already validated.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fix {
    /// UTC seconds since midnight.
    seconds_of_day: f64,
    lat: f64,
    lon: f64,
    /// m above the geoid, as GGA gives it.
    altitude: f64,
}

/// What one trace's attributes say.
#[derive(Debug, Clone, PartialEq)]
struct TraceMeta {
    record_length: usize,
    /// Seconds from the trigger to the first sample: negative, the
    /// pretrigger.
    initial_x: f64,
    /// Seconds per sample.
    x_increment: f64,
    /// Hz.
    sample_rate: f64,
    /// How many pulses were stacked. Zero for a trace the system wrote
    /// without capturing anything: its samples are all zero.
    stacking: u32,
    fix: Option<Fix>,
}

/// One line's attributes, and the traces of it that are kept.
#[derive(Debug, Clone)]
struct LineMeta {
    hardware_id: Option<String>,
    /// Indices into the file's `location_N`, of the traces kept: all but the
    /// empty ones.
    kept: Vec<usize>,
    traces: Vec<TraceMeta>,
    date: IceRadarDate,
    /// The first sample of the common grid, in samples from the trigger.
    first_sample: i64,
    samples: usize,
    x_increment: f64,
    sample_rate: f64,
}

/// The `<Val>` that follows `<Name>name</Name>` in a LabVIEW cluster XML.
///
/// The names are matched exactly, including leading spaces, since the two
/// layouts differ in exactly such details.
fn xml_val<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let tag = format!("<Name>{name}</Name>");
    let after = &xml[xml.find(&tag)? + tag.len()..];
    let after = after.trim_start().strip_prefix("<Val>")?;
    Some(&after[..after.find("</Val>")?])
}

fn xml_f64(xml: &str, name: &str, what: &str) -> Result<f64, Box<dyn Error>> {
    xml_val(xml, name)
        .ok_or_else(|| format!("{what} has no {name:?}"))?
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("{what}: {name:?} is not a number: {e}").into())
}

/// `ddmm.mmmm` (or `dddmm.mmmm`) to decimal degrees, refusing anything that
/// is not that shape: v1 files hold shifted fields in place of positions
/// when the logger mis-split a sentence.
fn ddmm_to_degrees(s: &str, max_degrees: f64) -> Option<f64> {
    let (whole, frac) = s.split_once('.')?;
    if !(4..=5).contains(&whole.len())
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || frac.is_empty()
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let value = s.parse::<f64>().ok()?;
    let degrees = (value / 100.).floor();
    let minutes = value - degrees * 100.;
    (minutes < 60. && degrees <= max_degrees).then_some(degrees + minutes / 60.)
}

/// `hhmmss[.s]` to seconds since midnight.
fn hhmmss_to_seconds(s: &str) -> Option<f64> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.len() != 6
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let h = whole[0..2].parse::<f64>().ok()?;
    let m = whole[2..4].parse::<f64>().ok()?;
    let sec = s[4..].parse::<f64>().ok()?;
    (h < 24. && m < 60. && sec < 61.).then_some(h * 3600. + m * 60. + sec)
}

/// A trace's GPS fix, or `None` when it cannot be trusted.
///
/// Both flags must be set, and every field must have the shape GGA gives
/// it. The flags alone are not enough: a v1 file has traces flagged as a
/// good message whose fields are shifted, with an altitude where the
/// satellite count belongs and a checksum in the fix quality.
fn parse_fix(xml: &str, names: &Names) -> Option<Fix> {
    if xml_val(xml, names.fix_valid)? != "1" || xml_val(xml, names.message_ok)? != "1" {
        return None;
    }
    // GGA fix quality: 1 GPS, 2 DGPS, 4 RTK fixed, 5 RTK float. 0 is no fix,
    // and anything else is a shifted field.
    if !matches!(xml_val(xml, names.fix_quality)?, "1" | "2" | "4" | "5") {
        return None;
    }
    Some(Fix {
        seconds_of_day: hhmmss_to_seconds(xml_val(xml, names.time)?)?,
        lat: ddmm_to_degrees(xml_val(xml, names.lat)?, 90.)?,
        lon: ddmm_to_degrees(xml_val(xml, names.lon)?, 180.)?,
        altitude: xml_val(xml, names.alt)?.parse::<f64>().ok()?,
    })
}

/// An open HDF5 file, read through `hdf5-sys` rather than the `netcdf`
/// crate: netcdf-c reads the metadata of every group when it opens a file,
/// which for an IceRadar file of 9000 traces is half a second per open,
/// where HDF5 reads only what is asked for. It also reaches the group
/// comments that hold a v1 file's date, which netcdf-c cannot.
///
/// Every call holds the lock the `netcdf` crate takes for its own, since
/// both share one HDF5 library, which is not thread-safe. It is reentrant,
/// so a method may call another.
struct H5File {
    id: hdf5_sys::h5i::hid_t,
    path: std::path::PathBuf,
}

impl Drop for H5File {
    fn drop(&mut self) {
        let _guard = netcdf_sys::libnetcdf_lock.lock();
        // SAFETY: an id `open` returned, closed once.
        unsafe {
            hdf5_sys::h5f::H5Fclose(self.id);
        }
    }
}

impl H5File {
    fn open(path: &Path) -> Result<H5File, Box<dyn Error>> {
        let c_path = CString::new(path.to_string_lossy().as_bytes())?;
        let _guard = netcdf_sys::libnetcdf_lock.lock();
        // SAFETY: a NUL-terminated path that outlives the call.
        let id = unsafe {
            hdf5_sys::h5::H5open();
            hdf5_sys::h5f::H5Fopen(
                c_path.as_ptr(),
                hdf5_sys::h5f::H5F_ACC_RDONLY,
                hdf5_sys::h5p::H5P_DEFAULT,
            )
        };
        if id < 0 {
            return Err(format!("Could not open {} as IceRadar HDF5", path.display()).into());
        }
        Ok(H5File {
            id,
            path: path.to_path_buf(),
        })
    }

    /// Whether every link along `object` (`/line_0/location_3`) exists,
    /// checked a level at a time so a missing one is not an HDF5 error,
    /// which the library would print.
    fn exists(&self, object: &str) -> bool {
        let _guard = netcdf_sys::libnetcdf_lock.lock();
        let mut prefix = String::new();
        for part in object.split('/').filter(|p| !p.is_empty()) {
            prefix.push('/');
            prefix.push_str(part);
            let Ok(c) = CString::new(prefix.as_str()) else {
                return false;
            };
            // SAFETY: a NUL-terminated name that outlives the call.
            if unsafe { hdf5_sys::h5l::H5Lexists(self.id, c.as_ptr(), hdf5_sys::h5p::H5P_DEFAULT) }
                <= 0
            {
                return false;
            }
        }
        true
    }

    fn missing(&self, object: &str) -> Box<dyn Error> {
        format!("{} has no {object}", self.path.display()).into()
    }

    /// The names of the links in the group `group`.
    fn children(&self, group: &str) -> Result<Vec<String>, Box<dyn Error>> {
        if !self.exists(group) {
            return Err(self.missing(group));
        }
        let c_group = CString::new(group)?;
        let _guard = netcdf_sys::libnetcdf_lock.lock();
        // SAFETY: plain out-parameters, NUL-terminated names, and buffers of
        // the size passed.
        unsafe {
            let mut info = std::mem::zeroed::<hdf5_sys::h5g::H5G_info_t>();
            if hdf5_sys::h5g::H5Gget_info_by_name(
                self.id,
                c_group.as_ptr(),
                &mut info,
                hdf5_sys::h5p::H5P_DEFAULT,
            ) < 0
            {
                return Err(self.missing(group));
            }
            let mut names = Vec::with_capacity(info.nlinks as usize);
            for index in 0..info.nlinks {
                let mut buf = vec![0u8; 64];
                let len = hdf5_sys::h5l::H5Lget_name_by_idx(
                    self.id,
                    c_group.as_ptr(),
                    hdf5_sys::h5::H5_index_t::H5_INDEX_NAME,
                    hdf5_sys::h5::H5_iter_order_t::H5_ITER_INC,
                    index,
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    hdf5_sys::h5p::H5P_DEFAULT,
                );
                if len < 0 {
                    return Err(format!("Could not list {group} in {}", self.path.display()).into());
                }
                let len = len as usize;
                if len >= buf.len() {
                    buf = vec![0u8; len + 1];
                    hdf5_sys::h5l::H5Lget_name_by_idx(
                        self.id,
                        c_group.as_ptr(),
                        hdf5_sys::h5::H5_index_t::H5_INDEX_NAME,
                        hdf5_sys::h5::H5_iter_order_t::H5_ITER_INC,
                        index,
                        buf.as_mut_ptr().cast(),
                        buf.len(),
                        hdf5_sys::h5p::H5P_DEFAULT,
                    );
                }
                names.push(String::from_utf8_lossy(&buf[..len]).into_owned());
            }
            Ok(names)
        }
    }

    /// The string attribute `name` of `object`, fixed-length or variable,
    /// or `None` when there is none or it is not a string.
    fn string_attr(&self, object: &str, name: &str) -> Option<String> {
        let c_object = CString::new(object).ok()?;
        let c_name = CString::new(name).ok()?;
        let _guard = netcdf_sys::libnetcdf_lock.lock();
        // SAFETY: NUL-terminated names; every id opened is closed; a
        // variable-length string is copied and then freed by HDF5, and a
        // fixed-length one read into a buffer of its own size.
        unsafe {
            use hdf5_sys::{h5a, h5p::H5P_DEFAULT, h5t};
            if h5a::H5Aexists_by_name(self.id, c_object.as_ptr(), c_name.as_ptr(), H5P_DEFAULT) <= 0
            {
                return None;
            }
            let attr = h5a::H5Aopen_by_name(
                self.id,
                c_object.as_ptr(),
                c_name.as_ptr(),
                H5P_DEFAULT,
                H5P_DEFAULT,
            );
            if attr < 0 {
                return None;
            }
            let file_type = h5a::H5Aget_type(attr);
            let memory_type = h5t::H5Tcopy(*h5t::H5T_C_S1);
            let value = if h5t::H5Tis_variable_str(file_type) > 0 {
                h5t::H5Tset_size(memory_type, h5t::H5T_VARIABLE);
                let mut pointer: *mut std::ffi::c_char = std::ptr::null_mut();
                let read = h5a::H5Aread(
                    attr,
                    memory_type,
                    (&mut pointer as *mut *mut std::ffi::c_char).cast(),
                );
                if read < 0 || pointer.is_null() {
                    None
                } else {
                    let value = std::ffi::CStr::from_ptr(pointer)
                        .to_string_lossy()
                        .into_owned();
                    hdf5_sys::h5::H5free_memory(pointer.cast());
                    Some(value)
                }
            } else if h5t::H5Tget_class(file_type) == h5t::H5T_class_t::H5T_STRING {
                let size = h5t::H5Tget_size(file_type);
                h5t::H5Tset_size(memory_type, size);
                let mut buf = vec![0u8; size];
                (h5a::H5Aread(attr, memory_type, buf.as_mut_ptr().cast()) >= 0).then(|| {
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    String::from_utf8_lossy(&buf[..end]).into_owned()
                })
            } else {
                None
            };
            h5t::H5Tclose(memory_type);
            h5t::H5Tclose(file_type);
            h5a::H5Aclose(attr);
            value
        }
    }

    /// The comment of `object`, empty when there is none.
    fn comment(&self, object: &str) -> Result<String, Box<dyn Error>> {
        if !self.exists(object) {
            return Err(self.missing(object));
        }
        let c_object = CString::new(object)?;
        let _guard = netcdf_sys::libnetcdf_lock.lock();
        let mut buf = vec![0u8; 512];
        // SAFETY: a NUL-terminated name and a buffer of the size passed.
        let len = unsafe {
            hdf5_sys::h5o::H5Oget_comment_by_name(
                self.id,
                c_object.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                hdf5_sys::h5p::H5P_DEFAULT,
            )
        };
        if len < 0 {
            return Err(format!(
                "Could not read the comment of {object} in {}",
                self.path.display()
            )
            .into());
        }
        let len = (len as usize).min(buf.len() - 1);
        Ok(String::from_utf8_lossy(&buf[..len]).into_owned())
    }

    /// The length of the one-dimensional dataset `dataset`, and, when
    /// `read`, its values as doubles, whatever their byte order on disk.
    fn dataset(&self, dataset: &str, read: bool) -> Result<(usize, Vec<f64>), Box<dyn Error>> {
        if !self.exists(dataset) {
            return Err(self.missing(dataset));
        }
        let c_dataset = CString::new(dataset)?;
        let _guard = netcdf_sys::libnetcdf_lock.lock();
        // SAFETY: a NUL-terminated name, every id opened closed, and a
        // buffer of exactly the dataset's number of points.
        unsafe {
            use hdf5_sys::{h5d, h5p::H5P_DEFAULT, h5s};
            let id = h5d::H5Dopen2(self.id, c_dataset.as_ptr(), H5P_DEFAULT);
            if id < 0 {
                return Err(self.missing(dataset));
            }
            let space = h5d::H5Dget_space(id);
            let n = h5s::H5Sget_simple_extent_npoints(space).max(0) as usize;
            h5s::H5Sclose(space);
            let mut values = Vec::new();
            let mut status = 0;
            if read {
                values = vec![0f64; n];
                status = h5d::H5Dread(
                    id,
                    *hdf5_sys::h5t::H5T_NATIVE_DOUBLE,
                    h5s::H5S_ALL,
                    h5s::H5S_ALL,
                    H5P_DEFAULT,
                    values.as_mut_ptr().cast(),
                );
            }
            h5d::H5Dclose(id);
            if status < 0 {
                return Err(format!("Could not read {dataset} in {}", self.path.display()).into());
            }
            Ok((n, values))
        }
    }
}

/// The PC clock's date in an IceRadar timestamp, `M/D/YYYY ...`.
///
/// Month first in both layouts: v1's dates have days above 12 in the
/// second field. v2's only example is the reset value `1/1/2000`, which
/// cannot tell, so that is an assumption.
fn pc_date(timestamp: &str) -> Option<chrono::NaiveDate> {
    let date = timestamp.split_whitespace().next()?;
    let mut parts = date.split('/');
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    let year = parts.next()?.parse().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, day)
}

fn line_index(name: &str) -> Option<u32> {
    name.strip_prefix("line_")?.parse().ok()
}

fn echogram(line: &str, index: usize) -> String {
    format!("/{line}/location_{index}/datacapture_0/echogram_0")
}

/// The lines of an IceRadar file: their numbers (`line_N`), ascending, and
/// how many traces each holds.
pub fn iceradar_lines(path: &Path) -> Result<Vec<(u32, usize)>, Box<dyn Error>> {
    let file = H5File::open(path)?;
    let mut lines = Vec::new();
    for name in file.children("/")? {
        if let Some(n) = line_index(&name) {
            lines.push((n, file.children(&format!("/{name}"))?.len()));
        }
    }
    lines.sort();
    if lines.is_empty() {
        return Err(format!(
            "{} holds no IceRadar lines (no line_N groups)",
            path.display()
        )
        .into());
    }
    Ok(lines)
}

fn read_line_meta(
    path: &Path,
    line: u32,
    date: Option<chrono::NaiveDate>,
) -> Result<LineMeta, Box<dyn Error>> {
    let file = H5File::open(path)?;
    let line_name = format!("line_{line}");
    let n = file.children(&format!("/{line_name}"))?.len();
    if n == 0 {
        return Err(format!("{line_name} of {} holds no traces", path.display()).into());
    }

    let first = echogram(&line_name, 0);
    let layout = if file.string_attr(&first, "DigitizerMetaData_xml").is_some() {
        Layout::V2
    } else if file.string_attr(&first, "Digitizer-MetaData_xml").is_some() {
        Layout::V1
    } else {
        return Err(format!(
            "{} is HDF5 but not an IceRadar layout Ridal knows: its traces have neither \
             DigitizerMetaData_xml nor Digitizer-MetaData_xml",
            path.display()
        )
        .into());
    };
    let names = layout.names();

    let mut traces = Vec::with_capacity(n);
    for index in 0..n {
        let what = format!("{line_name}/location_{index}");
        let echogram = echogram(&line_name, index);
        let digitizer = file
            .string_attr(&echogram, names.digitizer)
            .ok_or_else(|| format!("{what} has no {}", names.digitizer))?;
        let gps = file.string_attr(&echogram, names.gps).unwrap_or_default();
        traces.push(TraceMeta {
            record_length: file.dataset(&echogram, false)?.0,
            initial_x: xml_f64(&digitizer, names.initial_x, &what)?,
            x_increment: xml_f64(&digitizer, names.x_increment, &what)?,
            sample_rate: xml_f64(&digitizer, names.sample_rate, &what)?,
            stacking: xml_f64(&digitizer, "Stacking", &what)? as u32,
            fix: parse_fix(&gps, &names),
        });
    }

    let kept = (0..n)
        .filter(|&i| traces[i].stacking > 0)
        .collect::<Vec<_>>();
    if kept.is_empty() {
        return Err(format!(
            "Every trace of {line_name} in {} is empty (stacking 0)",
            path.display()
        )
        .into());
    }

    // One sample interval per line: traces of different intervals could
    // not share a time axis.
    let x_increment = traces[kept[0]].x_increment;
    let sample_rate = traces[kept[0]].sample_rate;
    if let Some(&other) = kept.iter().find(|&&i| traces[i].sample_rate != sample_rate) {
        return Err(format!(
            "The sample rate changes within {line_name} of {}: {} Hz at location_{} and {} Hz at location_{other}",
            path.display(),
            sample_rate,
            kept[0],
            traces[other].sample_rate
        )
        .into());
    }

    // The record length (and with it the pretrigger, a fixed share of it)
    // can change within a line. Traces are aligned on the trigger, on a grid
    // from the earliest first sample to the latest last one, which is the
    // longest record's.
    let start = |t: &TraceMeta| (t.initial_x / x_increment).round() as i64;
    let first_sample = kept.iter().map(|&i| start(&traces[i])).min().unwrap();
    let last_sample = kept
        .iter()
        .map(|&i| start(&traces[i]) + traces[i].record_length as i64)
        .max()
        .unwrap();

    let date = match date {
        Some(date) => IceRadarDate::Given(date),
        None => {
            let timestamp = match layout {
                Layout::V1 => file
                    .comment(&format!("/{line_name}/location_0"))?
                    .rsplit("[delimiter]")
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                Layout::V2 => file
                    .string_attr(&format!("/{line_name}"), "CreationTimestamp")
                    .unwrap_or_default(),
            };
            let date = pc_date(&timestamp).ok_or_else(|| {
                format!(
                    "{line_name} of {} gives no date the PC clock recorded ({timestamp:?}). \
                     Give the UTC date with --override-date YYYY-MM-DD (override_date= in Python)",
                    path.display()
                )
            })?;
            // 2000-01-01 is what a PC with a flat clock battery starts at,
            // which no IceRadar line was recorded on.
            if date == chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap() {
                return Err(format!(
                    "{line_name} of {} was recorded with the PC clock at {timestamp:?}, its reset \
                     value, so the file does not know its date, and the GPS records only the \
                     time of day. Give the UTC date with --override-date YYYY-MM-DD (override_date= in Python)",
                    path.display()
                )
                .into());
            }
            IceRadarDate::PcClock(date)
        }
    };

    Ok(LineMeta {
        hardware_id: file.string_attr("/", "HardwareID"),
        kept,
        traces,
        date,
        first_sample,
        samples: (last_sample - first_sample) as usize,
        x_increment,
        sample_rate,
    })
}

impl LineMeta {
    /// The kept traces' fixes as Unix times, with the trace (counted among
    /// the kept ones) each belongs to.
    ///
    /// A fix whose time is not after the previous one's is dropped, as a
    /// repeated or out-of-order sentence. A time of day more than twelve
    /// hours before the previous one is read as the next day.
    fn timed_fixes(&self) -> Vec<(u32, f64, Fix)> {
        let midnight = self
            .date
            .date()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64;
        let mut out: Vec<(u32, f64, Fix)> = Vec::new();
        let mut day = 0.;
        let mut previous_of_day: Option<f64> = None;
        for (trace_n, &i) in self.kept.iter().enumerate() {
            let Some(fix) = self.traces[i].fix else {
                continue;
            };
            if let Some(previous) = previous_of_day {
                if fix.seconds_of_day < previous - 43200. {
                    day += 86400.;
                }
            }
            previous_of_day = Some(fix.seconds_of_day);
            let time = midnight + day + fix.seconds_of_day;
            if out.last().is_some_and(|&(_, last, _)| time <= last) {
                continue;
            }
            out.push((trace_n as u32, time, fix));
        }
        out
    }

    /// Seconds per trace: the median of the fixes' intervals, since the
    /// system triggers on a timer whose period wanders (0.2 to 1.6 s in a
    /// line set to 0.5 s).
    fn time_interval(&self, fixes: &[(u32, f64, Fix)]) -> f32 {
        let mut rates = fixes
            .windows(2)
            .map(|w| (w[1].1 - w[0].1) / (w[1].0 - w[0].0) as f64)
            .collect::<Vec<_>>();
        if rates.is_empty() {
            return 1.;
        }
        rates.sort_by(f64::total_cmp);
        rates[rates.len() / 2] as f32
    }
}

/// The header of one line of an IceRadar file.
///
/// The file records neither the antenna frequency nor the antenna
/// separation. The frequency must be given; the separation is 0 without
/// one, which the caller warns about.
pub fn load_iceradar_meta(
    path: &Path,
    line: u32,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
    date: Option<chrono::NaiveDate>,
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let meta = read_line_meta(path, line, date)?;
    let antenna = format!(
        "BSI IceRadar{}",
        meta.hardware_id
            .as_deref()
            .map(|id| format!(" {id}"))
            .unwrap_or_default()
    );
    let antenna_mhz = override_antenna_mhz.ok_or_else(|| {
        super::UnknownAntennaFrequency::new(
            antenna.as_str(),
            "IceRadar files do not record the antenna frequency. Give it with \
             --override-antenna-mhz (override_antenna_mhz= in Python)"
                .to_string(),
        )
    })?;
    let fixes = meta.timed_fixes();
    Ok(gpr::GPRMeta {
        samples: meta.samples as u32,
        frequency: (meta.sample_rate / 1e6) as f32,
        frequency_steps: 0,
        time_interval: meta.time_interval(&fixes),
        antenna,
        antenna_mhz,
        antenna_separation: override_antenna_separation.unwrap_or(0.),
        time_window: (meta.samples as f64 * meta.x_increment * 1e9) as f32,
        last_trace: meta.kept.len() as u32,
        data_filepath: path.to_path_buf(),
        medium_velocity,
    })
}

/// The positions of one line of an IceRadar file, at the traces with a
/// usable fix. The traces between are interpolated when the radargram is
/// built.
pub fn load_iceradar_positions(
    path: &Path,
    line: u32,
    date: Option<chrono::NaiveDate>,
    projected_crs: Option<&String>,
) -> Result<gpr::GPRLocation, Box<dyn Error>> {
    let meta = read_line_meta(path, line, date)?;
    let fixes = meta.timed_fixes();
    if fixes.is_empty() {
        return Err(format!(
            "line_{line} of {} has no usable GPS fix. Give its positions with --cor (cor= in Python)",
            path.display()
        )
        .into());
    }
    let coords = fixes
        .iter()
        .map(|(_, _, fix)| crate::coords::Coord {
            x: fix.lon,
            y: fix.lat,
        })
        .collect::<Vec<_>>();
    let mut points = fixes
        .iter()
        .map(|&(trace_n, time_seconds, fix)| gpr::CorPoint {
            trace_n,
            time_seconds,
            easting: 0.,
            northing: 0.,
            altitude: fix.altitude,
        })
        .collect::<Vec<_>>();
    let crs = super::project_points(&mut points, &coords, projected_crs)?;
    Ok(gpr::GPRLocation {
        cor_points: points,
        correction: gpr::LocationCorrection::None,
        time_source: gpr::TimeSource::Recorded,
        crs,
    })
}

/// The traces of one line of an IceRadar file, in mV, (samples, traces).
///
/// Empty traces (stacking 0) are left out, as in [`load_iceradar_meta`].
/// Traces with a shorter record than the longest are aligned on the
/// trigger and padded with zeros: the processing steps are not all
/// NaN-safe, and the padding is at most the difference in record length.
pub fn load_iceradar_data(
    path: &Path,
    line: u32,
    date: Option<chrono::NaiveDate>,
) -> Result<Array2<f32>, Box<dyn Error>> {
    let meta = read_line_meta(path, line, date)?;
    let file = H5File::open(path)?;
    let line_name = format!("line_{line}");
    let mut data = Array2::<f32>::zeros((meta.samples, meta.kept.len()));
    for (column, &i) in meta.kept.iter().enumerate() {
        let trace = &meta.traces[i];
        let (_, values) = file.dataset(&echogram(&line_name, i), true)?;
        let offset =
            ((trace.initial_x / meta.x_increment).round() as i64 - meta.first_sample) as usize;
        for (k, value) in values.iter().enumerate() {
            data[[offset + k, column]] = (value * 1000.) as f32;
        }
    }
    Ok(data)
}

/// Whether the file at `path`, with its date as the caller resolved it,
/// took its date from the PC clock: for a caller that tells the user.
pub fn iceradar_date(
    path: &Path,
    line: u32,
    date: Option<chrono::NaiveDate>,
) -> Result<IceRadarDate, Box<dyn Error>> {
    Ok(read_line_meta(path, line, date)?.date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_values_match_names_exactly() {
        let xml = "<DBL>\r\n<Name> Sample Rate</Name>\r\n<Val>250000000.0</Val>\r\n</DBL>\
                   <String>\r\n<Name>Lat</Name>\r\n<Val>6148.62112</Val>\r\n</String>";
        assert_eq!(xml_val(xml, " Sample Rate"), Some("250000000.0"));
        assert_eq!(xml_val(xml, "Sample Rate"), None);
        assert_eq!(xml_val(xml, "Lat"), Some("6148.62112"));
        assert_eq!(xml_val(xml, "Long"), None);
    }

    #[test]
    fn positions_and_times_refuse_shifted_fields() {
        let lat = ddmm_to_degrees("6012.34567", 90.).unwrap();
        assert!((lat - (60. + 12.34567 / 60.)).abs() < 1e-12);
        let lon = ddmm_to_degrees("01023.45678", 180.).unwrap();
        assert!((lon - (10. + 23.45678 / 60.)).abs() < 1e-12);
        // What a mis-split v1 sentence puts in these fields.
        for bad in ["N", "E", "", "1475.3", "M", "*4A"] {
            assert_eq!(ddmm_to_degrees(bad, 90.), None, "{bad}");
        }
        assert_eq!(
            hhmmss_to_seconds("103045.6"),
            Some(10. * 3600. + 30. * 60. + 45.6)
        );
        assert_eq!(hhmmss_to_seconds("M"), None);
        assert_eq!(hhmmss_to_seconds("6012.34600"), None);
    }

    fn gps_xml(fields: [(&str, &str); 7]) -> String {
        fields
            .iter()
            .map(|(name, val)| {
                format!("<String>\r\n<Name>{name}</Name>\r\n<Val>{val}</Val>\r\n</String>")
            })
            .collect()
    }

    #[test]
    fn a_fix_needs_both_flags_and_every_field_in_shape() {
        let names = Layout::V1.names();
        let good = [
            ("GPS_timestamp_UTC", "103046.8"),
            ("Lat_N", "6012.34600"),
            ("Long_ W", "01023.45700"),
            ("Fix_Quality", "1"),
            ("Alt_asl_m", "1234.5"),
            ("GPS Fix valid", "1"),
            ("GPS Message ok", "1"),
        ];
        let fix = parse_fix(&gps_xml(good), &names).unwrap();
        // East, whatever the field is called.
        assert!(fix.lon > 8.);
        assert_eq!(fix.altitude, 1234.5);

        let mut flagged = good;
        flagged[6].1 = "0";
        assert_eq!(parse_fix(&gps_xml(flagged), &names), None);

        // Flagged good, but shifted: a v1 file has these.
        let mut shifted = good;
        shifted[1].1 = "N";
        shifted[3].1 = "*4A";
        assert_eq!(parse_fix(&gps_xml(shifted), &names), None);
    }

    #[test]
    fn the_pc_date_is_month_first() {
        assert_eq!(
            pc_date("6/18/2024 1:30:00 PM"),
            chrono::NaiveDate::from_ymd_opt(2024, 6, 18)
        );
        assert_eq!(
            pc_date("01/01/2000 06:00:00 AM "),
            chrono::NaiveDate::from_ymd_opt(2000, 1, 1)
        );
        assert_eq!(pc_date(""), None);
    }

    /// One trace of a synthetic IceRadar file.
    #[derive(Clone)]
    pub(crate) struct FakeTrace {
        pub record_length: usize,
        /// Samples before the trigger.
        pub pretrigger: usize,
        pub stacking: u32,
        /// `hhmmss.s`, or `None` for a trace flagged as a bad message.
        pub time: Option<&'static str>,
    }

    const DT: f64 = 4e-9;

    fn v1_digitizer(t: &FakeTrace) -> String {
        format!(
            "<Cluster>\r\n<Name>Digitizer MetaData</Name>\r\n<DBL>\r\n<Name> Sample Rate</Name>\r\n\
             <Val>250000000.0</Val>\r\n</DBL>\r\n<U16>\r\n<Name>Stacking</Name>\r\n<Val>{}</Val>\r\n\
             </U16>\r\n<DBL>\r\n<Name>relativeInitialX</Name>\r\n<Val>{:E}</Val>\r\n</DBL>\r\n<DBL>\r\n\
             <Name>xIncrement</Name>\r\n<Val>{DT:E}</Val>\r\n</DBL>\r\n</Cluster>",
            t.stacking,
            -(t.pretrigger as f64) * DT
        )
    }

    fn v2_digitizer(t: &FakeTrace) -> String {
        v1_digitizer(t)
            .replace(" Sample Rate", "SampleRate")
            .replace("relativeInitialX", "RelativeInitialX")
            .replace("xIncrement", "XIncrement")
    }

    fn gps(layout: Layout, t: &FakeTrace) -> String {
        let n = layout.names();
        let ok = if t.time.is_some() { "1" } else { "0" };
        // East of Greenwich, whatever v1 calls the field.
        [
            (n.time, t.time.unwrap_or("M")),
            (n.lat, "6012.34567"),
            (n.lon, "01023.45678"),
            (n.fix_quality, "2"),
            (n.alt, "1234.5"),
            (n.fix_valid, "1"),
            (n.message_ok, ok),
        ]
        .iter()
        .map(|(name, val)| {
            format!("<String>\r\n<Name>{name}</Name>\r\n<Val>{val}</Val>\r\n</String>\r\n")
        })
        .collect()
    }

    /// Write `lines` as an IceRadar file of `layout`, its PC clock at
    /// `pc_timestamp`. Trace `i` holds `i + 1` mV everywhere but at the
    /// trigger, where it holds 10 mV, so alignment and order are visible.
    pub(crate) fn write_fixture(
        path: &Path,
        layout: Layout,
        lines: &[Vec<FakeTrace>],
        pc_timestamp: &str,
    ) {
        {
            let mut file = netcdf::create(path).unwrap();
            if layout == Layout::V2 {
                file.add_attribute("HardwareID", "GX0/0000").unwrap();
            }
            for (l, traces) in lines.iter().enumerate() {
                let mut line = file.add_group(&format!("line_{l}")).unwrap();
                if layout == Layout::V2 {
                    line.add_attribute("CreationTimestamp", pc_timestamp)
                        .unwrap();
                }
                for (i, t) in traces.iter().enumerate() {
                    let mut location = line.add_group(&format!("location_{i}")).unwrap();
                    let mut capture = location.add_group("datacapture_0").unwrap();
                    capture
                        .add_dimension("phony_dim_0", t.record_length)
                        .unwrap();
                    let mut var = capture
                        .add_variable::<f64>("echogram_0", &["phony_dim_0"])
                        .unwrap();
                    let mut values = vec![
                        if t.stacking == 0 {
                            0.
                        } else {
                            0.001 * (i + 1) as f64
                        };
                        t.record_length
                    ];
                    if t.stacking > 0 {
                        values[t.pretrigger] = 0.01;
                    }
                    var.put_values(&values, ..).unwrap();
                    let (digitizer, gps_name) = match layout {
                        Layout::V1 => (v1_digitizer(t), "GPS Cluster- MetaData_xml"),
                        Layout::V2 => (v2_digitizer(t), "GPSData_xml"),
                    };
                    var.put_attribute(layout.names().digitizer, digitizer.as_str())
                        .unwrap();
                    var.put_attribute(gps_name, gps(layout, t).as_str())
                        .unwrap();
                }
            }
        }
        if layout == Layout::V1 {
            let c_path = CString::new(path.to_string_lossy().as_bytes()).unwrap();
            let _guard = netcdf_sys::libnetcdf_lock.lock();
            // SAFETY: as in `hdf5_comment`. Deprecated in favour of attributes,
            // but a comment is where v1 files keep their date.
            #[allow(deprecated)]
            unsafe {
                let file = hdf5_sys::h5f::H5Fopen(
                    c_path.as_ptr(),
                    hdf5_sys::h5f::H5F_ACC_RDWR,
                    hdf5_sys::h5p::H5P_DEFAULT,
                );
                assert!(file >= 0);
                for l in 0..lines.len() {
                    let object = CString::new(format!("/line_{l}/location_0")).unwrap();
                    let comment =
                        CString::new(format!("location_0[delimiter][delimiter]{pc_timestamp}"))
                            .unwrap();
                    assert!(
                        hdf5_sys::h5o::H5Oset_comment_by_name(
                            file,
                            object.as_ptr(),
                            comment.as_ptr(),
                            hdf5_sys::h5p::H5P_DEFAULT
                        ) >= 0
                    );
                }
                hdf5_sys::h5f::H5Fclose(file);
            }
        }
    }

    pub(crate) fn trace(
        record_length: usize,
        stacking: u32,
        time: Option<&'static str>,
    ) -> FakeTrace {
        FakeTrace {
            record_length,
            pretrigger: record_length / 20,
            stacking,
            time,
        }
    }

    fn process(
        path: &Path,
        lines: Option<&str>,
        date: Option<&str>,
        mhz: Option<f32>,
    ) -> Result<crate::gpr::GPR, String> {
        run(
            path,
            crate::processing::ProcessingOptions {
                override_antenna_mhz: mhz,
                override_date: date.map(|d| d.parse().unwrap()),
                lines: lines.map(|l| l.parse().unwrap()),
                ..Default::default()
            },
        )
    }

    /// Process `path` quietly into UTM 32N with `options`.
    fn run(
        path: &Path,
        options: crate::processing::ProcessingOptions,
    ) -> Result<crate::gpr::GPR, String> {
        let params = crate::processing::RunParams {
            filepaths: vec![path.to_path_buf()],
            output_path: None,
            track_path: None,
            render_path: None,
            radargram_id: Some("test".into()),
            display_name: None,
            options: crate::processing::ProcessingOptions {
                quiet: true,
                crs: Some("EPSG:32632".into()),
                ..options
            },
        };
        crate::processing::build_processed_gpr(params)
            .map(|(gpr, _)| gpr)
            .map_err(|e| e.to_string())
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // PROJ, as for the other position readers.
    fn a_v2_line_is_aligned_on_the_trigger_and_dated_as_told() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heli.hdf5");
        let traces = vec![
            trace(40, 256, Some("120000.2")),
            // Captured nothing: left out.
            trace(40, 0, Some("120000.8")),
            // A shorter record, whose pretrigger is shorter too.
            trace(20, 256, None),
            trace(40, 255, Some("120002.0")),
        ];
        write_fixture(&path, Layout::V2, &[traces], "1/1/2000 6:00:00 AM");

        // The PC clock is at its reset value, and the frequency is unknown.
        let error = process(&path, None, None, Some(10.)).err().unwrap();
        assert!(error.contains("--override-date"), "{error}");
        let error = process(&path, None, Some("2024-08-15"), None)
            .err()
            .unwrap();
        assert!(error.contains("--override-antenna-mhz"), "{error}");

        let gpr = process(&path, None, Some("2024-08-15"), Some(10.)).unwrap();
        assert_eq!(gpr.data.shape(), &[40, 3]);
        assert_eq!(gpr.metadata.antenna, "BSI IceRadar GX0/0000");
        assert_eq!(gpr.metadata.antenna_separation, 0.);
        // In mV, and every trace's trigger on the same row.
        for column in 0..3 {
            assert_eq!(gpr.data[[2, column]], 10., "trace {column}");
        }
        assert_eq!(gpr.data[[0, 0]], 1.);
        // The short trace starts a sample later and ends earlier: zeros.
        assert_eq!(gpr.data[[0, 1]], 0.);
        assert_eq!(gpr.data[[1, 1]], 3.);
        assert_eq!(gpr.data[[21, 1]], 0.);

        let points = &gpr.location.cor_points;
        assert_eq!(points.len(), 3);
        let start = chrono::NaiveDate::from_ymd_opt(2024, 8, 15)
            .unwrap()
            .and_hms_milli_opt(12, 0, 0, 200)
            .unwrap()
            .and_utc()
            .timestamp_millis() as f64
            / 1000.;
        assert!((points[0].time_seconds - start).abs() < 1e-6);
        // The middle trace had no fix: halfway between its neighbours.
        assert!((points[1].time_seconds - (start + 0.9)).abs() < 1e-6);
        assert_eq!(gpr.inputs.as_ref().unwrap().bundles[0].part, None);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn v1_lines_are_chosen_dated_by_the_pc_and_refused_when_they_differ() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stor.h5");
        let line = |length| {
            vec![
                trace(length, 256, Some("103045.6")),
                trace(length, 256, Some("103046.8")),
            ]
        };
        write_fixture(
            &path,
            Layout::V1,
            &[line(40), line(40), line(30)],
            "6/18/2024 1:30:00 PM",
        );

        let error = process(&path, None, None, Some(20.)).err().unwrap();
        assert!(error.contains("holds 3 lines"), "{error}");
        assert!(error.contains("line_2 (2 traces)"), "{error}");
        let error = process(&path, Some("5"), None, Some(20.)).err().unwrap();
        assert!(error.contains("has no line 5"), "{error}");

        let gpr = process(&path, Some("1"), None, Some(20.)).unwrap();
        assert_eq!(gpr.width(), 2);
        assert_eq!(
            gpr.inputs.as_ref().unwrap().bundles[0].part,
            Some(crate::formats::Part { line: Some(1) })
        );
        let dated =
            chrono::DateTime::from_timestamp(gpr.location.cor_points[0].time_seconds as i64, 0)
                .unwrap();
        assert_eq!(dated.to_rfc3339(), "2024-06-18T10:30:45+00:00");
        // "Long_ W" but east: UTM 32N eastings, not ones from 8 degrees west.
        assert!(gpr.location.cor_points[0].easting > 400_000.);

        let merged = process(&path, Some("0-1"), None, Some(20.)).unwrap();
        assert_eq!(merged.width(), 4);
        assert_eq!(merged.inputs.as_ref().unwrap().bundles.len(), 2);

        let out = tempfile::tempdir().unwrap();
        let error = crate::processing::batch::run_batch(crate::processing::batch::BatchRunParams {
            filepaths: vec![path.clone()],
            output_dir: out.path().to_path_buf(),
            track_dir: None,
            render_dir: None,
            merge: None,
            id_pattern: None,
            display_name_pattern: None,
            options: crate::processing::ProcessingOptions {
                quiet: true,
                override_antenna_mhz: Some(20.),
                ..Default::default()
            },
        })
        .unwrap_err();
        assert!(
            error.contains("does not split a file into lines yet"),
            "{error}"
        );

        let error = process(&path, Some("all"), None, Some(20.)).err().unwrap();
        assert!(error.contains("Cannot merge"), "{error}");
        assert!(error.contains("line_2"), "{error}");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn one_coordinate_csv_positions_every_line_by_its_line_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stor.h5");
        let line = || {
            vec![
                trace(40, 256, Some("103045.6")),
                trace(40, 256, Some("103046.8")),
            ]
        };
        write_fixture(&path, Layout::V1, &[line(), line()], "6/18/2024 1:30:00 PM");
        let options = |lines: &str, cor: Option<&Path>| crate::processing::ProcessingOptions {
            override_antenna_mhz: Some(20.),
            lines: Some(lines.parse().unwrap()),
            cor_path: cor.map(Path::to_path_buf),
            ..Default::default()
        };

        // Beside the file: found for every line, as before, but no longer
        // read as every line's positions.
        let beside = dir.path().join("stor.coords.csv");
        let rows = "trace,time,lon,lat,alt\n\
                    0,2024-06-18T10:30:45Z,9.0,60.2,1200\n\
                    1,2024-06-18T10:30:46Z,9.0,60.2001,1200\n";
        std::fs::write(&beside, rows).unwrap();
        let error = run(&path, options("1", None)).err().unwrap();
        assert!(error.contains("no line column"), "{error}");

        // Line 1 a degree further east.
        let with_lines = "line,trace,time,lon,lat,alt\n\
                          0,0,2024-06-18T10:30:45Z,9.0,60.2,1200\n\
                          0,1,2024-06-18T10:30:46Z,9.0,60.2001,1200\n\
                          1,0,2024-06-18T10:35:00Z,10.0,60.2,1200\n\
                          1,1,2024-06-18T10:35:01Z,10.0,60.2001,1200\n";
        std::fs::write(&beside, with_lines).unwrap();
        let merged = run(&path, options("all", None)).unwrap();
        let points = &merged.location.cor_points;
        assert_eq!(merged.width(), 4);
        assert!(
            points[3].easting - points[0].easting > 50_000.,
            "{points:?}"
        );
        let line_1 = run(&path, options("1", None)).unwrap();
        assert!((line_1.location.cor_points[0].easting - points[3].easting).abs() < 100.);

        // One --cor file for every line of one file is not one file for
        // several recordings.
        let elsewhere = dir.path().join("other.csv");
        std::fs::rename(&beside, &elsewhere).unwrap();
        let merged = run(&path, options("all", Some(&elsewhere))).unwrap();
        assert_eq!(merged.location.cor_points.len(), 4);
    }

    #[test]
    fn files_that_are_not_iceradar_lines_are_refused() {
        let dir = tempfile::tempdir().unwrap();

        let empty = dir.path().join("empty.h5");
        netcdf::create(&empty).unwrap();
        let error = iceradar_lines(&empty).unwrap_err().to_string();
        assert!(error.contains("no line_N groups"), "{error}");

        // The groups of a line, but a trace without either layout's metadata.
        let foreign = dir.path().join("foreign.h5");
        {
            let mut file = netcdf::create(&foreign).unwrap();
            let mut line = file.add_group("line_0").unwrap();
            let mut location = line.add_group("location_0").unwrap();
            let mut capture = location.add_group("datacapture_0").unwrap();
            capture.add_dimension("phony_dim_0", 4).unwrap();
            capture
                .add_variable::<f64>("echogram_0", &["phony_dim_0"])
                .unwrap();
        }
        let error = read_line_meta(&foreign, 0, None).err().unwrap().to_string();
        assert!(error.contains("not an IceRadar layout"), "{error}");

        let blank = dir.path().join("blank.h5");
        write_fixture(
            &blank,
            Layout::V2,
            &[vec![trace(40, 0, None), trace(40, 0, None)]],
            "6/18/2024 1:30:00 PM",
        );
        let error = read_line_meta(&blank, 0, None).err().unwrap().to_string();
        assert!(error.contains("Every trace of line_0"), "{error}");
    }

    #[test]
    fn a_pc_date_that_cannot_be_read_asks_for_one() {
        let dir = tempfile::tempdir().unwrap();
        for layout in [Layout::V1, Layout::V2] {
            let path = dir.path().join(format!("{layout:?}.h5"));
            write_fixture(&path, layout, &[vec![trace(40, 256, None)]], "18.6.2024");
            let error = read_line_meta(&path, 0, None).err().unwrap().to_string();
            assert!(error.contains("gives no date"), "{layout:?}: {error}");
            assert!(error.contains("\"18.6.2024\""), "{layout:?}: {error}");
            assert!(error.contains("--override-date"), "{layout:?}: {error}");
            // A given date does not need the PC's.
            let date = chrono::NaiveDate::from_ymd_opt(2024, 6, 18).unwrap();
            assert_eq!(
                iceradar_date(&path, 0, Some(date)).unwrap(),
                IceRadarDate::Given(date)
            );
        }
    }

    #[test]
    fn what_was_assumed_is_said_unless_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stor.h5");
        let line = || vec![trace(40, 256, Some("103045.6"))];
        write_fixture(&path, Layout::V1, &[line(), line()], "6/18/2024 1:30:00 PM");
        let resolved = crate::formats::with_lines(
            vec![crate::formats::resolve_input(&path).unwrap()],
            Some(&"all".parse().unwrap()),
        )
        .unwrap();

        let options = crate::processing::ProcessingOptions::default();
        let warnings = crate::processing::iceradar_assumptions(&resolved, &options).unwrap();
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings[0].contains("taken as 0 m"), "{}", warnings[0]);
        for (warning, n) in warnings[1..].iter().zip(0..) {
            assert!(
                warning.contains(&format!("line_{n} is dated 2024-06-18")),
                "{warning}"
            );
        }

        let options = crate::processing::ProcessingOptions {
            override_antenna_separation: Some(10.),
            override_date: chrono::NaiveDate::from_ymd_opt(2024, 6, 18),
            ..Default::default()
        };
        let warnings = crate::processing::iceradar_assumptions(&resolved, &options).unwrap();
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            crate::processing::iceradar_assumptions(&[], &Default::default()).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn line_selections_parse() {
        use crate::formats::LineSelection;
        assert_eq!("all".parse::<LineSelection>(), Ok(LineSelection::All));
        assert_eq!(
            "3".parse::<LineSelection>(),
            Ok(LineSelection::Some(vec![3]))
        );
        assert_eq!(
            "0, 2-4,7".parse::<LineSelection>(),
            Ok(LineSelection::Some(vec![0, 2, 3, 4, 7]))
        );
        for bad in ["", "x", "4-2", "1,,2", "-1"] {
            assert!(bad.parse::<LineSelection>().is_err(), "{bad:?}");
        }
        assert!("1,0-2"
            .parse::<LineSelection>()
            .unwrap_err()
            .contains("twice"));
    }
}
