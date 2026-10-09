use core::ops::{Add, Div, Mul, Sub};
use enterpolation::{linear::Linear, Signal};
use ndarray::{Array1, Array2, ArrayView1};
use num::Float;
use rayon::prelude::*;
/// Miscellaneous functions that are used in other parts of the program
use std::path::PathBuf;

/// Parse a provided step list (or filepath to a step list)
///
/// # Arguments
/// - `steps`: An unformatted list of steps, or a filepath
///
/// # Returns
/// Formatted steps either from the string itself or from the parsed file.
pub fn parse_step_list(steps: &str) -> Result<Vec<String>, String> {
    crate::steps::split_step_list(steps)
}

/// Start an external tool (`projinfo`, `cs2cs`, the GDAL utilities)
/// without handing it the process's open NetCDF files.
///
/// Every subprocess Ridal starts goes through this (#129). HDF5 opens files
/// without close-on-exec and locks them with `flock`, and that lock belongs
/// to the open file description, which a child shares. A tool started from
/// any thread while another thread has a NetCDF file open would hold that
/// file's lock until it exits, and opening the same file in a conflicting
/// mode meanwhile -- reading back what was just written, or writing what
/// was just read -- fails with `NC_EHDFERR` (-101).
///
/// Two parts, because either alone leaves a gap:
///
/// - The child marks every descriptor above stderr close-on-exec, so the
///   tool itself holds nothing. That alone still leaves the child holding
///   them from `fork` until `exec`, about one reopen in a hundred under
///   load.
/// - The spawn happens under the lock the `netcdf` crate takes for every
///   call, so no file is closed or opened in that window. `spawn` returns
///   only once `exec` has succeeded, which is when the window ends.
///
/// CI also sets `HDF5_USE_FILE_LOCKING=FALSE` (#154), which hides the
/// failure rather than preventing it, and only there.
pub fn spawn_tool(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    #[cfg(test)]
    if TOOLS_HIDDEN.get() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure runs between fork and exec, where only
        // async-signal-safe calls are allowed. It makes nothing but the
        // `close_range` and `fcntl` system calls, and allocates nothing.
        unsafe {
            command.pre_exec(close_inherited_descriptors_on_exec);
        }
    }
    let _guard = netcdf_sys::libnetcdf_lock.lock();
    command.spawn()
}

#[cfg(test)]
thread_local! {
    static TOOLS_HIDDEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` as if no external tool were installed: every [`spawn_tool`] on
/// this thread fails as a missing program does.
///
/// For the tests that simulate a machine without PROJ or GDAL (#108). They
/// used to unset `PATH`, which is process-wide, so any test spawning a tool
/// meanwhile failed too unless it was marked to stay out of their way, and
/// one that was not failed at random on macOS. This touches only the
/// calling thread.
#[cfg(test)]
pub fn without_tools<T>(f: impl FnOnce() -> T) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            TOOLS_HIDDEN.set(false);
        }
    }
    TOOLS_HIDDEN.set(true);
    let _restore = Restore;
    f()
}

/// Mark every descriptor above stderr close-on-exec, in a forked child.
///
/// Close-on-exec rather than closed: std's own error pipe to the parent is
/// among them and must stay open until `exec` succeeds.
#[cfg(unix)]
fn close_inherited_descriptors_on_exec() -> std::io::Result<()> {
    // One call on Linux 5.11 and later. An older kernel refuses it, which
    // falls through to the loop below.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: a plain system call on integers.
        let marked = unsafe {
            libc::close_range(
                3,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC as libc::c_int,
            )
        };
        if marked == 0 {
            return Ok(());
        }
    }
    // SAFETY: plain system calls on integers. A descriptor that is not open
    // fails with EBADF, which is the answer wanted for it.
    let limit = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    let limit = if limit > 0 {
        limit as libc::c_int
    } else {
        1024
    };
    for fd in 3..limit {
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    Ok(())
}

/// Read a text file and return all lines as a vec
///
/// # Arguments
/// - `filepath`: The filepath of the text file
///
/// # Returns
/// Each line as a trimmed String
pub fn read_text(filepath: &PathBuf) -> Result<Vec<String>, std::io::Error> {
    let content = std::fs::read_to_string(filepath)?;

    let mut lines = Vec::<String>::new();

    for line in content.lines() {
        // Remove comments and trailing whitespace
        let line = line.split("#").collect::<Vec<&str>>()[0].trim();
        // Skip empty lines
        if line.is_empty() {
            continue;
        }
        lines.push(line.to_owned());
    }

    Ok(lines)
}

/// Interpolate an arbitrary amount of independent values between two known points
///
/// # Arguments
/// - `x0`: The first known explanatory variable
/// - `y0`: The first known independent variables
/// - `x1`: The second known explanatory variable
/// - `y1`: The second known independent variables
/// - `x`: The explanatory point at which to interpolate the independent variables
///
/// # Returns
/// The interpolated independent (y) values.
///
/// # Examples
/// ```
/// assert_eq!(interpolate_values(0_f32, &[0., 5.], 1., &[-1., 10.], 0.5), &[-0.5, 7.5]);
///
/// ```
///
/// # Panics
/// - The first slice of independent values is longer than the second: `y0.len()` > `y1.len()`
pub fn interpolate_values<
    T: Add<Output = T> + Sub<Output = T> + Mul<Output = T> + Div<Output = T> + Copy,
>(
    x0: T,
    y0: &[T],
    x1: T,
    y1: &[T],
    x: T,
) -> Vec<T> {
    (0..y0.len())
        .map(|i| interpolate_between_known((x0, y0[i]), (x1, y1[i]), x))
        .collect::<Vec<T>>()
}

/// Interpolate linearly between two known points
///
/// <https://en.wikipedia.org/wiki/Linear_interpolation#Linear_interpolation_between_two_known_points>
///
/// # Arguments
/// - `known_xy0`: The first known point as (explanatory, independent)
/// - `known_xy1`: The second known point as (explanatory, independent)
/// - `x`: The explanatory point at which to interpolate the independent variables
///
/// # Returns
/// The interpolated independent (y) value.
pub fn interpolate_between_known<
    T: Add<Output = T> + Sub<Output = T> + Mul<Output = T> + Div<Output = T> + Copy,
>(
    known_xy0: (T, T),
    known_xy1: (T, T),
    x: T,
) -> T {
    (known_xy0.1 * (known_xy1.0 - x) + known_xy1.1 * (x - known_xy0.0))
        / (known_xy1.0 - known_xy0.0)
}

fn interpolate_vec<T: Float + Copy + Sub<Output = T> + std::fmt::Debug>(
    x_old: &[T],
    y_old: &[T],
    x_new: &[T],
) -> Vec<T> {
    if x_old.len() != y_old.len() {
        panic!("Interpolation failed. x_old and y_old must have the same length");
    }

    let model = Linear::builder()
        .elements(y_old)
        .knots(x_old)
        .build()
        .unwrap();
    model.sample(x_new.iter().copied()).collect()
}

fn interpolate_ndarray<T: Float + std::fmt::Debug>(
    x_old: &ArrayView1<T>,
    y_old: &ArrayView1<T>,
    x_new: &ArrayView1<T>,
) -> Array1<T> {
    Array1::<T>::from_vec(interpolate_vec(
        &x_old.to_vec(),
        &y_old.to_vec(),
        &x_new.to_vec(),
    ))
}

/// Derive the quantiles of an iterator of values
///
/// # Arguments
/// - `values`: An iterator of values
/// - `quantiles`: The quantiles to derive
/// - `downsample`: Downsample the data to increase performance.
pub fn quantiles<'a, T: 'a + PartialOrd + Copy, I, const L: usize>(
    values: I,
    quantiles: &[f32; L],
    downsample: Option<usize>,
) -> [T; L]
where
    I: IntoIterator<Item = &'a T>,
{
    let mut vals: Vec<&T> = values
        .into_iter()
        .step_by(downsample.unwrap_or(1))
        .collect();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut output = [*vals[0]; L];

    for (i, quantile) in quantiles.iter().enumerate() {
        output[i] = *vals[((vals.len() as f32 * quantile) as usize).min(vals.len() - 1)];
    }

    output
}

/// Convert numbers of seconds since UNIX epoch into an RFC3339 datetime string in UTC
///
/// # Arguments
/// - `seconds`: The number of seconds since UNIX epoch
///
/// # Examples
///
///
/// # Returns
/// A string representation of the datetime
pub fn seconds_to_rfc3339(seconds: f64) -> String {
    chrono::DateTime::from_timestamp(seconds as i64, (seconds.fract() * 1e9) as u32)
        .unwrap()
        .to_rfc3339()
}

pub enum Axis2D {
    Row,
    Col,
}
/// The speed of light in air, in m/ns (vacuum speed over a refractive index
/// of 1.0003).
pub const SPEED_OF_LIGHT_AIR_M_PER_NS: f32 = 0.2997;

/// Convert a two-way travel time, measured from time zero, to depth.
///
/// Time zero is the direct wave's arrival at the receiver, which is where
/// `zero_corr` puts it. The pulse left the transmitter
/// `antenna_separation / direct_velocity` before that, so the time since it
/// was emitted is
///
/// ```text
/// t_emission = return_time + antenna_separation / direct_velocity
/// ```
///
/// The air wave is always the first arrival, so `direct_velocity` is
/// normally [`SPEED_OF_LIGHT_AIR_M_PER_NS`]. ImpDAR's `nmo` uses the medium
/// velocity instead, which assumes time zero is on the ground wave.
///
/// and a reflection straight below the antenna midpoint travels two slant
/// legs of `sqrt(depth² + (antenna_separation / 2)²)` each, so
///
/// ```text
/// depth = sqrt((t_emission * velocity / 2)² - (antenna_separation / 2)²)
/// ```
///
/// # Arguments
/// - `return_time`: The two-way travel time since time zero, in ns
/// - `velocity`: The wave velocity in the medium, in m/ns
/// - `antenna_separation`: The separation between the transmitter and the receiver, in m
/// - `direct_velocity`: The velocity of the wave that time zero was picked on, in m/ns
///
/// # Returns
/// The depth in m corresponding to the return time, or 0. if the return time is smaller than
/// theoretically possible given the antenna separation.
pub fn return_time_to_depth(
    return_time: f32,
    velocity: f32,
    antenna_separation: f32,
    direct_velocity: f32,
) -> f32 {
    let emission_time = return_time + antenna_separation / direct_velocity;
    let slant_leg = emission_time * velocity / 2.;
    let half_separation = antenna_separation / 2.;
    match slant_leg > half_separation {
        true => (slant_leg.powi(2) - half_separation.powi(2)).sqrt(),
        false => 0.,
    }
}

fn digitize<F: Float>(values: &[F], bins: &[F]) -> Vec<usize> {
    let mut bins = bins.iter().collect::<Vec<&F>>();
    bins.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut indices = Vec::<usize>::new();

    for value in values {
        // Initialize the upper bin index by the "out of bounds" value
        let mut upper_bin = 0;

        if value >= bins[bins.len() - 1] {
            upper_bin = bins.len();
        } else if value > bins[0] {
            for bin in &bins {
                if bin > &value {
                    break;
                }
                upper_bin += 1;
            }
        }
        indices.push(upper_bin);
    }

    indices
}

pub struct Resampler<F: Float> {
    pub x_values: Array1<F>,
    pub target_x_values: Array1<F>,
    pub digitized: Vec<usize>,
    slope_indices: Vec<Vec<usize>>,
    intercept_indices: Vec<Vec<usize>>,
    _debug: bool,
}

fn equally_spaced_from_sparse<F: Float>(sparse: &Array1<F>, resolution: F) -> Array1<F> {
    let min_val = sparse.iter().cloned().fold(F::max_value(), F::min);
    let max_val = sparse.iter().cloned().fold(F::min_value(), F::max);
    Array1::<F>::range(min_val, max_val + resolution, resolution)
}

impl<F: Float + std::fmt::Display + std::iter::Sum + Send + Sync + std::fmt::Debug> Resampler<F> {
    fn _new(x_values: Array1<F>, target_x_values: Array1<F>, debug: bool) -> Resampler<F> {
        //let target_x_values = Array1::<F>::range(*x_values.min().unwrap(), x_values.max().unwrap().clone() + resolution, resolution);

        let digitized = digitize(
            x_values.as_slice().unwrap(),
            target_x_values.as_slice().unwrap(),
        );

        let mut slope_indices = Vec::<Vec<usize>>::new();
        let mut intercept_indices = slope_indices.clone();
        for i in 0..target_x_values.len() {
            let mut indices_between = Vec::<usize>::new();
            let mut potential_outside_behind = Vec::<(usize, usize)>::new();
            let mut potential_outside_ahead = Vec::<(usize, usize)>::new();
            let mut indices_outside = Vec::<usize>::new();
            for (j, k) in digitized.iter().enumerate() {
                match k.cmp(&i) {
                    std::cmp::Ordering::Less => {
                        potential_outside_behind.push((i - k, j));
                    }
                    std::cmp::Ordering::Greater => {
                        potential_outside_ahead.push((*k - i, j));
                    }
                    std::cmp::Ordering::Equal => {
                        indices_between.push(j);
                    }
                };
            }

            if indices_between.len() < 2 {
                potential_outside_behind.sort_by_key(|a| a.0);
                potential_outside_ahead.sort_by_key(|a| a.0);

                if let Some(point_behind) = potential_outside_behind.first() {
                    for index in potential_outside_behind
                        .iter()
                        .filter_map(|(distance, index)| {
                            (distance == &point_behind.0).then_some(index)
                        })
                    {
                        indices_outside.push(*index);
                    }
                }
                if let Some(point_ahead) = potential_outside_ahead.first() {
                    for index in potential_outside_ahead
                        .iter()
                        .filter_map(|(distance, index)| {
                            (distance == &point_ahead.0).then_some(index)
                        })
                    {
                        indices_outside.push(*index);
                    }
                }
            };
            let mut all_indices = indices_outside.clone();
            all_indices.append(indices_between.clone().as_mut());
            let indices_for_intercept = match indices_between.is_empty() {
                true => all_indices.clone(),
                false => indices_between.clone(),
            };
            let indices_for_slope = match indices_between.len() < 2 {
                true => all_indices.clone(),
                false => indices_between,
            };

            intercept_indices.push(indices_for_intercept);
            slope_indices.push(indices_for_slope);
        }

        Resampler {
            x_values,
            target_x_values,
            digitized,
            slope_indices,
            intercept_indices,
            _debug: debug,
        }
    }

    pub fn new(x_values: Array1<F>, resolution: F) -> Self {
        let target_x_values = equally_spaced_from_sparse::<F>(&x_values, resolution);
        Resampler::_new(x_values, target_x_values, false)
    }

    /// Resample onto `target_x_values` exactly, rather than onto a grid
    /// derived from the values' own range.
    pub fn new_with_target(x_values: Array1<F>, target_x_values: Array1<F>) -> Self {
        Resampler::_new(x_values, target_x_values, false)
    }

    fn _new_debug(x_values: Array1<F>, resolution: F) -> Self {
        let target_x_values = equally_spaced_from_sparse::<F>(&x_values, resolution);
        Self::_new(x_values, target_x_values, true)
    }
    fn _resample<F2: Float + std::fmt::Display + std::iter::Sum + Send + std::fmt::Debug>(
        &self,
        x_values: &Array1<F2>,
        target_x_values: &Array1<F2>,
        y_values: &ArrayView1<F2>,
    ) -> Array1<F2> {
        interpolate_ndarray::<F2>(&x_values.view(), y_values, &target_x_values.view())
    }
    pub fn resample_convert<
        F2: Float + std::fmt::Display + std::iter::Sum + Send + std::fmt::Debug,
    >(
        &self,
        y_values: &ArrayView1<F2>,
    ) -> Array1<F2> {
        let target_x_values: Array1<F2> = self.target_x_values.mapv(|v| F2::from(v).unwrap());
        let x_values = self.x_values.mapv(|v| F2::from(v).unwrap());

        self._resample(&x_values, &target_x_values, y_values)
    }
    pub fn resample(&self, y_values: &ArrayView1<F>) -> Array1<F> {
        self._resample(&self.x_values, &self.target_x_values, y_values)
    }

    /*
    pub fn resample_along_axis(&self, data: &Array2<F>, axis: Axis2D) -> Array2<F> {
        let nd_axis = match axis {
            Axis2D::Row => ndarray::Axis(0),
            Axis2D::Col => ndarray::Axis(1),
        };
        let slice = ndarray::Slice::new(0, Some(self.target_x_values.len() as isize), 1);

        let length = match axis {
            Axis2D::Row => data.shape()[0],
            Axis2D::Col => data.shape()[1],
        };

        let mut buffer = Array1::<F>::zeros(length);

        let mut data2 = data.clone();

        let axis_values = match axis {
            Axis2D::Row => data2.columns_mut(),
            Axis2D::Col => data2.rows_mut(),
        };

        for mut arr in axis_values {
            let resampled = self.resample(&arr.view());

            let mut slice = buffer.slice_axis_mut(ndarray::Axis(0), slice);
            slice.assign(&resampled);
            arr.assign(&buffer);
        }
        data2.slice_axis_inplace(nd_axis, slice);
        data2
    }
    */

    pub fn resample_along_axis_par(&self, data: &Array2<F>, axis: Axis2D) -> Array2<F> {
        let length = match axis {
            Axis2D::Row => data.shape()[1],
            Axis2D::Col => data.shape()[0],
        };

        let out_shape = match axis {
            Axis2D::Row => (self.target_x_values.len(), data.shape()[1]),
            Axis2D::Col => (data.shape()[0], self.target_x_values.len()),
        };

        let output: Vec<Array1<F>> = (0..length)
            .into_par_iter()
            .map(|i| {
                let y_vals = match axis {
                    Axis2D::Row => data.column(i),
                    Axis2D::Col => data.row(i),
                };
                self.resample(&y_vals)
            })
            .collect();

        let mut out = Array2::<F>::zeros(out_shape);

        let iterator = match axis {
            Axis2D::Row => out.columns_mut(),
            Axis2D::Col => out.rows_mut(),
        };
        for (i, mut slice) in iterator.into_iter().enumerate() {
            slice.assign(&output[i]);
        }

        out
    }
}

impl std::convert::From<Resampler<f32>> for Resampler<f64> {
    fn from(resampler: Resampler<f32>) -> Resampler<f64> {
        let x_values = resampler.x_values.mapv(f64::from);
        let target_x_values = resampler.target_x_values.mapv(f64::from);
        Resampler {
            x_values,
            target_x_values,
            digitized: resampler.digitized.clone(),
            slope_indices: resampler.slope_indices.clone(),
            intercept_indices: resampler.intercept_indices.clone(),
            _debug: resampler._debug,
        }
    }
}

#[cfg(test)]
mod tests {
    use ndarray::Array1;

    /// The files a running process has open, by `/proc`.
    #[cfg(target_os = "linux")]
    fn open_files_of(pid: u32) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap()
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .collect()
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[serial_test::serial(netcdf)]
    fn a_tool_does_not_inherit_an_open_netcdf_file() {
        // #129: a child holding an HDF5 descriptor holds its lock, and the
        // file cannot be reopened until the child exits. Checked by what
        // the child has open rather than by provoking -101, because CI turns
        // HDF5's locking off (#154) and the failure could not show there.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("open.nc");
        let file = netcdf::create(&path).unwrap();

        let holds_the_file = |mut child: std::process::Child| {
            let held = open_files_of(child.id()).contains(&path);
            child.kill().unwrap();
            child.wait().unwrap();
            held
        };
        // The control: without the helper the child does inherit it. If
        // this starts failing, HDF5 has begun opening files close-on-exec
        // and `spawn_tool` no longer has anything to do.
        assert!(
            holds_the_file(
                std::process::Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .unwrap()
            ),
            "a plain Command was expected to inherit HDF5's descriptor"
        );
        assert!(
            !holds_the_file(
                super::spawn_tool(std::process::Command::new("sleep").arg("30")).unwrap()
            ),
            "a tool started through spawn_tool inherited an open NetCDF file"
        );
        drop(file);
    }

    #[test]
    fn test_read_step_list() {
        let step_list = vec!["subset(0 200)", "equidistant_traces", "gain(0.1)"];

        let mut all_parsed = Vec::<Vec<String>>::new();

        // Parse as a comma separated list
        all_parsed.push(super::parse_step_list(&step_list.join(",")).unwrap());

        // Write a steps.txt file and try to parse it
        let temp_dir = tempfile::tempdir().unwrap();
        let step_path = temp_dir.path().join("steps.txt");
        let mut step_list_forfile = step_list.clone();
        // Add an empty line (should be removed)
        step_list_forfile.insert(2, "\n  \t     \n");
        std::fs::write(
            &step_path,
            step_list_forfile
                .iter()
                .map(|s| format!("{s} # Optional comment")) // Add comments (should be removed)
                .collect::<Vec<String>>()
                .join("\n"),
        )
        .unwrap();
        all_parsed.push(super::parse_step_list(step_path.as_os_str().to_str().unwrap()).unwrap());

        for i in 0..step_list.len() {
            for (j, parsed) in all_parsed.iter().enumerate() {
                assert_eq!(step_list[i], parsed[i], "Parsed {j} didn't work");
            }
        }
    }

    #[test]
    fn test_interpolate_between_known() {
        let known_xy0 = (0_f64, 0_f64);
        let known_xy1 = (5_f64, 10_f64);

        assert_eq!(
            super::interpolate_between_known(known_xy0, known_xy1, 2.5),
            5.0
        )
    }

    #[test]
    fn test_interpolate_values() {
        let coord0 = vec![0_f64, 0_f64, 0_f64];
        let time0 = 0_f64;

        let coord1 = vec![5_f64, 10_f64, 15_f64];
        let time1 = 1_f64;

        assert_eq!(
            super::interpolate_values(time0, &coord0, time1, &coord1, 0.5),
            vec![2.5, 5.0, 7.5]
        )
    }

    #[test]
    fn test_quantiles() {
        let values = vec![4, 1, 2, 3, 0];

        assert_eq!(super::quantiles(&values, &[0.1, 0.5, 0.9], None), [0, 2, 4]);
        assert_eq!(
            super::quantiles(&values, &[0.1, 0.5, 0.9], Some(2)),
            [0, 2, 4]
        );
    }

    #[test]
    fn test_seconds_to_rfc3339() {
        let seconds = 1_600_000_000_f64;

        assert_eq!(
            super::seconds_to_rfc3339(seconds),
            "2020-09-13T12:26:40+00:00"
        );
    }

    #[test]
    fn test_interpolate() {
        let mut tests = Vec::<[Vec<f64>; 4]>::new();

        let x = vec![1.0, 2.0, 3.0];
        let y = vec![1.0, 2.0, 3.0];
        let x_new = vec![1.5, 2.5, 0., 4.0]; // Includes values for extrapolation
        tests.push([x, y, x_new.clone(), x_new.clone()]);

        let x = vec![1.0, 3.0, 5.0];
        let y = vec![1.0, 3.0, 5.0];
        let x_new = vec![2.0, 4.0, 0.0, 6.0]; // Includes values for extrapolation

        tests.push([x, y, x_new.clone(), x_new.clone()]);

        let x = vec![0., 2., 5.];
        let y = vec![0., 4., 1.];
        let x_new = vec![-1., 0., 1., 2., 3., 4., 5., 6.];
        let y_test = vec![-2., 0., 2., 4., 3., 2., 1., 0.];

        tests.push([x, y, x_new, y_test]);

        for test_case in tests {
            let y_new = super::interpolate_vec(&test_case[0], &test_case[1], &test_case[2])
                .iter()
                .map(|v| (v * 10_f64).round() / 10.)
                .collect::<Vec<f64>>();
            assert_eq!(y_new, test_case[3]);

            let y_new_arr = super::interpolate_ndarray(
                &Array1::from_vec(test_case[0].clone()).view(),
                &Array1::from_vec(test_case[1].clone()).view(),
                &Array1::from_vec(test_case[2].clone()).view(),
            )
            .mapv(|v| (v * 10_f64).round() / 10.);

            assert_eq!(y_new_arr, Array1::from_vec(test_case[3].clone()));
        }
    }

    #[test]
    #[should_panic(expected = "x_old and y_old must have the same length")]
    fn test_interpolate_panic() {
        let x = vec![1.0, 2.0, 3.0];
        let y = vec![1.0, 2.0];
        let x_new = vec![1.5, 2.5];
        let _ = super::interpolate_vec(&x, &y, &x_new);
    }
    /*
    #[test]
    fn test_groupby_average() {
        let test_data = Array1::<f32>::range(0., 25., 1.)
            .into_shape((5, 5))
            .unwrap();

        let xs = Array1::<f32>::from_vec(vec![0., 1., 1., 2., 3.]);

        let mut test_data0 = test_data.clone();
        super::groupby_average(&mut test_data0, super::Axis2D::Row, &xs, 1.);

        assert_eq!(test_data0.shape(), &[4_usize, 5_usize]);
        let expected = (test_data.get((1, 0)).unwrap() + test_data.get((2, 0)).unwrap()) / 2.;
        assert_eq!(test_data0.get((1, 0)), Some(&expected));

        let mut test_data1 = test_data.clone();
        super::groupby_average(&mut test_data1, super::Axis2D::Col, &(xs * 2.), 2.);
        assert_eq!(test_data1.shape(), &[5_usize, 4_usize]);
        let expected = (test_data.get((0, 1)).unwrap() + test_data.get((0, 2)).unwrap()) / 2.;
        assert_eq!(test_data1.get((0, 1)), Some(&expected));
    }
    */
    #[test]
    fn test_return_time_to_depth() {
        // The depth without antenna distance should be the time * velocity / 2
        let depth = super::return_time_to_depth(200., 0.1, 0., super::SPEED_OF_LIGHT_AIR_M_PER_NS);
        assert_eq!(depth, 10.);

        // Forward-model a reflector below the midpoint and invert it (#261): the
        // pulse leaves the transmitter, travels two slant legs through the
        // medium, and is timed from the air wave's arrival at the receiver.
        for separation in [0.5_f32, 1., 2.2, 6.2] {
            for true_depth in [0.5_f32, 2., 10., 100., 500.] {
                let slant = (true_depth.powi(2) + (separation / 2.).powi(2)).sqrt();
                let since_emission = 2. * slant / 0.168;
                let since_time_zero =
                    since_emission - separation / super::SPEED_OF_LIGHT_AIR_M_PER_NS;
                let depth = super::return_time_to_depth(
                    since_time_zero,
                    0.168,
                    separation,
                    super::SPEED_OF_LIGHT_AIR_M_PER_NS,
                );
                assert!(
                    (depth - true_depth).abs() < 1e-3 * true_depth.max(1.),
                    "separation {separation} m, depth {true_depth} m: got {depth} m"
                );
            }
        }

        // Timed from the ground wave instead (ImpDAR's `nmo`), the lead is the
        // separation's flight time through the medium.
        let since_ground_wave = 2. * (10_f32.powi(2) + 1_f32).sqrt() / 0.168 - 2. / 0.168;
        let depth = super::return_time_to_depth(since_ground_wave, 0.168, 2., 0.168);
        assert!((depth - 10.).abs() < 1e-3, "{depth}");

        // The case from #261: 25 MHz on Drønbreen, 6.2 m apart, 100 m deep.
        // Using the full separation instead of half of it and ignoring the
        // air-wave lead both put the depth well away from 100 m.
        let since_time_zero = 2. * (100_f32.powi(2) + 3.1_f32.powi(2)).sqrt() / 0.168
            - 6.2 / super::SPEED_OF_LIGHT_AIR_M_PER_NS;
        let depth = super::return_time_to_depth(
            since_time_zero,
            0.168,
            6.2,
            super::SPEED_OF_LIGHT_AIR_M_PER_NS,
        );
        assert!((depth - 100.).abs() < 0.01, "{depth}");
        assert!((since_time_zero * 0.168 / 2. - 100.).abs() > 1.);

        // Before the air wave could have reached a reflector and come back, the
        // depth is 0.
        assert_eq!(
            super::return_time_to_depth(0., 0.1, 2., super::SPEED_OF_LIGHT_AIR_M_PER_NS),
            0.
        );
        assert_eq!(
            super::return_time_to_depth(2., 0.1, 2., super::SPEED_OF_LIGHT_AIR_M_PER_NS),
            0.
        );

        // A weird NAN error came up with these settings which should not occur
        let depth =
            super::return_time_to_depth(5.9624557, 0.168, 1.0, super::SPEED_OF_LIGHT_AIR_M_PER_NS);
        assert!(depth.is_finite(), "{}", depth);
    }

    #[test]
    fn test_digitize() {
        let bins = [0., 1., 2., 3.];
        let values = [-0.5, 1., 0.5, 0.8, 0.9, 2.3, 3.4];

        let expected = [0, 2, 1, 1, 1, 3, 4];

        assert_eq!(super::digitize(&values, &bins), expected);
    }

    #[test]
    fn test_resample() {
        let x_values = Array1::from_vec(vec![0., 0.01, 0.5, 0.99, 1.99, 2.05, 2.05, 5.05]);
        let y_values = Array1::from_vec(vec![1., 1., 2., 3., 4., 4., 4., 5.]);

        for i in 0..x_values.len() {
            let xval = &x_values[i];
            let yval = &y_values[i];
            println!("{i}: x={xval}, y={yval}");
        }

        let resolution = 1.;

        let mut resampler = super::Resampler::<f32>::_new_debug(x_values, resolution);
        println!("{:?}", resampler.target_x_values);

        let new_ys = resampler.resample(&y_values.view());
        let new_ys_f64 = resampler.resample_convert::<f64>(&y_values.mapv(|v| f64::from(v)).view());

        assert_eq!(new_ys_f64[0] as f32, new_ys[0]);

        assert_eq!(new_ys[0], 1.);
        assert!((new_ys[1] - 3.).abs() < 1e-1);
        assert!((new_ys[2] > 3.5) & (new_ys[2] < 5.));

        assert!(
            new_ys
                .iter()
                .max_by(|a, b| a.partial_cmp(b).unwrap())
                .unwrap()
                < &5.5
        );

        resampler._debug = false;
        let y_matrix_manyrows =
            ndarray::Array2::from_shape_fn((50, y_values.len()), |(_, j)| y_values[j]);
        let resampled_colwise =
            resampler.resample_along_axis_par(&y_matrix_manyrows, super::Axis2D::Col);
        assert_eq!(
            resampled_colwise.shape(),
            [50, resampler.target_x_values.len()]
        );

        assert!(resampled_colwise.iter().all(|v| !v.is_nan()));

        let y_matrix_manycols =
            ndarray::Array2::from_shape_fn((y_values.len(), 50), |(i, _)| y_values[i]);
        let resampled_rowwise =
            resampler.resample_along_axis_par(&y_matrix_manycols, super::Axis2D::Row);
        assert_eq!(
            resampled_rowwise.shape(),
            [resampler.target_x_values.len(), 50]
        );
        assert!(resampled_rowwise.iter().all(|v| !v.is_nan()));
    }
}
