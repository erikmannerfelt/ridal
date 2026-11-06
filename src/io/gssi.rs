use std::error::Error;
use std::path::Path;

use ndarray::Array2;

use crate::gpr;

/// A `$GPRMC` date, `DDMMYY`, in this century.
fn gssi_gps_date(date: &str) -> Result<chrono::NaiveDate, Box<dyn Error>> {
    let field = |range: std::ops::Range<usize>| {
        date.get(range)
            .filter(|_| date.len() == 6)
            .and_then(|s| s.parse::<u32>().ok())
    };
    match (field(0..2), field(2..4), field(4..6)) {
        (Some(day), Some(month), Some(year)) => {
            chrono::NaiveDate::from_ymd_opt(2000 + year as i32, month, day)
        }
        _ => None,
    }
    .ok_or_else(|| format!("Invalid GSSI date: {date}").into())
}

/// How far a GPS receiver's dates are behind the recording when its 10-bit
/// week number has rolled over: every 1024 weeks.
const GPS_WEEK_ROLLOVER_DAYS: i64 = 1024 * 7;

/// The days to add to a GPS date of `gps` on a recording made on
/// `recorded`, when they are a whole number of week rollovers apart: 0
/// otherwise.
///
/// A receiver that does not know about a rollover reports dates 1024 weeks
/// (19.6 years) too early, with the right time of day. The DZT's creation
/// date comes from the control unit's clock, which does not roll over,
/// and is local time, hence the tolerance of two days around each
/// rollover.
///
/// ```text
/// 2005-09-04 on a file created 2025-04-20 -> 7168 days, 1024 weeks
/// ```
fn gps_rollover_days(gps: chrono::NaiveDate, recorded: chrono::NaiveDate) -> i64 {
    let behind = (recorded - gps).num_days();
    let rollovers = (behind as f64 / GPS_WEEK_ROLLOVER_DAYS as f64).round() as i64;
    if rollovers >= 1 && (behind - rollovers * GPS_WEEK_ROLLOVER_DAYS).abs() <= 2 {
        rollovers * GPS_WEEK_ROLLOVER_DAYS
    } else {
        0
    }
}

/// The date of a DZT header's `rh_create` field: a packed DOS-style date
/// and time, local to the control unit. `None` when unset (all zero, as
/// `rh_modify` usually is).
fn gssi_create_date(bytes: &[u8]) -> Option<chrono::NaiveDate> {
    let packed = u32::from_le_bytes(bytes.get(32..36)?.try_into().ok()?);
    let day = (packed >> 16) & 0x1f;
    let month = (packed >> 21) & 0xf;
    let year = (packed >> 25) as i32 + 1980;
    chrono::NaiveDate::from_ymd_opt(year, month, day)
}

/// The date a DZT says it was created on, if it says.
pub fn gssi_recorded_date(dzt: &Path) -> Result<Option<chrono::NaiveDate>, Box<dyn Error>> {
    let mut header = [0u8; 36];
    std::io::Read::read_exact(&mut std::fs::File::open(dzt)?, &mut header)?;
    Ok(gssi_create_date(&header))
}

/// A DZG's GPS dates corrected for a week-number rollover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpsRollover {
    /// The first date the receiver reported.
    pub reported: chrono::NaiveDate,
    /// That date corrected.
    pub corrected: chrono::NaiveDate,
}

/// Whether the GPS dates in `dzg` are a week-number rollover behind the
/// DZT it belongs to, judged by the first `$GPRMC` date.
pub fn gssi_gps_rollover(dzt: &Path, dzg: &Path) -> Result<Option<GpsRollover>, Box<dyn Error>> {
    let Some(recorded) = gssi_recorded_date(dzt)? else {
        return Ok(None);
    };
    let content = std::fs::read_to_string(dzg)?;
    let Some(date) = content.lines().find_map(|line| {
        let parts = line.split(',').collect::<Vec<_>>();
        (parts.first() == Some(&"$GPRMC"))
            .then(|| parts.get(9).copied())
            .flatten()
    }) else {
        return Ok(None);
    };
    let reported = gssi_gps_date(date)?;
    Ok(match gps_rollover_days(reported, recorded) {
        0 => None,
        days => Some(GpsRollover {
            reported,
            corrected: reported + chrono::Duration::days(days),
        }),
    })
}

/// The size of one channel's header. A DZT starts with one per channel,
/// one after another.
const HEADER_SIZE: usize = 1024;

/// How much of a DZT is read to find its headers: room for 16 channels.
const HEADERS_READ: usize = 16 * HEADER_SIZE;

/// The fields of one channel's header that Ridal reads.
#[derive(Debug, Clone)]
struct ChannelHeader {
    /// `rh_data`: where the data start, in units of [`HEADER_SIZE`] when
    /// below it.
    data: u16,
    samples: u16,
    bits: u16,
    /// Scans per second.
    sps: f32,
    /// The recorded window, in ns.
    range: f32,
    channels: u16,
    antenna: String,
}

fn read_channel_header(bytes: &[u8]) -> Result<ChannelHeader, Box<dyn Error>> {
    if bytes.len() < 128 {
        return Err("GSSI header is too short".into());
    }
    let u16_at = |offset: usize| u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
    let f32_at = |offset: usize| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    Ok(ChannelHeader {
        data: u16_at(2),
        samples: u16_at(4),
        bits: u16_at(6),
        sps: f32_at(10),
        range: f32_at(26),
        channels: u16_at(52),
        antenna: String::from_utf8_lossy(&bytes[98..112])
            .trim_matches('\0')
            .trim()
            .to_string(),
    })
}

/// Where a DZT's channels are: one header each, and the scans after them,
/// each scan one trace of every channel in turn.
#[derive(Debug)]
struct DztLayout {
    channels: Vec<ChannelHeader>,
    data_offset: usize,
    /// Whole scans in the file. A recording cut short (a full disk, a power
    /// loss) can end inside one, which is dropped rather than refusing the
    /// file.
    scans: usize,
}

impl DztLayout {
    /// `head` is the start of a file of `file_len` bytes, at least its
    /// headers when the file has them.
    fn read(head: &[u8], file_len: usize) -> Result<DztLayout, Box<dyn Error>> {
        let first = read_channel_header(head)?;
        let n_channels = first.channels.max(1) as usize;
        let bytes_per_sample = (first.bits as usize / 8).max(1);
        let data_offset = if first.data < HEADER_SIZE as u16 {
            HEADER_SIZE * first.data as usize
        } else {
            HEADER_SIZE * n_channels
        };
        let data_offset = match data_offset < file_len {
            true => data_offset,
            false => 32768 * bytes_per_sample,
        };
        if data_offset >= file_len {
            return Err("File too short: no data found".into());
        }
        let mut channels = vec![first];
        for index in 1..n_channels {
            let start = index * HEADER_SIZE;
            if start + HEADER_SIZE > data_offset {
                return Err(format!(
                    "Invalid GSSI header: {n_channels} channels do not fit before the data at \
                     byte {data_offset}"
                )
                .into());
            }
            let header = head
                .get(start..)
                .ok_or_else(|| format!("GSSI files of {n_channels} channels are not supported"))?;
            channels.push(read_channel_header(header)?);
        }
        let first = &channels[0];
        if let Some(other) = channels
            .iter()
            .position(|c| c.samples != first.samples || c.bits != first.bits)
        {
            return Err(format!(
                "Channel {} has {} samples of {} bits, but channel 1 has {} of {}; channels \
                 of different sizes are not supported",
                other + 1,
                channels[other].samples,
                channels[other].bits,
                first.samples,
                first.bits
            )
            .into());
        }
        let scan_bytes = n_channels * first.samples as usize * bytes_per_sample;
        if scan_bytes == 0 {
            return Err("Invalid GSSI header: zero sample size".into());
        }
        let scans = (file_len - data_offset) / scan_bytes;
        Ok(DztLayout {
            channels,
            data_offset,
            scans,
        })
    }

    /// The layout of the DZT at `path`, from its headers alone.
    fn of(path: &Path) -> Result<DztLayout, Box<dyn Error>> {
        let mut file = std::fs::File::open(path)?;
        let file_len = file.metadata()?.len() as usize;
        let mut head = Vec::with_capacity(HEADERS_READ);
        std::io::Read::read_to_end(
            &mut std::io::Read::take(&mut file, HEADERS_READ as u64),
            &mut head,
        )?;
        DztLayout::read(&head, file_len).map_err(|e| format!("{}: {e}", path.display()).into())
    }

    /// The header of channel `channel`, numbered from 1 as GSSI numbers
    /// them.
    fn channel(&self, channel: u32, path: &Path) -> Result<&ChannelHeader, Box<dyn Error>> {
        (channel as usize)
            .checked_sub(1)
            .and_then(|index| self.channels.get(index))
            .ok_or_else(|| {
                format!(
                    "{} has no channel {channel}; its channels are 1 to {}",
                    path.display(),
                    self.channels.len()
                )
                .into()
            })
    }
}

fn gssi_bits_to_millivolt(bits: u16) -> f32 {
    // RGPR's readDZT.R uses bits2volt(Vmax = 50, nbits = bits), i.e.
    // abs(Vmax - Vmin) / 2^nbits with Vmin = -Vmax, then ridal exports mV.
    100000.0 / 2f32.powi(bits as i32)
}

/// The frequency of the antenna named `antname` in the header of channel
/// `channel` (from 1).
fn gssi_antenna_mhz(antname: &str, channel: u32) -> Option<f32> {
    // A dual-frequency antenna names both frequencies, and records the
    // higher one on the first channel: checked on a SIR 4000 recording by
    // the spectra of the two channels.
    match (antname.trim(), channel) {
        ("300/800D", 1) => return Some(800.),
        ("300/800D", 2) => return Some(300.),
        ("300/800D", _) => return None,
        _ => {}
    }
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

/// A channel of a DZT, as listings name it.
#[derive(Debug, Clone, PartialEq)]
pub struct GssiChannel {
    /// From 1, as GSSI numbers them.
    pub number: u32,
    pub antenna_mhz: Option<f32>,
    /// The recorded window, in ns.
    pub time_window: f32,
    pub traces: u32,
}

/// The channels of the DZT at `dzt`: one for most files, and one per
/// antenna or frequency for a file recorded with several, side by side.
pub fn gssi_channels(dzt: &Path) -> Result<Vec<GssiChannel>, Box<dyn Error>> {
    let layout = DztLayout::of(dzt)?;
    Ok(layout
        .channels
        .iter()
        .zip(1..)
        .map(|(header, number)| GssiChannel {
            number,
            antenna_mhz: gssi_antenna_mhz(&header.antenna, number),
            time_window: header.range,
            traces: layout.scans as u32,
        })
        .collect())
}

/// The header of channel `channel` (from 1) of the DZT at `filepath`.
pub fn load_gssi_dzt(
    filepath: &Path,
    channel: u32,
    medium_velocity: f32,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
) -> Result<gpr::GPRMeta, Box<dyn Error>> {
    let layout = DztLayout::of(filepath)?;
    let header = layout.channel(channel, filepath)?;
    // `range` is the whole recorded window in ns. `position` only says where
    // time zero sits inside it, which the zero_corr step finds from the
    // data, so it must not shorten the window (#506).
    let time_window = header.range;
    let frequency = 1000. * (header.samples as f32) / time_window;
    let antenna = header.antenna.clone();
    let antenna_mhz = match override_antenna_mhz {
        Some(v) => v,
        None => gssi_antenna_mhz(&antenna, channel).ok_or_else(|| {
            super::UnknownAntennaFrequency::new(
                antenna.as_str(),
                format!(
                    "Could not read frequency from the GSSI antenna field ({antenna:?}). Try using the antenna MHz override"
                ),
            )
        })?,
    };

    Ok(gpr::GPRMeta {
        samples: header.samples as u32,
        frequency,
        frequency_steps: 0,
        time_interval: 1.0 / header.sps,
        antenna_mhz,
        antenna,
        antenna_separation: override_antenna_separation.unwrap_or(0.0),
        time_window,
        last_trace: layout.scans as u32,
        data_filepath: filepath.to_path_buf(),
        medium_velocity,
    })
}

/// The traces of channel `channel` (from 1) of the DZT at `filepath`, as
/// `height` samples by trace.
pub fn load_dzt(
    filepath: &Path,
    height: usize,
    channel: u32,
) -> Result<Array2<f32>, Box<dyn Error>> {
    // Attacker-controlled for an uploaded file; zero would divide by zero
    // when the width is computed below.
    if height == 0 {
        return Err("trace height (samples) must be greater than zero".into());
    }

    let bytes = std::fs::read(filepath)?;
    let layout =
        DztLayout::read(&bytes, bytes.len()).map_err(|e| format!("{}: {e}", filepath.display()))?;
    let header = layout.channel(channel, filepath)?;
    if height != header.samples as usize {
        return Err(format!(
            "{} has {} samples per trace, not {height}",
            filepath.display(),
            header.samples
        )
        .into());
    }
    if layout.scans == 0 {
        return Err(format!("File too short: {:?}", filepath).into());
    }

    let bits = header.bits;
    let trace_bytes = height * (bits as usize / 8).max(1);
    let scan_bytes = trace_bytes * layout.channels.len();
    let first = layout.data_offset + (channel as usize - 1) * trace_bytes;
    let traces = (0..layout.scans).map(|scan| {
        let start = first + scan * scan_bytes;
        &bytes[start..start + trace_bytes]
    });

    let mut data: Vec<f32> = Vec::with_capacity(layout.scans * height);
    let scale = gssi_bits_to_millivolt(bits);
    match bits {
        8 => {
            for byte in traces.flatten() {
                data.push((*byte as i16 - 128) as f32 * scale);
            }
        }
        16 => {
            for chunk in traces.flat_map(|trace| trace.as_chunks::<2>().0) {
                let value = u16::from_le_bytes(*chunk) as i32 - 32768;
                data.push(value as f32 * scale);
            }
        }
        32 => {
            for chunk in traces.flat_map(|trace| trace.as_chunks::<4>().0) {
                let value = i32::from_le_bytes(*chunk);
                data.push(value as f32 * scale);
            }
        }
        other => return Err(format!("Unsupported GSSI sample width: {other} bits").into()),
    }

    Ok(Array2::from_shape_vec((layout.scans, height), data)?.reversed_axes())
}

/// The positions in a DZG. `recorded` is the date of the DZT it belongs to
/// ([`gssi_recorded_date`]), against which GPS dates a week-number rollover
/// behind are corrected ([`gps_rollover_days`]).
pub fn load_gssi_dzg(
    filepath: &Path,
    projected_crs: Option<&String>,
    recorded: Option<chrono::NaiveDate>,
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
                    let mut date = gssi_gps_date(date)?;
                    if let Some(recorded) = recorded {
                        date += chrono::Duration::days(gps_rollover_days(date, recorded));
                    }
                    date_str = Some(date.format("%Y-%m-%d").to_string());
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
        time_source: gpr::TimeSource::Recorded,
        crs,
    })
}

/// Synthetic multi-channel recordings for tests across the crate.
#[cfg(test)]
pub(crate) mod test_dzt {
    use std::path::{Path, PathBuf};

    pub const SAMPLES: usize = 8;
    pub const SCANS: usize = 5;
    /// Each channel's window, in ns: the 800 MHz channel's is shorter.
    pub const WINDOWS: [f32; 2] = [40., 100.];

    /// The sample at `sample` of the trace of `channel` (from 1) in scan
    /// `scan`, as stored: every value tells where it came from.
    pub fn stored(scan: usize, channel: usize, sample: usize) -> i16 {
        (scan * 100 + channel * 10 + sample) as i16
    }

    /// A 16-bit DZT of two channels from a `300/800D` antenna, interleaved
    /// scan by scan, with its data after 2 KiB of headers.
    pub fn two_channel_dzt() -> Vec<u8> {
        let data_offset = 2 * 1024;
        let mut bytes = vec![0u8; data_offset];
        for (index, window) in WINDOWS.iter().enumerate() {
            let header = &mut bytes[index * 1024..];
            // rh_data in units of 1 KiB, as a SIR 4000 writes it.
            header[2..4].copy_from_slice(&2u16.to_le_bytes());
            header[4..6].copy_from_slice(&(SAMPLES as u16).to_le_bytes());
            header[6..8].copy_from_slice(&16u16.to_le_bytes());
            header[10..14].copy_from_slice(&100f32.to_le_bytes());
            header[26..30].copy_from_slice(&window.to_le_bytes());
            header[52..54].copy_from_slice(&2u16.to_le_bytes());
            header[98..106].copy_from_slice(b"300/800D");
        }
        for scan in 0..SCANS {
            for channel in 1..=2 {
                for sample in 0..SAMPLES {
                    let value = (stored(scan, channel, sample) as i32 + 32768) as u16;
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        bytes
    }

    /// `<dir>/<stem>.DZT` from [`two_channel_dzt`], with a `.DZG` of two
    /// fixes, returning the DZT's path.
    pub fn write_two_channel_recording(dir: &Path, stem: &str) -> PathBuf {
        let dzt = dir.join(format!("{stem}.DZT"));
        std::fs::write(&dzt, two_channel_dzt()).unwrap();
        let dzg = [
            "$GSSIS,0,-1",
            "$GPRMC,140541,A,7901.4763,N,01332.0122,E,0.0,357.5,140424,8.8,E,A*16",
            "$GPGGA,140541,7901.4763,N,01332.0122,E,1,12,0.7,753.0,M,35.3,M,,*4C",
            "$GSSIS,4,-1",
            "$GPRMC,140545,A,7901.4790,N,01332.0123,E,0.0,357.5,140424,8.8,E,A*15",
            "$GPGGA,140545,7901.4790,N,01332.0123,E,1,12,0.7,752.9,M,35.3,M,,*4F",
        ]
        .join("\n");
        std::fs::write(dir.join(format!("{stem}.DZG")), dzg).unwrap();
        dzt
    }
}

#[cfg(test)]
mod tests {
    use super::test_dzt::{self, SAMPLES, SCANS, WINDOWS};
    use super::{
        gps_rollover_days, gssi_antenna_mhz, gssi_channels, gssi_create_date, gssi_gps_rollover,
        load_dzt, load_gssi_dzg, load_gssi_dzt, GpsRollover, GssiChannel,
    };
    use chrono::NaiveDate;

    #[test]
    fn load_dzt_rejects_zero_height_without_panicking() {
        // Same divide-by-zero as the Malå reader: `width = data.len() / 0`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.dzt");
        std::fs::write(&path, [0u8; 256]).unwrap();
        let err = load_dzt(&path, 0, 1).unwrap_err().to_string();
        assert!(err.contains("greater than zero"), "{err}");
    }

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
        assert_eq!(gssi_antenna_mhz("5106", 1), Some(200.));
        assert_eq!(gssi_antenna_mhz("3101A", 1), Some(900.));
        assert_eq!(gssi_antenna_mhz("50300", 1), Some(300.));
        // A dual-frequency antenna: the higher frequency on the first
        // channel.
        assert_eq!(gssi_antenna_mhz("300/800D", 1), Some(800.));
        assert_eq!(gssi_antenna_mhz("300/800D", 2), Some(300.));
        assert_eq!(gssi_antenna_mhz("300/800D", 3), None);
    }

    #[test]
    fn test_load_gssi_dzt() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dzt_path = temp_dir.path().join("track.DZT");
        std::fs::write(&dzt_path, make_gssi_dzt(4)).unwrap();

        let meta = load_gssi_dzt(&dzt_path, 1, 0.1, Some(200.), Some(1.5)).unwrap();
        assert_eq!(meta.samples, 4);
        assert_eq!(meta.last_trace, 2);
        assert_eq!(meta.time_interval, 1.0 / 12.0);
        assert_eq!(meta.antenna_mhz, 200.);
        assert_eq!(meta.antenna, "5106");
        assert_eq!(meta.antenna_separation, 1.5);
        // The header has position = -60 and range = 600: the window is the
        // range alone, not range - position (#506).
        assert_eq!(meta.time_window, 600.);
        assert_eq!(meta.frequency, 1000. * 4. / 600.);

        let data = load_dzt(&dzt_path, 4, 1).unwrap();
        assert_eq!(data.shape(), &[4, 2]);
        assert!(data[[0, 0]] > 0.0);
        assert!(data[[0, 1]] > 0.0);
        assert!(data[[0, 0]] < 1.0);
        assert!(data[[0, 1]] < 1.0);
    }

    #[test]
    fn load_dzt_drops_a_partial_trailing_trace() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dzt_path = temp_dir.path().join("track.DZT");
        let mut bytes = make_gssi_dzt(2);
        bytes.extend_from_slice(&[0u8; 2 * 3]);
        std::fs::write(&dzt_path, bytes).unwrap();

        let data = load_dzt(&dzt_path, 4, 1).unwrap();
        assert_eq!(data.shape(), &[4, 2]);
    }

    #[test]
    fn the_channels_of_a_file_are_listed_with_their_own_headers() {
        let dir = tempfile::tempdir().unwrap();
        let dzt = test_dzt::write_two_channel_recording(dir.path(), "track");
        assert_eq!(
            gssi_channels(&dzt).unwrap(),
            [
                GssiChannel {
                    number: 1,
                    antenna_mhz: Some(800.),
                    time_window: WINDOWS[0],
                    traces: SCANS as u32,
                },
                GssiChannel {
                    number: 2,
                    antenna_mhz: Some(300.),
                    time_window: WINDOWS[1],
                    traces: SCANS as u32,
                },
            ]
        );
    }

    #[test]
    fn each_channel_is_read_with_its_header_and_its_own_traces() {
        let dir = tempfile::tempdir().unwrap();
        let dzt = test_dzt::write_two_channel_recording(dir.path(), "track");
        for channel in [1, 2] {
            let meta = load_gssi_dzt(&dzt, channel, 0.168, None, None).unwrap();
            assert_eq!(meta.samples, SAMPLES as u32);
            assert_eq!(meta.last_trace, SCANS as u32);
            assert_eq!(meta.time_window, WINDOWS[channel as usize - 1]);
            assert_eq!(meta.antenna_mhz, [800., 300.][channel as usize - 1]);

            let data = load_dzt(&dzt, SAMPLES, channel).unwrap();
            assert_eq!(data.shape(), &[SAMPLES, SCANS]);
            let scale = 100000.0 / 2f32.powi(16);
            for scan in 0..SCANS {
                for sample in 0..SAMPLES {
                    let stored = test_dzt::stored(scan, channel as usize, sample);
                    assert_eq!(data[[sample, scan]], stored as f32 * scale);
                }
            }
        }
    }

    #[test]
    fn a_channel_the_file_does_not_have_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let dzt = test_dzt::write_two_channel_recording(dir.path(), "track");
        for channel in [0, 3] {
            for err in [
                load_gssi_dzt(&dzt, channel, 0.168, None, None).unwrap_err(),
                load_dzt(&dzt, SAMPLES, channel).unwrap_err(),
            ] {
                assert!(err.to_string().contains("its channels are 1 to 2"), "{err}");
            }
        }
    }

    #[test]
    fn a_partial_trailing_scan_is_dropped_from_every_channel() {
        let dir = tempfile::tempdir().unwrap();
        let dzt = dir.path().join("track.DZT");
        let mut bytes = test_dzt::two_channel_dzt();
        // A whole trace of channel 1 and half of channel 2's.
        bytes.extend_from_slice(&[0u8; 2 * (SAMPLES + SAMPLES / 2)]);
        std::fs::write(&dzt, bytes).unwrap();
        for channel in [1, 2] {
            assert_eq!(
                load_dzt(&dzt, SAMPLES, channel).unwrap().shape(),
                &[SAMPLES, SCANS]
            );
        }
    }

    #[test]
    fn channels_of_different_sizes_and_a_wrong_height_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let dzt = dir.path().join("track.DZT");
        let mut bytes = test_dzt::two_channel_dzt();
        std::fs::write(&dzt, &bytes).unwrap();
        let err = load_dzt(&dzt, SAMPLES + 1, 1).unwrap_err().to_string();
        assert!(
            err.contains(&format!("has {SAMPLES} samples per trace")),
            "{err}"
        );

        // Channel 2 with twice the samples of channel 1.
        bytes[1024 + 4..1024 + 6].copy_from_slice(&(2 * SAMPLES as u16).to_le_bytes());
        std::fs::write(&dzt, bytes).unwrap();
        let err = gssi_channels(&dzt).unwrap_err().to_string();
        assert!(err.contains("channels of different sizes"), "{err}");
    }

    #[test]
    fn channels_that_do_not_fit_before_the_data_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let dzt = dir.path().join("track.DZT");
        let mut bytes = test_dzt::two_channel_dzt();
        // Three channels' headers, with the data after two.
        bytes[52..54].copy_from_slice(&3u16.to_le_bytes());
        std::fs::write(&dzt, bytes).unwrap();
        let err = gssi_channels(&dzt).unwrap_err().to_string();
        assert!(err.contains("3 channels do not fit"), "{err}");
    }

    #[test]
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

        let locations = load_gssi_dzg(&dzg_path, Some(&"EPSG:4326".to_string()), None).unwrap();
        assert_eq!(locations.cor_points.len(), 2);
        assert_eq!(locations.cor_points[0].trace_n, 0);
        assert_eq!(locations.cor_points[1].trace_n, 3);
        assert!((locations.cor_points[0].altitude - 753.0).abs() < 1e-6);
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// `rh_create` packed as the control unit writes it.
    fn packed_create(y: u32, m: u32, d: u32, hour: u32, minute: u32) -> [u8; 4] {
        (((y - 1980) << 25) | (m << 21) | (d << 16) | (hour << 11) | (minute << 5)).to_le_bytes()
    }

    #[test]
    fn create_date_is_decoded_and_unset_is_none() {
        let mut header = [0u8; 128];
        header[32..36].copy_from_slice(&packed_create(2025, 4, 20, 16, 57));
        assert_eq!(gssi_create_date(&header), Some(date(2025, 4, 20)));
        // rh_modify, as a SIR4000 leaves it.
        assert_eq!(gssi_create_date(&[0u8; 128]), None);
    }

    #[test]
    fn gps_dates_a_rollover_behind_are_corrected_and_others_are_not() {
        let recorded = date(2025, 4, 20);
        assert_eq!(gps_rollover_days(date(2005, 9, 4), recorded), 7168);
        // The creation date is local time, and a recording crosses midnight.
        assert_eq!(gps_rollover_days(date(2005, 9, 3), recorded), 7168);
        assert_eq!(gps_rollover_days(date(2005, 9, 5), recorded), 7168);
        // Two rollovers: a receiver from before 1999.
        assert_eq!(gps_rollover_days(date(1986, 1, 19), recorded), 2 * 7168);
        for gps in [
            recorded,
            date(2025, 4, 21),
            date(2005, 8, 1),
            date(2024, 4, 20),
        ] {
            assert_eq!(gps_rollover_days(gps, recorded), 0, "{gps}");
        }
    }

    const ROLLED_OVER_DZG: &str = "$GSSIS,0,-1\n\
        $GPRMC,145326,A,7904.8531,N,01330.2982,E,0.0,354.2,040905,2.6,E,A*15\n\
        $GPGGA,145326,7904.8531,N,01330.2982,E,1,12,0.7,924.2,M,32.5,M,,*4D\n\
        $GSSIS,5,-1\n\
        $GPRMC,145328,A,7904.8532,N,01330.2982,E,3.1,354.7,040905,2.6,E,A*1F\n\
        $GPGGA,145328,7904.8532,N,01330.2982,E,1,12,0.7,924.2,M,32.5,M,,*40\n";

    #[test]
    fn a_rolled_over_dzg_is_read_with_the_dzt_date() {
        let dir = tempfile::tempdir().unwrap();
        let dzt = dir.path().join("track.DZT");
        let dzg = dir.path().join("track.DZG");
        let mut bytes = make_gssi_dzt(4);
        bytes[32..36].copy_from_slice(&packed_create(2025, 4, 20, 16, 57));
        std::fs::write(&dzt, bytes).unwrap();
        std::fs::write(&dzg, ROLLED_OVER_DZG).unwrap();
        let crs = Some("EPSG:4326".to_string());

        let corrected = load_gssi_dzg(&dzg, crs.as_ref(), Some(date(2025, 4, 20))).unwrap();
        let expected = date(2025, 4, 20).and_hms_opt(14, 53, 26).unwrap();
        assert_eq!(
            corrected.cor_points[0].time_seconds,
            expected.and_utc().timestamp() as f64
        );
        let as_reported = load_gssi_dzg(&dzg, crs.as_ref(), None).unwrap();
        assert_eq!(
            corrected.cor_points[1].time_seconds - as_reported.cor_points[1].time_seconds,
            7168. * 86400.
        );

        assert_eq!(
            gssi_gps_rollover(&dzt, &dzg).unwrap(),
            Some(GpsRollover {
                reported: date(2005, 9, 4),
                corrected: date(2025, 4, 20),
            })
        );
        let options = crate::processing::ProcessingOptions {
            overrides: crate::sidecar::Overrides {
                antenna_separation_m: Some(0.),
                ..Default::default()
            },
            ..Default::default()
        };
        let resolved = crate::formats::resolve_input(&dzt).unwrap();
        let warnings = crate::processing::assumptions(&[resolved], &options).unwrap();
        let [warning] = &warnings[..] else {
            panic!("expected one warning: {warnings:?}");
        };
        assert!(warning.contains("dates it 2005-09-04"), "{warning}");
        assert!(warning.contains("read as from 2025-04-20"), "{warning}");

        // Without a creation date there is nothing to correct against.
        std::fs::write(&dzt, make_gssi_dzt(4)).unwrap();
        assert_eq!(gssi_gps_rollover(&dzt, &dzg).unwrap(), None);
    }
}
