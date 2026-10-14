//! What a radargram says of how it was surveyed: the typical trace spacing
//! and speed along it (#412), from its `distance` and `time` per trace.
//!
//! Medians, not means: a survey stops, turns, loses its GPS and stands
//! still, and a mean of those would describe none of the survey (see
//! `ARCHITECTURE.md`). Both leave out the intervals where it did not move, so
//! traces recorded at a stop, which `remove_standstills` does not always
//! take away, cannot pull them to zero.

/// The median distance between neighbouring traces that are apart, in the
/// units of `distance` (metres in a Ridal file).
///
/// `None` with fewer than two traces, or when no two neighbours are apart.
///
/// ```text
/// distance: [0, 1, 2, 2, 2, 3]  ->  Some(1)  (the stop is left out)
/// ```
pub fn median_trace_spacing(distance: &[f64]) -> Option<f64> {
    median(
        distance
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|d| d.is_finite() && *d > 0.)
            .collect(),
    )
}

/// The median speed between neighbouring traces, in the units of
/// `distance` per unit of `time` (m/s in a Ridal file, whose `time` is in
/// seconds).
///
/// Taken interval by interval: `Δdistance / Δtime` between each trace and
/// the next, over the intervals where both are positive. That needs times
/// finer than the trace interval, which every format Ridal reads gives:
/// a trace's time is interpolated between the fixes around it, as its
/// position is, so neighbouring traces almost never share a time (checked
/// on MALA files from 2005 to 2026, where steps are 0.05--0.5 s and fewer
/// than 1 in 1000 is zero). An interval at a stop, or across a gap left by
/// removed standstills, is one of many and moves the median little.
///
/// `None` with fewer than two traces, times and distances of different
/// lengths, or no interval in which the survey moved.
pub fn median_survey_speed(distance: &[f64], time: &[f64]) -> Option<f64> {
    if distance.len() != time.len() {
        return None;
    }
    median(
        distance
            .windows(2)
            .zip(time.windows(2))
            .filter_map(|(d, t)| {
                let (dd, dt) = (d[1] - d[0], t[1] - t[0]);
                (dd.is_finite() && dt.is_finite() && dd > 0. && dt > 0.).then(|| dd / dt)
            })
            .collect(),
    )
}

/// The median of `values`, the mean of the middle two for an even count.
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.
    } else {
        values[mid]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` traces `step` m and `dt` s apart.
    fn uniform(n: usize, step: f64, dt: f64) -> (Vec<f64>, Vec<f64>) {
        (
            (0..n).map(|i| i as f64 * step).collect(),
            (0..n).map(|i| 1.7e9 + i as f64 * dt).collect(),
        )
    }

    #[test]
    fn uniform_motion_is_its_own_median() {
        // Epoch seconds near 1.7e9 carry about 2e-7 s of rounding, so a
        // speed from 0.1 s steps is good to about 1e-5 of itself.
        let (distance, time) = uniform(100, 0.5, 0.1);
        assert!((median_trace_spacing(&distance).unwrap() - 0.5).abs() < 1e-9);
        assert!((median_survey_speed(&distance, &time).unwrap() - 5.).abs() < 1e-3);
    }

    #[test]
    fn a_stop_does_not_move_the_medians() {
        // 50 traces moving, 40 standing still, 50 moving again.
        let mut distance: Vec<f64> = (0..50).map(|i| i as f64 * 0.5).collect();
        let at = *distance.last().unwrap();
        distance.extend(std::iter::repeat(at).take(40));
        distance.extend((1..=50).map(|i| at + i as f64 * 0.5));
        let time: Vec<f64> = (0..distance.len()).map(|i| i as f64 * 0.1).collect();
        assert!((median_trace_spacing(&distance).unwrap() - 0.5).abs() < 1e-9);
        assert!((median_survey_speed(&distance, &time).unwrap() - 5.).abs() < 1e-3);
    }

    #[test]
    fn a_gap_from_removed_standstills_is_one_interval_of_many() {
        // A minute taken out between two halves: one interval of 60 s.
        let (mut distance, mut time) = uniform(100, 0.5, 0.1);
        for i in 50..100 {
            time[i] += 60.;
            distance[i] += 0.5;
        }
        assert!((median_trace_spacing(&distance).unwrap() - 0.5).abs() < 1e-9);
        assert!((median_survey_speed(&distance, &time).unwrap() - 5.).abs() < 1e-3);
    }

    #[test]
    fn varying_speed_gives_the_typical_one() {
        // Mostly 5 m/s, with a few fast stretches that would pull a mean.
        let mut distance = vec![0.];
        let mut time = vec![0.];
        for i in 0..100 {
            let speed = if i % 10 == 0 { 30. } else { 5. };
            time.push(time[i] + 0.1);
            distance.push(distance[i] + speed * 0.1);
        }
        assert!((median_survey_speed(&distance, &time).unwrap() - 5.).abs() < 1e-3);
    }

    #[test]
    fn nothing_to_say_without_movement_or_traces() {
        assert_eq!(median_trace_spacing(&[]), None);
        assert_eq!(median_trace_spacing(&[3.]), None);
        assert_eq!(median_survey_speed(&[3.], &[1.]), None);
        assert_eq!(median_trace_spacing(&[2., 2., 2.]), None);
        assert_eq!(median_survey_speed(&[2., 2., 2.], &[0., 1., 2.]), None);
        // Times that never advance give no speed, though the spacing stands.
        assert_eq!(median_survey_speed(&[0., 1., 2.], &[5., 5., 5.]), None);
        assert_eq!(median_trace_spacing(&[0., 1., 2.]), Some(1.));
        // Mismatched lengths are not a survey.
        assert_eq!(median_survey_speed(&[0., 1., 2.], &[0., 1.]), None);
    }
}
