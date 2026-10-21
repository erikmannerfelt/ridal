use std::collections::HashSet;
use std::error::Error;
/// Functions to process GPR data
use std::path::{Path, PathBuf};

use crate::formats::{self, FormatKind, ResolvedInput};
use serde::Serialize;
use std::time::SystemTime;

use ndarray::{Array1, Array2, Axis, Slice};
use rayon::prelude::*;

use crate::filters::{rolling, zero_corr};
use crate::{dem, filters, io, tools, user_metadata};

pub(crate) const DEFAULT_ZERO_CORR_FACTOR: f32 = 1.0;
pub(crate) const DEFAULT_ZERO_CORR_SIGMA: f32 = 5.0;
pub(crate) const DEFAULT_ZERO_CORR_SMOOTH_WINDOW: u32 = 51;
pub(crate) const DEFAULT_EMPTY_TRACE_STRENGTH: f32 = 1.0;
pub(crate) const DEFAULT_AUTOGAIN_N_BINS: usize = 100;
pub(crate) const DEFAULT_BANDPASS_LOW_CUTOFF: f32 = 0.1;
pub(crate) const DEFAULT_BANDPASS_HIGH_CUTOFF: f32 = 0.9;
pub(crate) const DEFAULT_BANDPASS_Q: f32 = 0.707;

/// Which geometry `correct_antenna_separation` converts travel time to
/// depth with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeparationMethod {
    /// Two slant legs below the antenna midpoint, timed from the air wave's
    /// arrival at the receiver. See [`tools::return_time_to_depth`].
    #[default]
    Slant,
    /// The pre-0.7 conversion, kept so that data processed with it -- and
    /// picks made on its grid -- can be regenerated exactly. It uses the
    /// full separation where the geometry needs half, and shrinks the
    /// separation by time zero times the medium velocity, which after a
    /// zero correction removes it altogether (#261).
    Legacy,
}

impl std::fmt::Display for SeparationMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SeparationMethod::Slant => "slant",
            SeparationMethod::Legacy => "legacy",
        })
    }
}

impl std::str::FromStr for SeparationMethod {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "slant" => Ok(SeparationMethod::Slant),
            "legacy" => Ok(SeparationMethod::Legacy),
            _ => Err("expected `slant` or `legacy`".into()),
        }
    }
}

/// Metadata associated with a GPR dataset
///
/// This contains all required information except the location data and the actual data
#[derive(Debug, Clone)]
pub struct GPRMeta {
    /// The number of samples per trace (the vertical data size)
    pub samples: u32,
    /// The control unit sampling frequency (MHz)
    pub frequency: f32,
    pub frequency_steps: u32,
    /// The interval between traces (s)
    pub time_interval: f32,
    /// The name of the antenna
    pub antenna: String,
    /// The frequency of the antenna (MHz)
    pub antenna_mhz: f32,
    /// The horizontal separation between the antenna transmitter and receiver (m)
    pub antenna_separation: f32,
    /// The return time window (ns)
    pub time_window: f32,
    /// The number of traces in the data (the horizontal data size)
    pub last_trace: u32,
    /// The path to the data file
    pub data_filepath: PathBuf,
    /// The velocity of the medium (m / ns)
    pub medium_velocity: f32,
}

impl GPRMeta {
    /// Find a ".cor" file based on the location of the ".rd3" file
    ///
    /// # Arguments
    /// - `projected_crs`: The CRS to project coordinates into
    #[cfg(test)]
    pub fn find_cor(&self, projected_crs: Option<&String>) -> Result<GPRLocation, Box<dyn Error>> {
        let cor = crate::formats::find_neighbor_case_insensitive(&self.data_filepath, "cor")
            .unwrap_or_else(|| self.data_filepath.with_extension("cor"));
        io::load_cor(&cor, projected_crs)
    }
}

impl std::fmt::Display for GPRMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "
GPR Metadata
------------
Filepath:\t\t{:?}
Samples (height):\t{}
Traces (width):\t\t{}
Time window:\t\t{} ns
Max depth:\t\t{:.1} m
Medium velocity:\t{} m/ns
Sampling frequency:\t{} MHz
Time between traces:\t{} s
Antenna:\t\t{}
Antenna separation:\t{} m
",
            self.data_filepath,
            self.samples,
            self.last_trace,
            self.time_window,
            0.5 * self.time_window * self.medium_velocity,
            self.medium_velocity,
            self.frequency,
            self.time_interval,
            self.antenna,
            self.antenna_separation,
        )
    }
}

#[derive(Debug, Copy, Clone)]
pub struct CorPoint {
    pub trace_n: u32,
    pub time_seconds: f64,
    pub easting: f64,
    pub northing: f64,
    pub altitude: f64,
}

impl CorPoint {
    fn txyz(&self) -> [f64; 4] {
        [
            self.time_seconds,
            self.easting,
            self.northing,
            self.altitude,
        ]
    }
}

#[derive(Debug, Clone)]
pub enum LocationCorrection {
    None,
    Dem {
        path: PathBuf,
        resampling: crate::dem::Resampling,
    },
}

/// How many fixes at an end of a track set the rate its times are
/// extrapolated at (#350). Fixes are timed to the second, so the two
/// nearest alone could be off by half.
const EDGE_RATE_FIXES: usize = 10;

#[derive(Debug, Clone)]
pub struct GPRLocation {
    pub cor_points: Vec<CorPoint>,
    pub correction: LocationCorrection,
    pub crs: String,
}

impl GPRLocation {
    /// Get a coordinate of a trace by interp- or extrapolation
    ///
    /// If the trace_n is lower than or equal to the first trace, the first coordinate is given
    /// (bfill)
    ///
    /// If the trace_n is higher than or equal to the last trace, the last coordinate is given
    /// (ffill)
    ///
    /// If the trace_n is equal to an existing coordinate, that coordinate is given
    ///
    /// If the trace_n is between two existing coordinates, the linearly interpolated coordinate is
    /// given.
    fn time_and_coord_at_trace(&self, trace_n: u32) -> (f64, f64, f64, f64) {
        let mut first_point: &CorPoint = &self.cor_points[0];
        let last_point = &self.cor_points[self.cor_points.len() - 1];

        // Return the first point if the requested trace_n is equal or lower to the first.
        if trace_n <= first_point.trace_n {
            return (
                first_point.time_seconds,
                first_point.easting,
                first_point.northing,
                first_point.altitude,
            );
        };

        if trace_n < last_point.trace_n {
            // At this time, the trace
            for point in &self.cor_points {
                if point.trace_n == trace_n {
                    return (
                        point.time_seconds,
                        point.easting,
                        point.northing,
                        point.altitude,
                    );
                };

                if trace_n < point.trace_n {
                    let v = tools::interpolate_values(
                        first_point.trace_n as f64,
                        &first_point.txyz(),
                        point.trace_n as f64,
                        &point.txyz(),
                        trace_n as f64,
                    );
                    return (v[0], v[1], v[2], v[3]);
                };
                first_point = point;
            }
        };
        (
            last_point.time_seconds,
            last_point.easting,
            last_point.northing,
            last_point.altitude,
        )
    }

    fn velocities(&self) -> Array1<f64> {
        //let mut offsets: Vec<[f64; 4]> = Vec::new();

        let mut offsets = Array2::from_elem((self.cor_points.len(), 4), 0_f64);

        for i in 1..self.cor_points.len() {
            let mut slice =
                offsets.slice_axis_mut(Axis(0), Slice::new(i as isize, Some(i as isize + 1), 1));
            slice.assign(&Array1::from_vec(vec![
                self.cor_points[i].time_seconds - self.cor_points[i - 1].time_seconds,
                self.cor_points[i].easting - self.cor_points[i - 1].easting,
                self.cor_points[i].northing - self.cor_points[i - 1].northing,
                self.cor_points[i].altitude - self.cor_points[i - 1].altitude,
            ]));
        }

        // Euclidean norm of the (easting, northing, altitude) step. The
        // `sqrt` is what makes this a length rather than a squared length;
        // without it every velocity is scaled by the step length itself.
        let d = offsets
            .slice_axis(Axis(1), Slice::new(1, None, 1))
            .mapv(|f| f.powi(2))
            .sum_axis(Axis(1))
            .mapv(f64::sqrt);

        let vel = (d / offsets.column(0)).mapv(|f| if f.is_finite() { f } else { 0.0 });

        vel
    }

    pub fn distances(&self) -> Array1<f64> {
        let mut offsets = Array2::from_elem((self.cor_points.len(), 3), 0_f64);

        for i in 1..self.cor_points.len() {
            let mut slice =
                offsets.slice_axis_mut(Axis(0), Slice::new(i as isize, Some(i as isize + 1), 1));
            slice.assign(&Array1::from_vec(vec![
                self.cor_points[i].time_seconds - self.cor_points[i - 1].time_seconds,
                self.cor_points[i].easting - self.cor_points[i - 1].easting,
                self.cor_points[i].northing - self.cor_points[i - 1].northing,
                //self.cor_points[i].altitude - self.cor_points[i - 1].altitude,
            ]));
        }

        // Per-step horizontal length, then a cumulative sum of those
        // lengths. The `sqrt` must be applied per step, before the
        // accumulation: cumulatively summing the squared steps yields a
        // quantity that is not a distance at all, and whose error grows
        // with the trace spacing rather than being a constant factor.
        // Altitude is deliberately excluded (see the commented-out term
        // above) -- this is distance along the ground track, not 3D path
        // length, which is what `velocities` measures.
        let mut dist = offsets
            .slice_axis(Axis(1), Slice::new(1, None, 1))
            .mapv(|f| f.powi(2))
            .sum_axis(Axis(1))
            .mapv(f64::sqrt);
        dist.accumulate_axis_inplace(Axis(0), |prev, cur| *cur += prev);

        dist
    }

    /// One point per trace from `start_trace` to `end_trace` (exclusive).
    ///
    /// Positions before the first fix and after the last are held, since
    /// nothing says where the radar went. Times are not (#350): the radar
    /// kept recording at its trace interval, and a held time makes the
    /// trace-time axis stop at the last fix, which drops every carried pick
    /// that reaches past it. They are extrapolated at
    /// [`Self::seconds_per_trace_beyond`] instead, or held if there is no
    /// usable rate.
    ///
    /// # Arguments
    /// - `header_seconds_per_trace`: the header's trace interval (s).
    fn range_fill(
        &self,
        start_trace: u32,
        end_trace: u32,
        header_seconds_per_trace: f32,
    ) -> GPRLocation {
        let mut new_points: Vec<CorPoint> = Vec::new();
        let first = self.cor_points[0];
        let last = self.cor_points[self.cor_points.len() - 1];
        let rate_before = self.seconds_per_trace_beyond(false, header_seconds_per_trace);
        let rate_after = self.seconds_per_trace_beyond(true, header_seconds_per_trace);

        for i in start_trace..end_trace {
            let txyz = self.time_and_coord_at_trace(i);
            let time_seconds = match (rate_before, rate_after) {
                (Some(rate), _) if i < first.trace_n => {
                    first.time_seconds - (first.trace_n - i) as f64 * rate
                }
                (_, Some(rate)) if i > last.trace_n => {
                    last.time_seconds + (i - last.trace_n) as f64 * rate
                }
                _ => txyz.0,
            };

            new_points.push(CorPoint {
                trace_n: i,
                time_seconds,
                easting: txyz.1,
                northing: txyz.2,
                altitude: txyz.3,
            })
        }

        GPRLocation {
            cor_points: new_points,
            correction: self.correction.clone(),
            crs: self.crs.clone(),
        }
    }

    /// The seconds per trace to extrapolate time with past the first fix
    /// (`after_last = false`) or the last (`after_last = true`), or `None`
    /// to hold the time instead.
    ///
    /// The header's interval is exact for a time-triggered radargram. For a
    /// distance-triggered one it means nothing (Malå still writes a value),
    /// so it is only trusted when it agrees within a factor of two with the
    /// rate the fixes nearest that end imply. Without a header interval the
    /// fixes' rate is used on its own. Fixes are timed to the second, so the
    /// rate is taken over up to [`EDGE_RATE_FIXES`] of them rather than
    /// between the last two.
    fn seconds_per_trace_beyond(&self, after_last: bool, header: f32) -> Option<f64> {
        let n = self.cor_points.len().min(EDGE_RATE_FIXES);
        let (near, far) = if after_last {
            let points = &self.cor_points[self.cor_points.len() - n..];
            (points[n - 1], points[0])
        } else {
            (self.cor_points[0], self.cor_points[n - 1])
        };
        let traces = near.trace_n.abs_diff(far.trace_n) as f64;
        let from_fixes = Some((near.time_seconds - far.time_seconds).abs() / traces)
            .filter(|rate| rate.is_finite() && *rate > 0.);
        let header = Some(header as f64).filter(|rate| rate.is_finite() && *rate > 0.);

        match (header, from_fixes) {
            (Some(header), Some(fixes)) if (0.5..=2.).contains(&(header / fixes)) => Some(header),
            (Some(header), None) => Some(header),
            (_, fixes) => fixes,
        }
    }

    pub fn get_dem_elevations(&mut self, dem_path: &Path) -> Result<(), String> {
        let coords = self
            .cor_points
            .iter()
            .map(|cor| crate::coords::Coord {
                x: cor.easting,
                y: cor.northing,
            })
            .collect::<Vec<crate::coords::Coord>>();

        let coords_wgs84 =
            crate::coords::to_wgs84(&coords, &crate::coords::Crs::from_user_input(&self.crs)?)?;
        let sample = dem::sample_dem(dem_path, &coords_wgs84)?;

        for (i, point) in self.cor_points.iter_mut().enumerate() {
            if let Some(e) = sample.elevations.get(i) {
                point.altitude = *e as f64;
            }
        }

        self.correction = LocationCorrection::Dem {
            path: dem_path.to_path_buf(),
            resampling: sample.resampling,
        };
        Ok(())
    }

    pub fn to_csv(&self, filepath: &Path) -> Result<(), std::io::Error> {
        let mut output = "trace_n,easting,northing,altitude\n".to_string();

        for point in &self.cor_points {
            output += &format!(
                "{},{},{},{}\n",
                point.trace_n, point.easting, point.northing, point.altitude
            );
        }

        std::fs::write(filepath, output)
    }

    pub fn length(&self) -> f64 {
        let dists = self.distances();
        dists.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
    }

    #[cfg(test)]
    pub fn duration_since(&self, other: &GPRLocation) -> f64 {
        let self_times = Array1::from_iter(self.cor_points.iter().map(|p| p.time_seconds));
        let other_times = Array1::from_iter(other.cor_points.iter().map(|p| p.time_seconds));

        let self_times_iter = self_times.iter().cloned().collect::<Vec<f64>>();
        let other_times_iter = other_times.iter().cloned().collect::<Vec<f64>>();

        let self_min = self_times_iter
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        let self_max = self_times_iter
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);

        let other_min = other_times_iter
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        let other_max = other_times_iter
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);

        if self_min > other_min {
            other_max - self_min
        } else {
            other_min - self_max
        }
    }
}

impl std::fmt::Display for GPRLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let altitudes = Array1::from_iter(self.cor_points.iter().map(|point| point.altitude));
        let eastings = Array1::from_iter(self.cor_points.iter().map(|point| point.easting));
        let northings = Array1::from_iter(self.cor_points.iter().map(|point| point.northing));

        let length = self.length();
        let duration = self.cor_points[self.cor_points.len() - 1].time_seconds
            - self.cor_points[0].time_seconds;
        write!(
            f,
            "

GPR Location data
-----------------
Start time:\t\t{}
Stop time:\t\t{}
Duration:\t\t{:.1} s
Number of points:\t{}
Points per m / per s:\t{:.2},{:.2}
Track length:\t\t{:.1} m
Altitude range:\t\t{:.1}-{:.1} m.
Centroid:\t\tE {:.1} m, N: {:.1} m, Z: {:.1} m.
CRS:\t\t\t{}
",
            tools::seconds_to_rfc3339(self.cor_points[0].time_seconds),
            tools::seconds_to_rfc3339(self.cor_points[self.cor_points.len() - 1].time_seconds),
            duration,
            self.cor_points.len(),
            self.cor_points.len() as f32 / length as f32,
            self.cor_points.len() as f64 / duration,
            length,
            altitudes.iter().cloned().fold(f64::INFINITY, f64::min),
            altitudes.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            eastings.mean().unwrap(),
            northings.mean().unwrap(),
            altitudes.mean().unwrap(),
            self.crs,
        )
    }
}

/// An in-memory GPR dataset
#[allow(clippy::upper_case_acronyms)]
pub struct GPR {
    /// The data matrix, with rows of traces (originally in mV)
    pub data: Array2<f32>,
    /// Topographically corrected data matrix, if processed.
    pub topo_data: Option<Array2<f32>>,
    /// Trace X/Y/Z location data
    pub location: GPRLocation,
    /// Metadata for the GPR dataset
    pub metadata: GPRMeta,
    /// Processing log. Each line is one processing step
    pub log: Vec<String>,
    /// The steps that the user provided
    pub steps: Vec<String>,
    /// The antenna separation that the data still needs correcting for (m).
    ///
    /// Starts as [`GPRMeta::antenna_separation`], the acquisition fact, and
    /// is zeroed by [`GPR::correct_antenna_separation`] once the geometry
    /// has been taken out of the data. The two are therefore different
    /// quantities: one is what the instrument did, this is processing
    /// state, and only this one says what `twtt` currently refers to.
    antenna_separation_effective: f32,
    /// Where each trace's first sample sits on the original recording's
    /// clock, in nanoseconds, one value per trace.
    ///
    /// A position, not an amount removed. The two are numerically the same
    /// while a zero correction crops exactly at time zero, and part company
    /// when it keeps a margin above it or something crops further (#152).
    ///
    /// Per trace rather than one number because `zero_corr(<method>, trace)`
    /// aligns each trace's first break on its own, and a single
    /// value would be the mean of crops that no individual trace actually
    /// received. That mean is not a rounding error: it is wrong by the
    /// spread of the first breaks, on the axis whose origin re-anchoring
    /// depends on.
    ///
    /// Invariant: `crop_ns.len() == width()`. Every operation that changes
    /// the trace count maintains it alongside `location.cor_points`, which
    /// has the same shape and the same rule.
    crop_ns: Vec<f32>,
    /// Where time zero -- the direct wave's arrival at the receiver -- sits on
    /// the recording clock, in nanoseconds, per trace.
    ///
    /// Set only by a zero correction, which is the only thing that locates
    /// it. `0` means it has never been located, which is the honest reading
    /// of an uncorrected file: its travel times are measured from whenever
    /// the instrument started sampling.
    ///
    /// Distinct from `crop_ns` because "how much was thrown away" and
    /// "where zero is" are different facts. They are equal after a zero
    /// correction that keeps no margin above time zero, and part company
    /// with a margin (`zero_corr(margin=...)`) or a later crop (`subset`)
    /// (#152).
    ///
    /// Same length invariant, maintained by the same five operations.
    time_zero_ns: Vec<f32>,
    /// User-supplied metadata that should be carried through processing/export.
    pub user_metadata: user_metadata::UserMetadata,
    /// Persistent radargram identity metadata (#116). Resolved once, at the
    /// end of [`build_processed_gpr`], from CLI arguments and the output
    /// stem -- not populated on GPR instances still mid-processing.
    pub identity: RidalIdentity,
    /// The raw files and settings this radargram was read with, when it
    /// was read from raw files (#382). With [`GPR::steps`], they make the
    /// recipe that export records and the revision id is derived from.
    pub inputs: Option<crate::recipe::Inputs>,
}

/// Persistent identity metadata for one processed output. See
/// [`crate::identity`] for the concepts and their precedence rules.
#[derive(Debug, Clone, Default)]
pub struct RidalIdentity {
    pub radargram_id: Option<crate::identity::RadargramId>,
    pub display_name: Option<crate::identity::DisplayName>,
    pub group_name: Option<crate::identity::GroupName>,
    pub group_id: Option<crate::identity::GroupId>,
}

impl GPR {
    /// Run one step, given as written (e.g. `dewow(10)`).
    ///
    /// The step's own definition lives in [`crate::steps::Step`]; this is
    /// the part every step shares: after it runs, the per-trace metadata
    /// must still describe the data, and the call is recorded.
    ///
    /// The record is the canonical form, every argument named and defaults
    /// filled in (`dewow(window=10)`), not what was typed: `dewow` alone
    /// would stop saying what ran the day its default changes.
    pub fn process(&mut self, step_name: &str) -> Result<(), Box<dyn Error>> {
        let parsed = crate::steps::parse_one(step_name)?;
        let log_len = self.log.len();
        let non_finite_before = self.count_non_finite();
        parsed.step.apply(self)?;
        // Every step says what it did, including when it did nothing (#85).
        if self.log.len() == log_len {
            return Err(format!("Internal error: `{step_name}` ran without logging").into());
        }
        self.check_trace_metadata(step_name)?;
        self.check_finite(step_name, non_finite_before)?;
        self.steps.push(parsed.canonical);
        Ok(())
    }

    /// The recipe this radargram was processed with (#382), or `None` when
    /// it was not read from raw files and its inputs are unknown.
    ///
    /// Each step's version is that of the step as registered now, which is
    /// the one that ran: [`GPR::steps`] holds only steps this process ran.
    pub fn recipe(&self) -> Result<Option<crate::recipe::Recipe>, String> {
        let Some(inputs) = &self.inputs else {
            return Ok(None);
        };
        let steps = self
            .steps
            .iter()
            .map(|canonical| {
                let parsed = crate::steps::parse_one(canonical)?;
                Ok(crate::recipe::RecipeStep {
                    step: canonical.clone(),
                    version: parsed.step.version(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Some(crate::recipe::Recipe {
            format: crate::recipe::RECIPE_FORMAT,
            core_version: crate::recipe::CORE_PROCESSING_VERSION,
            bundles: inputs.bundles.clone(),
            dem: inputs.dem.clone(),
            dem_resampling: match &self.location.correction {
                LocationCorrection::Dem { resampling, .. } => *resampling,
                LocationCorrection::None => crate::dem::Resampling::default(),
            },
            crs: self.location.crs.clone(),
            medium_velocity: self.metadata.medium_velocity,
            antenna_mhz_override: inputs.antenna_mhz_override,
            antenna_separation_override: inputs.antenna_separation_override,
            steps,
        }))
    }

    fn count_non_finite(&self) -> usize {
        self.data.iter().filter(|v| !v.is_finite()).count()
    }

    /// The step did not add NaN or infinite samples.
    ///
    /// A NaN spreads through every later step and still exports, so without
    /// this the first sign of it is a renderer with nothing to draw (#266).
    /// Only samples the step *added* count, so input that was already
    /// non-finite is not blamed on whichever step happens to run first.
    fn check_finite(&self, step_name: &str, before: usize) -> Result<(), String> {
        let after = self.count_non_finite();
        if after > before {
            return Err(format!(
                "`{step_name}` made {} of {} samples NaN or infinite",
                after - before,
                self.data.len()
            ));
        }
        Ok(())
    }

    /// Every per-trace vector has one entry per trace.
    ///
    /// Each step that changes the trace count has to keep these in step by
    /// hand; this catches the one that does not, at the step that broke it
    /// rather than at export.
    fn check_trace_metadata(&self, step_name: &str) -> Result<(), String> {
        let width = self.width();
        for (what, len) in [
            ("coordinates", self.location.cor_points.len()),
            ("crop offsets", self.crop_ns.len()),
            ("time-zero offsets", self.time_zero_ns.len()),
        ] {
            if len != width {
                return Err(format!(
                    "Internal error after `{step_name}`: {len} {what} for {width} traces"
                ));
            }
        }
        Ok(())
    }

    pub fn bandpass(
        &mut self,
        low_cutoff: f32,
        high_cutoff: f32,
        q: f32,
        normalized: bool,
    ) -> Result<(), String> {
        let start_time = SystemTime::now();

        if q <= 0. {
            return Err(format!("Bandpass q needs to be above 0 (provided: {q})"));
        }
        if normalized {
            if !(0. ..=1.).contains(&low_cutoff) {
                return Err(format!(
                    "Normalized low cutoff needs to be in the range 0-1 (provided: {low_cutoff})"
                ));
            }
            if !(0. ..=1.).contains(&high_cutoff) {
                return Err(format!(
                    "Normalized high cutoff needs to be in the range 0-1 (provided: {high_cutoff})"
                ));
            }
            if low_cutoff >= high_cutoff {
                return Err(format!(
                "Normalized low cutoff ({low_cutoff}) needs to be smaller than the high cutoff ({high_cutoff})."
            ));
            }
        } else if low_cutoff >= high_cutoff {
            return Err(format!(
                "Low cutoff ({low_cutoff}) needs to be smaller than the high cutoff ({high_cutoff})."
            ));
        }

        for mut col in self.data.columns_mut() {
            filters::bandpass::bandpass_hpf_then_lpf(
                &mut col,
                low_cutoff,
                high_cutoff,
                (!normalized).then_some(self.metadata.frequency),
                Some(q),
                true,
            )?;
        }

        if normalized {
            self.log_event(
                "bandpass",
                &format!(
                    "Applied a normalized zero-phase bandpass filter ({:.3}-{:.3}, q={:.3})",
                    low_cutoff, high_cutoff, q,
                ),
                start_time,
            );
        } else {
            self.log_event(
                "bandpass_mhz",
                &format!(
                    "Applied a zero-phase bandpass filter ({:.3}-{:.3} MHz, q={:.3})",
                    low_cutoff, high_cutoff, q
                ),
                start_time,
            );
        }

        Ok(())
    }

    pub fn subset(
        &self,
        min_trace: Option<u32>,
        max_trace: Option<u32>,
        min_sample: Option<u32>,
        max_sample: Option<u32>,
    ) -> Result<GPR, String> {
        let start_time = SystemTime::now();
        let min_trace_ = min_trace.unwrap_or(0);
        let max_trace_ = match max_trace {
            Some(x) => x,
            None => self.width() as u32,
        };
        let min_sample_ = min_sample.unwrap_or(0);
        let max_sample_ = match max_sample {
            Some(x) => x,
            None => self.height() as u32,
        };

        let checks = [
            (min_trace_, self.width(), "min trace number"),
            (max_trace_, self.width(), "max trace number"),
            (min_sample_, self.height(), "min sample number"),
            (max_sample_, self.height(), "max sample number"),
        ];

        for (val, maxval, desc) in checks {
            if val > maxval as u32 {
                return Err(format!(
                    "Subset failed: {desc} ({val}) out of the dataset bounds ({maxval})"
                ));
            }
        }

        let data_subset = self
            .data
            .slice(ndarray::s![
                min_sample_ as isize..max_sample_ as isize,
                min_trace_ as isize..max_trace_ as isize
            ])
            .to_owned();

        let location_subset = GPRLocation {
            cor_points: self.location.cor_points[min_trace_ as usize..max_trace_ as usize]
                .to_owned(),
            correction: self.location.correction.clone(),
            crs: self.location.crs.clone(),
        };

        // Cropping leading samples moves the origin of the travel-time
        // axis just as a zero correction does, so it adds to the same
        // offset. Measured against the pre-subset window and height, which
        // are what `min_sample_` indexes into; `metadata.time_window` is
        // rescaled a few lines below.
        let removed_ns =
            min_sample_ as f32 * self.metadata.time_window / self.height().max(1) as f32;

        let mut metadata = self.metadata.clone();

        metadata.last_trace = max_trace_;
        metadata.time_window *= (max_sample_ - min_sample_) as f32 / metadata.samples as f32;
        metadata.samples = max_sample_ - min_sample_;

        let log = self.log.clone();

        let mut new_gpr = GPR {
            data: data_subset,
            location: location_subset,
            metadata,
            log,
            steps: self.steps.clone(),
            topo_data: self.topo_data.clone(),
            antenna_separation_effective: self.antenna_separation_effective,
            crop_ns: self.crop_ns[min_trace_ as usize..max_trace_ as usize]
                .iter()
                .map(|crop| crop + removed_ns)
                .collect(),
            // Time zero does not move: the direct wave arrived when it
            // arrived, whatever is later done to the record. This is where
            // the two quantities part company, and the gap is exactly the
            // travel time of the new sample 0.
            time_zero_ns: self.time_zero_ns[min_trace_ as usize..max_trace_ as usize].to_vec(),
            user_metadata: self.user_metadata.clone(),
            identity: self.identity.clone(),
            inputs: self.inputs.clone(),
        };
        new_gpr.log_event(
            "subset",
            &format!(
                "Subset data from {:?} to ({}:{}, {}:{})",
                self.data.shape(),
                min_sample_,
                max_sample_,
                min_trace_,
                max_trace_
            ),
            start_time,
        );

        Ok(new_gpr)
    }

    /// The antenna separation the data has **not** yet been corrected for,
    /// in metres.
    ///
    /// Equal to `metadata.antenna_separation` until
    /// [`GPR::correct_antenna_separation`] runs, and zero afterwards. This
    /// is the one that says what `twtt` currently refers to; the metadata
    /// field is acquisition provenance and never changes.
    pub fn antenna_separation_effective(&self) -> f32 {
        self.antenna_separation_effective
    }

    /// Where each trace's first sample sits on the recording clock, in
    /// nanoseconds: how much of the front of the record was discarded.
    ///
    /// One value per trace, because `zero_corr(<method>, trace)` crops per trace.
    /// See [`GPR::twtt_crop_uniform_ns`] for the common case where they all
    /// agree, and [`GPR::twtt_time_zero_ns`] for where time zero sits.
    pub fn twtt_crop_ns(&self) -> &[f32] {
        &self.crop_ns
    }

    /// The one crop that describes every trace, when there is one.
    ///
    /// `None` when they differ, which is the per-trace `zero_corr` case. The
    /// export writes an `(x)`-dimensioned variable then, rather than a mean
    /// that is true of no trace.
    pub fn twtt_crop_uniform_ns(&self) -> Option<f32> {
        let first = *self.crop_ns.first()?;
        self.crop_ns
            .iter()
            .all(|v| (v - first).abs() < f32::EPSILON)
            .then_some(first)
    }

    /// Where time zero -- the direct wave's arrival at the receiver -- sits on
    /// the recording clock, per trace.
    ///
    /// `0` until a zero correction locates it, which is the honest reading
    /// of an uncorrected file: its travel times are measured from whenever
    /// the instrument started sampling.
    ///
    /// On the **same clock as the crop**, not an offset from sample 0. A
    /// zero correction without a margin therefore sets this *equal to*
    /// `crop_ns` rather than to zero — sample 0 becomes the pulse, so the
    /// travel time of sample 0 is zero; with a margin it is the margin
    /// later. Expecting `0` here after a correction is the
    /// natural reading and the wrong one, and it matters: under that
    /// reading a corrected and an uncorrected radargram would be
    /// indistinguishable, and carrying picks between them would report no
    /// movement while the data had shifted by the whole correction. That
    /// bug was reported against real data before this paragraph existed.
    ///
    /// The zero sentinel is what
    /// [`crate::interp::anchors::twtt_axis`] reads to decide that a
    /// radargram has no anchored travel-time axis at all.
    ///
    /// Named `time_zero` rather than `t0` on purpose. gprinterp's `t0` for
    /// the `twtt` axis is **neither this nor the crop** -- it is their
    /// difference, `crop - time_zero`, the travel-time value of sample 0 --
    /// and a reviewer with the SPEC in front of them substituted one for
    /// the other anyway. A name nobody reaches for by mistake is cheaper
    /// than the comment explaining why they should not have.
    pub fn twtt_time_zero_ns(&self) -> &[f32] {
        &self.time_zero_ns
    }

    /// The one time zero that describes every trace, when there is one.
    pub fn twtt_time_zero_uniform_ns(&self) -> Option<f32> {
        let first = *self.time_zero_ns.first()?;
        self.time_zero_ns
            .iter()
            .all(|v| (v - first).abs() < f32::EPSILON)
            .then_some(first)
    }

    /// Which gprinterp `y` anchor axis (SPEC §8.2) the `twtt` coordinate is.
    ///
    /// After an antenna-separation correction, `twtt` is no longer travel
    /// time between the antenna pair but travel time to and from a
    /// theoretical coincident antenna. Both are linear in sample index and
    /// both hold plausible nanosecond values, so a consumer cannot tell
    /// them apart from the numbers -- and re-anchoring one onto the other
    /// is wrong by the antenna geometry while looking entirely reasonable.
    ///
    /// Inferred here rather than tracked as a flag because Ridal owns both
    /// numbers and the inference is exact: the correction is the only thing
    /// that zeroes the effective separation, and it refuses to run when the
    /// acquisition separation is already zero. A radargram acquired at zero
    /// separation is reported as `twtt`, which is both true -- nothing was
    /// corrected -- and the conservative direction, since a corrected file
    /// is never mistaken for an uncorrected one.
    pub fn twtt_anchor_name(&self) -> &'static str {
        let corrected =
            self.antenna_separation_effective == 0. && self.metadata.antenna_separation > 0.;
        if corrected {
            "twtt_normal_incidence"
        } else {
            "twtt"
        }
    }

    pub fn vertical_resolution_ns(&self) -> f32 {
        self.metadata.time_window / self.metadata.samples as f32
    }

    pub fn from_meta_and_loc(
        location: GPRLocation,
        metadata: GPRMeta,
    ) -> Result<GPR, Box<dyn Error>> {
        let data = match metadata.data_filepath.extension().and_then(|s| s.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("rd3") => Ok(io::load_rd3(
                &metadata.data_filepath,
                metadata.samples as usize,
            )?),
            Some(ext) if ext.eq_ignore_ascii_case("dt1") => Ok(io::load_pe_dt1(
                &metadata.data_filepath,
                metadata.samples as usize,
                metadata.last_trace as usize,
            )?),
            Some(ext) if ext.eq_ignore_ascii_case("dzt") => Ok(io::load_dzt(
                &metadata.data_filepath,
                metadata.samples as usize,
            )?),
            _ => Err(format!("Unknown filetype: {:?}", metadata.data_filepath)),
        }?;

        let n_traces = data.shape()[1];
        let location_data = match data.shape()[1] == location.cor_points.len() {
            true => location,
            false => location.range_fill(0, data.shape()[1] as u32, metadata.time_interval),
        };
        let antenna_separation_effective = metadata.antenna_separation;

        Ok(GPR {
            data,
            location: location_data,
            metadata,
            log: Vec::new(),
            steps: Vec::new(),
            topo_data: None,
            antenna_separation_effective,
            crop_ns: vec![0.; n_traces],
            time_zero_ns: vec![0.; n_traces],
            user_metadata: user_metadata::UserMetadata::new(),
            identity: RidalIdentity::default(),
            inputs: None,
        })
    }

    /// A radargram whose data and vertical axis were made elsewhere: what
    /// [`crate::external::build`] assembles from arrays.
    ///
    /// The caller has already checked every argument against the others,
    /// including the length invariants of `crop_ns` and `time_zero_ns`;
    /// this only puts them where the rest of `GPR` expects them.
    pub(crate) fn from_external(
        data: Array2<f32>,
        location: GPRLocation,
        metadata: GPRMeta,
        antenna_separation_effective: f32,
        crop_ns: Vec<f32>,
        time_zero_ns: Vec<f32>,
        log: Vec<String>,
    ) -> GPR {
        GPR {
            data,
            location,
            metadata,
            log,
            steps: Vec::new(),
            topo_data: None,
            antenna_separation_effective,
            crop_ns,
            time_zero_ns,
            user_metadata: user_metadata::UserMetadata::new(),
            identity: RidalIdentity::default(),
            inputs: None,
        }
    }

    /// Render this radargram to an image with a render profile.
    ///
    /// Goes through the same pipeline as `ridal render` and the web GUI --
    /// same profiles, same resampling, same amplitude sampling -- from the
    /// array already in memory, since `--no-export --render` is allowed and
    /// then there is no file to read back.
    ///
    /// # Always `data`, never `data_topocorr`
    ///
    /// The renderer this replaced drew the topographically corrected array
    /// whenever topographic correction had run. That made one output file
    /// have two pictures: this one, and the different one the browser drew
    /// from the exported `data` variable.
    ///
    /// One array wins instead, and it is `data`, so every way of drawing a
    /// radargram agrees. The corrected view is an explicit request, `topo`
    /// (`--render-topo`, `render_topo=True`), not a thing `--render`
    /// silently switches to because an earlier step happened to run -- and
    /// it too is drawn from `data`, sheared by the elevation and depth axes
    /// this radargram exports, exactly as `ridal render --topo` and the
    /// browser's corrected view draw it (#289).
    ///
    /// This replaced a separate implementation (`io::render_jpg`) that
    /// stretched between the 1st and 99th percentile of every tenth sample
    /// and clamped the floor to zero when `unphase` had run. Two renderers
    /// that disagree about how to draw the same data is a bad property for
    /// an instrument display, and the `unphase` case was a fixed rule
    /// applied regardless of what the amplitudes actually looked like.
    pub fn render(
        &self,
        filepath: &Path,
        profile: &crate::render::profile::RenderProfile,
        width: Option<usize>,
        topo: bool,
    ) -> Result<(), Box<dyn Error>> {
        let source = crate::source::ArraySource::new(self.data.view());
        let request = crate::render::oneshot::RenderRequest {
            profile,
            width,
            quality: None,
        };
        if topo {
            let elevation: Vec<f64> = self
                .location
                .cor_points
                .iter()
                .map(|p| p.altitude)
                .collect();
            let depth = self.depths().to_vec();
            crate::render::oneshot::render_topo_to_file(
                &source,
                Some(&elevation),
                Some(&depth),
                filepath,
                &request,
            )?;
        } else {
            crate::render::oneshot::render_to_file(&source, filepath, &request)?;
        }
        Ok(())
    }

    /// Pick time zero with `method` and crop each trace to it.
    ///
    /// With [`zero_corr::Scope::Trace`] each trace is cropped by its own
    /// pick, and the bottom is trimmed to the shortest trace so that none
    /// is padded with zeros (#6). `legacy` goes to
    /// [`GPR::zero_corr_legacy`] and supports only the global scope.
    pub fn zero_corr(&mut self, settings: &zero_corr::Settings, factor: f32) -> Result<(), String> {
        let zero_corr::Settings {
            method,
            scope,
            time_zero: reference,
            margin,
            window,
            ..
        } = *settings;
        if method == zero_corr::Method::Legacy {
            if scope != zero_corr::Scope::Global {
                return Err("the legacy method only has a global scope".into());
            }
            self.zero_corr_legacy(factor);
            return Ok(());
        }
        let start_time = SystemTime::now();
        let name = format!("zero_corr({method}, {scope})");
        let step_ns = self.metadata.time_window / self.height() as f32;
        let Some(picks) = zero_corr::pick(&self.data, settings, step_ns)? else {
            self.log_event(
                &name,
                "Every trace is flat; nothing was cropped",
                start_time,
            );
            return Ok(());
        };
        let crops = picks.crops();

        // Both on the recording clock. The crop is accumulated because a
        // pipeline may crop more than once; time zero is a position and is
        // assigned. Computed before `update_data`, so `step_ns` is still
        // the interval the picks were made at.
        for (((crop, time_zero), removed), t0) in self
            .crop_ns
            .iter_mut()
            .zip(self.time_zero_ns.iter_mut())
            .zip(crops.iter())
            .zip(picks.time_zero.iter())
        {
            *time_zero = *crop + *t0 as f32 * step_ns;
            *crop += *removed as f32 * step_ns;
        }
        self.update_data(zero_corr::apply_shifts(&self.data, &crops));

        let (min, max) = (
            crops.iter().copied().min().unwrap_or(0),
            crops.iter().copied().max().unwrap_or(0),
        );
        let removed = if min == max {
            format!("removed the first {min} rows")
        } else {
            format!(
                "removed the first {min}-{max} rows per trace and trimmed the bottom by up to {} \
                 rows so that no trace is zero-padded",
                max - min
            )
        };
        let placed = if picks.shift == 0 {
            String::new()
        } else {
            format!(
                "; time zero is at the {reference}, {} samples ({:.2} ns) from the traces' \
                 aligned picks (the median distance over the traces)",
                picks.shift,
                picks.shift as f32 * step_ns
            )
        };
        let mut kept = format!(
            "; kept {} rows ({:.2} ns) above time zero (margin={margin})",
            picks.margin,
            picks.margin as f32 * step_ns
        );
        if picks.margin < picks.margin_wanted {
            kept += &format!(
                ", not {}, because a trace has no more record above its time zero",
                picks.margin_wanted
            );
        }
        let smoothed = match scope {
            zero_corr::Scope::Smooth => {
                format!("; picks smoothed by a running median over {window} traces")
            }
            _ => String::new(),
        };
        let outliers = match scope {
            zero_corr::Scope::Global => String::new(),
            zero_corr::Scope::Trace | zero_corr::Scope::Smooth => format!(
                "; {} of {} picks were outliers or failed and were replaced by their neighbours' \
                 median",
                picks.replaced,
                picks.time_zero.len()
            ),
        };
        let warning = picks
            .substituted
            .as_ref()
            .map(|why| format!("Warning: {why}. "))
            .unwrap_or_default();
        self.log_event(
            &name,
            &format!("{warning}Picked time zero and {removed}{placed}{kept}{outliers}{smoothed}"),
            start_time,
        );
        Ok(())
    }

    fn update_data(&mut self, data: Array2<f32>) {
        self.data = data;

        self.metadata.time_window *= self.height() as f32 / self.metadata.samples as f32;
        self.metadata.samples = self.height() as u32;
        self.metadata.last_trace = self.width() as u32;
    }

    /// Resample each trace onto an even depth grid, taking the antenna
    /// separation out of the geometry.
    ///
    /// `direct_velocity` is the velocity (m/ns) of the wave time zero was
    /// picked on; see [`tools::return_time_to_depth`]. [`SeparationMethod::Legacy`]
    /// ignores it.
    pub fn correct_antenna_separation(&mut self, method: SeparationMethod, direct_velocity: f32) {
        let start_time = SystemTime::now();

        if self.antenna_separation_effective == 0. {
            self.log_event(
                "correct_antenna_separation",
                "Skipping antenna separation correction since the antenna separation is 0 m.",
                start_time,
            );
            return;
        };

        let height_before = self.height();

        let depths = match method {
            SeparationMethod::Slant => self.depths_timed_from(direct_velocity),
            SeparationMethod::Legacy => self.legacy_depths(),
        };
        let max_depth = depths.iter().cloned().fold(0.0f32, f32::max);
        let min_depth = depths.iter().cloned().fold(f32::INFINITY, f32::min);

        if max_depth == 0.0 {
            eprintln!("correct_antenna_separation failed. Max depth after antenna correction ({} m) would be 0 m", self.antenna_separation_effective);
            panic!("");
        }

        // The grid the corrected radargram is resampled onto.
        //
        // Slant: the grid the file's own coordinates already describe
        // (#379). The sample interval is kept, so the exported `twtt` steps
        // by `dt` from the first sample's travel time, and `depth` is
        // `twtt * v / 2` -- so the grid has to start at the first sample's
        // `twtt * v / 2` and step by `v * dt / 2` for those coordinates to be
        // the depths of the samples they label. A spacing taken from the
        // data instead (the 0.8 quantile of the depth steps, which is
        // larger than `v * dt / 2` because the slant geometry stretches the
        // shallow part) left the `depth` coordinate short by 0.3 % on
        // Drønbreen, half a metre at 150 m. Being derivable from the file
        // is also what lets picks be carried across the correction (#370).
        //
        // Legacy keeps its own grid, which is what reproduces published
        // results to the micrometre (`derive_regression_tests`).
        let (resampler, resolution, origin_note) = match method {
            SeparationMethod::Slant => {
                let velocity = self.metadata.medium_velocity;
                let resolution = velocity * self.vertical_resolution_ns() / 2.;
                let origin = self.twtt_first_sample_ns() * velocity / 2.;
                let n_samples = ((max_depth - origin) / resolution).floor().max(0.) as usize + 1;
                let target =
                    Array1::from_iter((0..n_samples).map(|k| origin + k as f32 * resolution));
                // Only after a crop below time zero: then the first sample's
                // travel time is not the shallowest depth, and the grid
                // either leaves out or reaches above the shallowest data.
                let note = if (origin - min_depth).abs() > resolution / 2. {
                    format!(
                        " The grid starts at {origin} m, the depth of the first sample's travel \
                         time, while the shallowest sample was at {min_depth} m."
                    )
                } else {
                    String::new()
                };
                (
                    tools::Resampler::<f32>::new_with_target(depths, target),
                    resolution,
                    note,
                )
            }
            SeparationMethod::Legacy => {
                let resolution = vertical_resolution_of(&depths);
                (
                    tools::Resampler::<f32>::new(depths, resolution),
                    resolution,
                    String::new(),
                )
            }
        };

        //resampler.resample_along_axis(&mut self.data, tools::Axis2D::Row);
        self.update_data(resampler.resample_along_axis_par(&self.data, tools::Axis2D::Row));
        //tools::groupby_average(&mut self.data, tools::Axis2D::Row, &depths, *max_diff);
        self.log_event("correct_antenna_separation", &format!("Standardized depths to {} m ({} ns) per pixel by accounting for an antenna separation of {} m with the {} geometry (height changed from {} px to {} px).{}", resolution, resolution / (self.metadata.time_window / self.height() as f32), self.antenna_separation_effective, match method {
            SeparationMethod::Slant => format!("slant (direct wave at {direct_velocity} m/ns)"),
            SeparationMethod::Legacy => method.to_string(),
        }, height_before, self.height(), origin_note), start_time);

        self.antenna_separation_effective = 0.;
        self.metadata.samples = self.height() as u32;
    }

    pub fn abslog(&mut self) {
        let start_time = SystemTime::now();

        filters::abslog(&mut self.data);
        self.log_event("abslog", "Ran abslog (log10(abs(data))", start_time);
    }

    pub fn adaptive_siglog(&mut self, offset: f32) -> Result<(), String> {
        let start_time = SystemTime::now();

        let strength = filters::siglog::adaptive_siglog(&mut self.data, offset)?;
        self.log_event(
            "adaptive_siglog",
            &format!(
                "Ran adaptive siglog (noise floor: log10 {:.3}; offset: {offset}; resolved \
                 strength: {strength:.3})",
                strength - offset
            ),
            start_time,
        );
        Ok(())
    }

    pub fn siglog(&mut self, minval_log10: f32) {
        let start_time = SystemTime::now();

        filters::siglog::siglog(&mut self.data, minval_log10);
        self.log_event(
            "siglog",
            &format!(
                "Ran siglog (sign-corrected log10 of absolute values) (minval: 10e{minval_log10}))"
            ),
            start_time,
        );
    }

    pub fn correct_topography(&mut self) {
        let start_time = SystemTime::now();

        let mut altitudes = Array1::<f32>::from_iter(
            self.location
                .cor_points
                .iter()
                .map(|point| point.altitude as f32),
        );
        altitudes -= altitudes.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        altitudes *= -1.;

        // The same depths the exported `depth` axis has, so the grid and
        // the elevation axis written for it agree, including a first sample
        // above time zero.
        let depths = self.depths();
        let max_depth = depths[depths.len() - 1] - depths[0];

        let sample_per_meter = self.height() as f32 / max_depth;

        let start_indices = altitudes.mapv(|altitude| (altitude * sample_per_meter) as isize);

        let mut topo_data = Array2::<f32>::zeros((
            ((max_depth + altitudes.iter().cloned().fold(f32::NEG_INFINITY, f32::max))
                * sample_per_meter) as usize,
            self.width(),
        ));

        for (i, col) in self.data.columns().into_iter().enumerate() {
            topo_data
                .column_mut(i)
                .slice_axis_mut(
                    Axis(0),
                    Slice::new(
                        start_indices[i],
                        Some(self.height() as isize + start_indices[i]),
                        1,
                    ),
                )
                .assign(&col);
        }

        self.topo_data = Some(topo_data);

        self.log_event(
            "correct_topography",
            "Generated a profile that is corrected for topography (topo_data).",
            start_time,
        );
    }

    pub fn multiply(&mut self, factor: f32) {
        let start_time = SystemTime::now();

        self.data.mapv_inplace(|v| v * factor);

        self.log_event(
            "multiply",
            &format!(
                "Multiplied all sample values by a constant factor of {}",
                factor
            ),
            start_time,
        );
    }

    /// The pre-0.7 `zero_corr`: crop every trace at the first sample where
    /// the mean trace jumps by more than `factor` times half its standard
    /// deviation, and subtract the mean of what came before.
    pub fn zero_corr_legacy(&mut self, factor: f32) {
        let start_time = SystemTime::now();

        let mean_trace = self.data.mean_axis(Axis(1)).unwrap();

        let threshold = 0.5 * mean_trace.std(1.0) * factor;

        let mut first_rise = 0_isize;

        for i in 1..mean_trace.shape()[0] {
            if (mean_trace[i] - mean_trace[i - 1]).abs() > threshold {
                first_rise = i as isize;
                break;
            };
        }

        if first_rise == 0 {
            self.log_event(
                "zero_corr(legacy, global)",
                "Found no first rise in the mean trace; nothing was cropped",
                start_time,
            );
            return;
        };

        let mean_silent_val = mean_trace
            .slice_axis(Axis(0), Slice::new(0, Some(first_rise), 1))
            .mean()
            .unwrap();

        let removed = self.metadata.time_window * (first_rise as f32) / self.height() as f32;
        for (crop, time_zero) in self.crop_ns.iter_mut().zip(self.time_zero_ns.iter_mut()) {
            *crop += removed;
            *time_zero = *crop;
        }
        self.update_data(
            self.data
                .slice_axis(Axis(0), Slice::new(first_rise, None, 1))
                .to_owned(),
        );
        self.data -= mean_silent_val;

        self.log_event(
            "zero_corr(legacy, global)",
            &format!(
                "Applied a global zero-corr by removing the first {first_rise} rows (factor: \
                 {factor})"
            ),
            start_time,
        );
    }

    /// Subtract from each sample the running median or mean of the samples
    /// around it in the same trace, over `window` (#259).
    pub fn dewow(
        &mut self,
        window: rolling::DewowWindow,
        statistic: rolling::Statistic,
    ) -> Result<(), String> {
        let start_time = SystemTime::now();
        let step_ns = self.metadata.time_window / self.height() as f32;
        let half = window.half_samples(self.metadata.antenna_mhz, step_ns)?;
        rolling::dewow(&mut self.data, half, statistic);
        self.log_event(
            "dewow",
            &format!(
                "Subtracted the running {statistic} over {} samples ({:.2} ns) in each trace",
                2 * half + 1,
                (2 * half + 1) as f32 * step_ns
            ),
            start_time,
        );
        Ok(())
    }

    /// Subtract from each sample the median or mean of the same sample over
    /// `traces` (#259).
    pub fn background_removal(
        &mut self,
        traces: rolling::TraceWindow,
        statistic: rolling::Statistic,
    ) {
        let start_time = SystemTime::now();
        rolling::background_removal(&mut self.data, traces.half(), statistic);
        let over = match traces {
            rolling::TraceWindow::All => "all traces".to_string(),
            rolling::TraceWindow::Traces(n) => format!("a running window of {n} traces"),
        };
        self.log_event(
            "background_removal",
            &format!("Subtracted the {statistic} trace over {over}"),
            start_time,
        );
    }

    /// Remove narrowband interference measured below `start_ns` (travel
    /// time) from every sample. See [`filters::tones`].
    ///
    /// Never fails on the data: a noise window too short to measure leaves
    /// the data unchanged and the log says why.
    pub fn remove_tones(
        &mut self,
        start_ns: f32,
        max_tones: usize,
        traces: usize,
        prominence_db: f32,
    ) {
        let start_time = SystemTime::now();
        let step_ns = self.vertical_resolution_ns();
        let first_sample = self
            .twtt_ns()
            .iter()
            .position(|&t| t >= start_ns)
            .unwrap_or(self.height());
        let settings = filters::tones::Settings {
            first_sample,
            max_tones,
            traces,
            prominence_db,
        };
        // The tone is a sinusoid in the recorded samples. Resampling them
        // onto a depth grid bends it, and the fit then removes less.
        let resampled =
            if self.antenna_separation_effective == 0. && self.metadata.antenna_separation > 0. {
                " Warning: the traces were resampled by correct_antenna_separation, which \
             distorts the tones; run remove_tones before it."
            } else {
                ""
            };
        match filters::tones::remove_tones(&mut self.data, step_ns, &settings) {
            Ok(report) => {
                let strongest = match (report.strongest_mhz, report.strongest_amplitude) {
                    (Some((lo, hi)), Some(amplitude)) => format!(
                        "; the strongest tone in each trace was at {lo:.1}-{hi:.1} MHz with a \
                         median amplitude of {amplitude:.3}"
                    ),
                    _ => String::new(),
                };
                self.log_event(
                    "remove_tones",
                    &format!(
                        "Fitted {:.2} tones per trace on average (at most {max_tones}, {} of {} \
                         traces had any) over the {} samples from {start_ns} ns, and subtracted \
                         them from the whole trace{strongest}.{resampled}",
                        report.mean_tones,
                        report.traces_with_tones,
                        self.width(),
                        report.window_samples,
                    ),
                    start_time,
                );
            }
            Err(reason) => self.log_event(
                "remove_tones",
                &format!("Warning: {reason} (from {start_ns} ns); removed nothing.{resampled}"),
                start_time,
            ),
        }
    }

    /// Even out slow changes in trace amplitude along the profile, with the
    /// gain measured in a shallow window and below `deep_ns` (travel times).
    /// See [`filters::balance`].
    ///
    /// Never fails on the data: windows the record does not hold, or spans
    /// in seconds without a usable trace interval, leave the data unchanged
    /// and the log says why.
    pub fn balance_traces(
        &mut self,
        deep_ns: f32,
        shallow_start_ns: f32,
        shallow_end_ns: f32,
        traces: filters::balance::Span,
        reference: filters::balance::Span,
    ) {
        let start_time = SystemTime::now();
        let seconds_per_trace = self.metadata.time_interval;
        let (traces_n, reference_n) = match (
            traces.traces(seconds_per_trace),
            reference.traces(seconds_per_trace),
        ) {
            // The windows are centred, so an even count is one trace longer.
            (Ok(t), Ok(r)) => (2 * (t / 2) + 1, 2 * (r / 2) + 1),
            (Err(reason), _) | (_, Err(reason)) => {
                self.log_event(
                    "balance_traces",
                    &format!("Warning: {reason}; changed nothing"),
                    start_time,
                );
                return;
            }
        };
        // Both units, when the trace interval makes the seconds meaningful.
        let span = |n: usize| {
            if seconds_per_trace > 0. && seconds_per_trace.is_finite() {
                format!("{n} traces ({:.1} s)", n as f32 * seconds_per_trace)
            } else {
                format!("{n} traces")
            }
        };
        // A window as long as the profile is cut to it at every trace.
        let against = if reference_n >= self.width() {
            format!("the median of all {} traces", self.width())
        } else {
            format!("the running median of {}", span(reference_n))
        };

        let twtt = self.twtt_ns();
        let sample_at = |ns: f32| twtt.iter().position(|&t| t >= ns).unwrap_or(self.height());
        let settings = filters::balance::Settings {
            shallow: (sample_at(shallow_start_ns), sample_at(shallow_end_ns)),
            deep_start: sample_at(deep_ns),
            traces: traces_n,
            reference: reference_n,
        };
        let windows = format!(
            "{shallow_start_ns}-{shallow_end_ns} ns and from {deep_ns} ns, averaged over {} \
             against {against}",
            span(traces_n)
        );
        match filters::balance::balance_traces(&mut self.data, &settings) {
            Ok(report) => {
                let range = |(lo, median, hi): (f32, f32, f32)| {
                    format!("{lo:.2}-{hi:.2} (median {median:.2})")
                };
                self.log_event(
                    "balance_traces",
                    &format!(
                        "Scaled the traces by a gain measured at {windows}: amplitude factors of \
                         {} in the shallow window and {} in the deep one, interpolated in \
                         between",
                        range(report.shallow_factor),
                        range(report.deep_factor),
                    ),
                    start_time,
                );
            }
            Err(reason) => self.log_event(
                "balance_traces",
                &format!("Warning: {reason} (windows at {windows}); changed nothing"),
                start_time,
            ),
        }
    }

    /// Measure the display gain that levels the amplitude envelope below the
    /// direct wave, and apply it (#266).
    ///
    /// See [`filters::gain::estimate_display_gain`] for the measurement.
    ///
    /// Never fails on the data. When no gain can be measured (too short a
    /// record, no amplitude, a non-finite result) or the envelope grows with
    /// time, it applies no gain and the log says why: a radargram without
    /// gain is still usable, and one bad file should not stop a batch.
    pub fn auto_gain(&mut self, n_bins: usize) {
        let start_time = SystemTime::now();

        let est = match filters::gain::estimate_display_gain(
            &self.data,
            self.vertical_resolution_ns(),
            n_bins,
        ) {
            Ok(est) if est.db_per_ns.is_finite() => est,
            Ok(est) => {
                self.log_event(
                    "auto_gain",
                    &format!(
                        "Warning: measured a non-finite gain ({}); applied no gain",
                        est.db_per_ns
                    ),
                    start_time,
                );
                return;
            }
            Err(reason) => {
                self.log_event(
                    "auto_gain",
                    &format!("Warning: could not measure a gain ({reason}); applied no gain"),
                    start_time,
                );
                return;
            }
        };
        let window = format!(
            "the median bin-to-bin change of the median |amplitude| over {:.0}-{:.0} ns \
             ({} bin pairs of {n_bins} bins, below the direct wave's ring-down)",
            est.fit_start_ns, est.fit_end_ns, est.bin_pairs,
        );

        if est.db_per_ns < 0. {
            self.log_event(
                "auto_gain",
                &format!(
                    "Warning: the amplitude grows with time ({:.5} dB / ns, from {window}); \
                     applied no gain",
                    est.db_per_ns
                ),
                start_time,
            );
            return;
        }
        self.log_event(
            "auto_gain",
            &format!(
                "Measured a display gain of {:.5} dB / ns (TWT) from {window}",
                est.db_per_ns
            ),
            start_time,
        );
        self.gain(est.db_per_ns);
    }

    pub fn gain(&mut self, factor: f32) {
        let start_time = SystemTime::now();

        let ns_per_trace = self.vertical_resolution_ns();
        for i in 0..self.height() as isize {
            let mut view = self
                .data
                .slice_axis_mut(Axis(0), Slice::new(i, Some(i + 1), 1_isize));

            view *= 10_f32.powf(factor * (i as f32) * ns_per_trace / 20.);
        }
        self.log_event(
            "gain",
            &format!("Applied gain of {factor} dB / ns (TWT)",),
            start_time,
        );
    }

    pub fn make_equidistant(&mut self, step: Option<f32>) {
        let start_time = SystemTime::now();
        let distances = self.location.distances().mapv(|v| v as f32);
        let max_distance = distances.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

        let step = step.unwrap_or({
            let velocities = self.location.velocities().mapv(|v| v as f32);

            let normal_velocity = tools::quantiles(&velocities, &[0.5], None)[0];

            let mut seconds_moving = 0_f32;
            for i in 1..self.width() {
                if velocities[i] < (0.3 * normal_velocity) {
                    continue;
                };
                seconds_moving += self.metadata.time_interval;
            }

            let nominal_data_width =
                (seconds_moving / self.metadata.time_interval).floor() as usize;

            max_distance / (nominal_data_width as f32)
        });

        if (max_distance / step).round() as usize == self.width() {
            self.log_event(
                "equidistant_traces",
                "Traces were already equidistant.",
                start_time,
            );
            return;
        };

        let resampler = tools::Resampler::<f32>::new(distances, step);
        //resampler.resample_along_axis(&mut self.data, tools::Axis2D::Col);
        self.update_data(resampler.resample_along_axis_par(&self.data, tools::Axis2D::Col));

        //let eastings = resampler.resample(&Array1::from_vec(self.location.cor_points.iter().map(|p| p.easting).collect::<Vec<f64>>()).view());
        let eastings = resampler.resample_convert::<f64>(
            &Array1::from_vec(
                self.location
                    .cor_points
                    .iter()
                    .map(|p| p.easting)
                    .collect::<Vec<f64>>(),
            )
            .view(),
        );
        let northings = resampler.resample_convert::<f64>(
            &Array1::from_vec(
                self.location
                    .cor_points
                    .iter()
                    .map(|p| p.northing)
                    .collect::<Vec<f64>>(),
            )
            .view(),
        );
        // Resampled onto the new trace positions like every other
        // per-trace quantity here.
        let crops = resampler.resample_convert::<f64>(
            &Array1::from_vec(self.crop_ns.iter().map(|v| *v as f64).collect::<Vec<f64>>()).view(),
        );
        let time_zeros = resampler.resample_convert::<f64>(
            &Array1::from_vec(
                self.time_zero_ns
                    .iter()
                    .map(|v| *v as f64)
                    .collect::<Vec<f64>>(),
            )
            .view(),
        );
        let times = resampler.resample_convert::<f64>(
            &Array1::from_vec(
                self.location
                    .cor_points
                    .iter()
                    .map(|p| p.time_seconds)
                    .collect::<Vec<f64>>(),
            )
            .view(),
        );
        let altitudes = resampler.resample_convert::<f64>(
            &Array1::from_vec(
                self.location
                    .cor_points
                    .iter()
                    .map(|p| p.altitude)
                    .collect::<Vec<f64>>(),
            )
            .view(),
        );

        let mut cor_points = Vec::<CorPoint>::new();

        for i in 0..eastings.len() {
            let cor = CorPoint {
                trace_n: i as u32,
                time_seconds: times[i],
                easting: eastings[i],
                northing: northings[i],
                altitude: altitudes[i],
            };
            cor_points.push(cor);
        }

        self.metadata.last_trace = self.data.shape()[1] as u32;
        self.location.cor_points = cor_points;
        self.crop_ns = crops.iter().map(|v| *v as f32).collect();
        self.time_zero_ns = time_zeros.iter().map(|v| *v as f32).collect();
        self.log_event(
            "equidistant_traces",
            &format!("Ran equidistant traces with a spacing of {step} m"),
            start_time,
        );
        /*
        let breaks = tools::groupby_average(&mut self.data, tools::Axis2D::Col, &distances, step);

        self.metadata.last_trace = self.data.shape()[1] as u32;

        self.location.cor_points = breaks
            .iter()
            .map(|i| self.location.cor_points[*i].clone())
            .collect::<Vec<CorPoint>>();
        self.location
            .cor_points
            .insert(0, self.location.cor_points[0].clone());

        if self.width() != self.location.cor_points.len() {
            panic!(
                "Data width {} != cor points width {}. Was equidistant traces run twice?",
                self.width(),
                self.location.cor_points.len()
            )
        };

        self.log_event("equidistant_traces", "Ran equidistant traces", start_time);
        */
    }

    pub(crate) fn shift_coordinates(
        &mut self,
        along_track: f64,
        altitude: f64,
        cross_track: f64,
    ) -> Result<(), String> {
        let start_time = SystemTime::now();

        if self.location.cor_points.len() < 2 {
            return Err("shift_coordinates needs at least two valid coordinates".to_string());
        }

        self.location.cor_points = crate::filters::coordinates::shift_coordinates(
            &self.location.cor_points,
            along_track,
            altitude,
            cross_track,
        );

        self.log_event("shift_coordinates", &format!("Shifted coordinates {along_track} m along the track (+ is forward), {altitude} in altitude (+ is up) and {cross_track} m across the track (+ is right)."), start_time);

        Ok(())
    }

    fn log_event(&mut self, step_name: &str, event: &str, start_time: SystemTime) {
        self.log.push(format!(
            "{} (duration: {:.2}s):\t{}",
            step_name,
            SystemTime::now()
                .duration_since(start_time)
                .unwrap()
                .as_secs_f32(),
            event
        ));
    }

    pub fn kirchhoff_migration2d(&mut self) {
        let start_time = SystemTime::now();
        let x_coords = self.location.distances().mapv(|v| v as f32);
        let mut x_diff = 0_f32;
        for i in 1..x_coords.shape()[0] {
            x_diff += x_coords[i] - x_coords[i - 1];
        }
        x_diff /= (x_coords.shape()[0] - 1) as f32;

        // The z-coords will be negative height in relation to the highest point (all values are
        // positive).
        let mut z_coords = Array1::from_iter(
            self.location
                .cor_points
                .iter()
                .map(|point| point.altitude as f32),
        );
        z_coords -= z_coords.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        z_coords *= -1.0;

        // The vertical resolution in ns
        let t_diff = self.vertical_resolution_ns();

        // The vertical resolution in m
        let z_diff = t_diff * self.metadata.medium_velocity;

        // The minimum logical resolution, assuming just one wavelength
        let logical_res =
            (self.metadata.antenna_mhz / self.metadata.medium_velocity) * (1e-9 * 1e6);

        let old_data = self.data.iter().collect::<Vec<&f32>>();

        let height = self.height();
        let width = self.width();

        let output: Vec<f32> = (0..(width * height))
            .into_par_iter()
            .map(|sample_idx| {
                let row = sample_idx / width;
                let trace_n = sample_idx - (row * width);
                let trace_top_z = z_coords[trace_n];
                let trace_x = x_coords[trace_n];

                // The expected two-way time of the sample (assuming it is straight down)
                let t_0 = 2. * row as f32 * t_diff;
                let t_0_px = row;

                // Derive the Fresnel zone
                // This is the radius in which the sample may be affected horizontally
                // Pérez-Gracia et al. (2008) Horizontal resolution in a non-destructive
                // shallow GPR survey: An experimental evaluation. NDT & E International,
                // 41(8): 611–620. doi:10.1016/j.ndteint.2008.06.002
                let fresnel_radius = 0.5 * (logical_res * 2. * z_diff * row as f32).sqrt();

                // Derive the radius in pixel space
                let fresnel_width = fresnel_radius / x_diff;

                // If the fresnel width is zero pixels, no neighbors will change the sample. Therefore, just
                // take the old value and put it in the topographically correct place.
                if fresnel_width < 0.1 {
                    return old_data[(t_0_px * width) + trace_n].to_owned();
                };

                // Derive all of the neighboring columns that may affect the sample
                let min_neighbor = (trace_n as f32 - fresnel_width)
                    .floor()
                    .clamp(0_f32, width as f32) as usize;
                let max_neighbor = (trace_n + fresnel_width.ceil() as usize + 1).clamp(0, width);

                let mut ampl = 0_f32;
                let mut n_ampl = 0_f32;

                // Pixels that are entirely within the fresnel width should be included fully. Pixels
                // outside should be excluded. Pixels that border the fresnel width should be included
                // in a weighted average. Here, these weights are created. If the neighbour-trace
                // distance is larger than the fresnel width, the weight is first assigned 1.0.
                // The ones bordering the fresnel width are given the fraction how how much is covered
                // , e.g. 24% (0.24). With a fresnel width of e.g. 1.2, there would be three weights:
                // [0.2, 1.0, 0.2]
                let n_neighbors = (max_neighbor - min_neighbor) as f32;
                let in_fresnel_weight = n_neighbors / (n_neighbors - 2.);
                let out_fresnel_weight = fresnel_width.fract();

                for neighbor_n in min_neighbor..max_neighbor {
                    // Get the vertical component of the two-way time to the neighboring sample.
                    let t_top = t_0
                        - 2. * (z_coords[neighbor_n] - trace_top_z) / self.metadata.medium_velocity;

                    // Get the travel time to the sample accounting for the x distance
                    let t_x = (t_top.powi(2)
                        + (2. * (x_coords[neighbor_n] - trace_x) / self.metadata.medium_velocity)
                            .powi(2))
                    .sqrt();
                    // The sample will be in either the pixel when rounding down or when rounding up
                    // ... so these will both be evaluated
                    // These values have pixel units, as they are normalized to pixel resolution
                    let t_1_px = (0.5 * t_x / t_diff).floor() as usize;
                    let mut t_2_px = (0.5 * t_x / t_diff).ceil() as usize;

                    if t_2_px >= height {
                        t_2_px = t_1_px;
                    };

                    // If the travel times are within the bounds of the data and the pixel displacement is not zero,
                    // ... append a weighted amplitude accounting for the displacement distance
                    if t_1_px < height {
                        let weight = match t_1_px == t_2_px {
                            true => 0_f32,
                            false => ((t_1_px as f32 - (0.5 * t_x / t_diff))
                                / (t_1_px as f32 - t_2_px as f32))
                                .abs(),
                        };

                        ampl += (x_diff / (2. * std::f32::consts::PI * t_x * self.metadata.medium_velocity).sqrt()) // Account for the horizontal distance
                        * (t_top / t_x) // Account for the vertical distance
                        * ((1. - weight) * old_data[(t_1_px * width) + neighbor_n] + weight * old_data[(t_2_px * width) + neighbor_n]) // Account for the neigbour's value
                        * if (neighbor_n == min_neighbor) | (neighbor_n == max_neighbor - 1)
                         {out_fresnel_weight} else {in_fresnel_weight};
                        n_ampl += 1.0;
                    };
                }

                if n_ampl > 0. {
                    ampl / n_ampl
                } else {
                    0.
                }
            })
            .collect();

        self.update_data(Array2::from_shape_vec((height, width), output).unwrap());
        self.log_event(
            "kirchhoff_migration2d",
            &format!(
                "Ran 2D Kirchhoff migration with a velocity of {} m/ns",
                self.metadata.medium_velocity
            ),
            start_time,
        );
    }

    /// Remove traces based on their integer index
    pub fn remove_traces(&mut self, traces: &[usize], log: bool) -> Result<(), String> {
        let start_time = SystemTime::now();
        // The width will be called multiple times, so it's better to assign it statically.
        let width = self.width();

        // Make the traces unique, and validate that they are not out of bounds
        let mut unique_traces = Vec::<usize>::new();
        for trace in traces {
            if trace >= &width {
                return Err(format!(
                    "Trace index {trace:?} is out of bounds (number of traces: {width})"
                ));
            }
            if !unique_traces.contains(trace) {
                unique_traces.push(*trace);
            }
        }

        // Make a vec of all traces to keep (faster to subset than to explicitly remove in ndarray)
        // This is done before the index trick below, because the trick below is only needed for vecs.
        let traces_to_keep: Vec<usize> =
            (0..width).filter(|i| !unique_traces.contains(i)).collect();

        // Sort them, and then subtract the amount of removals that are done before this index.
        // For example, if trace 0,1 and 2 should be removed, by the time the loop reaches 2, it's now the zeroth index.
        // Therefore, the true index is index - i, where i is the count of indices before
        unique_traces.sort_unstable();
        for i in 0..unique_traces.len() {
            // This is ugly but makes cargo clippy happy. Sorry, future developers!
            let i2 = i;
            unique_traces[i2] -= i;
        }

        // Remove each selected trace from the data, (potentially) topo. corr. data, and the location info.
        self.data = self.data.select(Axis(1), &traces_to_keep);
        if let Some(data) = self.topo_data.as_mut() {
            self.topo_data = Some(data.select(Axis(1), &traces_to_keep));
        };
        self.crop_ns = traces_to_keep.iter().map(|i| self.crop_ns[*i]).collect();
        self.time_zero_ns = traces_to_keep
            .iter()
            .map(|i| self.time_zero_ns[*i])
            .collect();
        for trace in &unique_traces {
            self.location.cor_points.remove(*trace);
        }
        self.metadata.last_trace = self.width() as u32;

        if log {
            self.log_event(
                "remove_traces",
                &format!("Removed trace indices {unique_traces:?}"),
                start_time,
            );
        }

        Ok(())
    }

    pub fn average_traces(&mut self, window: usize) -> Result<(), String> {
        let start_time = SystemTime::now();

        let initial_width = self.width();
        let averaged_data = filters::average_traces(&self.data, window)?;

        if let Some(topo_data) = &self.topo_data {
            self.topo_data = Some(filters::average_traces(topo_data, window)?);
        }

        self.location.cor_points =
            filters::window_subset_vec(self.location.cor_points.clone(), window);
        // The same window rule as the positions, for the same reason: what
        // survives has to describe the traces that survive.
        self.crop_ns = filters::window_subset_vec(self.crop_ns.clone(), window);
        self.time_zero_ns = filters::window_subset_vec(self.time_zero_ns.clone(), window);
        self.metadata.time_interval *= window as f32;

        self.update_data(averaged_data);

        self.log_event(
            "average_traces",
            &format!("Averaged traces in a window={window}. Reduced trace number from {initial_width} to {}", self.width()),
            start_time,
        );

        Ok(())
    }
    // Remove all traces whose absolute mean is lower than the given "strength"
    pub fn remove_empty_traces(&mut self, strength: f32) -> Result<(), String> {
        let start_time = SystemTime::now();
        let mut traces_to_remove = Vec::<usize>::new();

        for (i, col) in self.data.columns().into_iter().enumerate() {
            if let Some(mad) = col.mapv(|v| v.abs()).mean() {
                if mad > strength {
                    continue;
                };
                traces_to_remove.push(i);
            }
        }

        let n_removed = traces_to_remove.len();
        self.remove_traces(&traces_to_remove, false)?;

        self.log_event(
            "remove_empty_traces",
            &format!("Removed {n_removed} empty traces (strength: {strength})."),
            start_time,
        );

        Ok(())
    }

    /// Replace each stretch recorded while the radar stood still with its
    /// median trace. See [`filters::standstill`].
    ///
    /// With `gps_uncertainty` (m), the positions are used as well
    /// ([`filters::standstill::gps`]). Positions that cannot help (too few
    /// fixes, or a track that never moves) are left out with a warning.
    ///
    /// Never fails on the data: too little to measure, or a duration in
    /// seconds without a usable trace interval, leaves the data unchanged
    /// and the log says why.
    pub fn remove_standstills(
        &mut self,
        strength: f32,
        min_duration: filters::balance::Span,
        gps_uncertainty: Option<f32>,
    ) {
        let start_time = SystemTime::now();
        let seconds_per_trace = self.metadata.time_interval;
        let min_traces = match min_duration.traces(seconds_per_trace) {
            Ok(n) => n,
            Err(reason) => {
                self.log_event(
                    "remove_standstills",
                    &format!("Warning: {reason}; changed nothing"),
                    start_time,
                );
                return;
            }
        };

        // One period of the antenna's nominal frequency, in samples, puts the
        // first sample used below the direct wave.
        let step_ns = self.vertical_resolution_ns();
        let antenna_mhz = self.metadata.antenna_mhz;
        let period = (antenna_mhz > 0. && antenna_mhz.is_finite() && step_ns > 0.)
            .then(|| ((1000. / antenna_mhz) / step_ns).round() as usize);
        let no_frequency = if period.is_none() {
            format!(
                " Warning: the antenna frequency is {antenna_mhz} MHz, so the direct wave was \
                 taken to end where the mean trace falls below a tenth of its peak."
            )
        } else {
            String::new()
        };
        let settings = filters::standstill::Settings {
            strength,
            min_traces,
            first_sample: filters::standstill::below_direct_wave(self.data.view(), period),
        };

        let seconds: Vec<f64> = self
            .location
            .cor_points
            .iter()
            .map(|p| p.time_seconds)
            .collect();
        let easting: Vec<f64> = self.location.cor_points.iter().map(|p| p.easting).collect();
        let northing: Vec<f64> = self
            .location
            .cor_points
            .iter()
            .map(|p| p.northing)
            .collect();
        let track = filters::standstill::gps::Track {
            seconds: &seconds,
            easting: &easting,
            northing: &northing,
        };
        let (gps, gps_note) = match gps_uncertainty {
            None => (None, String::new()),
            Some(uncertainty) => {
                match filters::standstill::gps::Gps::new(track, uncertainty as f64) {
                    Ok(gps) => {
                        let note = format!(
                        " GPS uncertainty {uncertainty} m: stretches of {:.1} s or more can be \
                         judged.",
                        gps.window_s
                    );
                        (Some(gps), note)
                    }
                    Err(reason) => (
                        None,
                        format!(" Warning: {reason}, so the GPS was not used."),
                    ),
                }
            }
        };

        let detection = match filters::standstill::detect(self.data.view(), &settings, gps.as_ref())
        {
            Ok(detection) => detection,
            Err(reason) => {
                self.log_event(
                    "remove_standstills",
                    &format!("Warning: {reason}; changed nothing.{no_frequency}{gps_note}"),
                    start_time,
                );
                return;
            }
        };

        let width = self.width();
        // A duration in seconds where the trace interval gives one.
        let duration = |n: usize| {
            if seconds_per_trace > 0. && seconds_per_trace.is_finite() {
                format!("{:.1} s", n as f32 * seconds_per_trace)
            } else {
                format!("{n} traces")
            }
        };
        let elsewhere = detection.highest_elsewhere.map_or(String::new(), |z| {
            format!(" Highest strength elsewhere: {z:.1}.")
        });
        // The minimum duration as it was given.
        let shortest = match min_duration {
            filters::balance::Span::Seconds(s) => format!("{s} s"),
            filters::balance::Span::Traces(n) => format!("{n} traces"),
        };
        let threshold = match gps_uncertainty {
            None => format!("strength >= {strength}, >= {shortest}"),
            Some(u) => format!("strength >= {strength}, >= {shortest}, gps_uncertainty = {u} m"),
        };
        // What the GPS changed.
        let mut gps_changes = String::new();
        if !detection.ruled_out.is_empty() {
            let listed: Vec<String> = detection
                .ruled_out
                .iter()
                .map(|s| format!("{}-{} ({:.1})", s.start, s.end - 1, s.peak))
                .collect();
            gps_changes += &format!(
                " The GPS showed movement through {} that reached the strength (traces \
                 (strength): {}).",
                listed.len(),
                listed.join(", ")
            );
        }
        if detection.cut > 0 {
            gps_changes += &format!(
                " The GPS showed movement in part of {}, which kept only the rest.",
                detection.cut
            );
        }
        if detection.unjudged > 0 {
            gps_changes += &format!(
                " {} had too few fixes for the GPS to judge.",
                detection.unjudged
            );
        }
        let vouched = detection
            .standstills
            .iter()
            .filter(|s| s.peak < strength)
            .count();
        if vouched > 0 {
            gps_changes += &format!(
                " {vouched} below the strength {} kept because the GPS showed standing still.",
                if vouched == 1 { "was" } else { "were" }
            );
        }
        let skipped = match detection.too_short.as_slice() {
            [] => String::new(),
            short => format!(
                " Skipped {} shorter than {shortest} (strongest {:.1}).",
                short.len(),
                short
                    .iter()
                    .map(|s| s.peak)
                    .fold(f32::NEG_INFINITY, f32::max)
            ),
        };
        if detection.standstills.is_empty() {
            self.log_event(
                "remove_standstills",
                &format!(
                    "Found no standstills ({threshold}).{skipped}{elsewhere}{gps_changes}\
                     {no_frequency}{gps_note}"
                ),
                start_time,
            );
            return;
        }

        // Each output trace stands for one input trace (its time, position
        // and offsets) and, inside a standstill, for the whole stretch.
        let mut representatives = Vec::with_capacity(width);
        let mut medians = Vec::with_capacity(detection.standstills.len());
        let mut next = 0;
        for standstill in &detection.standstills {
            representatives.extend(next..standstill.start);
            medians.push((representatives.len(), *standstill));
            representatives.push(standstill.representative(width));
            next = standstill.end;
        }
        representatives.extend(next..width);

        let replace = |data: &Array2<f32>| {
            let mut out = data.select(Axis(1), &representatives);
            for (column, standstill) in &medians {
                let stretch = data.slice(ndarray::s![.., standstill.start..standstill.end]);
                for (out_value, row) in out.column_mut(*column).iter_mut().zip(stretch.rows()) {
                    let mut values = row.to_vec();
                    let middle = values.len() / 2;
                    *out_value = *values.select_nth_unstable_by(middle, f32::total_cmp).1;
                }
            }
            out
        };
        self.data = replace(&self.data);
        if let Some(topo_data) = &self.topo_data {
            self.topo_data = Some(replace(topo_data));
        }
        self.location.cor_points = representatives
            .iter()
            .map(|&i| self.location.cor_points[i])
            .collect();
        self.crop_ns = representatives.iter().map(|&i| self.crop_ns[i]).collect();
        self.time_zero_ns = representatives
            .iter()
            .map(|&i| self.time_zero_ns[i])
            .collect();
        self.metadata.last_trace = self.width() as u32;

        let listed: Vec<String> = detection
            .standstills
            .iter()
            .map(|s| {
                format!(
                    "{}-{} ({}, {:.1})",
                    s.start,
                    s.end - 1,
                    duration(s.len()),
                    s.peak
                )
            })
            .collect();
        let replaced = match detection.standstills.len() {
            1 => "1 standstill with its median trace".to_string(),
            n => format!("{n} standstills with their median trace"),
        };
        self.log_event(
            "remove_standstills",
            &format!(
                "Replaced {replaced} ({threshold}). Traces (duration, strength): {}. {width} -> {} \
                 traces.{skipped}{elsewhere}{gps_changes}{no_frequency}{gps_note}",
                listed.join(", "),
                self.width()
            ),
            start_time,
        );
    }

    pub fn height(&self) -> usize {
        self.data.shape()[0]
    }
    pub fn width(&self) -> usize {
        self.data.shape()[1]
    }

    pub fn export(&self, nc_filepath: &Path) -> Result<(), String> {
        let ds = self.export_dataset()?;
        io::export_netcdf(&ds, nc_filepath)
    }

    /// Travel time of the first sample, in nanoseconds.
    ///
    /// `crop - time_zero`: zero right after a zero correction that crops
    /// exactly at time zero, negative when it kept a margin above time
    /// zero, and positive once something crops further. One number for the
    /// whole radargram, because every trace is aligned on its own time zero
    /// and so has the same difference; the mean guards the export against
    /// a pipeline that breaks that.
    pub fn twtt_first_sample_ns(&self) -> f32 {
        if self.crop_ns.is_empty() {
            return 0.;
        }
        self.crop_ns
            .iter()
            .zip(&self.time_zero_ns)
            .map(|(crop, time_zero)| crop - time_zero)
            .sum::<f32>()
            / self.crop_ns.len() as f32
    }

    /// Travel time of every sample, in nanoseconds.
    pub fn twtt_ns(&self) -> Array1<f32> {
        let first = self.twtt_first_sample_ns();
        let step = self.vertical_resolution_ns();
        Array1::from_iter((0..self.height()).map(|i| first + i as f32 * step))
    }

    /// Depth of every sample, in metres. Samples above time zero (a margin
    /// kept by `zero_corr`) get negative depths, as a straight path through
    /// the medium, so the axis keeps increasing.
    ///
    /// The geometry is [`tools::return_time_to_depth`]'s, with the separation
    /// the data has not yet been corrected for. Where time zero sits on the
    /// recording clock plays no part: that is mostly the instrument's
    /// pre-trigger delay, not a flight time (#261). Before a zero correction,
    /// sample 0 stands in for time zero.
    ///
    /// Time zero is taken to be on the air wave. Only
    /// `correct_antenna_separation` can be told otherwise, and after it has
    /// run there is no separation left for the choice to matter.
    pub fn depths(&self) -> Array1<f32> {
        self.depths_timed_from(tools::SPEED_OF_LIGHT_AIR_M_PER_NS)
    }

    /// [`GPR::depths`], with time zero on a direct wave travelling at
    /// `direct_velocity` (m/ns).
    fn depths_timed_from(&self, direct_velocity: f32) -> Array1<f32> {
        self.twtt_ns().mapv(|time| {
            if time < 0. {
                time * self.metadata.medium_velocity / 2.
            } else {
                tools::return_time_to_depth(
                    time,
                    self.metadata.medium_velocity,
                    self.antenna_separation_effective,
                    direct_velocity,
                )
            }
        })
    }

    /// [`GPR::depths`] as it was before #261, for
    /// [`SeparationMethod::Legacy`] only.
    fn legacy_depths(&self) -> Array1<f32> {
        let velocity = self.metadata.medium_velocity;
        let time_zero_mean = match self.time_zero_ns.len() {
            0 => 0.,
            n => self.time_zero_ns.iter().sum::<f32>() / n as f32,
        };
        let separation = (self.antenna_separation_effective.powi(2)
            - (time_zero_mean * velocity).powi(2))
        .max(0.)
        .sqrt();
        self.twtt_ns().mapv(|time| {
            let two_way_distance = time * velocity;
            if time < 0. {
                two_way_distance / 2.
            } else if two_way_distance > 2. * separation {
                (two_way_distance.powi(2) - 4. * separation.powi(2)).sqrt() / 2.
            } else {
                0.
            }
        })
    }

    /// Append `other`'s traces to these.
    ///
    /// Refused, naming both values, when anything the merged file would
    /// state once for all its traces differs: the CRS, antenna frequency,
    /// time window, antenna separation (nominal and effective) or medium
    /// velocity.
    pub fn merge(&mut self, other: &GPR) -> Result<(), String> {
        let start_time = SystemTime::now();
        if let Some(refusal) = MergeKey::of(&self.metadata, &self.location)
            .refusal(&MergeKey::of(&other.metadata, &other.location))
        {
            Err(refusal)
        } else if self.antenna_separation_effective != other.antenna_separation_effective {
            // Unlike the others this is recoverable, so say how.
            Err(format!(
                "Effective antenna separations are different: {} m vs {} m. One radargram \
                 has had correct_antenna_separation applied and the other has not; apply it \
                 to both before merging",
                self.antenna_separation_effective, other.antenna_separation_effective
            ))
        } else {
            self.location
                .cor_points
                .append(other.location.cor_points.clone().as_mut());

            self.data.append(Axis(1), other.data.view()).unwrap();
            // Per trace, so it concatenates with the traces. Two inputs
            // cropped by different amounts is exactly the case a single
            // number could not describe.
            self.crop_ns.extend_from_slice(&other.crop_ns);
            self.time_zero_ns.extend_from_slice(&other.time_zero_ns);

            self.metadata.time_window *= self.height() as f32 / self.metadata.samples as f32;
            self.metadata.samples = self.height() as u32;
            self.metadata.last_trace = self.width() as u32;
            user_metadata::merge_prefer_first(&mut self.user_metadata, &other.user_metadata);

            self.log_event(
                "merge",
                &format!("Merged {:?}", other.metadata.data_filepath),
                start_time,
            );

            Ok(())
        }
    }
}

/// The typical spacing of a depth axis, in metres: its 80th-percentile step.
fn vertical_resolution_of(depths: &Array1<f32>) -> f32 {
    let diffs = &depths.slice(ndarray::s![1..]) - &depths.slice(ndarray::s![..-1]);
    tools::quantiles(&diffs, &[0.8], None)[0]
}

#[derive(Debug, Clone)]
pub struct RunParams {
    pub filepaths: Vec<PathBuf>,
    pub output_path: Option<PathBuf>,
    pub dem_path: Option<PathBuf>,
    pub cor_path: Option<PathBuf>,
    pub medium_velocity: f32,
    pub crs: Option<String>,
    pub quiet: bool,
    pub track_path: Option<Option<PathBuf>>,
    pub steps: Vec<String>,
    pub no_export: bool,
    pub render_path: Option<Option<PathBuf>>,
    /// Render profile for `render_path`: a built-in name or a TOML file.
    /// `None` means the built-in `default`.
    pub render_profile: Option<String>,
    /// Output width for `render_path`. `None` means one pixel per trace.
    pub render_width: Option<usize>,
    /// Render the topographically corrected view to `render_path` (#289).
    pub render_topo: bool,
    pub override_antenna_mhz: Option<f32>,
    pub override_antenna_separation: Option<f32>,
    pub user_metadata: user_metadata::UserMetadata,
    pub radargram_id: Option<String>,
    pub display_name: Option<String>,
    pub group: Option<String>,
    pub group_id: Option<String>,
}
#[derive(Debug, Clone)]
pub struct BatchRunParams {
    pub filepaths: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub dem_path: Option<PathBuf>,
    pub cor_path: Option<PathBuf>,
    pub medium_velocity: f32,
    pub crs: Option<String>,
    pub quiet: bool,
    pub track_dir: Option<PathBuf>,
    pub steps: Vec<String>,
    pub no_export: bool,
    pub render_dir: Option<PathBuf>,
    /// Render profile for `render_dir`, as in [`RunParams::render_profile`].
    pub render_profile: Option<String>,
    /// Output width for `render_dir`, as in [`RunParams::render_width`].
    pub render_width: Option<usize>,
    /// As in [`RunParams::render_topo`].
    pub render_topo: bool,
    pub merge: Option<String>,
    pub override_antenna_mhz: Option<f32>,
    pub override_antenna_separation: Option<f32>,
    pub user_metadata: user_metadata::UserMetadata,
    // Batch mode derives a radargram ID and (absent) display name per output
    // from each output's own stem -- an explicit single ID or display name
    // would collide across outputs -- unless patterns say otherwise (#368).
    // A group applies uniformly to the whole batch, since batch runs
    // typically process one survey or campaign.
    pub group: Option<String>,
    pub group_id: Option<String>,
    /// An id pattern (see [`crate::id_pattern`]). The outputs are then named
    /// after their ids, `<id>.nc`.
    pub id_pattern: Option<String>,
    pub display_name_pattern: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BatchProcessResult {
    pub output_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct InfoParams {
    pub filepaths: Vec<PathBuf>,
    pub dem_path: Option<PathBuf>,
    pub cor_path: Option<PathBuf>,
    pub medium_velocity: f32,
    pub crs: Option<String>,
    pub override_antenna_mhz: Option<f32>,
    pub override_antenna_separation: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct NamedFormat {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct MetadataSummary {
    pub samples: u32,
    pub sampling_frequency_mhz: f32,
    pub frequency_steps: u32,
    pub time_interval_s: f32,
    pub antenna_name: String,
    pub antenna_mhz: f32,
    pub antenna_separation_m: f32,
    pub time_window_ns: f32,
    pub last_trace: u32,
    pub medium_velocity_m_per_ns: f32,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct CentroidSummary {
    pub easting: f64,
    pub northing: f64,
    pub altitude: f64,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct CorrectionSummary {
    pub kind: String,
    #[cfg_attr(feature = "server", schema(required = true))]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct LocationSummary {
    pub n_points: usize,
    pub crs: String,
    pub start_time: String,
    pub stop_time: String,
    pub duration_s: f64,
    pub track_length_m: f64,
    pub altitude_min_m: f64,
    pub altitude_max_m: f64,
    pub centroid: CentroidSummary,
    pub correction: CorrectionSummary,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct RelatedFiles {
    pub header: String,
    pub data: String,
    pub coordinates: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct InfoRecord {
    pub input: String,
    pub format: NamedFormat,
    pub metadata: MetadataSummary,
    pub location: LocationSummary,
    pub related_files: RelatedFiles,
}

/// Whether `records`, in merge order, can be merged into one radargram, from
/// their headers alone: the checks [`GPR::merge`] makes that do not need the
/// data, so a mismatch can be refused before anything is processed (#382).
/// Keep the two in step.
///
/// The message counts the recordings with each value of the first property
/// that differs, and names the files in every group but a clear majority,
/// so that one odd file among many stands out.
pub fn check_mergeable(records: &[InfoRecord]) -> Result<(), String> {
    type Property = (&'static str, &'static str, fn(&InfoRecord) -> String);
    let properties: [Property; 4] = [
        (
            "their positions are in different CRSs",
            "; give one CRS for all of them",
            |r| r.location.crs.clone(),
        ),
        ("their antenna frequencies differ", "", |r| {
            format!("{} MHz", r.metadata.antenna_mhz)
        }),
        ("their time windows differ", "", |r| {
            format!("{} ns", r.metadata.time_window_ns)
        }),
        ("their antenna separations differ", "", |r| {
            format!("{} m", r.metadata.antenna_separation_m)
        }),
    ];
    let name = |record: &InfoRecord| {
        Path::new(&record.input)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| record.input.clone())
    };
    for (what, hint, value_of) in properties {
        // Groups in order of first appearance, then largest first.
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for record in records {
            let value = value_of(record);
            match groups.iter_mut().find(|(v, _)| *v == value) {
                Some((_, names)) => names.push(name(record)),
                None => groups.push((value, vec![name(record)])),
            }
        }
        if groups.len() < 2 {
            continue;
        }
        groups.sort_by_key(|(_, names)| std::cmp::Reverse(names.len()));
        let clear_majority = groups[0].1.len() > groups[1].1.len();
        let described: Vec<String> = groups
            .iter()
            .enumerate()
            .map(|(i, (value, names))| {
                let verb = if names.len() == 1 { "is" } else { "are" };
                let count = format!("{} {verb} {value}", names.len());
                if i == 0 && clear_majority {
                    return count;
                }
                const SHOWN: usize = 3;
                let mut listed = names
                    .iter()
                    .take(SHOWN)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                if names.len() > SHOWN {
                    listed.push_str(&format!(" and {} more", names.len() - SHOWN));
                }
                format!("{count} ({listed})")
            })
            .collect();
        return Err(format!(
            "These recordings cannot be merged into one radargram: {what}{hint}. {}.",
            described.join("; ")
        ));
    }
    Ok(())
}

/// What `ridal info` reports for a NetCDF file Ridal processed (#11).
///
/// Identity rather than contents: enough to match a local file to a radargram
/// a server already has. A file from a Ridal too old to have radargram ids
/// is reported rather than refused, with `reprocess_reason` set and the
/// identity fields `None`, so a caller sorting many files does not have to
/// tell that apart from a failure.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessedInfoRecord {
    pub input: String,
    pub format: NamedFormat,
    /// The Ridal version that wrote the file.
    pub ridal_version: String,
    /// Why the file has to be reprocessed before Ridal can use it, or
    /// `None` when it does not.
    pub reprocess_reason: Option<String>,
    pub radargram_id: Option<String>,
    pub display_name: Option<String>,
    pub group_name: Option<String>,
    pub group_id: Option<String>,
    /// Verbatim from the file, since the revision id of a file without a
    /// recipe hashes this string.
    pub processing_datetime: Option<String>,
    /// The hash of what the file was made from (#382), or `None` for a
    /// file written before recipes were recorded.
    pub recipe_hash: Option<String>,
    /// The id a server gives this revision of the radargram (#117): from
    /// `recipe_hash` when there is one, otherwise from
    /// `processing_datetime`.
    pub revision_id: Option<String>,
    pub samples: Option<usize>,
    pub traces: Option<usize>,
}

/// One inspected input: a raw recording, or a file Ridal processed.
///
/// Untagged, so each serializes as its own record; `format.name` tells them
/// apart (`ridal` for a processed file).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Inspected {
    Raw(InfoRecord),
    Processed(ProcessedInfoRecord),
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub output_path: PathBuf,
}

fn has_glob_chars(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.contains('*') || text.contains('?') || text.contains('[')
}

pub fn expand_input_paths(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut expanded = Vec::<PathBuf>::new();
    for input in inputs {
        if has_glob_chars(input) {
            let mut matches = glob::glob(&input.to_string_lossy())
                .map_err(|e| format!("Invalid glob pattern {:?}: {e}", input))?
                .filter_map(Result::ok)
                .collect::<Vec<PathBuf>>();
            matches.sort();
            if matches.is_empty() {
                return Err(format!("Glob pattern matched no files: {:?}", input));
            }
            expanded.extend(matches);
        } else {
            expanded.push(input.clone());
        }
    }
    if expanded.is_empty() {
        return Err("No input files were provided.".to_string());
    }
    Ok(expanded)
}

fn derive_output_path(
    output_path: Option<&PathBuf>,
    first_resolved: &ResolvedInput,
) -> Result<PathBuf, String> {
    let default_output = first_resolved.header.with_extension("nc");
    match output_path {
        Some(path) => {
            if path.is_dir() {
                Ok(path
                    .join(first_resolved.header.file_stem().unwrap())
                    .with_extension("nc"))
            } else {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() && !parent.is_dir() {
                        return Err(format!(
                            "Output directory of path is not a directory: {:?}",
                            path
                        ));
                    }
                }
                Ok(path.clone())
            }
        }
        None => Ok(default_output),
    }
}

fn load_meta_and_location(
    resolved: &ResolvedInput,
    medium_velocity: f32,
    crs: Option<&String>,
    cor_path: Option<&PathBuf>,
    dem_path: Option<&PathBuf>,
    override_antenna_mhz: Option<f32>,
    override_antenna_separation: Option<f32>,
) -> Result<(GPRMeta, GPRLocation), Box<dyn Error>> {
    let (gpr_meta, mut gpr_location) = match resolved.kind {
        FormatKind::Ramac => {
            if !resolved.header.is_file() {
                return Err(format!("File not found: {:?}", resolved.header).into());
            }
            let meta = io::load_rad(
                &resolved.header,
                medium_velocity,
                override_antenna_mhz,
                override_antenna_separation,
            )?;
            let cor_source = match cor_path {
                Some(path) => path.clone(),
                None => resolved.coordinates.clone(),
            };
            let location = io::load_cor(&cor_source, crs)?;
            (meta, location)
        }
        FormatKind::PulseEkko => {
            if cor_path.is_some() {
                return Err("The --cor option is only supported for RAMAC inputs.".into());
            }
            if !resolved.header.is_file() {
                return Err(format!("File not found: {:?}", resolved.header).into());
            }
            let meta = io::load_pe_hd(
                &resolved.header,
                medium_velocity,
                override_antenna_mhz,
                override_antenna_separation,
            )?;
            let location = io::load_pe_gp2(&resolved.coordinates, crs)?;
            (meta, location)
        }
        FormatKind::Gssi => {
            if cor_path.is_some() {
                return Err("The --cor option is only supported for RAMAC inputs.".into());
            }
            if !resolved.header.is_file() {
                return Err(format!("File not found: {:?}", resolved.header).into());
            }
            let meta = io::load_gssi_dzt(
                &resolved.header,
                medium_velocity,
                override_antenna_mhz,
                override_antenna_separation,
            )?;
            let location = io::load_gssi_dzg(&resolved.coordinates, crs)?;
            (meta, location)
        }
    };

    if let Some(dem_path) = dem_path {
        gpr_location.get_dem_elevations(dem_path)?;
    }

    Ok((gpr_meta, gpr_location))
}

fn metadata_summary(meta: &GPRMeta) -> MetadataSummary {
    MetadataSummary {
        samples: meta.samples,
        sampling_frequency_mhz: meta.frequency,
        frequency_steps: meta.frequency_steps,
        time_interval_s: meta.time_interval,
        antenna_name: meta.antenna.clone(),
        antenna_mhz: meta.antenna_mhz,
        antenna_separation_m: meta.antenna_separation,
        time_window_ns: meta.time_window,
        last_trace: meta.last_trace,
        medium_velocity_m_per_ns: meta.medium_velocity,
    }
}

fn correction_summary(location: &GPRLocation) -> CorrectionSummary {
    match &location.correction {
        LocationCorrection::None => CorrectionSummary {
            kind: "none".to_string(),
            source: None,
        },
        LocationCorrection::Dem { path, .. } => CorrectionSummary {
            kind: "dem".to_string(),
            source: Some(path.to_string_lossy().to_string()),
        },
    }
}

fn location_summary(location: &GPRLocation) -> Result<LocationSummary, String> {
    if location.cor_points.is_empty() {
        return Err("No coordinate points were available for the input.".to_string());
    }
    let altitudes = Array1::from_iter(location.cor_points.iter().map(|point| point.altitude));
    let eastings = Array1::from_iter(location.cor_points.iter().map(|point| point.easting));
    let northings = Array1::from_iter(location.cor_points.iter().map(|point| point.northing));
    let length = location.length();
    let duration = location.cor_points[location.cor_points.len() - 1].time_seconds
        - location.cor_points[0].time_seconds;

    Ok(LocationSummary {
        n_points: location.cor_points.len(),
        crs: location.crs.clone(),
        start_time: tools::seconds_to_rfc3339(location.cor_points[0].time_seconds),
        stop_time: tools::seconds_to_rfc3339(
            location.cor_points[location.cor_points.len() - 1].time_seconds,
        ),
        duration_s: duration,
        track_length_m: length,
        altitude_min_m: altitudes.iter().cloned().fold(f64::INFINITY, f64::min),
        altitude_max_m: altitudes.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        centroid: CentroidSummary {
            easting: eastings.mean().unwrap(),
            northing: northings.mean().unwrap(),
            altitude: altitudes.mean().unwrap(),
        },
        correction: correction_summary(location),
    })
}

fn info_record(
    resolved: &ResolvedInput,
    meta: &GPRMeta,
    location: &GPRLocation,
) -> Result<InfoRecord, String> {
    let fmt = formats::format_info(resolved.kind);
    Ok(InfoRecord {
        input: resolved.input.to_string_lossy().to_string(),
        format: NamedFormat {
            name: fmt.name,
            description: fmt.description,
        },
        metadata: metadata_summary(meta),
        location: location_summary(location)?,
        related_files: RelatedFiles {
            header: resolved.header.to_string_lossy().to_string(),
            data: resolved.data.to_string_lossy().to_string(),
            coordinates: resolved.coordinates.to_string_lossy().to_string(),
        },
    })
}

/// Whether `path` is a NetCDF file, which `info` reads as Ridal output
/// rather than as a raw recording.
fn is_netcdf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nc"))
}

/// Inspect a NetCDF file as Ridal output, without loading its amplitudes.
fn processed_info_record(path: &Path) -> Result<ProcessedInfoRecord, String> {
    let record = |ridal_version: String| ProcessedInfoRecord {
        input: path.to_string_lossy().to_string(),
        format: NamedFormat {
            name: "ridal",
            description: "NetCDF processed by Ridal",
        },
        ridal_version,
        reprocess_reason: None,
        radargram_id: None,
        display_name: None,
        group_name: None,
        group_id: None,
        processing_datetime: None,
        recipe_hash: None,
        revision_id: None,
        samples: None,
        traces: None,
    };
    match crate::io::inspect_ridal_netcdf(path)? {
        crate::io::RidalNetcdfKind::Supported(meta) => {
            let revision_id = meta.revision_id();
            let (samples, traces) = meta.shape;
            Ok(ProcessedInfoRecord {
                radargram_id: Some(meta.radargram_id.to_string()),
                display_name: meta.display_name.map(|name| name.to_string()),
                group_name: meta.group_name.map(|name| name.to_string()),
                group_id: meta.group_id.map(|id| id.to_string()),
                processing_datetime: Some(meta.processing_datetime),
                recipe_hash: meta.recipe_hash,
                revision_id: Some(revision_id.to_string()),
                samples: Some(samples),
                traces: Some(traces),
                ..record(meta.ridal_version)
            })
        }
        crate::io::RidalNetcdfKind::Legacy(version) => Ok(ProcessedInfoRecord {
            reprocess_reason: Some(format!(
                "This file was {}",
                crate::io::legacy_reason(&version)
            )),
            ..record(version)
        }),
        crate::io::RidalNetcdfKind::NotRidal => Err(format!(
            "{} is a NetCDF file but not one Ridal processed.",
            path.display()
        )),
    }
}

pub fn inspect(params: InfoParams) -> Result<Vec<Inspected>, Box<dyn Error>> {
    let expanded = expand_input_paths(&params.filepaths)?;
    let mut records = Vec::<Inspected>::new();

    // These describe how to read a raw recording. A processed file already
    // carries the result, so given one they would be silently ignored.
    let raw_only = [
        ("cor", params.cor_path.is_some()),
        ("dem", params.dem_path.is_some()),
        ("crs", params.crs.is_some()),
        (
            "override_antenna_mhz",
            params.override_antenna_mhz.is_some(),
        ),
        (
            "override_antenna_separation",
            params.override_antenna_separation.is_some(),
        ),
    ];
    if let Some(netcdf) = expanded.iter().find(|input| is_netcdf(input)) {
        let given: Vec<&str> = raw_only
            .iter()
            .filter(|(_, set)| *set)
            .map(|(name, _)| *name)
            .collect();
        if !given.is_empty() {
            return Err(format!(
                "{} only apply to raw recordings, and {} was processed already.",
                given.join(", "),
                netcdf.display()
            )
            .into());
        }
    }

    for input in expanded {
        if is_netcdf(&input) {
            records.push(Inspected::Processed(processed_info_record(&input)?));
            continue;
        }
        let resolved = formats::resolve_input(&input)?;
        let (meta, location) = load_meta_and_location(
            &resolved,
            params.medium_velocity,
            params.crs.as_ref(),
            params.cor_path.as_ref(),
            params.dem_path.as_ref(),
            params.override_antenna_mhz,
            params.override_antenna_separation,
        )?;
        records.push(Inspected::Raw(info_record(&resolved, &meta, &location)?));
    }

    Ok(records)
}

pub fn build_processed_gpr(
    params: RunParams,
) -> Result<(GPR, PathBuf), Box<dyn std::error::Error>> {
    build_processed_gpr_with_progress(params, &mut |_, _, _| {})
}

/// [`build_processed_gpr`], calling `on_step(index, of, step)` before each
/// step runs, for a caller that reports progress other than by printing
/// (`ridal process-job`, #382).
pub fn build_processed_gpr_with_progress(
    params: RunParams,
    on_step: &mut dyn FnMut(usize, usize, &str),
) -> Result<(GPR, PathBuf), Box<dyn std::error::Error>> {
    let expanded = expand_input_paths(&params.filepaths)?;
    if expanded.is_empty() {
        return Err("No input files were provided.".into());
    }
    let mut resolved_inputs = expanded
        .iter()
        .map(|input| crate::formats::resolve_input(input))
        .collect::<Result<Vec<crate::formats::ResolvedInput>, String>>()?;
    if resolved_inputs.is_empty() {
        return Err("No input files were resolved.".into());
    }

    let output_path = derive_output_path(params.output_path.as_ref(), &resolved_inputs[0])?;
    // Hashed here, from the same resolution the readers use, so the recipe
    // names exactly the files that were read: the `--cor` override in
    // place of each recording's own, as `load_meta_and_location` does.
    let inputs = crate::recipe::Inputs {
        bundles: resolved_inputs
            .iter()
            .map(|resolved| crate::recipe::bundle_of(resolved, params.cor_path.as_deref()))
            .collect::<Result<Vec<_>, String>>()?,
        dem: params
            .dem_path
            .as_deref()
            .map(crate::recipe::hash_file)
            .transpose()?,
        antenna_mhz_override: params.override_antenna_mhz,
        antenna_separation_override: params.override_antenna_separation,
    };
    let first_resolved = resolved_inputs.remove(0);
    let (first_meta, first_location) = load_meta_and_location(
        &first_resolved,
        params.medium_velocity,
        params.crs.as_ref(),
        params.cor_path.as_ref(),
        params.dem_path.as_ref(),
        params.override_antenna_mhz,
        params.override_antenna_separation,
    )?;
    let mut gpr = GPR::from_meta_and_loc(first_location, first_meta).map_err(|e| {
        format!(
            "Error loading GPR data from {:?}: {:?}",
            first_resolved.data, e
        )
    })?;
    gpr.user_metadata = params.user_metadata.clone();
    gpr.inputs = Some(inputs);

    for resolved in resolved_inputs {
        let (meta, location) = load_meta_and_location(
            &resolved,
            params.medium_velocity,
            params.crs.as_ref(),
            params.cor_path.as_ref(),
            params.dem_path.as_ref(),
            params.override_antenna_mhz,
            params.override_antenna_separation,
        )?;
        let other = GPR::from_meta_and_loc(location, meta)
            .map_err(|e| format!("Error loading GPR data from {:?}: {:?}", resolved.data, e))?;
        if !params.quiet {
            println!(
                "Merging {:?} -> {:?}",
                resolved.header, first_resolved.header
            );
        }
        gpr.merge(&other)?;
    }

    let start_time = std::time::SystemTime::now();
    if !params.quiet {
        println!("Processing {:?}", gpr.metadata.data_filepath);
    }
    for (i, step) in params.steps.iter().enumerate() {
        on_step(i, params.steps.len(), step);
        if !params.quiet {
            println!(
                "{}/{}, t+{:.2} s, Running step {}.",
                i + 1,
                params.steps.len(),
                std::time::SystemTime::now()
                    .duration_since(start_time)
                    .unwrap()
                    .as_secs_f32(),
                step,
            );
        }
        gpr.process(step)
            .map_err(|e| format!("Error on step {}: {:?}", step, e))?;
    }

    // Resolved once, here, from CLI arguments and the output stem. There is
    // no "inherited" tier yet because Ridal cannot currently read its own
    // processed NetCDF outputs back in as input (#123 adds metadata-only
    // inspection; full reading into GPR is explicitly future work there).
    // Passing `None` for `inherited` keeps `identity::resolve_*` already
    // shaped for that follow-up rather than requiring a redesign then.
    let output_stem = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("Output path has no valid file stem: {output_path:?}"))?;
    let (radargram_id, id_note) =
        crate::identity::resolve_radargram_id(params.radargram_id.as_deref(), None, output_stem)?;
    if let Some(note) = id_note {
        if !params.quiet {
            println!("{note}");
        }
    }
    let display_name = crate::identity::resolve_display_name(params.display_name.as_deref(), None);
    let group = crate::identity::resolve_group(
        params.group.as_deref(),
        params.group_id.as_deref(),
        None,
        None,
    )?;
    let (group_name, group_id) = match group {
        Some((name, id)) => (Some(name), Some(id)),
        None => (None, None),
    };
    gpr.identity = RidalIdentity {
        radargram_id: Some(radargram_id),
        display_name,
        group_name,
        group_id,
    };

    Ok((gpr, output_path))
}

pub fn run(params: RunParams) -> Result<ProcessResult, Box<dyn std::error::Error>> {
    let (gpr, output_path) = build_processed_gpr(params.clone())?;

    if !params.no_export {
        if !params.quiet {
            println!("Exporting to {:?}", output_path);
        }
        gpr.export(&output_path)
            .map_err(|e| format!("Error exporting data: {:?}", e))?;
    }

    if let Some(potential_fp) = &params.render_path {
        let render_filepath = match potential_fp {
            Some(fp) => {
                if fp.is_dir() {
                    fp.join(output_path.file_stem().unwrap())
                        .with_extension("jpg")
                } else {
                    fp.clone()
                }
            }
            None => output_path.with_extension("jpg"),
        };
        let profile = crate::render::profile::RenderProfile::resolve(
            params.render_profile.as_deref().unwrap_or("default"),
        )?;
        if !params.quiet {
            println!(
                "Rendering image to {:?} with the '{}' profile",
                render_filepath, profile.name
            );
        }
        gpr.render(
            &render_filepath,
            &profile,
            params.render_width,
            params.render_topo,
        )
        .map_err(|e| format!("Error writing image: {:?}", e))?;
    }

    if let Some(potential_track_path) = &params.track_path {
        crate::io::export_locations(
            &gpr.location,
            potential_track_path.into(),
            &output_path,
            !params.quiet,
        )?;
    }

    Ok(ProcessResult { output_path })
}

pub fn run_batch(params: BatchRunParams) -> Result<BatchProcessResult, String> {
    if !params.output_dir.is_dir() {
        return Err(format!(
            "output_dir must be an existing directory: {}",
            params.output_dir.display()
        ));
    }
    if let Some(render_dir) = &params.render_dir {
        if !render_dir.is_dir() {
            return Err(format!(
                "render_dir must be an existing directory: {}",
                render_dir.display()
            ));
        }
    }
    if let Some(track_dir) = &params.track_dir {
        if !track_dir.is_dir() {
            return Err(format!(
                "track_dir must be an existing directory: {}",
                track_dir.display()
            ));
        }
    }

    let plan = plan_batch(&params)?;
    let mut output_paths = Vec::<PathBuf>::new();

    for planned in plan.outputs {
        let group = planned.inputs;
        let output_nc = planned.output;
        let radargram_id = Some(planned.radargram_id);
        let display_name = planned.display_name;
        let stem = output_nc
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Could not derive output stem from {}", output_nc.display()))?
            .to_string();

        let render_path = params
            .render_dir
            .as_ref()
            .map(|dir| Some(dir.join(format!("{stem}.jpg"))));

        let track_path = params
            .track_dir
            .as_ref()
            .map(|dir| Some(dir.join(format!("{stem}_track.csv"))));

        let run_params = RunParams {
            filepaths: group.clone(),
            output_path: Some(output_nc.clone()),
            dem_path: params.dem_path.clone(),
            cor_path: params.cor_path.clone(),
            medium_velocity: params.medium_velocity,
            crs: params.crs.clone(),
            quiet: params.quiet,
            track_path,
            steps: params.steps.clone(),
            no_export: params.no_export,
            render_path,
            render_profile: params.render_profile.clone(),
            render_width: params.render_width,
            render_topo: params.render_topo,
            override_antenna_mhz: params.override_antenna_mhz,
            override_antenna_separation: params.override_antenna_separation,
            user_metadata: params.user_metadata.clone(),
            // Always given, from the plan: without a pattern it is the id
            // process mode would fall back to, and giving it here keeps that
            // mode's advice to pass --radargram-id, which batch mode does not
            // have, from being printed once per output (#368).
            radargram_id,
            display_name,
            group: params.group.clone(),
            group_id: params.group_id.clone(),
        };

        // process-mode semantics: one group -> one output, in the order we pass here.
        // For merge groups, group_batch_inputs_chronologically() already ordered them chronologically.
        let result = run(run_params).map_err(|e| e.to_string())?;
        output_paths.push(result.output_path);
    }

    Ok(BatchProcessResult { output_paths })
}

/// Why an output of a merging batch starts a new radargram instead of
/// continuing the one before it, in time order (#382 phase 2).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct Split {
    /// From the end of the previous output to the start of this one.
    pub gap_seconds: f64,
    /// Whether the gap alone is more than `--merge` allows.
    pub over_threshold: bool,
    /// Why the two cannot be one radargram, when they cannot: the first
    /// thing `GPR::merge` refused.
    #[cfg_attr(feature = "server", schema(required = true))]
    pub incompatible: Option<String>,
    /// Whether a person asked for a new radargram here, in the ingest
    /// planner (#382); never in `batch-process`.
    pub manual: bool,
}

/// One output of a batch, as it would be written.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlannedOutput {
    pub radargram_id: String,
    pub display_name: Option<String>,
    /// The `.nc` file it is written to.
    pub output: PathBuf,
    /// In the order they are merged.
    pub inputs: Vec<PathBuf>,
    /// Why this output is not part of the one before it; `None` for the
    /// first output, and for every output of a batch that does not merge.
    pub split: Option<Split>,
}

/// What a batch would do, before it does any of it: which inputs make which
/// output, under which id, and why the inputs were split where they were.
///
/// `run_batch` processes exactly this, and `batch-process --dry-run` prints
/// it, so the two cannot disagree. Every id is checked here, so a plan that
/// exists is one that can run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BatchPlan {
    /// `--merge`, in seconds, or `None` when the batch does not merge.
    pub merge_threshold_seconds: Option<f64>,
    pub outputs: Vec<PlannedOutput>,
}

pub fn plan_batch(params: &BatchRunParams) -> Result<BatchPlan, String> {
    let expanded_filepaths = expand_cli_inputs(&params.filepaths)?;
    let threshold = params
        .merge
        .as_deref()
        .map(|merge| {
            parse_duration::parse(merge)
                .map(|d| d.as_secs_f64())
                .map_err(|e| format!("Failed to parse merge duration '{merge}': {e}"))
        })
        .transpose()?;
    if params.display_name_pattern.is_some() && params.id_pattern.is_none() {
        return Err(
            "--display-name-pattern needs --id-pattern too: each output's display name \
             is rendered beside its id"
                .to_string(),
        );
    }

    // Merging orders the inputs by time, and a pattern renders from what
    // the headers say: either way each header and its positions are read,
    // once, and the data is not.
    let read = (threshold.is_some() || params.id_pattern.is_some())
        .then(|| {
            expanded_filepaths
                .iter()
                .map(|path| read_for_grouping(path, params))
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;

    let groups: Vec<(Vec<usize>, Option<Split>)> = match (threshold, &read) {
        (Some(threshold), Some(read)) => {
            group_chronologically(read, Some(threshold), &GroupingEdits::default())
                .groups
                .into_iter()
                .map(|group| (group.members, group.split))
                .collect()
        }
        _ => (0..expanded_filepaths.len())
            .map(|i| (vec![i], None))
            .collect(),
    };
    let names = match &read {
        Some(read) => batch_names(params, &groups, &expanded_filepaths, read)?,
        None => None,
    };

    let mut outputs = Vec::with_capacity(groups.len());
    let mut written = std::collections::HashMap::<PathBuf, PathBuf>::new();
    for (index, (members, split)) in groups.into_iter().enumerate() {
        let group: Vec<PathBuf> = members
            .iter()
            .map(|i| expanded_filepaths[*i].clone())
            .collect();
        let first = &group[0];
        let file_stem = first
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Could not derive output stem from {}", first.display()))?;
        let (radargram_id, display_name, stem) = match &names {
            Some((ids, display_names)) => (
                ids[index].to_string(),
                display_names
                    .as_ref()
                    .and_then(|names| names[index].as_ref())
                    .map(|name| name.as_str().to_string()),
                ids[index].to_string(),
            ),
            None => (
                crate::identity::RadargramId::from_fallback(file_stem)?.to_string(),
                None,
                file_stem.to_string(),
            ),
        };
        let output = params.output_dir.join(format!("{stem}.nc"));
        // Two inputs with one file name in different directories used to
        // overwrite each other's output without a word.
        if let Some(other) = written.insert(output.clone(), first.clone()) {
            return Err(format!(
                "{} and {} would both be written to {}. Rename one, or name the outputs \
                 with --id-pattern.",
                other.display(),
                first.display(),
                output.display()
            ));
        }
        outputs.push(PlannedOutput {
            radargram_id,
            display_name,
            output,
            inputs: group,
            split,
        });
    }
    Ok(BatchPlan {
        merge_threshold_seconds: threshold,
        outputs,
    })
}

/// A raw recording's header and positions, without its data, with
/// positions in `crs` and elevations from `dem`: what the ingest planner
/// reads of each recording (#382 phase 7).
pub fn read_header_and_positions(
    resolved: &ResolvedInput,
    crs: Option<&String>,
    dem: Option<&PathBuf>,
    antenna_mhz: Option<f32>,
) -> Result<(GPRMeta, GPRLocation), HeaderReadError> {
    load_meta_and_location(resolved, 0.168, crs, None, dem, antenna_mhz, None).map_err(|e| {
        match e.downcast::<crate::io::UnknownAntennaFrequency>() {
            Ok(unknown) => HeaderReadError::UnknownFrequency(*unknown),
            Err(other) => HeaderReadError::Other(other.to_string()),
        }
    })
}

/// Why [`read_header_and_positions`] could not read a recording.
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderReadError {
    /// Readable but for its antenna frequency, which an override gives.
    UnknownFrequency(crate::io::UnknownAntennaFrequency),
    Other(String),
}

/// One input's header and positions, as planning a batch reads them.
fn read_for_grouping(path: &Path, params: &BatchRunParams) -> Result<GroupingRecording, String> {
    let resolved = formats::resolve_input(path)
        .map_err(|e| format!("Failed to resolve input '{}': {e}", path.display()))?;
    let (meta, location) = load_meta_and_location(
        &resolved,
        params.medium_velocity,
        params.crs.as_ref(),
        params.cor_path.as_ref(),
        None,
        params.override_antenna_mhz,
        params.override_antenna_separation,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    GroupingRecording::of(&meta, &location)
        .ok_or_else(|| format!("No location points found in '{}'", path.display()))
}

/// What a pattern is rendered from, for an output made of `members`, the
/// first of which is `first` and reads as `recording`.
pub fn output_facts(
    first: &Path,
    recording: &GroupingRecording,
    n_files: usize,
) -> crate::id_pattern::OutputFacts {
    crate::id_pattern::OutputFacts {
        label: first
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        stem: first
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        start_seconds: recording.start_seconds,
        antenna_mhz: recording.key.antenna_mhz,
        n_files,
    }
}

/// The ids and display names the patterns give each output of a batch, in
/// output order, or `None` without an id pattern (#368).
///
/// Rendered from each output's first input as `read` has it: the start time
/// orders `{n}` and gives `{date}`, and the antenna gives `{freq}`.
/// `from=next` continues after the ids of the processed files already in
/// the output directory.
#[allow(clippy::type_complexity)]
fn batch_names(
    params: &BatchRunParams,
    groups: &[(Vec<usize>, Option<Split>)],
    paths: &[PathBuf],
    read: &[GroupingRecording],
) -> Result<
    Option<(
        Vec<crate::identity::RadargramId>,
        Option<Vec<Option<crate::identity::DisplayName>>>,
    )>,
    String,
> {
    use crate::id_pattern::{BatchFacts, Pattern, PatternKind};

    let parse = |text: &str, kind: PatternKind, flag: &str| {
        Pattern::parse(text, kind).map_err(|e| format!("--{flag}: {}", e.render(text)))
    };
    let Some(id_text) = params.id_pattern.as_deref() else {
        return Ok(None);
    };
    let id_pattern = parse(id_text, PatternKind::Id, "id-pattern")?;
    let display_pattern = params
        .display_name_pattern
        .as_deref()
        .map(|text| parse(text, PatternKind::DisplayName, "display-name-pattern"))
        .transpose()?;

    let group = crate::identity::resolve_group(
        params.group.as_deref(),
        params.group_id.as_deref(),
        None,
        None,
    )?;
    let batch = match group {
        Some((name, id)) => BatchFacts {
            group_id: Some(id),
            group_name: Some(name),
        },
        None => BatchFacts::default(),
    };

    let outputs: Vec<_> = groups
        .iter()
        .map(|(members, _)| output_facts(&paths[members[0]], &read[members[0]], members.len()))
        .collect();

    let existing = if id_pattern.needs_existing_ids() {
        existing_radargram_ids(&params.output_dir)
    } else {
        Vec::new()
    };
    let ids = crate::id_pattern::render_ids(&id_pattern, &outputs, &batch, &existing)?;
    let display_names = display_pattern
        .map(|pattern| {
            crate::id_pattern::render_display_names(
                &pattern,
                &id_pattern,
                &outputs,
                &batch,
                &ids,
                &existing,
            )
        })
        .transpose()?;
    Ok(Some((ids, display_names)))
}

/// The radargram ids of the processed files directly in `dir`. A file that
/// is not one is skipped: this only finds where a counter left off.
fn existing_radargram_ids(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_netcdf(path))
        .filter_map(|path| match crate::io::inspect_ridal_netcdf(&path) {
            Ok(crate::io::RidalNetcdfKind::Supported(meta)) => Some(meta.radargram_id.to_string()),
            _ => None,
        })
        .collect()
}

fn expand_cli_inputs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::<PathBuf>::new();

    for input in inputs {
        let text = input.to_string_lossy();
        let looks_like_glob = text.contains('*') || text.contains('?') || text.contains('[');

        if looks_like_glob {
            let mut matched_any = false;
            let pattern =
                glob::glob(&text).map_err(|e| format!("Invalid glob pattern '{}': {e}", text))?;
            for entry in pattern {
                let path = entry.map_err(|e| format!("Error expanding glob '{}': {e}", text))?;
                matched_any = true;
                out.push(path);
            }
            if !matched_any {
                return Err(format!("Glob did not match any files: {text}"));
            }
        } else {
            out.push(input.clone());
        }
    }

    if out.is_empty() {
        return Err("No input files were provided".to_string());
    }

    Ok(out)
}

/// What decides whether two recordings can be one radargram, read from
/// their headers and positions: the checks [`GPR::merge`] makes before it
/// touches the data, which it makes through this, so the two cannot drift.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeKey {
    pub crs: String,
    pub antenna_mhz: f32,
    pub time_window: f32,
    pub antenna_separation: f32,
    pub medium_velocity: f32,
}

impl MergeKey {
    pub fn of(meta: &GPRMeta, location: &GPRLocation) -> Self {
        Self {
            crs: location.crs.clone(),
            antenna_mhz: meta.antenna_mhz,
            time_window: meta.time_window,
            antenna_separation: meta.antenna_separation,
            medium_velocity: meta.medium_velocity,
        }
    }

    /// Why a recording with `other` cannot follow one with `self`, or
    /// `None` when it can.
    pub fn refusal(&self, other: &MergeKey) -> Option<String> {
        if self.crs != other.crs {
            Some(format!("CRS are different: {} vs {}", self.crs, other.crs))
        } else if self.antenna_mhz != other.antenna_mhz {
            Some(format!(
                "Antenna frequencies are different: {} vs {}",
                self.antenna_mhz, other.antenna_mhz
            ))
        } else if self.time_window != other.time_window {
            Some(format!(
                "Time windows are different: {} vs {}",
                self.time_window, other.time_window
            ))
        // The checks below do not stop the arrays concatenating; they stop
        // the result declaring one value for the whole file that is true of
        // only part of it (#151).
        } else if self.antenna_separation != other.antenna_separation {
            Some(format!(
                "Antenna separations are different: {} m vs {} m",
                self.antenna_separation, other.antenna_separation
            ))
        } else if self.medium_velocity != other.medium_velocity {
            Some(format!(
                "Medium velocities are different: {} m/ns vs {} m/ns",
                self.medium_velocity, other.medium_velocity
            ))
        } else {
            None
        }
    }
}

/// One recording as grouping sees it: when it ran, and what decides what
/// it can be merged with. Read from the header and positions, not the data,
/// so a few hundred recordings are cheap (#382).
#[derive(Debug, Clone, PartialEq)]
pub struct GroupingRecording {
    /// The first and last position's time, seconds since the Unix epoch.
    pub start_seconds: f64,
    pub stop_seconds: f64,
    pub key: MergeKey,
}

impl GroupingRecording {
    /// `None` when the recording has no positions to give it a time.
    pub fn of(meta: &GPRMeta, location: &GPRLocation) -> Option<Self> {
        let times = location.cor_points.iter().map(|p| p.time_seconds);
        let start_seconds = times.clone().fold(f64::INFINITY, f64::min);
        let stop_seconds = times.fold(f64::NEG_INFINITY, f64::max);
        start_seconds.is_finite().then(|| Self {
            start_seconds,
            stop_seconds,
            key: MergeKey::of(meta, location),
        })
    }
}

/// A person's changes to where a merging batch splits, by index into the
/// recordings grouped (#382 phase 7).
#[derive(Debug, Clone, Default)]
pub struct GroupingEdits {
    /// Recordings to keep with the one before them in time, whatever the
    /// gap. A join the headers refuse is not made, and is reported.
    pub joins: HashSet<usize>,
    /// Recordings that start a new output although they would merge. A
    /// recording in both is split.
    pub splits: HashSet<usize>,
}

/// One output: its recordings in merge order, and why it is not part of
/// the output before it.
#[derive(Debug, Clone, PartialEq)]
pub struct Grouped {
    pub members: Vec<usize>,
    pub split: Option<Split>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Grouping {
    /// In time order.
    pub groups: Vec<Grouped>,
    /// Each join that was asked for and refused, with why.
    pub refused_joins: Vec<(usize, String)>,
}

/// Group `recordings` into outputs: in time order, a recording continues
/// the output before it when the gap is at most `threshold` (never, with
/// `None`) and the two can be merged, unless `edits` say otherwise.
pub fn group_chronologically(
    recordings: &[GroupingRecording],
    threshold: Option<f64>,
    edits: &GroupingEdits,
) -> Grouping {
    let mut order: Vec<usize> = (0..recordings.len()).collect();
    order.sort_by(|a, b| {
        recordings[*a]
            .start_seconds
            .total_cmp(&recordings[*b].start_seconds)
            .then(a.cmp(b))
    });

    let mut groups = Vec::<Grouped>::new();
    let mut refused_joins = Vec::new();
    // The current output's key and the end of its last recording.
    let mut current: Option<(&MergeKey, f64)> = None;
    for index in order {
        let recording = &recordings[index];
        let Some((key, stop)) = current.as_mut() else {
            groups.push(Grouped {
                members: vec![index],
                split: None,
            });
            current = Some((&recording.key, recording.stop_seconds));
            continue;
        };
        let gap = (recording.start_seconds - *stop).abs();
        // Asked whatever the gap, so that a split can say both.
        let incompatible = key.refusal(&recording.key);
        let over_threshold = threshold.is_none_or(|t| gap > t);
        let manual = edits.splits.contains(&index);
        let joined = !manual && edits.joins.contains(&index);
        if joined {
            if let Some(reason) = &incompatible {
                refused_joins.push((index, reason.clone()));
            }
        }
        let continues = incompatible.is_none() && !manual && (joined || !over_threshold);
        if continues {
            groups
                .last_mut()
                .expect("current is set")
                .members
                .push(index);
            *stop = stop.max(recording.stop_seconds);
        } else {
            groups.push(Grouped {
                members: vec![index],
                split: Some(Split {
                    gap_seconds: gap,
                    over_threshold,
                    incompatible,
                    manual,
                }),
            });
            current = Some((&recording.key, recording.stop_seconds));
        }
    }
    Grouping {
        groups,
        refused_joins,
    }
}

pub fn all_available_steps() -> Vec<(String, String)> {
    crate::steps::descriptions()
}

/// Check that every entry is exactly one registered step with valid
/// arguments.
pub fn validate_steps(steps: &[String]) -> Result<(), String> {
    for step in steps {
        crate::steps::parse_one(step)?;
    }
    Ok(())
}

pub fn default_processing_profile() -> Vec<String> {
    vec![
        "remove_empty_traces".to_string(),
        "zero_corr".to_string(),
        "correct_antenna_separation".to_string(),
        "dewow".to_string(),
        "background_removal".to_string(),
        format!("auto_gain({})", DEFAULT_AUTOGAIN_N_BINS),
    ]
}

#[cfg(test)]
pub mod tests {
    use std::{path::PathBuf, str::FromStr};

    use ndarray::{Array1, Axis, Slice};

    use super::{CorPoint, GPRLocation, LocationCorrection};

    fn make_cor_points(n_points: usize, spacing: f64) -> Vec<CorPoint> {
        let eastings = Array1::range(0_f64, n_points as f64 * spacing, spacing);

        let northings = Array1::<f64>::zeros(n_points);

        let altitudes = eastings.clone();
        let seconds = eastings.clone();

        let trace_n = eastings.mapv(|v| v as u32);

        (0..n_points)
            .map(|i| CorPoint {
                trace_n: trace_n[i],
                time_seconds: seconds[i],
                easting: eastings[i],
                northing: northings[i],
                altitude: altitudes[i],
            })
            .collect::<Vec<CorPoint>>()
    }

    fn make_gpr_location(
        n_points: usize,
        spacing: Option<f64>,
        crs: Option<String>,
        correction: Option<LocationCorrection>,
    ) -> GPRLocation {
        let cor_points = make_cor_points(n_points, spacing.unwrap_or(1.));
        let crs = match crs {
            Some(c) => c,
            None => {
                let first_coord = crate::coords::Coord {
                    x: cor_points[0].easting,
                    y: cor_points[0].northing,
                };
                crate::coords::UtmCrs::optimal_crs(&first_coord).to_epsg_str()
            }
        };
        GPRLocation {
            cor_points,
            correction: correction.unwrap_or(LocationCorrection::None),
            crs,
        }
    }

    pub fn make_dummy_gpr(n_traces: usize, n_samples: usize, spacing: Option<f64>) -> super::GPR {
        let gpr_location = make_gpr_location(n_traces, spacing, None, None);
        let metadata = super::GPRMeta {
            samples: n_samples as u32,
            frequency: 5000.,
            frequency_steps: 0,
            time_interval: 0.2,
            antenna: "500MHz".to_string(),
            antenna_mhz: 500.,
            antenna_separation: 1.,
            time_window: 2000.,
            last_trace: n_traces as u32,
            data_filepath: std::path::PathBuf::from_str("filepath.rd3").unwrap(),
            medium_velocity: 0.167,
        };

        let mut data = ndarray::Array2::<f32>::zeros((n_samples, n_traces));
        let new_row = ndarray::Array1::<f32>::range(0., n_traces as f32, 1.);
        for mut row in data.rows_mut() {
            row.assign(&new_row);
        }

        super::GPR {
            location: gpr_location,
            metadata,
            data,
            topo_data: None,
            steps: Vec::new(),
            crop_ns: vec![0.; n_traces],
            time_zero_ns: vec![0.; n_traces],
            antenna_separation_effective: 1.,
            log: Vec::new(),
            user_metadata: crate::user_metadata::UserMetadata::new(),
            // A real radargram_id is required for export() to succeed
            // (#116); test helpers give it one directly rather than routing
            // through build_processed_gpr()'s CLI-argument resolution.
            identity: super::RidalIdentity {
                radargram_id: Some(crate::identity::RadargramId::new("test-radargram").unwrap()),
                display_name: None,
                group_name: None,
                group_id: None,
            },
            inputs: None,
        }
    }

    #[test]
    fn test_gpr_location() {
        let mut gpr_location = make_gpr_location(10, None, None, None);
        // The first time+coordinate should be all zero
        assert_eq!(gpr_location.time_and_coord_at_trace(0), (0., 0., 0., 0.));

        // The second time+coordinate should be all one except for the northing
        assert_eq!(gpr_location.time_and_coord_at_trace(1), (1., 1., 0., 1.));
        // If the second is removed, it should still be linearly interpolated correctly
        gpr_location.cor_points.remove(1);
        // Check that interpolation works expectedly
        assert_eq!(gpr_location.time_and_coord_at_trace(1), (1., 1., 0., 1.));

        assert_eq!(
            gpr_location.time_and_coord_at_trace(100),
            gpr_location.time_and_coord_at_trace(gpr_location.cor_points.last().unwrap().trace_n)
        );

        gpr_location = gpr_location.range_fill(0, 10, 1.);

        // Check that the velocities are consistent along the track
        // The first and second velocities will still be a bit weird
        let velocities = gpr_location.velocities();
        assert_eq!(
            Some(velocities[2]),
            velocities
                .slice_axis(Axis(0), Slice::new(1, None, 1))
                .mean()
        );

        let distances = gpr_location.distances();
        assert_eq!(distances[0], 0.);
        assert_eq!(distances[9], 9.);
    }

    /// Fixes at traces 10, 20, ..., 50, one second apart (0.1 s per trace),
    /// filled out to traces 0..60 (#350).
    fn filled_with_header_interval(header: f32) -> GPRLocation {
        let cor_points = (1..=5_u32)
            .map(|i| CorPoint {
                trace_n: 10 * i,
                time_seconds: 100. + i as f64,
                easting: i as f64,
                northing: 0.,
                altitude: 0.,
            })
            .collect();
        GPRLocation {
            cor_points,
            correction: LocationCorrection::None,
            crs: "EPSG:32633".to_string(),
        }
        .range_fill(0, 60, header)
    }

    #[test]
    fn times_are_extrapolated_past_the_fixes_but_positions_are_held() {
        let filled = filled_with_header_interval(0.1);
        let times: Vec<f64> = filled.cor_points.iter().map(|p| p.time_seconds).collect();
        assert!(times.windows(2).all(|w| w[1] > w[0]), "{times:?}");
        assert!((times[0] - 100.).abs() < 1e-6, "{}", times[0]);
        assert!((times[59] - 105.9).abs() < 1e-6, "{}", times[59]);
        // Where the radar was outside the fixes is unknown, so it stays put.
        assert_eq!(filled.cor_points[0].easting, 1.);
        assert_eq!(filled.cor_points[59].easting, 5.);
        // Inside the fixes nothing changes. (The tolerance is for the header's
        // interval, which is an f32.)
        assert!((filled.cor_points[25].time_seconds - 102.5).abs() < 1e-6);
    }

    #[test]
    fn a_header_interval_the_fixes_contradict_is_not_used() {
        // A distance-triggered radargram's header interval means nothing:
        // the fixes say 0.1 s per trace, so 5 s is ignored for them.
        let filled = filled_with_header_interval(5.);
        assert!((filled.cor_points[59].time_seconds - 105.9).abs() < 1e-6);
        // And with no header interval at all, the fixes set the rate.
        let filled = filled_with_header_interval(0.);
        assert!((filled.cor_points[59].time_seconds - 105.9).abs() < 1e-6);
    }

    #[test]
    fn a_single_fix_without_a_header_interval_holds_its_time() {
        let location = GPRLocation {
            cor_points: vec![CorPoint {
                trace_n: 3,
                time_seconds: 7.,
                easting: 0.,
                northing: 0.,
                altitude: 0.,
            }],
            correction: LocationCorrection::None,
            crs: "EPSG:32633".to_string(),
        };
        let held = location.range_fill(0, 6, 0.);
        assert!(held.cor_points.iter().all(|p| p.time_seconds == 7.));
        let extrapolated = location.range_fill(0, 6, 0.5);
        assert_eq!(extrapolated.cor_points[0].time_seconds, 5.5);
        assert_eq!(extrapolated.cor_points[5].time_seconds, 8.);
    }

    /// The radargram #350 was found on: its `.cor` ends five traces before
    /// the data does, and those traces all had the last fix's time.
    #[test]
    fn dronbreen_traces_after_the_last_fix_keep_advancing() {
        let rad = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/mala/dronbreen-20250327-DAT_0066_A1.rad");
        let meta = crate::io::load_rad(&rad, 0.168, None, None).unwrap();
        let location = meta.find_cor(Some(&"EPSG:32633".to_string())).unwrap();
        let gpr = super::GPR::from_meta_and_loc(location, meta).unwrap();
        let times: Vec<f64> = gpr
            .location
            .cor_points
            .iter()
            .map(|p| p.time_seconds)
            .collect();
        assert_eq!(times.len(), 3548);
        let tail = &times[3540..];
        assert!(tail.windows(2).all(|w| w[1] > w[0]), "{tail:?}");
        // The anchor now reaches the last trace, so a pick there survives a
        // carry.
        let axis = crate::interp::anchors::trace_time_axis(&times).unwrap();
        let last = axis.points.unwrap().last().unwrap().trace;
        assert_eq!(last, 3547., "the trace_time anchor ends at trace {last}");
    }

    #[test]
    fn merge_refuses_what_the_merged_file_could_not_state_once() {
        let base = || make_dummy_gpr(20, 10, Some(1.));

        let mut other = base();
        other.metadata.antenna_separation = 4.;
        let message = base().merge(&other).unwrap_err();
        assert!(
            message.contains("Antenna separations are different"),
            "{message}"
        );

        // Corrected on one side only: recoverable, and the message says how.
        let mut other = base();
        other.antenna_separation_effective = 0.;
        let message = base().merge(&other).unwrap_err();
        assert!(message.contains("correct_antenna_separation"), "{message}");

        let mut other = base();
        other.metadata.medium_velocity = 0.1;
        let message = base().merge(&other).unwrap_err();
        assert!(
            message.contains("Medium velocities are different"),
            "{message}"
        );

        // Differently cropped inputs are fine: the offsets are per trace.
        let mut other = base();
        other.crop_ns = vec![3.; other.width()];
        let mut merged = base();
        merged.merge(&other).unwrap();
        assert_eq!(merged.crop_ns.len(), 40);
    }

    /// `distances` and `velocities` must take the square root of the summed
    /// squared components.
    ///
    /// The unit-spacing fixture used by `test_gpr_location` cannot detect a
    /// missing `sqrt`, because a step of length one is its own square. This
    /// uses a 3-4-5 step, where the true per-step length (5) and the squared
    /// length (25) differ, so only a correct implementation passes.
    #[test]
    fn test_distances_and_velocities_are_lengths_not_squared_lengths() {
        // Steps of (3, 4) in (easting, northing) -- length exactly 5 -- with
        // altitude held constant so the 3D step `velocities` measures is the
        // same 5, and one second per step so speed equals step length.
        let cor_points: Vec<CorPoint> = (0..5_u32)
            .map(|i| CorPoint {
                trace_n: i,
                time_seconds: i as f64,
                easting: 3. * i as f64,
                northing: 4. * i as f64,
                altitude: 0.,
            })
            .collect();
        let location = GPRLocation {
            cor_points,
            correction: LocationCorrection::None,
            crs: "EPSG:32633".to_string(),
        };

        let distances = location.distances();
        assert_eq!(distances[0], 0.);
        // Cumulative length, not a cumulative sum of squares (which would
        // give 25, 50, 75, 100).
        assert_eq!(distances.to_vec(), vec![0., 5., 10., 15., 20.]);
        assert_eq!(location.length(), 20.);

        // The first entry is 0 by construction (no preceding point); every
        // later step covers 5 m in 1 s.
        let velocities = location.velocities();
        assert_eq!(velocities.to_vec(), vec![0., 5., 5., 5., 5.]);
    }

    #[test]
    fn test_gpr_location_duration_since() {
        let gpr_location0 = make_gpr_location(10, Some(1.), None, None);

        let mut gpr_location1 = gpr_location0.clone();

        for point in gpr_location1.cor_points.iter_mut() {
            point.time_seconds += 10.;
        }

        // gpr_location0 stops at 9s. gpr_location1 starts at 10s
        // So the difference should be +1s for gpr_location0-gpr_location1
        // And -1s for gpr_location1-gpr_location0

        assert_eq!(gpr_location0.duration_since(&gpr_location1), 1.);
        assert_eq!(gpr_location1.duration_since(&gpr_location0), -1.);

        for point in gpr_location1.cor_points.iter_mut() {
            point.time_seconds -= 30.;
        }
        assert_eq!(gpr_location0.duration_since(&gpr_location1), -11.);
        assert_eq!(gpr_location1.duration_since(&gpr_location0), 11.);
    }

    fn make_test_metadata(width: Option<usize>, height: Option<usize>) -> super::GPRMeta {
        super::GPRMeta {
            samples: height.unwrap_or(1024) as u32,
            frequency: 8000.,
            frequency_steps: 1,
            time_interval: 1000.,
            antenna: "800MHz".into(),
            antenna_mhz: 800.,
            antenna_separation: 2.,
            time_window: 500.,
            last_trace: width.unwrap_or(2048) as u32,
            data_filepath: PathBuf::new(),
            medium_velocity: 0.168,
        }
    }

    fn make_test_gpr(width: Option<usize>, height: Option<usize>) -> super::GPR {
        let width = width.unwrap_or(2024);
        let height = height.unwrap_or(1024);

        let gpr_location = make_gpr_location(width, Some(1.), None, None);

        let meta = make_test_metadata(Some(width), Some(height));

        let antenna_separation = meta.antenna_separation;

        let mut data = ndarray::Array2::<f32>::zeros((height, width));

        for mut col in data.columns_mut() {
            col.assign(&Array1::<f32>::linspace(0., (height - 1) as f32, height));
        }

        super::GPR {
            data,
            topo_data: None,
            location: gpr_location,
            metadata: meta,
            steps: Vec::new(),
            log: Vec::new(),
            antenna_separation_effective: antenna_separation,
            crop_ns: vec![0.; width],
            time_zero_ns: vec![0.; width],
            user_metadata: crate::user_metadata::UserMetadata::new(),
            identity: super::RidalIdentity::default(),
            inputs: None,
        }
    }

    #[test]
    fn render_topo_draws_the_corrected_view_from_memory() {
        // #289: `process --render-topo` shears the array in memory by the
        // elevation and depth it exports. The test track climbs a metre per
        // trace, so the corrected raster is taller than the standard one.
        let gpr = make_test_gpr(Some(50), Some(40));
        let dir = tempfile::tempdir().unwrap();
        let profile = crate::render::profile::RenderProfile::default_profile();
        let standard = dir.path().join("standard.png");
        let topo = dir.path().join("topo.png");

        gpr.render(&standard, &profile, None, false).unwrap();
        gpr.render(&topo, &profile, None, true).unwrap();

        let (standard_w, standard_h) = image::image_dimensions(&standard).unwrap();
        let (topo_w, topo_h) = image::image_dimensions(&topo).unwrap();
        assert_eq!((standard_w, standard_h), (50, 40));
        assert_eq!(topo_w, standard_w);
        assert!(
            topo_h > standard_h,
            "{topo_h} is not taller than {standard_h}"
        );
    }

    #[test]
    fn auto_gain_applies_no_gain_when_amplitude_grows() {
        let mut gpr = make_test_gpr(Some(5), Some(200));
        gpr.data = ndarray::Array2::from_shape_fn((200, 5), |(i, j)| {
            (i as f32 / 50.).exp() * if (i + j) % 2 == 0 { 1. } else { -1. }
        });
        let before = gpr.data.clone();
        gpr.process("auto_gain(20)").unwrap();
        assert_eq!(gpr.data, before);
        let entry = gpr.log.last().unwrap();
        assert!(entry.contains("grows with time"), "{entry}");
        assert!(entry.contains("applied no gain"), "{entry}");
    }

    #[test]
    fn auto_gain_skips_with_a_reason_instead_of_failing() {
        // Nothing to measure: the step still succeeds, applies no gain and
        // says why, so one bad file does not stop a batch.
        let mut gpr = make_test_gpr(Some(5), Some(200));
        gpr.data.fill(0.);
        gpr.process("auto_gain").unwrap();
        assert!(gpr.data.iter().all(|v| *v == 0.));
        let entry = gpr.log.last().unwrap();
        assert!(entry.contains("could not measure a gain"), "{entry}");
        assert!(entry.contains("applied no gain"), "{entry}");

        // A bad argument is still an error, at parse time.
        let err = gpr.process("auto_gain(1)").unwrap_err().to_string();
        assert!(err.contains("n_bins"), "{err}");
    }

    #[test]
    fn a_step_that_makes_nan_fails_at_that_step() {
        let mut gpr = make_test_gpr(Some(5), Some(200));
        let err = gpr.process("gain(nan)").unwrap_err().to_string();
        assert!(err.contains("`gain(nan)` made"), "{err}");
        assert!(err.contains("NaN or infinite"), "{err}");
        assert!(gpr.steps.is_empty(), "a failed step must not be recorded");
    }

    #[test]
    fn auto_gain_ignores_exact_zeros() {
        // A decaying signal with one trace that is exactly zero, as a median
        // background removal leaves the median trace.
        let mut gpr = make_test_gpr(Some(5), Some(200));
        gpr.data = ndarray::Array2::from_shape_fn((200, 5), |(i, j)| {
            if j == 2 {
                0.
            } else {
                (-(i as f32) / 50.).exp() * if i % 2 == 0 { 1. } else { -1. }
            }
        });
        gpr.auto_gain(10);
        assert!(gpr.data.iter().all(|v| v.is_finite()));
        // The gain undoes most of the decay.
        assert!(gpr.data[[199, 0]].abs() > 0.5 * gpr.data[[0, 0]].abs());
    }

    #[test]
    fn test_make_test_gpr() {
        let gpr = make_test_gpr(None, None);

        assert_eq!(gpr.data[[0, 0]], 0.);
        assert_eq!(
            gpr.data[[gpr.data.shape()[0] - 1, 0]],
            (gpr.data.shape()[0] - 1) as f32
        );
    }

    /// `make_test_gpr` plus the identity that `export_dataset` insists on,
    /// which `build_processed_gpr` would normally have resolved by now.
    fn make_exportable_gpr(width: usize, height: usize) -> super::GPR {
        let mut gpr = make_test_gpr(Some(width), Some(height));
        gpr.identity.radargram_id = Some(crate::identity::RadargramId::new("test_line").unwrap());
        gpr
    }

    /// A radargram with a detectable first break, which `make_test_gpr`'s
    /// linear ramp does not have: `zero_corr` looks for a jump larger than
    /// half the mean trace's standard deviation, and every step of a ramp
    /// is the same size, so it finds nothing and returns immediately.
    ///
    /// Silence, then the direct wave, then a weaker reflector further down
    /// so a second correction also has something to crop.
    fn make_gpr_with_first_break(width: usize, height: usize) -> super::GPR {
        let mut gpr = make_exportable_gpr(width, height);
        let direct = height / 10;
        let reflector = height / 2;
        gpr.data = ndarray::Array2::<f32>::from_shape_fn((height, width), |(row, _)| {
            if row == direct {
                100.
            } else if row == reflector {
                40.
            } else {
                0.
            }
        });
        gpr
    }

    fn f32_attr(ds: &crate::export::ExportDataset, name: &str) -> f32 {
        match ds.attrs.get(name) {
            Some(crate::export::ExportAttr::F32(v)) => *v,
            other => panic!("{name} should be an F32 attribute, got {other:?}"),
        }
    }

    fn str_attr(
        attrs: &std::collections::BTreeMap<String, crate::export::ExportAttr>,
        name: &str,
    ) -> String {
        match attrs.get(name) {
            Some(crate::export::ExportAttr::String(v)) => v.clone(),
            other => panic!("{name} should be a string attribute, got {other:?}"),
        }
    }

    /// Every attribute Ridal generates on its own must be ASCII (#256).
    ///
    /// A non-ASCII `NC_CHAR` attribute is flagged `H5T_CSET_ASCII` by HDF5,
    /// so readers that trust the flag -- xarray with the `h5netcdf` engine,
    /// `h5dump` -- garble the UTF-8 bytes. Ridal's own output must therefore
    /// never be the source of the problem. A user-supplied display or group
    /// name is the one deliberate exception, covered by the NetCDF
    /// round-trip test in `io.rs`.
    #[test]
    fn ridal_generated_attributes_are_ascii_by_default() {
        let gpr = make_exportable_gpr(16, 32);
        let ds = gpr.export_dataset().unwrap();
        for (name, value) in &ds.attrs {
            let text = match value {
                crate::export::ExportAttr::String(s) => s.clone(),
                crate::export::ExportAttr::Strings(v) => v.join("\n"),
                _ => continue,
            };
            assert!(
                text.is_ascii(),
                "generated attribute '{name}' is not ASCII: {text:?}"
            );
        }
    }

    #[test]
    fn the_export_distinguishes_acquired_separation_from_remaining_separation() {
        // Before the correction the two agree, and `twtt` is what the
        // antenna pair recorded.
        let mut gpr = make_exportable_gpr(64, 256);
        let acquired = gpr.metadata.antenna_separation;
        assert!(acquired > 0., "the fixture needs a real separation");

        let before = gpr.export_dataset().unwrap();
        assert_eq!(f32_attr(&before, "antenna_separation"), acquired);
        assert_eq!(f32_attr(&before, "antenna_separation_effective"), acquired);
        assert_eq!(
            str_attr(&before.coords["twtt"].attrs, "anchor_name"),
            "twtt"
        );

        gpr.correct_antenna_separation(
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        );
        let after = gpr.export_dataset().unwrap();

        // The acquisition fact survives -- it is still true that the survey
        // was flown with the antennas that far apart.
        assert_eq!(
            f32_attr(&after, "antenna_separation"),
            acquired,
            "acquisition provenance must not be rewritten by processing"
        );
        // What changed is how much of it the data still carries. Exporting
        // the acquisition value here is what would make a reader
        // double-correct.
        assert_eq!(f32_attr(&after, "antenna_separation_effective"), 0.);
        assert_eq!(
            str_attr(&after.coords["twtt"].attrs, "anchor_name"),
            "twtt_normal_incidence",
            "twtt now refers to a coincident antenna, not the pair"
        );
    }

    #[test]
    fn successive_crops_accumulate_into_one_crop() {
        let mut accumulating = make_gpr_with_first_break(64, 512);
        accumulating.zero_corr_legacy(1.);
        let first = accumulating.twtt_crop_uniform_ns().unwrap();
        assert!(first > 0., "the fixture must have a first break to find");

        // The same second crop, measured on a radargram that arrives at
        // the same data carrying no history.
        let mut in_isolation = make_gpr_with_first_break(64, 512);
        in_isolation.zero_corr_legacy(1.);
        in_isolation.crop_ns.fill(0.);
        in_isolation.time_zero_ns.fill(0.);
        in_isolation.zero_corr_legacy(1.);
        let second = in_isolation.twtt_crop_uniform_ns().unwrap();
        assert!(second > 0., "the second crop must remove something too");

        accumulating.zero_corr_legacy(1.);

        // Assigning instead of accumulating would report `second` alone and
        // describe the file as starting at the later offset — the same
        // silently shifted origin SPEC §7.5.1 exists to prevent, arrived at
        // from inside one pipeline rather than between two.
        let total = accumulating.twtt_crop_uniform_ns().unwrap();
        assert!(
            (total - (first + second)).abs() < 1e-4,
            "expected {first} + {second}, got {total}"
        );
    }

    #[test]
    fn a_margin_above_time_zero_has_negative_travel_times_and_depths() {
        let mut gpr = make_gpr_with_first_break(16, 512);
        let step = gpr.vertical_resolution_ns();
        gpr.process(&format!("zero_corr(max_peak, margin={})", 4. * step))
            .unwrap();

        let crop = gpr.twtt_crop_uniform_ns().unwrap();
        let time_zero = gpr.twtt_time_zero_uniform_ns().unwrap();
        assert!((time_zero - crop - 4. * step).abs() < 1e-3);
        // Row 4 is time zero, and the rows above it are before it.
        let twtt = gpr.twtt_ns();
        assert!((twtt[0] + 4. * step).abs() < 1e-3, "{}", twtt[0]);
        assert!(twtt[4].abs() < 1e-3, "{}", twtt[4]);
        let depths = gpr.depths();
        assert!(depths[0] < 0. && depths[3] < 0., "{depths:?}");
        assert!(depths.windows(2).into_iter().all(|w| w[1] >= w[0]));

        // And the file says the same.
        let exported = gpr.export_dataset().unwrap();
        match &exported.coords["twtt"].data {
            crate::export::ExportArray::F32Owned1D(values) => {
                assert!((values[0] + 4. * step).abs() < 1e-3, "{}", values[0])
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn subsetting_away_leading_samples_adds_to_the_crop() {
        // `subset(0 -1 <n> -1)` crops the front of every trace exactly as a
        // zero correction does, so it has to report the same way. Reporting
        // zero here would describe the file as starting at the original
        // time zero when it starts `n` samples later -- the silently
        // shifted origin SPEC §7.5.1 exists to prevent, reached by a
        // pipeline that never ran a zero correction at all.
        let gpr = make_gpr_with_first_break(16, 512);
        let step = gpr.metadata.time_window / gpr.height() as f32;
        assert_eq!(gpr.twtt_crop_uniform_ns().unwrap(), 0.);

        let cropped = gpr.subset(None, None, Some(40), None).unwrap();
        let expected = 40.0 * step;
        assert!(
            (cropped.twtt_crop_uniform_ns().unwrap() - expected).abs() < 1e-4,
            "expected {expected}, got {}",
            cropped.twtt_crop_uniform_ns().unwrap()
        );

        // And it accumulates with everything else, so two crops in one
        // pipeline still describe one origin.
        let twice = cropped.subset(None, None, Some(10), None).unwrap();
        let second_step = cropped.metadata.time_window / cropped.height() as f32;
        let total = expected + 10.0 * second_step;
        assert!(
            (twice.twtt_crop_uniform_ns().unwrap() - total).abs() < 1e-4,
            "expected {total}, got {}",
            twice.twtt_crop_uniform_ns().unwrap()
        );

        // Trimming the tail changes the window, not the origin.
        let tail = gpr.subset(None, None, None, Some(100)).unwrap();
        assert_eq!(tail.twtt_crop_uniform_ns().unwrap(), 0.);
    }

    #[test]
    fn cropping_further_moves_the_crop_and_leaves_time_zero_where_it_is() {
        // The reason there are two numbers. A zero correction locates time
        // zero and crops to it, so the two agree. Anything that crops
        // afterwards moves the record without moving the moment the pulse
        // left the antenna, and their difference becomes the travel time of
        // the new first sample -- which is precisely the gprinterp anchor
        // `t0` that neither of them is.
        let mut gpr = make_gpr_with_first_break(16, 512);
        gpr.zero_corr_legacy(1.);
        let at_zero = gpr.twtt_crop_uniform_ns().unwrap();
        assert!(at_zero > 0.);
        assert_eq!(
            gpr.twtt_time_zero_uniform_ns().unwrap(),
            at_zero,
            "the crop landed on time zero, so they agree"
        );

        let step = gpr.metadata.time_window / gpr.height() as f32;
        let cropped = gpr.subset(None, None, Some(40), None).unwrap();
        assert!(
            (cropped.twtt_crop_uniform_ns().unwrap() - (at_zero + 40.0 * step)).abs() < 1e-3,
            "the crop grew"
        );
        assert_eq!(
            cropped.twtt_time_zero_uniform_ns().unwrap(),
            at_zero,
            "time zero did not move: the pulse left when it left"
        );

        let anchor_t0 =
            cropped.twtt_crop_uniform_ns().unwrap() - cropped.twtt_time_zero_uniform_ns().unwrap();
        assert!(
            (anchor_t0 - 40.0 * step).abs() < 1e-3,
            "sample 0 is now 40 samples past time zero, got {anchor_t0}"
        );
    }

    #[test]
    fn a_radargram_nobody_has_zero_corrected_does_not_claim_to_know_time_zero() {
        // Zero is the honest answer, not a guess: its travel times really
        // are measured from whenever the instrument started sampling, and
        // the anchor `t0` that falls out -- crop minus zero -- is the crop,
        // which is right for exactly that reason.
        let gpr = make_gpr_with_first_break(16, 512);
        assert_eq!(gpr.twtt_time_zero_uniform_ns().unwrap(), 0.);
        assert_eq!(gpr.twtt_crop_uniform_ns().unwrap(), 0.);

        let cropped = gpr.subset(None, None, Some(40), None).unwrap();
        assert_eq!(cropped.twtt_time_zero_uniform_ns().unwrap(), 0.);
        assert!(cropped.twtt_crop_uniform_ns().unwrap() > 0.);
    }

    fn per_trace_max_peak(gpr: &mut super::GPR) {
        gpr.zero_corr(
            &super::zero_corr::Settings {
                method: super::zero_corr::Method::MaxPeak,
                scope: super::zero_corr::Scope::Trace,
                sigma: 5.,
                time_zero: super::zero_corr::Reference::Peak,
                margin: super::zero_corr::Margin::Ns(0.),
                window: 51,
            },
            1.,
        )
        .unwrap();
    }

    /// A radargram whose first break arrives at a different sample in each
    /// trace, which is the situation a per-trace zero correction exists for.
    fn make_gpr_with_a_wandering_first_break(width: usize, height: usize) -> super::GPR {
        let mut gpr = make_exportable_gpr(width, height);
        let reflector = height / 2;
        gpr.data = ndarray::Array2::<f32>::from_shape_fn((height, width), |(row, col)| {
            // The direct wave drifts by up to a dozen samples across the
            // profile, as it does when the antenna leaves the ground.
            let direct = height / 10 + col % 13;
            if row == direct {
                100.
            } else if row == reflector {
                40.
            } else {
                0.
            }
        });
        gpr
    }

    #[test]
    fn a_per_trace_zero_correction_records_a_per_trace_offset() {
        // The bug this replaced: a per-trace zero correction crops each trace by a
        // different amount and recorded the mean, which is an offset no
        // trace actually received. It is not a rounding error -- it is
        // wrong by the spread of the first breaks, on the axis whose origin
        // re-anchoring depends on.
        let mut gpr = make_gpr_with_a_wandering_first_break(64, 512);
        per_trace_max_peak(&mut gpr);

        let offsets = gpr.twtt_crop_ns();
        assert_eq!(offsets.len(), gpr.width(), "one per trace");
        let min = offsets.iter().cloned().fold(f32::INFINITY, f32::min);
        let max = offsets.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max > min,
            "the fixture's first break has to wander: {offsets:?}"
        );
        assert!(
            gpr.twtt_crop_uniform_ns().is_none(),
            "no single number describes these"
        );

        // A uniform crop still reports one number, which is the common case
        // and what keeps the ordinary file simple.
        let mut uniform = make_gpr_with_first_break(64, 512);
        uniform.zero_corr_legacy(1.);
        assert!(uniform.twtt_crop_uniform_ns().is_some());
    }

    #[test]
    fn the_per_trace_offset_follows_its_traces_through_every_reshaping() {
        // The invariant the whole change rests on: one value per trace,
        // maintained by everything that changes the trace count. A stale
        // vector would be worse than the mean it replaced, since it would
        // attribute one trace's crop to another.
        let mut gpr = make_gpr_with_a_wandering_first_break(64, 512);
        per_trace_max_peak(&mut gpr);
        let before = gpr.twtt_crop_ns().to_vec();

        let cropped = gpr.subset(Some(10), Some(40), None, None).unwrap();
        assert_eq!(cropped.twtt_crop_ns().len(), cropped.width());
        assert_eq!(
            cropped.twtt_crop_ns(),
            &before[10..40],
            "the traces it kept"
        );

        let mut averaged = make_gpr_with_a_wandering_first_break(64, 512);
        per_trace_max_peak(&mut averaged);
        averaged.average_traces(4).unwrap();
        assert_eq!(averaged.twtt_crop_ns().len(), averaged.width());

        let mut thinned = make_gpr_with_a_wandering_first_break(64, 512);
        per_trace_max_peak(&mut thinned);
        thinned.remove_traces(&[0, 1, 2], true).unwrap();
        assert_eq!(thinned.twtt_crop_ns().len(), thinned.width());
        assert_eq!(thinned.twtt_crop_ns()[0], before[3]);

        // Time zero carries the same invariant through the same operations,
        // and a length mismatch between the two would be a silent
        // misattribution rather than a panic.
        for gpr in [&cropped, &averaged, &thinned] {
            assert_eq!(
                gpr.twtt_time_zero_ns().len(),
                gpr.width(),
                "time zero must stay one per trace too"
            );
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn both_offsets_reach_the_file_shaped_by_whether_the_traces_agree() {
        // Written and read back as real NetCDF, because both shapes are new
        // to the writer: its `data_vars` loop took 2D arrays and `u8`
        // scalars and silently skipped everything else, so a dataset that
        // looks right proves nothing about the file.
        //
        // One test for both shapes because they are one property -- the
        // shape is the claim -- and because every extra NetCDF file in the
        // suite is another chance to meet #129.
        let dir = tempfile::tempdir().unwrap();

        let mut uniform = make_gpr_with_first_break(48, 512);
        uniform.zero_corr_legacy(1.);
        let uniform_expected = uniform.twtt_crop_uniform_ns().unwrap();
        assert!(uniform_expected > 0., "a crop of zero would prove nothing");
        let uniform_path = dir.path().join("uniform.nc");
        uniform.export(&uniform_path).unwrap();

        let mut wandering = make_gpr_with_a_wandering_first_break(48, 512);
        per_trace_max_peak(&mut wandering);
        let wandering_expected = wandering.twtt_crop_ns().to_vec();
        assert!(
            wandering.twtt_crop_uniform_ns().is_none(),
            "the fixture's first break has to wander"
        );
        let wandering_path = dir.path().join("wandering.nc");
        wandering.export(&wandering_path).unwrap();

        let file = netcdf::open(&uniform_path).unwrap();
        for name in ["twtt_crop", "twtt_time_zero"] {
            let var = file
                .variable(name)
                .unwrap_or_else(|| panic!("{name} should be written as a variable"));
            assert_eq!(
                var.dimensions().len(),
                0,
                "{name}: a scalar, not a length-1 array"
            );
            // The crop landed on time zero, so both hold it.
            let found = var.get_value::<f64, _>(()).unwrap();
            assert!(
                (found - uniform_expected as f64).abs() < 1e-6,
                "{name}: {found} != {uniform_expected}"
            );
            assert!(matches!(
                var.attribute("units").unwrap().value().unwrap(),
                netcdf::AttributeValue::Str(u) if u == "ns"
            ));
        }
        drop(file);

        let file = netcdf::open(&wandering_path).unwrap();
        for name in ["twtt_crop", "twtt_time_zero"] {
            let var = file.variable(name).unwrap();
            // `(x)` says "these differ per trace"; a scalar would say one
            // number is true of all of them.
            assert_eq!(var.dimensions().len(), 1, "{name}");
            assert_eq!(var.dimensions()[0].name(), "x", "{name}");
            let found = var.get_values::<f64, _>(..).unwrap();
            assert_eq!(found.len(), wandering_expected.len(), "{name}");
            for (found, expected) in found.iter().zip(wandering_expected.iter()) {
                assert!(
                    (found - *expected as f64).abs() < 1e-6,
                    "{name}: {found} != {expected}"
                );
            }
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_processed_radargram_can_describe_its_own_anchor_axes() {
        // The whole point of #144 read back by #146: an exported radargram
        // carries enough to build the `coordinates.axes` block that lets
        // picks drawn on it be carried onto a later revision.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.nc");

        let mut gpr = make_gpr_with_first_break(64, 512);
        gpr.zero_corr_legacy(1.);
        gpr.export(&path).unwrap();

        let declared = crate::interp::source::read_axis_declarations(&path).unwrap();
        assert!(
            declared.dt_ns > 0.0,
            "a sample interval from the axis itself"
        );
        assert_eq!(declared.twtt_anchor.as_deref(), Some("twtt"));

        let y = crate::interp::anchors::twtt_axis(
            declared.twtt_anchor.as_deref(),
            &declared.twtt_crop,
            &declared.twtt_time_zero,
            declared.dt_ns,
        )
        .expect("a zero-corrected radargram can say where its axis starts");
        // The crop landed on time zero, so sample 0 is travel time 0.
        assert_eq!(y.t0, Some(0.0));

        let x = crate::interp::anchors::trace_time_axis(&declared.time)
            .expect("the fixture's acquisition times advance");
        let points = x.points.as_ref().unwrap();
        assert!(!points.is_empty());
        assert_eq!(points[0].trace, 0.0);
        assert_eq!(
            points.last().unwrap().trace,
            (gpr.width() - 1) as f64,
            "the last trace is always a tiepoint, so nothing extrapolates"
        );

        assert!(crate::interp::anchors::Axes {
            x: Some(crate::interp::anchors::Axis { anchor: vec![x] }),
            y: Some(crate::interp::anchors::Axis { anchor: vec![y] })
        }
        .is_usable());
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_level_2_export_reads_back_what_the_radargram_declared() {
        // The writer and the reader, across a real file. Level 2 puts these
        // on every row so a point stays self-describing once several
        // radargrams' points are merged, and the value it writes has to be
        // the one this radargram actually declared.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.nc");

        let mut gpr = make_gpr_with_first_break(48, 512);
        gpr.correct_antenna_separation(
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        );
        gpr.export(&path).unwrap();

        let geometry = crate::interp::source::read_geometry(&path).unwrap();
        assert_eq!(geometry.antenna_separation_effective_m, Some(0.0));
        assert_eq!(
            geometry.twtt_anchor.as_deref(),
            Some("twtt_normal_incidence")
        );
    }

    #[test]
    fn a_radargram_acquired_at_zero_separation_still_declares_plain_twtt() {
        // Nothing was corrected -- `correct_antenna_separation` refuses to
        // run at all here -- so the axis is the recorded one. The two
        // quantities happen to coincide, but claiming the corrected name
        // would let an uncorrected file be re-anchored onto a corrected
        // one, and this is the direction that must fail closed.
        let mut gpr = make_exportable_gpr(32, 128);
        gpr.metadata.antenna_separation = 0.;
        gpr.antenna_separation_effective = 0.;

        gpr.correct_antenna_separation(
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        );
        let ds = gpr.export_dataset().unwrap();
        assert_eq!(str_attr(&ds.coords["twtt"].attrs, "anchor_name"), "twtt");
        assert_eq!(f32_attr(&ds, "antenna_separation_effective"), 0.);
    }

    #[test]
    fn depths_keep_the_antenna_separation_after_a_zero_correction() {
        // #261: time zero used to be read as the direct wave's flight time,
        // and a pre-trigger delay of 107 ns (25 MHz on Drønbreen) wiped out
        // a 6.2 m separation entirely. Where time zero sits on the recording
        // clock must not change the geometry.
        let mut gpr = make_dummy_gpr(4, 1024, None);
        gpr.metadata.medium_velocity = 0.168;
        gpr.antenna_separation_effective = 6.2;
        let before = gpr.depths();

        gpr.crop_ns = vec![107.; 4];
        gpr.time_zero_ns = vec![107.; 4];
        let after = gpr.depths();
        assert_eq!(before, after);

        let twtt = gpr.twtt_ns();
        for (time, depth) in twtt.iter().zip(after.iter()) {
            assert_eq!(
                *depth,
                crate::tools::return_time_to_depth(
                    *time,
                    0.168,
                    6.2,
                    crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
                )
            );
        }
        // Near the surface no reflection can have arrived yet, and deeper down
        // the air wave's lead puts depths below `twtt * v / 2`.
        assert_eq!(after[5], 0.);
        assert!(after[200] > twtt[200] * 0.168 / 2.);
    }

    #[test]
    fn test_correct_antenna_separation() {
        let mut gpr = make_test_gpr(Some(10), Some(1024));

        gpr.antenna_separation_effective = 30.;

        assert_eq!(gpr.data[[10, 0]], 10.);
        assert_eq!(gpr.log.len(), 0);
        gpr.correct_antenna_separation(
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        );
        assert!(gpr
            .log
            .last()
            .unwrap()
            .contains("correct_antenna_separation"));

        assert_ne!(gpr.height(), 1024);

        assert!(gpr.data[[10, 0]] > 10.);
    }

    #[test]
    fn a_slant_corrected_radargram_is_where_its_depth_coordinate_says() {
        // #379. Every sample holds its own original index, so after the
        // correction each row says which original sample it came from, and
        // that sample's depth before the correction has to be the depth
        // the corrected radargram labels the row with. The data-derived
        // spacing this replaces fell 0.3 % short of that on Drønbreen.
        let mut gpr = make_test_gpr(Some(4), Some(1024));
        gpr.antenna_separation_effective = 6.2;
        let before = gpr.depths_timed_from(crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS);
        gpr.correct_antenna_separation(
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        );
        let after = gpr.depths();
        let resolution = after[1] - after[0];
        assert!(
            (resolution - gpr.metadata.medium_velocity * gpr.vertical_resolution_ns() / 2.).abs()
                < 1e-6
        );

        let depth_of_original = |index: f32| {
            let low = index.floor() as usize;
            let fraction = index - low as f32;
            before[low] + fraction * (before[(low + 1).min(before.len() - 1)] - before[low])
        };
        let deepest = before[before.len() - 1];
        let mut checked = 0;
        for (row, labelled) in after.iter().enumerate() {
            // Above the direct wave every sample collapses to 0 m, so no
            // single original sample is "the" one there.
            if *labelled < 1.0 || *labelled > deepest - resolution {
                continue;
            }
            let actual = depth_of_original(gpr.data[[row, 0]]);
            assert!(
                (actual - labelled).abs() < 1e-3,
                "row {row}: labelled {labelled} m, holds data from {actual} m"
            );
            checked += 1;
        }
        assert!(checked > 500, "{checked}");
    }

    #[test]
    fn test_equidistant_traces() {
        let width = 128;
        let mut gpr = make_test_gpr(Some(width), Some(256));

        let first = gpr.location.cor_points[0].clone();

        let n_stationary = 10;

        for (i, point) in gpr.location.cor_points.iter_mut().enumerate() {
            if i == n_stationary {
                break;
            }

            point.easting = first.easting;
            point.northing = first.northing;
            point.altitude = first.altitude;
        }
        assert_eq!(gpr.width(), width);
        gpr.make_equidistant(None);

        // The stationary run collapses to a single trace: the whole cluster
        // sits at distance 0, so only the first target-grid position can
        // draw from it. This is the property the test is really about.
        assert!(gpr.location.cor_points[1].easting > first.easting);

        // The exact resulting width follows from the auto-step heuristic:
        // the track covers 127 m (the 10 stacked traces contribute nothing,
        // then 117 unit steps plus the 10 m jump out of the cluster), and
        // 118 traces are classified as moving, giving a step of
        // 127/118 ~= 1.076 m and 120 positions on the resampled grid.
        //
        // This previously read `width - (n_stationary - 1)` == 119, which
        // held only because `distances`/`velocities` both returned squared
        // lengths: the bogus max_distance (217) and the bogus step
        // (217/118) shared the same squared units, so their ratio landed
        // near the right trace count for this unit-spaced fixture. That
        // cancellation is why the fixture could not detect the missing
        // `sqrt` -- see
        // `test_distances_and_velocities_are_lengths_not_squared_lengths`.
        assert_eq!(gpr.width(), 120);
    }

    #[test]
    fn test_remove_traces() {
        let mut gpr = make_dummy_gpr(20, 30, Some(1.));

        gpr.topo_data = Some(gpr.data.clone());

        // Make sure that the dummy GPR is indeed 30x20 and it varies linearly from 0-19
        assert_eq!(gpr.width(), 20);
        assert_eq!(gpr.height(), 30);
        assert_eq!(gpr.location.cor_points.len(), 20);
        assert_eq!(gpr.data[[0, 0]], 0.);
        assert_eq!(gpr.data[[0, 19]], 19.);

        if let Some(topo_data) = &gpr.topo_data {
            assert_eq!(topo_data[[0, 19]], 19.);
        };
        // Remove indices 5 and 6, and "accidentally" duplicate one trace
        gpr.remove_traces(&[5, 5, 6], true).unwrap();

        // Validate that the dummy GPR is now shorter, and that the values are shifted correctly.
        assert_eq!(gpr.width(), 18);
        assert_eq!(gpr.height(), 30);
        assert_eq!(gpr.location.cor_points.len(), 18);
        assert_eq!(gpr.data[[0, 0]], 0.);
        assert_eq!(gpr.data[[0, 17]], 19.);
        assert_eq!(gpr.data[[0, 5]], 7.);

        if let Some(topo_data) = &gpr.topo_data {
            assert_eq!(topo_data[[0, 17]], 19.);
            assert_eq!(topo_data[[0, 5]], 7.);
        };

        // Check that the out of bounds error works expectedly
        assert_eq!(
            gpr.remove_traces(&[18], true),
            Err("Trace index 18 is out of bounds (number of traces: 18)".to_string())
        );
    }

    #[test]
    fn test_remove_empty_traces() {
        let mut gpr = make_dummy_gpr(20, 30, Some(1.));

        let empty_traces: Vec<usize> = vec![0, 10, 19];

        for (i, mut col) in gpr.data.columns_mut().into_iter().enumerate() {
            if empty_traces.iter().all(|i2| i2 != &i) {
                col.mapv_inplace(|_| 2.);
            } else {
                col.mapv_inplace(|_| 0.);
            };
        }
        gpr.remove_empty_traces(1.).unwrap();

        assert_eq!(gpr.width(), 17);
    }

    #[test]
    fn adaptive_siglog_is_its_own_step() -> Result<(), Box<dyn std::error::Error>> {
        // Steps are matched by substring, and "adaptive_siglog" contains
        // "siglog": each name must still reach its own step.
        let noisy = |row: usize, col: usize| ((row * 31 + col * 17) % 200) as f32 - 100.0;
        let make = || {
            let mut gpr = make_dummy_gpr(20, 30, None);
            gpr.data = ndarray::Array2::from_shape_fn(gpr.data.dim(), |(r, c)| noisy(r, c));
            gpr
        };
        let mut adaptive = make();
        let mut fixed = make();

        adaptive.process("adaptive_siglog(-0.5)")?;
        let log = adaptive.log.last().unwrap();
        assert!(log.starts_with("adaptive_siglog "), "{log}");
        assert!(log.contains("offset: -0.5"), "{log}");
        let mut expected = fixed.data.clone();
        let strength = crate::filters::siglog::adaptive_siglog(&mut expected, -0.5)?;
        assert!(
            log.contains(&format!("resolved strength: {strength:.3}")),
            "{log}"
        );
        assert_eq!(adaptive.data, expected);

        fixed.process("siglog(1)")?;
        assert!(fixed.log.last().unwrap().starts_with("siglog "));

        super::validate_steps(&[
            "adaptive_siglog".to_string(),
            "adaptive_siglog(-1)".to_string(),
        ])?;
        Ok(())
    }

    #[test]
    fn every_step_runs_logs_and_records_its_canonical_form() {
        // Each step once, with its first documented example or bare if it
        // has none, on a fresh radargram: `process` itself enforces the log
        // entry and the per-trace metadata, so this proves both for all of
        // them, not just the steps other tests happen to exercise.
        for (name, description) in crate::steps::descriptions() {
            let call = description
                .split('`')
                .skip(1)
                .step_by(2)
                .find(|s| s.starts_with(&format!("{name}(")))
                .unwrap_or(&name)
                .to_string();
            let mut gpr = make_dummy_gpr(40, 600, Some(1.));
            gpr.process(&call).unwrap_or_else(|e| panic!("{call}: {e}"));
            let recorded = gpr.steps.last().unwrap();
            assert!(recorded.starts_with(&name), "{call} recorded as {recorded}");
        }
    }

    #[test]
    fn test_multiply() -> Result<(), Box<dyn std::error::Error>> {
        let mut gpr = make_dummy_gpr(20, 30, Some(1.));

        let mean_pre = gpr.data.mean().unwrap();
        gpr.multiply(5.);
        assert_eq!(gpr.data.mean(), Some(mean_pre * 5.));

        gpr.process("multiply(5e0)")?;
        assert_eq!(gpr.data.mean(), Some(mean_pre * 5. * 5.));
        gpr.process("multiply(1e2)")?;
        assert_eq!(gpr.data.mean(), Some(mean_pre * 5. * 5. * 100.));

        let error = gpr.process("multiply(five)").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid value `five` for `factor`"),
            "Unexpected error: {error:?}"
        );

        let error = gpr.process("multiply(0)").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`factor` in `multiply`: cannot be zero"),
            "Unexpected error: {error:?}"
        );

        let error = gpr.process("multiply(inf)").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`factor` in `multiply`: must be finite"),
            "Unexpected error: {error:?}"
        );

        Ok(())
    }

    fn batch_params(output: &std::path::Path, id_pattern: Option<&str>) -> super::BatchRunParams {
        super::BatchRunParams {
            filepaths: vec![
                "assets/mala/dronbreen-20250327-DAT_0066_A1.rad".into(),
                "assets/mala/dronbreen-20220329-DAT_0237_A1.rad".into(),
                "assets/mala/scott_turnerbreen-20240207-DAT_0454_A1_2500-4000.rad".into(),
            ],
            output_dir: output.to_path_buf(),
            dem_path: None,
            cor_path: None,
            medium_velocity: 0.168,
            crs: None,
            quiet: true,
            track_dir: None,
            steps: vec!["subset(0 50 0 100)".to_string()],
            no_export: false,
            render_dir: None,
            render_profile: None,
            render_width: None,
            render_topo: false,
            merge: None,
            override_antenna_mhz: None,
            override_antenna_separation: None,
            user_metadata: crate::user_metadata::UserMetadata::new(),
            group: Some("Drønbreen".to_string()),
            group_id: None,
            id_pattern: id_pattern.map(str::to_string),
            display_name_pattern: None,
        }
    }

    fn ids_in(dir: &std::path::Path) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|path| {
                let record = processed(super::inspect(info_params(vec![path.clone()])).unwrap());
                (
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    record.radargram_id.unwrap(),
                )
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_batch_names_its_outputs_after_the_id_pattern() {
        // #368. The 2022 and 2024 lines are 100 MHz, the 2025 one 25 MHz,
        // and the counter runs in time order within each frequency.
        let dir = tempfile::tempdir().unwrap();
        let mut params = batch_params(dir.path(), Some("{group}-{freq}mhz-{n:02, per=freq}"));
        params.display_name_pattern = Some("{group_name}, {freq} MHz line {n}".to_string());
        super::run_batch(params).unwrap();
        assert_eq!(
            ids_in(dir.path()),
            [
                (
                    "dronbreen-100mhz-01.nc".to_string(),
                    "dronbreen-100mhz-01".to_string()
                ),
                (
                    "dronbreen-100mhz-02.nc".to_string(),
                    "dronbreen-100mhz-02".to_string()
                ),
                (
                    "dronbreen-25mhz-01.nc".to_string(),
                    "dronbreen-25mhz-01".to_string()
                ),
            ]
        );
        let record = processed(
            super::inspect(info_params(vec![dir.path().join("dronbreen-100mhz-02.nc")])).unwrap(),
        );
        assert_eq!(
            record.display_name.as_deref(),
            Some("Drønbreen, 100 MHz line 2")
        );

        // Again, continuing: one more of each frequency.
        super::run_batch(batch_params(
            dir.path(),
            Some("{group}-{freq}mhz-{n:02, per=freq, from=next}"),
        ))
        .unwrap();
        let names: Vec<String> = ids_in(dir.path()).into_iter().map(|(_, id)| id).collect();
        assert!(
            names.contains(&"dronbreen-100mhz-04".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"dronbreen-25mhz-02".to_string()),
            "{names:?}"
        );
        assert_eq!(names.len(), 6);
    }

    fn timed(start: f64, stop: f64, mhz: f32) -> super::GroupingRecording {
        super::GroupingRecording {
            start_seconds: start,
            stop_seconds: stop,
            key: super::MergeKey {
                crs: "EPSG:32633".to_string(),
                antenna_mhz: mhz,
                time_window: 500.,
                antenna_separation: 4.,
                medium_velocity: 0.168,
            },
        }
    }

    #[test]
    fn grouping_follows_time_and_a_persons_joins_and_splits() {
        use super::{group_chronologically, GroupingEdits};
        // Given out of order: 0 and 2 are 50 s apart, 3 is an hour after
        // 2, and 1 is another antenna right after 3.
        let recordings = vec![
            timed(0., 100., 100.),
            timed(3900., 4000., 800.),
            timed(150., 200., 100.),
            timed(3800., 3850., 100.),
        ];
        let members = |grouping: &super::Grouping| -> Vec<Vec<usize>> {
            grouping.groups.iter().map(|g| g.members.clone()).collect()
        };

        let grouping = group_chronologically(&recordings, Some(600.), &GroupingEdits::default());
        assert_eq!(members(&grouping), [vec![0, 2], vec![3], vec![1]]);
        let split = grouping.groups[1].split.as_ref().unwrap();
        assert_eq!(split.gap_seconds, 3600.);
        assert!(split.over_threshold && split.incompatible.is_none() && !split.manual);
        let split = grouping.groups[2].split.as_ref().unwrap();
        assert!(!split.over_threshold);
        assert!(split
            .incompatible
            .as_deref()
            .unwrap()
            .contains("Antenna frequencies"));

        // Joined whatever the gap; split however close; the other
        // antenna cannot be joined, and says so.
        let edits = GroupingEdits {
            joins: [3, 1].into(),
            splits: [2].into(),
        };
        let grouping = group_chronologically(&recordings, Some(600.), &edits);
        assert_eq!(members(&grouping), [vec![0], vec![2, 3], vec![1]]);
        assert!(grouping.groups[1].split.as_ref().unwrap().manual);
        assert_eq!(grouping.refused_joins.len(), 1);
        assert_eq!(grouping.refused_joins[0].0, 1);

        // Never merging: only joins put two together.
        let edits = GroupingEdits {
            joins: [2].into(),
            splits: Default::default(),
        };
        let grouping = group_chronologically(&recordings, None, &edits);
        assert_eq!(members(&grouping), [vec![0, 2], vec![3], vec![1]]);
        assert!(grouping.groups[1].split.as_ref().unwrap().over_threshold);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_plan_says_why_each_output_is_its_own_radargram() {
        let dir = tempfile::tempdir().unwrap();

        // Not merging: one output per input, and no splits to explain.
        let plan = super::plan_batch(&batch_params(dir.path(), None)).unwrap();
        assert_eq!(plan.merge_threshold_seconds, None);
        assert_eq!(plan.outputs.len(), 3);
        assert!(plan
            .outputs
            .iter()
            .all(|o| o.split.is_none() && o.inputs.len() == 1));
        assert_eq!(
            plan.outputs[0].radargram_id,
            "dronbreen-20250327-dat_0066_a1"
        );
        assert_eq!(
            plan.outputs[0].output,
            dir.path().join("dronbreen-20250327-DAT_0066_A1.nc")
        );

        // Merging within ten minutes: years apart, and not compatible
        // either, so both reasons are given, in time order.
        let mut params = batch_params(dir.path(), None);
        params.merge = Some("10 min".to_string());
        let plan = super::plan_batch(&params).unwrap();
        assert_eq!(plan.merge_threshold_seconds, Some(600.));
        let first = &plan.outputs[0];
        assert!(first.inputs[0].ends_with("dronbreen-20220329-DAT_0237_A1.rad"));
        assert_eq!(first.split, None);
        for later in &plan.outputs[1..] {
            let split = later.split.as_ref().expect("a later output says why");
            assert!(
                split.over_threshold && split.gap_seconds > 600.,
                "{split:?}"
            );
            assert!(split.incompatible.is_some(), "{split:?}");
        }

        // A threshold of years: only the incompatibility is left.
        params.merge = Some("3 years".to_string());
        let plan = super::plan_batch(&params).unwrap();
        let split = plan.outputs[2].split.as_ref().unwrap();
        assert!(!split.over_threshold);
        assert!(
            split
                .incompatible
                .as_deref()
                .is_some_and(|reason| reason.contains("Antenna frequencies")),
            "{split:?}"
        );

        // Nothing was processed.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn two_inputs_that_would_write_one_output_are_refused() {
        // They used to overwrite each other without a word.
        let dir = tempfile::tempdir().unwrap();
        let mut params = batch_params(dir.path(), None);
        params.filepaths = vec![params.filepaths[0].clone(), params.filepaths[0].clone()];
        let refused = super::plan_batch(&params).unwrap_err();
        assert!(refused.contains("would both be written to"), "{refused}");
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_pattern_that_collides_processes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let refused =
            super::run_batch(batch_params(dir.path(), Some("{group}-{freq}mhz"))).unwrap_err();
        assert!(
            refused.contains("would both be 'dronbreen-100mhz'"),
            "{refused}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        let mut params = batch_params(dir.path(), None);
        params.display_name_pattern = Some("{n}".to_string());
        assert!(super::run_batch(params)
            .unwrap_err()
            .contains("needs --id-pattern"));
    }

    fn info_params(inputs: Vec<PathBuf>) -> super::InfoParams {
        super::InfoParams {
            filepaths: inputs,
            dem_path: None,
            cor_path: None,
            medium_velocity: 0.168,
            crs: None,
            override_antenna_mhz: None,
            override_antenna_separation: None,
        }
    }

    fn processed(records: Vec<super::Inspected>) -> super::ProcessedInfoRecord {
        match records.into_iter().next() {
            Some(super::Inspected::Processed(record)) => record,
            other => panic!("expected a processed record, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn info_reports_a_processed_file_by_its_ids() {
        // #11: a local file is matched to a served radargram by its ids, so
        // the revision id must be the one the server computes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.nc");
        let gpr = make_dummy_gpr(20, 10, Some(1.));
        gpr.export(&path).unwrap();

        let record = processed(super::inspect(info_params(vec![path])).unwrap());
        let datetime = record.processing_datetime.clone().unwrap();
        let id = crate::identity::RadargramId::new("test-radargram").unwrap();
        assert_eq!(record.format.name, "ridal");
        assert_eq!(record.radargram_id.as_deref(), Some("test-radargram"));
        assert_eq!(
            record.revision_id,
            Some(crate::identity::RevisionId::fingerprint_v1(&id, &datetime).to_string())
        );
        assert_eq!((record.samples, record.traces), (Some(10), Some(20)));
        assert_eq!(record.reprocess_reason, None);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_file_is_named_by_its_recipe_so_processing_again_is_the_same_revision() {
        // #382: the revision id comes from what the file was made from, not
        // from when, so running the same processing twice gives one
        // revision and changing a step gives another.
        let dir = tempfile::tempdir().unwrap();
        let run = |name: &str, steps: &[&str]| {
            let output = dir.path().join(name);
            let mut params = info_run_params(&output);
            params.steps = steps.iter().map(|s| s.to_string()).collect();
            super::run(params).unwrap();
            processed(super::inspect(info_params(vec![output])).unwrap())
        };
        let first = run("first.nc", &["subset(0 200 0 300)", "dewow"]);
        let again = run("again.nc", &["subset(0 200 0 300)", "dewow"]);
        let other = run("other.nc", &["subset(0 200 0 300)", "dewow(method=mean)"]);

        let hash = first
            .recipe_hash
            .clone()
            .expect("a run from raw files records its recipe");
        assert_ne!(first.processing_datetime, again.processing_datetime);
        assert_eq!(again.recipe_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(first.revision_id, again.revision_id);
        let id = crate::identity::RadargramId::new("line-01").unwrap();
        assert_eq!(
            first.revision_id,
            Some(crate::identity::RevisionId::fingerprint_v2(&id, &hash).to_string())
        );
        assert_ne!(other.recipe_hash, first.recipe_hash);
        assert_ne!(other.revision_id, first.revision_id);

        // The stored recipe is the one that hashes to the stored hash, and
        // says what was read and run.
        let file = netcdf::open(dir.path().join("first.nc")).unwrap();
        let text = match file.attribute("ridal_recipe").unwrap().value().unwrap() {
            netcdf::AttributeValue::Str(text) => text,
            other => panic!("ridal_recipe is {other:?}"),
        };
        let recipe: crate::recipe::Recipe = serde_json::from_str(&text).unwrap();
        assert_eq!(recipe.hash(), hash);
        assert_eq!(recipe.core_version, crate::recipe::CORE_PROCESSING_VERSION);
        assert_eq!(recipe.bundles.len(), 1);
        assert_eq!(recipe.bundles[0].format, "ramac");
        assert_eq!(recipe.bundles[0].files.len(), 3);
        let steps: Vec<&str> = recipe.steps.iter().map(|s| s.step.as_str()).collect();
        assert_eq!(steps.len(), 2);
        assert!(steps[1].starts_with("dewow("), "{steps:?}");
        assert!(recipe.steps.iter().all(|s| s.version >= 1));
    }

    fn info_run_params(output: &std::path::Path) -> super::RunParams {
        super::RunParams {
            filepaths: vec!["assets/mala/dronbreen-20250327-DAT_0066_A1.rad".into()],
            output_path: Some(output.to_path_buf()),
            dem_path: None,
            cor_path: None,
            medium_velocity: 0.168,
            crs: None,
            quiet: true,
            track_path: None,
            steps: Vec::new(),
            no_export: false,
            render_path: None,
            render_profile: None,
            render_width: None,
            render_topo: false,
            override_antenna_mhz: None,
            override_antenna_separation: None,
            user_metadata: crate::user_metadata::UserMetadata::new(),
            radargram_id: Some("line-01".to_string()),
            display_name: None,
            group: None,
            group_id: None,
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn info_reports_a_legacy_file_rather_than_failing() {
        // A caller sorting many files sorts this one as "reprocess", which it
        // could only do from an error by reading its message.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.nc");
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_attribute("program_version", "ridal version 0.3.0")
                .unwrap();
        }

        let record = processed(super::inspect(info_params(vec![path])).unwrap());
        assert_eq!(record.ridal_version, "ridal version 0.3.0");
        assert!(record
            .reprocess_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("Reprocess it")));
        assert_eq!(record.radargram_id, None);
        assert_eq!(record.revision_id, None);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn info_refuses_a_netcdf_file_ridal_did_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.nc");
        {
            let mut file = netcdf::create(&path).unwrap();
            file.add_dimension("x", 1).unwrap();
        }
        let error = super::inspect(info_params(vec![path])).unwrap_err();
        assert!(
            error.to_string().contains("not one Ridal processed"),
            "{error}"
        );
    }

    #[test]
    fn info_refuses_raw_file_options_with_a_processed_file() {
        // Refused before any file is opened: they would be ignored, and an
        // ignored `--crs` is a silent wrong answer about where a line is.
        let mut params = info_params(vec![PathBuf::from("line.nc")]);
        params.crs = Some("EPSG:32633".to_string());
        let error = super::inspect(params).unwrap_err();
        assert!(error.to_string().contains("crs only apply"), "{error}");
    }

    /// A radargram with the synthetic standstills of
    /// `filters::standstill::tests`, on a grid where one antenna period is
    /// ten samples.
    fn make_gpr_with_standstills(stops: &[(usize, usize)]) -> super::GPR {
        use crate::filters::standstill::tests::{radargram, HEIGHT, WIDTH};
        let mut gpr = make_dummy_gpr(WIDTH, HEIGHT, None);
        gpr.data = radargram(stops);
        // 500 MHz is a 2 ns period: ten samples at 0.2 ns.
        gpr.metadata.time_window = HEIGHT as f32 * 0.2;
        gpr
    }

    #[test]
    fn remove_standstills_replaces_each_with_its_median_trace_and_logs_it() {
        let stops = [(0, 80), (500, 620), (1900, 2000)];
        let mut gpr = make_gpr_with_standstills(&stops);
        let before: Vec<f64> = gpr
            .location
            .cor_points
            .iter()
            .map(|p| p.time_seconds)
            .collect();
        gpr.process("remove_standstills").unwrap();

        // About 79 + 119 + 99 traces fewer, give or take the edges.
        let removed = before.len() - gpr.width();
        assert!((280..=310).contains(&removed), "removed {removed}");
        let log = gpr.log.last().unwrap();
        assert!(log.contains("Replaced 3 standstills"), "{log}");
        assert!(log.contains("(strength >= 8, >= 5 s)"), "{log}");
        assert!(log.contains("Traces (duration, strength): 0-"), "{log}");
        assert!(log.contains("Highest strength elsewhere"), "{log}");

        // Times stay strictly increasing, and the ends of the profile keep
        // their times: what keeps interpretations carryable.
        let after: Vec<f64> = gpr
            .location
            .cor_points
            .iter()
            .map(|p| p.time_seconds)
            .collect();
        assert!(after.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(after.first(), before.first());
        assert_eq!(after.last(), before.last());
    }

    #[test]
    fn remove_standstills_with_nothing_to_find_changes_nothing() {
        let mut gpr = make_gpr_with_standstills(&[]);
        let data = gpr.data.clone();
        gpr.process("remove_standstills").unwrap();
        assert_eq!(gpr.data, data);
        assert!(gpr.log.last().unwrap().contains("Found no standstills"));
    }

    #[test]
    fn remove_standstills_without_a_trace_interval_warns_for_seconds_only() {
        let mut gpr = make_gpr_with_standstills(&[(500, 620)]);
        gpr.metadata.time_interval = 0.;
        let width = gpr.width();
        gpr.process("remove_standstills").unwrap();
        assert_eq!(gpr.width(), width);
        assert!(gpr.log.last().unwrap().contains("Warning"));
        // In traces it needs no interval.
        gpr.process("remove_standstills(min_duration=10)").unwrap();
        assert!(gpr.width() < width);
    }

    /// `make_gpr_with_standstills`, with positions that stand still only in
    /// `still` and drive at 4 m/s elsewhere, at 0.1 s per trace.
    fn make_gpr_with_positions(stops: &[(usize, usize)], still: &[(usize, usize)]) -> super::GPR {
        let mut gpr = make_gpr_with_standstills(stops);
        let (seconds, easting, northing) =
            crate::filters::standstill::gps::tests::track(gpr.width(), still, 0.5);
        gpr.metadata.time_interval =
            crate::filters::standstill::gps::tests::SECONDS_PER_TRACE as f32;
        for (i, point) in gpr.location.cor_points.iter_mut().enumerate() {
            point.time_seconds = seconds[i];
            point.easting = easting[i];
            point.northing = northing[i];
        }
        gpr
    }

    #[test]
    fn remove_standstills_with_gps_keeps_only_what_stood_still_and_logs_it() {
        // 1200..1350 is coherent, as lost signal is, while driving.
        let stops = [(500, 620), (1200, 1350)];
        let mut radar = make_gpr_with_positions(&stops, &[(500, 620)]);
        let mut gps = make_gpr_with_positions(&stops, &[(500, 620)]);
        radar.process("remove_standstills").unwrap();
        gps.process("remove_standstills(gps_uncertainty=1)")
            .unwrap();
        assert!(
            gps.width() > radar.width() + 100,
            "{}",
            gps.log.last().unwrap()
        );

        let log = gps.log.last().unwrap();
        assert!(log.contains("Replaced 1 standstill"), "{log}");
        assert!(log.contains("gps_uncertainty = 1 m"), "{log}");
        assert!(log.contains("The GPS showed movement through 1"), "{log}");
        assert!(
            log.contains("stretches of 3.0 s or more can be judged"),
            "{log}"
        );
        assert_eq!(
            gps.steps.last().unwrap(),
            "remove_standstills(strength=8, min_duration=5s, gps_uncertainty=1)"
        );
    }

    #[test]
    fn remove_standstills_without_usable_positions_uses_the_radar() {
        // `make_dummy_gpr` positions move one metre per trace; these do not.
        let still = || {
            let mut gpr = make_gpr_with_standstills(&[(500, 620)]);
            for point in gpr.location.cor_points.iter_mut() {
                point.easting = 0.;
                point.northing = 0.;
            }
            gpr
        };
        let (mut gpr, mut radar) = (still(), still());
        radar.process("remove_standstills").unwrap();
        gpr.process("remove_standstills(gps_uncertainty=1)")
            .unwrap();
        assert_eq!(gpr.data, radar.data);
        let log = gpr.log.last().unwrap();
        // No bends in the track: no fixes, as without a GPS at all.
        assert!(log.contains("Warning: there are only 2 fixes"), "{log}");
        assert!(log.contains("so the GPS was not used"), "{log}");
    }

    /// Interpretations drawn before `remove_standstills` carry onto its
    /// output through the `trace_time` anchor, with the real gprinterp
    /// re-anchoring, including lines that start in an opening standstill or
    /// end in a closing one (#327).
    #[test]
    fn interpretations_carry_across_remove_standstills() {
        let stops = [(0, 80), (500, 620), (1900, 2000)];
        let mut gpr = make_gpr_with_standstills(&stops);
        let times = |gpr: &super::GPR| -> Vec<f64> {
            gpr.location
                .cor_points
                .iter()
                .map(|p| p.time_seconds)
                .collect()
        };
        let before = times(&gpr);
        gpr.process("remove_standstills").unwrap();
        let after = times(&gpr);

        let anchor = |times: &[f64]| -> gprinterp::AnchorAxis {
            let axis = crate::interp::anchors::trace_time_axis(times).unwrap();
            serde_json::from_value(serde_json::to_value(axis).unwrap()).unwrap()
        };
        let twtt: gprinterp::AnchorAxis = serde_json::from_value(serde_json::json!(
            {"name": "twtt", "unit": "ns", "type": "regular", "t0": 0.0, "dt": 0.2}
        ))
        .unwrap();
        let lines = [
            (
                "starts in the opening standstill",
                vec![[5., 100.], [300., 110.]],
            ),
            (
                "crosses the middle one",
                vec![[400., 50.], [560., 55.], [700., 60.]],
            ),
            ("lies inside the middle one", vec![[520., 70.], [600., 72.]]),
            (
                "ends in the closing standstill",
                vec![[1700., 40.], [1990., 45.]],
            ),
        ];
        let features: Vec<serde_json::Value> = lines
            .iter()
            .map(|(label, coordinates)| {
                serde_json::json!({
                    "type": "Feature",
                    "geometry": {"type": "LineString", "coordinates": coordinates},
                    "properties": {"label": label},
                })
            })
            .collect();
        let document: gprinterp::Document = serde_json::from_value(serde_json::json!({
            "key": "test-radargram",
            "coordinates": {"space": "index2d", "axes": {
                "x": {"primary": {"name": "trace", "unit": "index"}, "anchor": [anchor(&before)]},
                "y": {"primary": {"name": "sample", "unit": "index"}, "anchor": [twtt.clone()]},
            }},
            "features": features,
        }))
        .unwrap();
        let target = gprinterp::RevisionAxes {
            x: vec![anchor(&after)],
            y: vec![twtt],
        };
        let carried = gprinterp::reanchor(&document, &target).unwrap();
        assert!(carried.dropped.is_empty(), "{:?}", carried.dropped);
        for feature in &carried.document.features {
            let gprinterp::Geometry::LineString(points) = &feature.geometry else {
                panic!("not a line");
            };
            let x: Vec<f64> = points.iter().map(|p| p.0[0]).collect();
            assert!(x.windows(2).all(|w| w[1] >= w[0]), "doubles back: {x:?}");
            assert!(
                x.iter().all(|&v| v >= 0. && v <= (gpr.width() - 1) as f64),
                "{x:?}"
            );
        }
    }

    fn header(name: &str, antenna_mhz: f32) -> super::InfoRecord {
        super::InfoRecord {
            input: format!("/somewhere/{name}"),
            format: super::NamedFormat {
                name: "ramac",
                description: "",
            },
            metadata: super::MetadataSummary {
                samples: 500,
                sampling_frequency_mhz: 1000.0,
                frequency_steps: 1,
                time_interval_s: 0.1,
                antenna_name: String::new(),
                antenna_mhz,
                antenna_separation_m: 1.0,
                time_window_ns: 500.0,
                last_trace: 99,
                medium_velocity_m_per_ns: 0.168,
            },
            location: super::LocationSummary {
                n_points: 100,
                crs: "EPSG:32633".to_string(),
                start_time: String::new(),
                stop_time: String::new(),
                duration_s: 0.0,
                track_length_m: 0.0,
                altitude_min_m: 0.0,
                altitude_max_m: 0.0,
                centroid: super::CentroidSummary {
                    easting: 0.0,
                    northing: 0.0,
                    altitude: 0.0,
                },
                correction: super::CorrectionSummary {
                    kind: String::new(),
                    source: None,
                },
            },
            related_files: super::RelatedFiles {
                header: String::new(),
                data: String::new(),
                coordinates: String::new(),
            },
        }
    }

    #[test]
    fn an_odd_recording_is_named_and_the_rest_counted() {
        let mut records = vec![header("DAT_0001.rad", 100.0)];
        for i in 2..=10 {
            records.push(header(&format!("DAT_{i:04}.rad"), 25.0));
        }
        let message = super::check_mergeable(&records).unwrap_err();
        assert_eq!(
            message,
            "These recordings cannot be merged into one radargram: their antenna \
             frequencies differ. 9 are 25 MHz; 1 is 100 MHz (DAT_0001.rad)."
        );
    }

    #[test]
    fn without_a_majority_every_group_is_named() {
        let records = [header("a.rad", 100.0), header("b.rad", 25.0)];
        let message = super::check_mergeable(&records).unwrap_err();
        assert!(
            message.ends_with("1 is 100 MHz (a.rad); 1 is 25 MHz (b.rad)."),
            "{message}"
        );
        assert!(super::check_mergeable(&[header("a.rad", 25.0), header("b.rad", 25.0)]).is_ok());
    }
}
