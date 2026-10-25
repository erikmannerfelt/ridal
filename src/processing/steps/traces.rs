//! Steps that select, average or resample traces.

use ndarray::{Array1, Array2};
use num::{Float, FromPrimitive};

use crate::gpr::{CorPoint, Traces, GPR};
use crate::processing::steps::StepReport;
use crate::tools;

pub fn average_traces<T: Float + FromPrimitive>(
    data: &Array2<T>,
    window: usize,
) -> Result<Array2<T>, String> {
    if window <= 1 {
        return Err(format!("Window size ({window}) needs to be >= 2"));
    }

    let (nrows, ncols) = data.dim();

    if window > ncols {
        return Err(format!(
            "Averaging window ({window}) is larger than the data width ({ncols})"
        ));
    }

    let new_ncols = ncols.div_ceil(window);

    let mut averaged = Array2::<T>::zeros((nrows, new_ncols));

    for i in 0..new_ncols {
        let start = i * window;
        if start >= ncols {
            break;
        }
        let end = (start + window).min(ncols) as isize;

        let view = data.slice_axis(
            ndarray::Axis(1),
            ndarray::Slice::new(start as isize, Some(end), 1),
        );

        let mut dest = averaged.column_mut(i);

        dest.assign(
            &view
                .mean_axis(ndarray::Axis(1))
                .ok_or("Failure in averaging".to_string())?,
        );
    }

    Ok(averaged)
}

pub fn window_subset_vec<T>(mut v: Vec<T>, window: usize) -> Vec<T> {
    assert!(window >= 1, "window must be >= 1");

    let len = v.len();
    if len == 0 {
        return Vec::new();
    }

    let n_windows = len.div_ceil(window);
    let mut new_vec = Vec::with_capacity(n_windows);

    // Compute the indices of the elements we want to keep
    let mut indices = Vec::with_capacity(n_windows);
    for i in 0..n_windows {
        let start = i * window;
        if start >= len {
            break;
        }
        let end = (start + window).min(len);
        let width = end - start;

        let mid_offset = (width - 1) / 2;
        let idx = start + mid_offset;
        indices.push(idx);
    }

    // Remove elements from the end to the start so indices stay valid
    indices.sort_unstable(); // ascending
    indices.reverse(); // process from largest to smallest

    for idx in indices {
        // `remove` moves T out, no clone required
        let item = v.remove(idx);
        new_vec.push(item);
    }

    new_vec.reverse();
    new_vec
}

pub(crate) fn run_subset(
    gpr: &mut GPR,
    min_trace: Option<u32>,
    max_trace: Option<u32>,
    min_sample: Option<u32>,
    max_sample: Option<u32>,
) -> Result<StepReport, String> {
    let min_trace_ = min_trace.unwrap_or(0);
    let max_trace_ = match max_trace {
        Some(x) => x,
        None => gpr.width() as u32,
    };
    let min_sample_ = min_sample.unwrap_or(0);
    let max_sample_ = match max_sample {
        Some(x) => x,
        None => gpr.height() as u32,
    };

    let checks = [
        (min_trace_, gpr.width(), "min trace number"),
        (max_trace_, gpr.width(), "max trace number"),
        (min_sample_, gpr.height(), "min sample number"),
        (max_sample_, gpr.height(), "max sample number"),
    ];

    for (val, maxval, desc) in checks {
        if val > maxval as u32 {
            return Err(format!(
                "Subset failed: {desc} ({val}) out of the dataset bounds ({maxval})"
            ));
        }
    }

    // `validate` refuses an empty range written out in full; one that is
    // empty only against this radargram, such as `subset(400)` on 400
    // traces, is refused here.
    if max_trace_ <= min_trace_ || max_sample_ <= min_sample_ {
        return Err(format!(
            "Subset failed: traces {min_trace_}..{max_trace_} and samples \
             {min_sample_}..{max_sample_} of a {} x {} radargram keep no data",
            gpr.width(),
            gpr.height()
        ));
    }

    let event = format!(
        "Subset data from {:?} to ({}:{}, {}:{})",
        gpr.data.shape(),
        min_sample_,
        max_sample_,
        min_trace_,
        max_trace_
    );
    *gpr = gpr.window(
        min_trace_ as usize..max_trace_ as usize,
        min_sample_ as usize..max_sample_ as usize,
    );
    Ok(StepReport::new("subset", event))
}

/// How many times as many traces `equidistant_traces` may make as it was
/// given.
///
/// Four times is a quarter of the typical spacing. A step far below the
/// spacing the radar recorded at adds no information, only traces: 1 mm
/// on a 2.5 km line of 3 548 traces asks for 2.5 million of them, about
/// 7 GB of samples, which ran for minutes before it was stopped.
pub(crate) const MAX_TRACE_FACTOR: usize = 4;

pub(crate) fn run_make_equidistant(gpr: &mut GPR, step: Option<f32>) -> Result<StepReport, String> {
    const NAME: &str = "equidistant_traces";
    let distances = gpr.location.distances().mapv(|v| v as f32);
    let max_distance = distances.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    // Bad data, not a bad argument: a single trace, or a profile recorded
    // standing still, has no track to resample along.
    if gpr.width() < 2 || max_distance.is_nan() || max_distance <= 0. {
        return Ok(StepReport::warning(
            NAME,
            format!(
                "The {} traces span {max_distance} m, so there is no track to resample \
                 along. The traces were left as they are.",
                gpr.width()
            ),
        ));
    }

    let step = match step {
        Some(step) => step,
        None => {
            let velocities = gpr.location.velocities().mapv(|v| v as f32);

            let normal_velocity = tools::quantiles(&velocities, &[0.5], None)[0];

            let mut seconds_moving = 0_f32;
            for i in 1..gpr.width() {
                if velocities[i] < (0.3 * normal_velocity) {
                    continue;
                };
                seconds_moving += gpr.metadata.time_interval;
            }

            let nominal_data_width = (seconds_moving / gpr.metadata.time_interval).floor() as usize;

            let step = max_distance / (nominal_data_width as f32);
            if !(step.is_finite() && step > 0.) {
                return Ok(StepReport::warning(
                    NAME,
                    format!(
                        "No step could be derived from how fast the radar moved (got \
                         {step} m). The traces were left as they are; give a step to \
                         resample anyway."
                    ),
                ));
            }
            step
        }
    };

    // As many as `Resampler::new` makes: one per step from the start of
    // the track to past its end. Counted in floating point so a tiny step
    // cannot overflow the count.
    let new_width = (max_distance / step).ceil() + 1.;
    let limit = MAX_TRACE_FACTOR * gpr.width();
    if new_width > limit as f32 {
        return Err(format!(
            "A step of {step} m would make {new_width} traces from {} along {max_distance:.1} \
             m, more than {MAX_TRACE_FACTOR} times as many. The smallest step allowed here \
             is about {:.3} m.",
            gpr.width(),
            max_distance / (limit - 1) as f32,
        ));
    }

    if (max_distance / step).round() as usize == gpr.width() {
        return Ok(StepReport::new(
            "equidistant_traces",
            "Traces were already equidistant.",
        ));
    };

    // The topographically corrected traces are not resampled, so they
    // would no longer match; refused here rather than at export.
    if gpr.topo_data.is_some() {
        return Err("equidistant_traces cannot run after correct_topography; run it before".into());
    }
    let resampler = tools::Resampler::<f32>::new(distances, step);
    let resample = |values: Vec<f64>| -> Vec<f64> {
        resampler
            .resample_convert::<f64>(&Array1::from_vec(values).view())
            .to_vec()
    };
    let cor_points = &gpr.location.cor_points;
    let eastings = resample(cor_points.iter().map(|p| p.easting).collect());
    let northings = resample(cor_points.iter().map(|p| p.northing).collect());
    let times = resample(cor_points.iter().map(|p| p.time_seconds).collect());
    let altitudes = resample(cor_points.iter().map(|p| p.altitude).collect());
    // Resampled onto the new trace positions like every other
    // per-trace quantity here.
    let crops = resample(gpr.twtt_crop_ns().iter().map(|v| *v as f64).collect());
    let time_zeros = resample(gpr.twtt_time_zero_ns().iter().map(|v| *v as f64).collect());

    let cor_points = (0..eastings.len())
        .map(|i| CorPoint {
            trace_n: i as u32,
            time_seconds: times[i],
            easting: eastings[i],
            northing: northings[i],
            altitude: altitudes[i],
        })
        .collect();

    let traces = Traces {
        data: resampler.resample_along_axis_par(&gpr.data, tools::Axis2D::Col),
        topo_data: None,
        cor_points,
        crop_ns: crops.iter().map(|v| *v as f32).collect(),
        time_zero_ns: time_zeros.iter().map(|v| *v as f32).collect(),
    };
    gpr.replace_traces(traces)?;
    Ok(StepReport::new(
        "equidistant_traces",
        format!("Ran equidistant traces with a spacing of {step} m"),
    ))
}

/// Remove traces based on their integer index
pub(crate) fn run_remove_traces(gpr: &mut GPR, traces: &[usize]) -> Result<StepReport, String> {
    // The width will be called multiple times, so it's better to assign it statically.
    let width = gpr.width();

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

    // Faster to keep the rest than to remove these one by one.
    let traces_to_keep: Vec<usize> = (0..width).filter(|i| !unique_traces.contains(i)).collect();
    gpr.select_traces(&traces_to_keep);
    unique_traces.sort_unstable();

    Ok(StepReport::new(
        "remove_traces",
        format!("Removed trace indices {unique_traces:?}"),
    ))
}

pub(crate) fn run_average_traces(gpr: &mut GPR, window: usize) -> Result<StepReport, String> {
    let initial_width = gpr.width();
    // The same window rule for every per-trace quantity, so that what
    // survives describes the traces that survive.
    let traces = Traces {
        data: average_traces(&gpr.data, window)?,
        topo_data: gpr
            .topo_data
            .as_ref()
            .map(|topo_data| average_traces(topo_data, window))
            .transpose()?,
        cor_points: window_subset_vec(gpr.location.cor_points.clone(), window),
        crop_ns: window_subset_vec(gpr.twtt_crop_ns().to_vec(), window),
        time_zero_ns: window_subset_vec(gpr.twtt_time_zero_ns().to_vec(), window),
    };
    gpr.replace_traces(traces)?;
    gpr.metadata.time_interval *= window as f32;

    Ok(StepReport::new(
        "average_traces",
        format!(
            "Averaged traces in a window={window}. Reduced trace number from {initial_width} to {}",
            gpr.width()
        ),
    ))
}

// Remove all traces whose absolute mean is lower than the given "strength"
pub(crate) fn run_remove_empty_traces(gpr: &mut GPR, strength: f32) -> Result<StepReport, String> {
    let mut traces_to_remove = Vec::<usize>::new();

    for (i, col) in gpr.data.columns().into_iter().enumerate() {
        if let Some(mad) = col.mapv(|v| v.abs()).mean() {
            if mad > strength {
                continue;
            };
            traces_to_remove.push(i);
        }
    }

    let n_removed = traces_to_remove.len();
    run_remove_traces(gpr, &traces_to_remove)?;

    Ok(StepReport::new(
        "remove_empty_traces",
        format!("Removed {n_removed} empty traces (strength: {strength})."),
    ))
}

#[cfg(test)]
mod tests {
    use crate::gpr::test_fixtures::*;

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
        crate::processing::steps::traces::run_make_equidistant(&mut gpr, None).unwrap();

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
        crate::processing::steps::traces::run_remove_traces(&mut gpr, &[5, 5, 6]).unwrap();

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
            crate::processing::steps::traces::run_remove_traces(&mut gpr, &[18]),
            Err("Trace index 18 is out of bounds (number of traces: 18)".to_string())
        );
    }

    #[test]
    fn a_subset_cuts_the_topographically_corrected_traces_too() {
        let gpr = make_labelled_gpr(10, 5);
        let subset = subset_of(&gpr, Some(4), Some(7), None, None).unwrap();

        assert_eq!(subset.width(), 3);
        assert_eq!(subset.metadata.last_trace, 3);
        assert_eq!(
            subset.topo_data.as_ref().unwrap().row(0).to_vec(),
            vec![4., 5., 6.]
        );
    }

    #[test]
    fn equidistant_traces_after_a_topographic_correction_is_refused() {
        // The corrected traces are not resampled, so carrying on would only
        // fail later, at export, with a shape error.
        let mut gpr = make_dummy_gpr(20, 30, Some(1.));
        gpr.location.cor_points[10].easting += 5.;
        crate::processing::steps::geometry::run_correct_topography(&mut gpr);
        let refusal =
            crate::processing::steps::traces::run_make_equidistant(&mut gpr, Some(2.)).unwrap_err();
        assert!(refusal.contains("after correct_topography"), "{refusal}");
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
        crate::processing::steps::traces::run_remove_empty_traces(&mut gpr, 1.).unwrap();

        assert_eq!(gpr.width(), 17);
    }

    #[test]
    fn test_average_traces() {
        let arr = ndarray::arr2(&[[1_f32, 2_f32, 3_f32], [1_f32, 3_f32, 3_f32]]);
        assert_eq!(arr.dim(), (2, 3));

        let avg = super::average_traces(&arr, 2).unwrap();
        assert_eq!(avg.dim(), (2, 2));

        let expected_avg = ndarray::arr2(&[[1.5_f32, 3.], [2., 3.]]);

        assert_eq!((avg - expected_avg).mean(), Some(0.));

        assert_eq!(super::average_traces(&arr, 3).unwrap().dim(), (2, 1));

        assert_eq!(
            super::average_traces(&arr, 4),
            Err("Averaging window (4) is larger than the data width (3)".to_string())
        );

        assert_eq!(
            super::average_traces(&arr, 0),
            Err("Window size (0) needs to be >= 2".to_string())
        );
    }

    #[test]
    fn test_window_subset_vec() {
        // Helper to keep the assertions readable
        fn check(input_len: usize, window: usize, expected: &[usize]) {
            let v = (0..input_len).collect::<Vec<_>>();
            let out = super::window_subset_vec(v, window);
            assert_eq!(out, expected, "len={input_len}, window={window}");
        }

        // 1) Basic odd window, exact multiple: len = 9, window = 3
        // windows: [0,1,2], [3,4,5], [6,7,8] -> mids: 1, 4, 7
        check(9, 3, &[1, 4, 7]);

        // 2) Odd window with remainder: len = 10, window = 3
        // windows: [0,1,2], [3,4,5], [6,7,8], [9] -> mids: 1, 4, 7, 9
        check(10, 3, &[1, 4, 7, 9]);

        // 3) Even window, exact multiple: len = 8, window = 2
        // windows: [0,1], [2,3], [4,5], [6,7] -> left-of-middle: 0, 2, 4, 6
        check(8, 2, &[0, 2, 4, 6]);

        // 4) Even window with remainder: len = 10, window = 4
        // windows: [0,1,2,3], [4,5,6,7], [8,9] -> mids: 1, 5, 8
        check(10, 4, &[1, 5, 8]);

        // 5) Window larger than len: len = 5, window = 10
        // single window [0..5], midpoint index 2
        check(5, 10, &[2]);
    }
}
