//! Where and when each trace was recorded.

use std::path::{Path, PathBuf};

use ndarray::{Array1, Array2, Axis, Slice};

use crate::{dem, tools};

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
    /// Elevations a DEM gave when `ridal coords --dem` wrote the coordinate
    /// CSV at `path` (#13): the CSV names the DEM in a comment, and the
    /// recipe hashes the CSV.
    InCoordinates {
        path: PathBuf,
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
    /// Whether `time_seconds` is when each trace was recorded.
    pub time_source: TimeSource,
}

/// Where a radargram's trace times come from (#13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeSource {
    /// Recorded with the positions, and interpolated between them.
    #[default]
    Recorded,
    /// Made up by `ridal coords`, from a start time and a trace interval,
    /// for a recording whose times are unknown: they order the traces but
    /// say nothing about when the radar stood still.
    Synthetic,
}

impl TimeSource {
    /// The source of a merge of two: synthetic if either is.
    pub fn and(self, other: TimeSource) -> TimeSource {
        if self == TimeSource::Synthetic || other == TimeSource::Synthetic {
            TimeSource::Synthetic
        } else {
            TimeSource::Recorded
        }
    }
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

    pub(crate) fn velocities(&self) -> Array1<f64> {
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
    pub(crate) fn range_fill(
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
            time_source: self.time_source,
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

#[cfg(test)]
mod tests {
    use ndarray::{Axis, Slice};

    use super::{CorPoint, GPRLocation, LocationCorrection};
    use crate::gpr::test_fixtures::*;

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
            time_source: super::TimeSource::Recorded,
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
            time_source: super::TimeSource::Recorded,
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
        let gpr = crate::gpr::GPR::from_meta_and_loc(location, meta).unwrap();
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
            time_source: super::TimeSource::Recorded,
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
}
