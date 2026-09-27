//! Subtract a rolling or global mean or median, along either axis (#259).
//!
//! `dewow` works down each trace (axis 0) and removes slow drift within it.
//! `background_removal` works across the traces (axis 1) and removes what
//! every trace shares at the same sample, such as antenna ringing.

use std::fmt;
use std::str::FromStr;

use ndarray::{Array2, Axis};
use rayon::prelude::*;

/// What is subtracted: the mean or the median of the values in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Statistic {
    Mean,
    /// Less pulled by a few large values, such as the direct wave in a
    /// dewow window or one strong hyperbola among the traces.
    Median,
}

impl fmt::Display for Statistic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Statistic::Mean => "mean",
            Statistic::Median => "median",
        })
    }
}

impl FromStr for Statistic {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mean" => Ok(Statistic::Mean),
            "median" => Ok(Statistic::Median),
            _ => Err("expected `median` or `mean`".into()),
        }
    }
}

/// How long the dewow window is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DewowWindow {
    /// Two periods of the antenna's nominal frequency. Over one period a
    /// running median distorts the wavelet, and over two a running mean
    /// leaves an artefact below the direct wave; a median over two periods
    /// does neither (#259).
    Auto,
    /// A fixed window in nanoseconds.
    Ns(f32),
}

impl fmt::Display for DewowWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DewowWindow::Auto => f.write_str("auto"),
            DewowWindow::Ns(ns) => write!(f, "{ns}"),
        }
    }
}

impl FromStr for DewowWindow {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "auto" {
            return Ok(DewowWindow::Auto);
        }
        match s.parse::<f32>() {
            Ok(ns) if ns > 0. && ns.is_finite() => Ok(DewowWindow::Ns(ns)),
            _ => Err("expected `auto` or a positive number of nanoseconds".into()),
        }
    }
}

impl DewowWindow {
    /// Half the window in samples, so that the `2 * half + 1` samples span
    /// the window as closely as an odd count can.
    ///
    /// `antenna_mhz` is used by `auto` only, `step_ns` is the sample
    /// interval.
    pub fn half_samples(self, antenna_mhz: f32, step_ns: f32) -> Result<usize, String> {
        let ns = match self {
            DewowWindow::Ns(ns) => ns,
            DewowWindow::Auto if antenna_mhz > 0. && antenna_mhz.is_finite() => 2000. / antenna_mhz,
            DewowWindow::Auto => {
                return Err(format!(
                    "`dewow(auto)` needs the antenna frequency, which is {antenna_mhz} MHz; \
                     give the window in nanoseconds instead, e.g. `dewow(10)`"
                ))
            }
        };
        let half = ((ns / step_ns - 1.) / 2.).round();
        if half.is_nan() || half < 1. {
            return Err(format!(
                "a dewow window of {ns} ns is under 3 samples ({step_ns} ns apart), which \
                 would remove the signal itself; use a longer window"
            ));
        }
        Ok(half as usize)
    }
}

/// Which traces the background is taken over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceWindow {
    /// Every trace in the radargram.
    All,
    /// The given odd number of traces, centred on each trace.
    Traces(usize),
}

impl fmt::Display for TraceWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceWindow::All => f.write_str("all"),
            TraceWindow::Traces(n) => write!(f, "{n}"),
        }
    }
}

impl FromStr for TraceWindow {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            return Ok(TraceWindow::All);
        }
        match s.parse::<usize>() {
            Ok(n) if n >= 3 && n % 2 == 1 => Ok(TraceWindow::Traces(n)),
            Ok(_) => Err("a window must be an odd number of traces, at least 3".into()),
            Err(_) => Err("expected `all` or an odd number of traces".into()),
        }
    }
}

impl TraceWindow {
    /// Traces on each side of the centre, or `None` for every trace.
    pub fn half(self) -> Option<usize> {
        match self {
            TraceWindow::All => None,
            TraceWindow::Traces(n) => Some(n / 2),
        }
    }
}

/// The statistic of the `2 * half + 1` values centred on each value, with
/// fewer at the ends.
pub fn rolling(values: &[f32], half: usize, statistic: Statistic) -> Vec<f32> {
    let n = values.len();
    let bounds = |i: usize| (i.saturating_sub(half), (i + half + 1).min(n));
    match statistic {
        Statistic::Mean => {
            // Prefix sums in f64, so a long trace does not accumulate f32
            // rounding error.
            let mut prefix = Vec::with_capacity(n + 1);
            prefix.push(0_f64);
            for &v in values {
                prefix.push(prefix[prefix.len() - 1] + v as f64);
            }
            (0..n)
                .map(|i| {
                    let (start, end) = bounds(i);
                    ((prefix[end] - prefix[start]) / (end - start) as f64) as f32
                })
                .collect()
        }
        Statistic::Median => {
            // A sorted copy of the window, updated by one insertion and one
            // removal per step.
            let mut window: Vec<f32> = Vec::with_capacity(2 * half + 1);
            let (mut start, mut end) = (0, 0);
            (0..n)
                .map(|i| {
                    let (new_start, new_end) = bounds(i);
                    for &v in &values[end..new_end] {
                        let at = window.partition_point(|w| w.total_cmp(&v).is_lt());
                        window.insert(at, v);
                    }
                    for v in &values[start..new_start] {
                        let at = window.partition_point(|w| w.total_cmp(v).is_lt());
                        window.remove(at);
                    }
                    (start, end) = (new_start, new_end);
                    median_of_sorted(&window)
                })
                .collect()
        }
    }
}

fn median_of_sorted(sorted: &[f32]) -> f32 {
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.
    } else {
        sorted[mid]
    }
}

/// Subtract from every sample the statistic of the `2 * half + 1` samples
/// around it in the same trace.
///
/// `data` is `(samples, traces)`. This is a zero-phase high-pass: with
/// `half` spanning about one wavelet period, the wavelet itself averages
/// out of the window and only the drift under it is removed.
pub fn dewow(data: &mut Array2<f32>, half: usize, statistic: Statistic) {
    subtract_rolling(data, Axis(1), Some(half), statistic);
}

/// Subtract from every sample the statistic of the same sample in the
/// traces around it: the `2 * half + 1` nearest, or every trace if `half`
/// is `None`.
///
/// `data` is `(samples, traces)`.
pub fn background_removal(data: &mut Array2<f32>, half: Option<usize>, statistic: Statistic) {
    subtract_rolling(data, Axis(0), half, statistic);
}

/// Subtract the rolling statistic along each lane of `data`, where `lanes`
/// is the axis the lanes are stacked along (`Axis(1)` for traces).
fn subtract_rolling(
    data: &mut Array2<f32>,
    lanes: Axis,
    half: Option<usize>,
    statistic: Statistic,
) {
    // A lane is strided in one of the two directions, so it is copied out
    // to a contiguous buffer for the window arithmetic.
    let view = data.view();
    let references: Vec<Vec<f32>> = (0..data.len_of(lanes))
        .into_par_iter()
        .map(|i| {
            let values = view.index_axis(lanes, i).to_vec();
            match half {
                Some(half) => rolling(&values, half, statistic),
                None => vec![global(values, statistic)],
            }
        })
        .collect();
    for (mut lane, reference) in data.axis_iter_mut(lanes).zip(references) {
        match half {
            Some(_) => lane.zip_mut_with(&ndarray::ArrayView1::from(&reference), |v, r| *v -= r),
            None => lane -= reference[0],
        }
    }
}

/// The statistic of all `values`.
fn global(mut values: Vec<f32>, statistic: Statistic) -> f32 {
    if values.is_empty() {
        return 0.;
    }
    match statistic {
        Statistic::Mean => {
            (values.iter().map(|&v| v as f64).sum::<f64>() / values.len() as f64) as f32
        }
        Statistic::Median => {
            values.sort_by(f32::total_cmp);
            median_of_sorted(&values)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::{array, Array1};

    #[test]
    fn the_window_is_cut_short_at_the_ends() {
        assert_eq!(
            rolling(&[1., 2., 9., 4.], 1, Statistic::Median),
            vec![1.5, 2., 4., 6.5]
        );
        assert_eq!(
            rolling(&[1., 2., 3., 6.], 1, Statistic::Mean),
            vec![1.5, 2., 11. / 3., 4.5]
        );
    }

    #[test]
    fn the_rolling_median_matches_a_brute_force_one() {
        let values: Vec<f32> = (0..200).map(|i| ((i * 37) % 23) as f32 - 11.).collect();
        for half in [0, 1, 2, 7, 150, 400] {
            let brute: Vec<f32> = (0..values.len())
                .map(|i| {
                    let mut near =
                        values[i.saturating_sub(half)..(i + half + 1).min(values.len())].to_vec();
                    near.sort_by(f32::total_cmp);
                    median_of_sorted(&near)
                })
                .collect();
            assert_eq!(
                rolling(&values, half, Statistic::Median),
                brute,
                "half={half}"
            );
        }
    }

    #[test]
    fn dewow_removes_drift_and_keeps_a_wavelet() {
        // A 10-sample-period sine on a wow five times as large that decays
        // over 150 samples, like the one after a direct wave. With a
        // two-period window the sine averages out and the wow is removed.
        let n = 400;
        let sine = Array1::from_shape_fn(n, |i| (i as f32 * std::f32::consts::TAU / 10.).sin());
        let ramp = Array1::from_shape_fn(n, |i| 5. * (-(i as f32) / 150.).exp());
        for statistic in [Statistic::Mean, Statistic::Median] {
            let mut data = (&sine + &ramp).insert_axis(Axis(1));
            dewow(&mut data, 10, statistic);
            // Away from the ends, where the window is cut short.
            let residual = (&data.column(0) - &sine)
                .slice(ndarray::s![10..n - 10])
                .to_owned();
            let worst = residual.iter().fold(0_f32, |m, v| m.max(v.abs()));
            assert!(worst < 0.15, "{statistic}: {worst}");
        }
    }

    #[test]
    fn dewow_works_on_each_trace_separately() {
        let mut data = array![[1., 10.], [1., 20.], [1., 30.]];
        dewow(&mut data, 5, Statistic::Mean);
        assert_eq!(data, array![[0., -10.], [0., 0.], [0., 10.]]);
    }

    #[test]
    fn background_removal_takes_what_the_traces_share() {
        // Row 0 is ringing in every trace; row 1 has one strong reflector
        // that the median must leave alone.
        let mut data = array![[5., 5., 5., 5., 5.], [0., 0., 100., 0., 0.]];
        background_removal(&mut data, None, Statistic::Median);
        assert_eq!(data, array![[0., 0., 0., 0., 0.], [0., 0., 100., 0., 0.]]);

        let mut data = array![[5., 5., 5., 5., 5.], [0., 0., 100., 0., 0.]];
        background_removal(&mut data, None, Statistic::Mean);
        assert_eq!(
            data,
            array![[0., 0., 0., 0., 0.], [-20., -20., 80., -20., -20.]]
        );
    }

    #[test]
    fn a_rolling_background_follows_a_slow_change() {
        // The shared value steps from 0 to 10 halfway; a three-trace window
        // removes it everywhere except at the step itself.
        let mut data = array![[0., 0., 0., 10., 10., 10.]];
        background_removal(&mut data, Some(1), Statistic::Median);
        assert_eq!(data, array![[0., 0., 0., 0., 0., 0.]]);
        let mut data = array![[0., 0., 0., 10., 10., 10.]];
        background_removal(&mut data, Some(1), Statistic::Mean);
        let expected = array![[0., 0., -10. / 3., 10. / 3., 0., 0.]];
        assert!((&data - &expected).iter().all(|d| d.abs() < 1e-5), "{data}");
    }
}
