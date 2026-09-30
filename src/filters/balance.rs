//! `balance_traces`: even out slow changes in trace amplitude along the
//! profile ("godrays") without flattening real lateral structure.
//!
//! Written for the 800 MHz Malå ProEx data from Austfonna, where for tens of
//! seconds at a time every trace gets louder: the direct wave, the firn
//! reflections and, more than either, the noise below the deepest
//! reflection. After `siglog` those stretches are bright vertical bands.
//! Measured on 2024 mala-02, the noise in them is Gaussian, broadband and
//! incoherent from trace to trace, so it cannot be subtracted; but the
//! change is a gain, so it can be divided out.
//!
//! The gain is measured in two windows only: a shallow one, where the
//! layering is usually uniform along the profile, and the noise below the
//! deepest reflection. In each, a trace's power averaged over `traces`
//! neighbouring traces is compared with the running median over `reference`
//! traces, and the trace is scaled towards that median. Between the two
//! window centres the gain is interpolated along depth (in log), and it is
//! constant outside them. A reflector between the windows therefore never
//! feeds into the gain, which is what keeps a real bright or dark zone
//! intact. A gain measured at every depth evens those out too, which was
//! tried and rejected.

use std::fmt;
use std::str::FromStr;

use ndarray::{Array2, Axis};
use rayon::prelude::*;

use super::rolling::{self, Statistic};

/// A stretch of the profile: a number of traces, or seconds of recording.
///
/// Seconds follow the trace interval in the file header (multiplied by
/// `average_traces`), so they mean the same with or without averaging and
/// do not depend on the GPS timestamps. Traces are for when that interval
/// is missing or wrong, or no longer holds, as after `equidistant_traces`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Span {
    Traces(usize),
    Seconds(f32),
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Span::Traces(n) => write!(f, "{n}"),
            Span::Seconds(s) => write!(f, "{s}s"),
        }
    }
}

impl FromStr for Span {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let expected = || "expected a number of traces such as `51` or seconds such as `10s`";
        match s.strip_suffix('s') {
            Some(seconds) => match seconds.parse::<f32>() {
                Ok(v) if v > 0. && v.is_finite() => Ok(Span::Seconds(v)),
                _ => Err(expected().into()),
            },
            None => match s.parse::<usize>() {
                Ok(n) if n >= 1 => Ok(Span::Traces(n)),
                _ => Err(expected().into()),
            },
        }
    }
}

impl Span {
    /// The span in traces, with `seconds_per_trace` the trace interval.
    pub fn traces(self, seconds_per_trace: f32) -> Result<usize, String> {
        match self {
            Span::Traces(n) => Ok(n),
            Span::Seconds(_) if !(seconds_per_trace > 0. && seconds_per_trace.is_finite()) => {
                Err(format!(
                    "{self} cannot be converted to traces: the trace interval is \
                     {seconds_per_trace} s; give it in traces instead, e.g. `51`"
                ))
            }
            Span::Seconds(s) => Ok(((s / seconds_per_trace).round() as usize).max(1)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Samples of the shallow window, as a half-open range.
    pub shallow: (usize, usize),
    /// First sample of the deep window, which runs to the bottom.
    pub deep_start: usize,
    /// Traces the power is averaged over before it is compared.
    pub traces: usize,
    /// Traces the reference (running median) is taken over.
    pub reference: usize,
}

/// What was applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Lowest, median and highest amplitude factor in the shallow window.
    pub shallow_factor: (f32, f32, f32),
    /// Lowest, median and highest amplitude factor in the deep window.
    pub deep_factor: (f32, f32, f32),
}

/// Scale `data` (samples along axis 0, traces along axis 1) to even out
/// slow changes in power along the profile. See the module docs.
///
/// Returns an error, and changes nothing, if a window is empty or they
/// overlap.
pub fn balance_traces(data: &mut Array2<f32>, settings: &Settings) -> Result<Report, String> {
    let height = data.nrows();
    let (shallow_start, shallow_end) = settings.shallow;
    let deep_start = settings.deep_start;
    if shallow_end <= shallow_start {
        return Err("the shallow window has no samples".into());
    }
    if deep_start >= height {
        return Err("the deep window has no samples".into());
    }
    if deep_start < shallow_end {
        return Err("the deep window starts inside the shallow one".into());
    }

    let log_gain = |start: usize, end: usize| -> Vec<f32> {
        let power: Vec<f32> = (0..data.ncols())
            .into_par_iter()
            .map(|j| {
                let window = data.slice(ndarray::s![start..end, j]);
                (window.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / window.len() as f64)
                    as f32
            })
            .collect();
        let smoothed = rolling::rolling(&power, settings.traces / 2, Statistic::Mean);
        let reference = rolling::rolling(&smoothed, settings.reference / 2, Statistic::Median);
        smoothed
            .iter()
            .zip(&reference)
            .map(|(&p, &r)| {
                // A silent trace, or silent neighbourhood, is left as it is.
                if p > 0. && r > 0. {
                    0.5 * (r / p).ln()
                } else {
                    0.
                }
            })
            .collect()
    };
    let shallow = log_gain(shallow_start, shallow_end);
    let deep = log_gain(deep_start, height);

    let shallow_centre = (shallow_start + shallow_end - 1) as f32 / 2.;
    let deep_centre = (deep_start + height - 1) as f32 / 2.;
    let weights: Vec<f32> = (0..height)
        .map(|i| ((i as f32 - shallow_centre) / (deep_centre - shallow_centre)).clamp(0., 1.))
        .collect();
    for (mut trace, (&g_shallow, &g_deep)) in
        data.axis_iter_mut(Axis(1)).zip(shallow.iter().zip(&deep))
    {
        for (v, &w) in trace.iter_mut().zip(&weights) {
            *v *= ((1. - w) * g_shallow + w * g_deep).exp();
        }
    }

    let summary = |log_gains: &[f32]| {
        let mut factors: Vec<f32> = log_gains.iter().map(|g| g.exp()).collect();
        factors.sort_by(f32::total_cmp);
        (
            factors[0],
            factors[factors.len() / 2],
            factors[factors.len() - 1],
        )
    };
    Ok(Report {
        shallow_factor: summary(&shallow),
        deep_factor: summary(&deep),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEIGHT: usize = 200;
    const WIDTH: usize = 600;

    fn settings() -> Settings {
        Settings {
            shallow: (10, 40),
            deep_start: 150,
            traces: 11,
            reference: 301,
        }
    }

    /// A deterministic, noise-like value that does not repeat along either
    /// axis within the test.
    fn texture(i: usize, j: usize) -> f32 {
        let x = ((i * 7919 + j * 104_729) % 1009) as f32 / 1009.;
        (x - 0.5) * 2.
    }

    /// Uniform layering, a gain burst over traces 250-350 that is stronger
    /// at depth, and a real bright zone between the windows (samples
    /// 80-100) over traces 400-500.
    fn synthetic() -> (Array2<f32>, Array2<f32>) {
        let mut clean = Array2::<f32>::zeros((HEIGHT, WIDTH));
        let mut burst = Array2::<f32>::zeros((HEIGHT, WIDTH));
        for ((i, j), v) in clean.indexed_iter_mut() {
            let bright = if (80..100).contains(&i) && (400..500).contains(&j) {
                5.
            } else {
                1.
            };
            *v = bright * texture(i, j);
        }
        for ((i, j), g) in burst.indexed_iter_mut() {
            let depth = i as f32 / HEIGHT as f32;
            *g = if (250..350).contains(&j) {
                1.4 + 0.6 * depth
            } else {
                1.
            };
        }
        (&clean * &burst, clean)
    }

    fn rms(data: &Array2<f32>, rows: std::ops::Range<usize>, cols: std::ops::Range<usize>) -> f32 {
        let view = data.slice(ndarray::s![rows, cols]);
        (view.iter().map(|v| v * v).sum::<f32>() / view.len() as f32).sqrt()
    }

    #[test]
    fn a_gain_burst_is_divided_out_at_every_depth() {
        let (mut data, clean) = synthetic();
        balance_traces(&mut data, &settings()).unwrap();
        // Inside the burst, away from its edges where the averaging blurs it.
        for rows in [10..40, 60..80, 150..200] {
            let got = rms(&data, rows.clone(), 270..330);
            let expected = rms(&clean, rows.clone(), 270..330);
            assert!(
                (got / expected - 1.).abs() < 0.1,
                "{rows:?}: {got} vs {expected}"
            );
        }
    }

    #[test]
    fn a_bright_zone_between_the_windows_is_kept() {
        let (mut data, clean) = synthetic();
        balance_traces(&mut data, &settings()).unwrap();
        let got = rms(&data, 80..100, 420..480);
        let expected = rms(&clean, 80..100, 420..480);
        assert!((got / expected - 1.).abs() < 0.05, "{got} vs {expected}");
    }

    #[test]
    fn uniform_data_is_left_alone() {
        let (_, clean) = synthetic();
        let mut data = clean.slice(ndarray::s![.., ..250]).to_owned();
        let original = data.clone();
        let report = balance_traces(&mut data, &settings()).unwrap();
        let worst = (&data - &original)
            .iter()
            .zip(&original)
            .map(|(d, o)| (d / o.abs().max(1e-3)).abs())
            .fold(0_f32, f32::max);
        assert!(worst < 0.2, "{worst}");
        assert!(report.shallow_factor.1 > 0.95 && report.shallow_factor.1 < 1.05);
    }

    #[test]
    fn a_span_is_traces_or_seconds() {
        assert_eq!("51".parse(), Ok(Span::Traces(51)));
        assert_eq!("10s".parse(), Ok(Span::Seconds(10.)));
        assert_eq!("2.5s".parse(), Ok(Span::Seconds(2.5)));
        for bad in ["0", "0s", "-1s", "s", "ten", "1.5", "infs"] {
            assert!(bad.parse::<Span>().is_err(), "{bad}");
        }
        assert_eq!(Span::Seconds(10.).to_string(), "10s");
        // 0.2 s per trace, as after `average_traces(2)` at 0.1 s.
        assert_eq!(Span::Seconds(10.).traces(0.2), Ok(50));
        assert_eq!(Span::Seconds(0.01).traces(0.2), Ok(1));
        assert_eq!(Span::Traces(51).traces(f32::NAN), Ok(51));
        let err = Span::Seconds(10.).traces(0.).unwrap_err();
        assert!(err.contains("give it in traces"), "{err}");
    }

    #[test]
    fn bad_windows_change_nothing() {
        let (mut data, _) = synthetic();
        let original = data.clone();
        for (shallow, deep_start, fragment) in [
            ((40, 10), 150, "shallow window"),
            ((10, 40), HEIGHT, "deep window has no samples"),
            ((10, 160), 150, "inside the shallow one"),
        ] {
            let err = balance_traces(
                &mut data,
                &Settings {
                    shallow,
                    deep_start,
                    ..settings()
                },
            )
            .unwrap_err();
            assert!(err.contains(fragment), "{err}");
            assert_eq!(data, original);
        }
    }
}
