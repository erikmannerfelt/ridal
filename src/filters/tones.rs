//! `remove_tones`: subtract narrowband interference ("tones") from each
//! trace, measured where the record holds nothing but noise.
//!
//! The case this was written for is 800 MHz MALA data from Austfonna (2023
//! and 2024), where an interferer shows up as one sinusoid per trace, at
//! a constant amplitude, over the whole record. Its frequency in the record
//! drifts slowly along the profile (660 to 570 MHz over a 35 minute profile),
//! and its phase jumps from trace to trace by an amount that drifts with it.
//! Where that jump passes a whole number of cycles, neighbouring traces
//! agree, which draws the stacked "hyperbolas"; and averaging traces (in
//! the instrument's stacking or in `average_traces`) keeps the tone there
//! and cancels it in between, which is the slow, regular rise and fall in
//! power along the profile.
//!
//! Because it is one sinusoid per trace, it can be measured below the
//! deepest reflection and subtracted everywhere:
//!
//! 1. Every trace's power spectrum over the noise window, averaged over a
//!    running window of traces so that a tone stands out from the noise
//!    and its frequency changes smoothly from trace to trace.
//! 2. In each trace's averaged spectrum, the strongest peaks that stand out
//!    from the median of the spectrum around them, refined between bins.
//! 3. For each trace on its own, the amplitude and phase of every picked
//!    frequency by a joint least-squares fit over the noise window (a
//!    constant included), and the fitted sinusoids subtracted from every
//!    sample of the trace.
//!
//! Only the sinusoids are subtracted, so what is not at a picked frequency
//! is untouched. The frequency comes from many traces and the phase from
//! one, which is what the interference looks like: a tone whose phase
//! jumps between traces cannot be removed by anything that works across
//! traces, such as `background_removal`.

use ndarray::{Array2, Axis};
use num_complex::Complex64;
use rayon::prelude::*;

/// Zero-padding factor of the spectrum, for a peak frequency that can be
/// refined between bins.
const PAD: usize = 4;

/// Width of the frequency window whose median a peak must stand out from.
const BACKGROUND_MHZ: f64 = 100.;

/// Half-width of that window in bins of the unpadded spectrum, at least.
const MIN_BACKGROUND_HALF: usize = 8;

/// Traces on each side that the spectra are averaged over at least, even
/// at the ends of the profile, where the window is otherwise shrunk to stay
/// centred.
const MIN_EDGE_HALF: usize = 10;

/// Fewer noise samples than this cannot tell tones apart.
pub const MIN_WINDOW_SAMPLES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// First sample of the noise window.
    pub first_sample: usize,
    /// Most tones to fit in each trace.
    pub max_tones: usize,
    /// Number of traces the spectra are averaged over to pick frequencies.
    pub traces: usize,
    /// How far above the median of the surrounding spectrum a peak must be,
    /// in dB.
    pub prominence_db: f32,
}

/// What was removed.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Samples in the noise window.
    pub window_samples: usize,
    /// Mean number of tones fitted per trace.
    pub mean_tones: f32,
    /// Traces with at least one tone.
    pub traces_with_tones: usize,
    /// Lowest and highest frequency of each trace's strongest tone, in MHz.
    pub strongest_mhz: Option<(f32, f32)>,
    /// Median amplitude of each trace's strongest tone, in data units.
    pub strongest_amplitude: Option<f32>,
}

/// Remove the tones measured in `data[settings.first_sample..]` from every
/// sample of `data` (samples along axis 0, traces along axis 1).
///
/// `step_ns` is the sample interval. Returns an error, and changes nothing,
/// when the noise window is shorter than [`MIN_WINDOW_SAMPLES`].
pub fn remove_tones(
    data: &mut Array2<f32>,
    step_ns: f32,
    settings: &Settings,
) -> Result<Report, String> {
    let (height, width) = data.dim();
    let n = height.saturating_sub(settings.first_sample);
    if n < MIN_WINDOW_SAMPLES {
        return Err(format!(
            "the noise window has {n} samples, and at least {MIN_WINDOW_SAMPLES} are needed"
        ));
    }
    let nfft = (n * PAD).next_power_of_two();
    let nbins = nfft / 2 + 1;
    // Frequency of a bin, in cycles per ns.
    let bin_ghz = 1. / (nfft as f64 * step_ns as f64);
    let fft = Fft::new(nfft);
    let hann: Vec<f64> = (0..n)
        .map(|i| 0.5 - 0.5 * (2. * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos())
        .collect();

    let window = |j: usize| -> Vec<f64> {
        data.column(j)
            .iter()
            .skip(settings.first_sample)
            .map(|&v| v as f64)
            .collect()
    };

    let spectra: Vec<Vec<f32>> = (0..width)
        .into_par_iter()
        .map(|j| {
            let values = window(j);
            let mean = values.iter().sum::<f64>() / n as f64;
            let mut buf = vec![Complex64::new(0., 0.); nfft];
            for (b, (v, w)) in buf.iter_mut().zip(values.iter().zip(&hann)) {
                b.re = (v - mean) * w;
            }
            fft.forward(&mut buf);
            buf[..nbins].iter().map(|c| c.norm_sqr() as f32).collect()
        })
        .collect();

    let picks = pick_frequencies(&spectra, nbins, bin_ghz, settings);
    drop(spectra);

    // Fit each trace's tones over the noise window and model them over the
    // whole trace.
    let fits: Vec<Vec<(f64, f64, f64)>> = picks
        .par_iter()
        .enumerate()
        .map(|(j, freqs)| fit_trace(&window(j), settings.first_sample, freqs))
        .collect();

    for (mut trace, fit) in data.axis_iter_mut(Axis(1)).zip(&fits) {
        for &(freq, a, b) in fit {
            let omega = 2. * std::f64::consts::PI * freq;
            for (i, v) in trace.iter_mut().enumerate() {
                let phase = omega * i as f64;
                *v -= (a * phase.cos() + b * phase.sin()) as f32;
            }
        }
    }

    let strongest: Vec<(f64, f64)> = fits
        .iter()
        .filter_map(|fit| fit.first())
        .map(|&(freq, a, b)| (freq / step_ns as f64 * 1000., a.hypot(b)))
        .collect();
    let mut amplitudes: Vec<f64> = strongest.iter().map(|&(_, amp)| amp).collect();
    amplitudes.sort_by(f64::total_cmp);
    Ok(Report {
        window_samples: n,
        mean_tones: fits.iter().map(Vec::len).sum::<usize>() as f32 / width.max(1) as f32,
        traces_with_tones: strongest.len(),
        strongest_mhz: strongest.iter().map(|&(mhz, _)| mhz as f32).fold(
            None,
            |acc: Option<(f32, f32)>, mhz| match acc {
                None => Some((mhz, mhz)),
                Some((lo, hi)) => Some((lo.min(mhz), hi.max(mhz))),
            },
        ),
        strongest_amplitude: amplitudes.get(amplitudes.len() / 2).map(|&a| a as f32),
    })
}

/// Each trace's tone frequencies, in cycles per sample, strongest first.
///
/// The spectra are averaged over a running window of `settings.traces`
/// traces (shorter at the ends), and the peaks picked from that average.
fn pick_frequencies(
    spectra: &[Vec<f32>],
    nbins: usize,
    bin_ghz: f64,
    settings: &Settings,
) -> Vec<Vec<f64>> {
    let width = spectra.len();
    let half = settings.traces / 2;
    // Never narrower than a few main lobes, or a short window's tone would
    // be its own background.
    let background_half =
        ((BACKGROUND_MHZ / 1000. / bin_ghz / 2.).round() as usize).max(MIN_BACKGROUND_HALF * PAD);
    let min_ratio = 10_f64.powf(settings.prominence_db as f64 / 10.);
    // Two bins of the unpadded spectrum: the main lobe of a Hann-windowed
    // tone, so that one tone is not picked twice.
    let min_separation = 2 * PAD;
    let nfft = (nbins - 1) * 2;

    let mut sum = vec![0_f64; nbins];
    let mut in_window = 0_usize;
    let mut added = 0_usize;
    let mut removed = 0_usize;
    let mut picks = Vec::with_capacity(width);
    let mut neighbourhood: Vec<f64> = Vec::with_capacity(2 * background_half + 1);
    for j in 0..width {
        // Centred on the trace, so that a drifting tone's frequency is not
        // biased towards the ends of the profile; except that at the very
        // ends a few traces are too noisy to pick from.
        let reach = (width - 1 - j).min(j).max(MIN_EDGE_HALF).min(half);
        let end = (j + reach + 1).min(width);
        let start = j.saturating_sub(reach);
        while added < end {
            for (s, &p) in sum.iter_mut().zip(&spectra[added]) {
                *s += p as f64;
            }
            added += 1;
            in_window += 1;
        }
        while removed < start {
            for (s, &p) in sum.iter_mut().zip(&spectra[removed]) {
                *s -= p as f64;
            }
            removed += 1;
            in_window -= 1;
        }
        // Float cancellation in the running sum must not produce negatives.
        let power: Vec<f64> = sum
            .iter()
            .map(|&s| (s / in_window as f64).max(f64::MIN_POSITIVE))
            .collect();

        // Local maxima above the lowest resolvable frequency (slower is
        // wow and DC, not a tone), by decreasing power.
        let mut candidates: Vec<usize> = (PAD + 1..nbins - 1)
            .filter(|&k| power[k] > power[k - 1] && power[k] >= power[k + 1])
            .collect();
        candidates.sort_by(|&a, &b| power[b].total_cmp(&power[a]));

        let mut chosen: Vec<usize> = Vec::new();
        for k in candidates {
            if chosen.len() == settings.max_tones {
                break;
            }
            if chosen.iter().any(|&c| c.abs_diff(k) < min_separation) {
                continue;
            }
            neighbourhood.clear();
            neighbourhood.extend(
                &power[k.saturating_sub(background_half)..(k + background_half + 1).min(nbins)],
            );
            neighbourhood.sort_by(f64::total_cmp);
            let background = neighbourhood[neighbourhood.len() / 2];
            if power[k] >= min_ratio * background {
                chosen.push(k);
            }
        }
        picks.push(
            chosen
                .into_iter()
                .map(|k| {
                    // Parabola through the log power of the peak and its
                    // neighbours.
                    let (a, b, c) = (power[k - 1].ln(), power[k].ln(), power[k + 1].ln());
                    let denominator = a - 2. * b + c;
                    let offset = if denominator < 0. {
                        (0.5 * (a - c) / denominator).clamp(-0.5, 0.5)
                    } else {
                        0.
                    };
                    (k as f64 + offset) / nfft as f64
                })
                .collect(),
        );
    }
    picks
}

/// Least-squares amplitudes of `freqs` (cycles per sample) in `values`,
/// which start at sample `first_sample` of the trace, as `(freq, a, b)`
/// for `a cos + b sin` of the phase at each sample of the whole trace.
///
/// A constant is fitted alongside and not returned. If the system is
/// singular, which only near-identical frequencies cause, nothing is.
fn fit_trace(values: &[f64], first_sample: usize, freqs: &[f64]) -> Vec<(f64, f64, f64)> {
    if freqs.is_empty() {
        return Vec::new();
    }
    let m = 1 + 2 * freqs.len();
    let mut normal = vec![0_f64; m * m];
    let mut rhs = vec![0_f64; m];
    let mut row = vec![0_f64; m];
    for (i, &v) in values.iter().enumerate() {
        let sample = (first_sample + i) as f64;
        row[0] = 1.;
        for (k, &f) in freqs.iter().enumerate() {
            let phase = 2. * std::f64::consts::PI * f * sample;
            row[1 + 2 * k] = phase.cos();
            row[2 + 2 * k] = phase.sin();
        }
        for r in 0..m {
            rhs[r] += row[r] * v;
            for c in r..m {
                normal[r * m + c] += row[r] * row[c];
            }
        }
    }
    for r in 0..m {
        for c in 0..r {
            normal[r * m + c] = normal[c * m + r];
        }
    }
    match solve(&mut normal, &mut rhs, m) {
        Some(x) => freqs
            .iter()
            .enumerate()
            .map(|(k, &f)| (f, x[1 + 2 * k], x[2 + 2 * k]))
            .collect(),
        None => Vec::new(),
    }
}

/// Solve `a x = b` (`a` is `m` by `m`, row-major) by Gaussian elimination
/// with partial pivoting. `None` if `a` is singular.
fn solve(a: &mut [f64], b: &mut [f64], m: usize) -> Option<Vec<f64>> {
    let scale = (0..m).map(|i| a[i * m + i].abs()).fold(0., f64::max);
    for col in 0..m {
        let pivot =
            (col..m).max_by(|&x, &y| a[x * m + col].abs().total_cmp(&a[y * m + col].abs()))?;
        if a[pivot * m + col].abs() <= scale * 1e-12 {
            return None;
        }
        if pivot != col {
            for c in 0..m {
                a.swap(pivot * m + c, col * m + c);
            }
            b.swap(pivot, col);
        }
        for r in col + 1..m {
            let factor = a[r * m + col] / a[col * m + col];
            for c in col..m {
                a[r * m + c] -= factor * a[col * m + c];
            }
            b[r] -= factor * b[col];
        }
    }
    let mut x = vec![0_f64; m];
    for r in (0..m).rev() {
        let tail: f64 = (r + 1..m).map(|c| a[r * m + c] * x[c]).sum();
        x[r] = (b[r] - tail) / a[r * m + r];
    }
    Some(x)
}

/// An in-place radix-2 FFT of one power-of-two length.
///
/// Small enough to keep here rather than add a dependency for one use; its
/// only job is a zero-padded power spectrum.
struct Fft {
    n: usize,
    twiddles: Vec<Complex64>,
}

impl Fft {
    fn new(n: usize) -> Self {
        assert!(n.is_power_of_two(), "FFT length {n} is not a power of two");
        let twiddles = (0..n / 2)
            .map(|k| Complex64::from_polar(1., -2. * std::f64::consts::PI * k as f64 / n as f64))
            .collect();
        Fft { n, twiddles }
    }

    fn forward(&self, buf: &mut [Complex64]) {
        let n = self.n;
        assert_eq!(buf.len(), n);
        let bits = n.trailing_zeros();
        if bits == 0 {
            return;
        }
        for i in 0..n {
            let j = i.reverse_bits() >> (usize::BITS - bits);
            if j > i {
                buf.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let stride = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let w = self.twiddles[k * stride];
                    let u = buf[start + k];
                    let v = buf[start + k + len / 2] * w;
                    buf[start + k] = u + v;
                    buf[start + k + len / 2] = u - v;
                }
            }
            len *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// A deterministic stand-in for Gaussian noise (sum of uniforms).
    struct Noise(u64);
    impl Noise {
        fn uniform(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1_u64 << 53) as f64
        }
        fn normal(&mut self) -> f64 {
            (0..12).map(|_| self.uniform()).sum::<f64>() - 6.
        }
    }

    fn settings(first_sample: usize) -> Settings {
        Settings {
            first_sample,
            max_tones: 5,
            traces: 51,
            prominence_db: 6.,
        }
    }

    fn rms(values: impl Iterator<Item = f32>) -> f32 {
        let (sum, count) = values.fold((0_f64, 0), |(s, c), v| (s + (v as f64).powi(2), c + 1));
        (sum / count as f64).sqrt() as f32
    }

    #[test]
    fn the_fft_matches_a_direct_dft() {
        let n = 64;
        let mut noise = Noise(3);
        let input: Vec<Complex64> = (0..n)
            .map(|_| Complex64::new(noise.normal(), noise.normal()))
            .collect();
        let mut fast = input.clone();
        Fft::new(n).forward(&mut fast);
        for (k, got) in fast.iter().enumerate() {
            let expected: Complex64 = input
                .iter()
                .enumerate()
                .map(|(i, x)| x * Complex64::from_polar(1., -2. * PI * (i * k) as f64 / n as f64))
                .sum();
            assert!(
                (got - expected).norm() < 1e-9,
                "bin {k}: {got} vs {expected}"
            );
        }
    }

    /// A reflection near the top, white noise throughout, and a tone whose
    /// frequency drifts along the profile and whose phase jumps between
    /// traces, as in the Austfonna data.
    fn synthetic(width: usize) -> (Array2<f32>, Array2<f32>, Array2<f32>) {
        let height = 600;
        let mut noise = Noise(7);
        let mut clean = Array2::<f32>::zeros((height, width));
        let mut tone = Array2::<f32>::zeros((height, width));
        let mut phase0 = 0_f64;
        for j in 0..width {
            // Real drift is ~15% over 20000 traces; this is far faster.
            let freq = 0.08 - 0.002 * j as f64 / width as f64;
            phase0 += 2. * PI * (0.37 + 3e-4 * j as f64);
            for i in 0..height {
                let t = i as f64 - 100.;
                let wavelet = 200. * (-(t / 6.).powi(2)).exp() * (2. * PI * 0.05 * t).cos();
                clean[[i, j]] = (wavelet + 3. * noise.normal()) as f32;
                tone[[i, j]] = (20. * (2. * PI * freq * i as f64 + phase0).cos()) as f32;
            }
        }
        let data = &clean + &tone;
        (data, clean, tone)
    }

    #[test]
    fn a_drifting_tone_is_removed_from_the_whole_trace() {
        let (mut data, clean, tone) = synthetic(400);
        let report = remove_tones(&mut data, 0.1, &settings(300)).unwrap();
        let left = &data - &clean;
        let before = rms(tone.iter().copied());
        let after = rms(left.iter().copied());
        assert!(after < 0.1 * before, "{after} of {before} left");
        // Above the noise window too, where the reflection is.
        let top = rms(left.slice(ndarray::s![..300, ..]).iter().copied());
        assert!(
            top < 0.1 * before,
            "{top} of {before} left above the window"
        );
        assert_eq!(report.window_samples, 300);
        assert_eq!(report.traces_with_tones, 400);
        let (lo, hi) = report.strongest_mhz.unwrap();
        // 0.078 to 0.08 cycles per sample at 0.1 ns per sample.
        assert!(lo > 770. && hi < 810., "{lo}-{hi} MHz");
        let amplitude = report.strongest_amplitude.unwrap();
        assert!((amplitude - 20.).abs() < 2., "{amplitude}");
    }

    #[test]
    fn noise_without_a_tone_is_left_alone() {
        let (_, clean, _) = synthetic(200);
        let mut data = clean.clone();
        let report = remove_tones(&mut data, 0.1, &settings(300)).unwrap();
        assert_eq!(report.traces_with_tones, 0);
        assert_eq!(data, clean);
    }

    #[test]
    fn two_tones_are_both_removed() {
        let (mut data, clean, tone) = synthetic(300);
        let mut second = Array2::<f32>::zeros(data.dim());
        for ((i, j), v) in second.indexed_iter_mut() {
            *v = (8. * (2. * PI * 0.21 * i as f64 + 1.3 * j as f64).cos()) as f32;
        }
        data += &second;
        let report = remove_tones(&mut data, 0.1, &settings(300)).unwrap();
        let before = rms(tone.iter().zip(&second).map(|(a, b)| a + b));
        let after = rms((&data - &clean).iter().copied());
        assert!(after < 0.1 * before, "{after} of {before} left");
        assert!(report.mean_tones >= 2., "{}", report.mean_tones);
    }

    #[test]
    fn a_short_noise_window_changes_nothing() {
        let (mut data, _, _) = synthetic(10);
        let original = data.clone();
        let err = remove_tones(&mut data, 0.1, &settings(590)).unwrap_err();
        assert!(err.contains("10 samples"), "{err}");
        assert_eq!(data, original);
    }
}
