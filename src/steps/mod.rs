//! Processing steps: one definition per step (#85).
//!
//! Each step is a variant of [`Step`], and that variant is the whole
//! definition: its name, its documentation (the doc comment), its typed
//! arguments with their defaults and validation (the fields), and what it
//! does ([`Step::apply`]). The CLI help, the Python descriptions and
//! `steps.md` are all generated from it, so none of them can drift from
//! what actually runs.
//!
//! clap does the argument work. A step call is turned into an argv --
//! `dewow(10)` becomes `["dewow", "--window=10"]` -- and parsed as a clap
//! subcommand. Every argument is a named `--option` to clap; positional
//! arguments are mapped to names here, in declaration order. That keeps one
//! code path for `dewow(10)` and `dewow(window=10)`, and the `--name=value`
//! form means a negative value such as `-1` is never mistaken for a flag.
//!
//! [`crate::gpr::GPR::process`] runs a step through here, then checks that
//! it logged what it did and that the per-trace metadata still lines up
//! with the data.

pub mod parse;

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{CommandFactory, FromArgMatches, Parser};

use crate::filters::{rolling, zero_corr};
use crate::gpr::GPR;
use parse::{RawStep, Span};

/// Every registered step, in the order `steps.md` lists them.
///
/// The doc comment of a variant is its user-facing documentation: the first
/// paragraph is the summary, the rest is detail. Examples in backticks that
/// call a registered step are parsed by a test, so they cannot rot. Field
/// docs describe the arguments; their defaults come from the `#[arg]`
/// attributes and are never repeated in prose.
///
/// Every variant needs `#[command(rename_all = "snake_case")]`: the one on
/// the enum names the steps, and clap does not carry it down to the
/// arguments, which would otherwise be `min-trace`.
#[derive(Debug, Clone, PartialEq, clap::Subcommand)]
#[command(rename_all = "snake_case")]
pub enum Step {
    /// Subset the data in x (traces) and/or y (samples).
    ///
    /// Indices are zero-based and the end is exclusive; `-1` means "to the
    /// end". Clip to the first 500 samples: `subset(0 -1 0 500)`. Clip to the
    /// first 300 traces: `subset(0 300)`. At least one argument is required;
    /// the rest keep their defaults, so `subset(max_sample=1000)` crops the
    /// height on its own.
    #[command(rename_all = "snake_case")]
    Subset {
        /// First trace to keep.
        #[arg(long, default_value_t = 0)]
        min_trace: u32,
        /// Trace to stop before, or -1 for the last one.
        #[arg(long, default_value = "-1")]
        max_trace: End,
        /// First sample to keep.
        #[arg(long, default_value_t = 0)]
        min_sample: u32,
        /// Sample to stop before, or -1 for the last one.
        #[arg(long, default_value = "-1")]
        max_sample: End,
    },
    /// Manually remove trace indices, for example in case they are visually
    /// deemed bad.
    ///
    /// Remove the first two traces: `remove_traces(0 1)`. Inclusive ranges
    /// are allowed too: `remove_traces(0 5-9)`.
    #[command(rename_all = "snake_case")]
    RemoveTraces {
        /// Trace indices or inclusive ranges (`5-9`) to remove.
        #[arg(long, required = true)]
        traces: Vec<TraceSelection>,
    },
    /// Remove all traces that appear empty.
    ///
    /// Recommended to be run as the first filter if required! The strength
    /// threshold (mean absolute trace value) can be tweaked. Example:
    /// `remove_empty_traces(2)`.
    #[command(rename_all = "snake_case")]
    RemoveEmptyTraces {
        /// Mean absolute trace value below which a trace counts as empty.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_EMPTY_TRACE_STRENGTH)]
        strength: f32,
    },
    /// Replace each stretch recorded while the radar stood still with its
    /// median trace.
    ///
    /// A time-triggered radar keeps recording when it stops, and in a
    /// standstill each trace repeats the one before it. This is found from
    /// the radar data alone, without the positions, by how coherent
    /// neighbouring traces are below the direct wave. Each stretch is
    /// scored as a robust z against the rest of the profile, and a stretch
    /// is a standstill when it reaches `strength` and lasts at least
    /// `min_duration`, given in seconds of recording (`5s`) or traces
    /// (`25`). Seconds use the trace interval in the file header, which
    /// `average_traces` keeps up to date.
    ///
    /// The log lists every standstill with its traces, duration and
    /// strength, and the highest strength elsewhere, so the threshold can
    /// be judged against both. Each median trace keeps the time and
    /// position of the middle of its stretch, or of the first or last trace
    /// for a standstill at the start or end of the profile, so that
    /// interpretations made before this step still carry over.
    ///
    /// Run it early, before the filters. A running `background_removal`
    /// takes out standstills as long as its window before this step can see
    /// them, and after `dewow` it missed one of eight on 800 MHz data.
    /// Run it before `equidistant_traces` too, which it can replace when the
    /// positions are poor. Standstills that together make up more than half
    /// the profile are not found. Examples:
    /// `remove_standstills`, `remove_standstills(6)`,
    /// `remove_standstills(min_duration=20)`.
    ///
    /// So far this has only been tested on Malå data (25 and 800 MHz).
    /// Check the result before relying on it for other instruments.
    #[command(rename_all = "snake_case")]
    RemoveStandstills {
        /// How far a standstill must stand out from the rest of the profile,
        /// as a robust z. Lower finds more.
        #[arg(long, default_value_t = crate::filters::standstill::DEFAULT_STRENGTH,
              value_parser = finite_positive)]
        strength: f32,
        /// The shortest standstill: seconds (`5s`) or traces (`25`).
        #[arg(long, default_value = "5s")]
        min_duration: crate::filters::balance::Span,
    },
    /// Average traces in a given window.
    ///
    /// The coordinate information is picked from the middle averaged trace.
    /// Example: `average_traces(3)`.
    #[command(rename_all = "snake_case")]
    AverageTraces {
        /// Number of traces to average.
        #[arg(long)]
        window: usize,
    },
    /// Move time zero to where the direct wave starts, and crop what came
    /// before it.
    ///
    /// `method` decides how each trace's direct wave is found, and so what
    /// the traces are aligned on. `coppens` takes the steepest rise of the
    /// smoothed energy ratio, or the rise of a weaker leading lobe that runs
    /// into it out of the noise. `first_break` takes the first sample more than
    /// `sigma` noise standard deviations out of the noise, and mostly agrees
    /// with `coppens`. `aic` splits the record where it best divides into
    /// noise and signal, which puts it at the start of a gradual rise, often
    /// a sample earlier. `max_peak` takes the direct wave's largest value
    /// with the sign that most traces' largest value has, and survives
    /// noisy or corrupted first samples best. `legacy` is the
    /// pre-0.7 threshold on the mean trace, and also subtracts the mean of
    /// what it crops. All but `legacy` look for the direct wave around the
    /// first strong arrival, and none of them depend on the amplitude scale.
    /// The onset methods only accept an onset where the signal stays out of
    /// the noise for most of the next quarter period, so isolated early
    /// samples do not start the direct wave.
    ///
    /// `time_zero` says which feature of the direct wave time zero goes on,
    /// `onset` or `peak`, whichever method aligned the traces. When the
    /// method finds the other feature, time zero moves by the median
    /// distance between the two over the traces, so every method means the
    /// same time zero by default.
    ///
    /// `scope` is `global`, one time zero from the mean trace; `trace`, one
    /// per trace; or `smooth`, one per trace from the running median of the
    /// per-trace picks over `window` traces, for a time zero that drifts
    /// slowly and would otherwise gain the scatter of single picks.
    /// Per-trace picks that stray from their neighbours by more than three
    /// quarters of a period, and whose distance to the other end of their
    /// own direct wave is also unusual, are replaced, and the bottom is
    /// trimmed so that no trace is zero-padded.
    ///
    /// `margin` keeps some record above time zero, the same amount in every
    /// trace, and the travel times of those samples are negative. `auto`
    /// keeps back to where the direct wave starts: nothing with
    /// `time_zero=onset`, and the start of the wavelet with `time_zero=peak`.
    /// Examples: `zero_corr(coppens, trace)`, `zero_corr(max_peak, trace)`,
    /// `zero_corr(coppens, smooth, window=101)`,
    /// `zero_corr(max_peak, trace, peak)`,
    /// `zero_corr(coppens, margin=5)`, `zero_corr(first_break, sigma=4)`,
    /// `zero_corr(legacy, factor=0.9)`.
    #[command(rename_all = "snake_case")]
    ZeroCorr {
        /// `coppens`, `first_break`, `aic`, `max_peak` or `legacy`.
        #[arg(long, default_value = "coppens")]
        method: zero_corr::Method,
        /// `global`, `trace` or `smooth`.
        #[arg(long, default_value = "global")]
        scope: zero_corr::Scope,
        /// Where on the direct wave time zero goes: `onset` or `peak`.
        #[arg(long, default_value = "onset")]
        time_zero: zero_corr::Reference,
        /// How much record to keep above time zero: `auto`, back to where
        /// the direct wave starts, or a number of nanoseconds.
        #[arg(long, default_value = "auto")]
        margin: zero_corr::Margin,
        /// `legacy` only: multiplier on the first-rise threshold; lower
        /// picks earlier.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_ZERO_CORR_FACTOR)]
        factor: f32,
        /// `first_break` only: how many noise standard deviations count as
        /// signal.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_ZERO_CORR_SIGMA)]
        sigma: f32,
        /// `smooth` only: how many traces the running median of the picks
        /// spans.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_ZERO_CORR_SMOOTH_WINDOW,
              value_parser = clap::value_parser!(u32).range(2..))]
        window: u32,
    },
    /// Apply a zero-phase bandpass filter to each trace individually.
    ///
    /// The given frequencies are normalized (0: 0Hz, 1: Nyquist). Example
    /// (with default values): `bandpass(0.1 0.9)`.
    ///
    /// A high-pass and a low-pass section (Butterworth at the default `q`)
    /// are run forward and then backward along the trace, so reflections
    /// keep their shape and position. The two passes square the response:
    /// each cutoff is where the amplitude has fallen to half (-6 dB).
    #[command(rename_all = "snake_case")]
    Bandpass {
        /// Lower cutoff, as a fraction of the Nyquist frequency.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_BANDPASS_LOW_CUTOFF)]
        low: f32,
        /// Upper cutoff, as a fraction of the Nyquist frequency.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_BANDPASS_HIGH_CUTOFF)]
        high: f32,
        /// Filter strength (quality factor). Must be above 0.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_BANDPASS_Q)]
        q: f32,
    },
    /// Apply a zero-phase bandpass filter to each trace individually, with
    /// the frequencies in MHz.
    ///
    /// Example: `bandpass_mhz(100 800)`. Filters as `bandpass` does, so each
    /// cutoff is at -6 dB.
    #[command(rename_all = "snake_case")]
    BandpassMhz {
        /// Lower cutoff, in MHz.
        #[arg(long)]
        low: f32,
        /// Upper cutoff, in MHz.
        #[arg(long)]
        high: f32,
        /// Filter strength (quality factor). Must be above 0.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_BANDPASS_Q)]
        q: f32,
    },
    /// Make all traces equidistant by resampling them in a fixed horizontal
    /// grid.
    ///
    /// Unless provided, the step size is determined from the median moving
    /// velocity. Other step sizes in m can be given, e.g.
    /// `equidistant_traces(2.)` for 2 m.
    #[command(rename_all = "snake_case")]
    EquidistantTraces {
        /// Distance between traces, in m. Determined from the data if left
        /// out.
        #[arg(long)]
        step: Option<f32>,
    },
    /// Shift trace coordinates along the track.
    ///
    /// Useful if the location data were collected away from the GPR antenna.
    /// Edge coordinates are clamped to the min/max bounds of the original
    /// data. Example for moving the location data (along-track) forward 3 m
    /// (if the GPR is ahead of the GNSS), down 2 m (GNSS mounted on a pole)
    /// and (cross-track) right 1 m (GNSS mounted on the left):
    /// `shift_coordinates(3 -2 1)`
    #[command(rename_all = "snake_case")]
    ShiftCoordinates {
        /// Along-track shift in m; positive is forward.
        #[arg(long)]
        along_track: f64,
        /// Vertical shift in m; positive is up.
        #[arg(long, default_value_t = 0.)]
        altitude: f64,
        /// Cross-track shift in m; positive is right.
        #[arg(long, default_value_t = 0.)]
        cross_track: f64,
    },
    /// Remove slow drift ("wow") from each trace by subtracting the running
    /// median or mean of the samples around each sample.
    ///
    /// This is a zero-phase high-pass that works on each trace separately.
    /// `auto` makes the window two periods of the antenna's nominal
    /// frequency, which removes drift slower than that and keeps the
    /// wavelet. A window much shorter than a period removes the signal
    /// itself. The median is the default because the mean is pulled by the
    /// strong direct wave and leaves an artefact below it, and a median
    /// over only one period distorts the wavelet. Examples: `dewow`,
    /// `dewow(10)` for a 10 ns window, `dewow(method=mean)`.
    #[command(rename_all = "snake_case")]
    Dewow {
        /// `auto`, two periods of the antenna frequency, or a window in
        /// nanoseconds.
        #[arg(long, default_value = "auto")]
        window: rolling::DewowWindow,
        /// `median` or `mean`.
        #[arg(long, default_value = "median")]
        method: rolling::Statistic,
    },
    /// Remove what the traces share at the same sample, such as antenna
    /// ringing and horizontal banding, by subtracting the median or mean
    /// trace.
    ///
    /// `traces` is `all`, one background for the whole radargram, or an odd
    /// number of traces for a running background centred on each trace,
    /// which follows ringing that changes along the profile. Anything
    /// horizontal and as long as the window is removed too, including a
    /// flat bed or the direct wave, so a running window should be much
    /// longer than any flat reflector worth keeping. The median keeps a
    /// reflector found in fewer than half the traces of the window intact;
    /// the mean spreads a fraction of it into every trace. Examples:
    /// `background_removal`, `background_removal(501)`,
    /// `background_removal(all, mean)`.
    #[command(rename_all = "snake_case")]
    BackgroundRemoval {
        /// `all`, or an odd number of traces for a running background.
        #[arg(long, default_value = "all")]
        traces: rolling::TraceWindow,
        /// `median` or `mean`.
        #[arg(long, default_value = "median")]
        method: rolling::Statistic,
    },
    /// Remove narrowband interference ("tones") that is measured below the
    /// deepest reflection and subtracted from the whole trace.
    ///
    /// A continuous interferer can appear in every trace as a sinusoid of
    /// constant amplitude from top to bottom, whose frequency drifts slowly
    /// along the profile and whose phase jumps from trace to trace. It
    /// draws stacks of hyperbola-like stripes where neighbouring traces
    /// happen to agree in phase, and averaging traces turns it into a slow,
    /// regular rise and fall of power along the profile.
    ///
    /// `start` is the travel time in ns below which the record is taken to
    /// be only noise. There, each trace's spectrum is averaged over
    /// `traces` neighbouring traces, and up to `max_tones` peaks that stand
    /// `prominence` dB above the median of the spectrum within 50 MHz of
    /// them (wider for a short noise window) are taken as tones. Their amplitudes and phases are then fitted
    /// in each trace on its own, and only those sinusoids are subtracted,
    /// from every sample. Anything above `start` that is not at a picked
    /// frequency is left as it was.
    ///
    /// The tones are sinusoids in the samples as recorded, so run this
    /// first: before `average_traces`, which blurs a tone whose phase jumps
    /// between traces, and before `correct_antenna_separation`, which
    /// resamples the traces. If the record is too short below `start`,
    /// nothing is removed and the log says so. Examples:
    /// `remove_tones(150)`, `remove_tones(150, max_tones=2)`.
    ///
    /// So far this has only been tested on Malå ProEx 800 MHz data. Check
    /// the result before relying on it for other instruments or antennas.
    #[command(rename_all = "snake_case")]
    RemoveTones {
        /// Travel time in ns below which the record is only noise.
        #[arg(long, value_parser = finite)]
        start: f32,
        /// Most tones to remove from each trace. At least 1.
        #[arg(long, default_value_t = 5,
              value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
        max_tones: usize,
        /// How many traces the spectra are averaged over to find the tones.
        #[arg(long, default_value_t = 201,
              value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
        traces: usize,
        /// How far a tone must stand above the surrounding spectrum, in dB.
        #[arg(long, default_value_t = 6., value_parser = finite_positive)]
        prominence: f32,
    },
    /// Even out slow rises and falls of amplitude along the profile, such as
    /// bright vertical bands ("godrays"), without evening out real bright or
    /// dark zones.
    ///
    /// The gain is measured in two windows: a shallow one from
    /// `shallow_start` to `shallow_end`, where the layering should look the
    /// same along the profile, and from `deep` to the bottom, where the
    /// record should be only noise. In each, a trace's power averaged over
    /// `traces` neighbouring traces is compared with its running median over
    /// `reference` traces, and the trace is scaled towards that median.
    /// Between the two windows the gain changes smoothly with depth, and it
    /// is constant above and below them. Nothing between the windows is
    /// measured, so a reflector there keeps its brightness. All times are
    /// travel times in ns.
    ///
    /// Changes shorter than `traces` are too short to be measured, and
    /// changes longer than `reference` are kept. Both are seconds of
    /// recording, with an `s` (`10s`), or a number of traces (`51`).
    /// Seconds use the trace interval in the file header, which
    /// `average_traces` keeps up to date and GPS timestamps do not affect;
    /// give traces if that interval is missing or wrong, or after
    /// `equidistant_traces`, after which a trace is a distance and not a
    /// time. On a profile shorter than `reference`, the reference is the
    /// median of the whole profile.
    ///
    /// Amplitudes are no longer comparable along the profile afterwards,
    /// only within each stretch of it. If a window is outside the record,
    /// or seconds cannot be converted to traces, nothing is changed and the
    /// log says so. Examples: `balance_traces(150)`,
    /// `balance_traces(150, shallow_start=5, shallow_end=30)`,
    /// `balance_traces(150, traces=51, reference=2001)`.
    ///
    /// So far this has only been tested on Malå ProEx 800 MHz data. Check
    /// the result before relying on it for other instruments or antennas.
    #[command(rename_all = "snake_case")]
    BalanceTraces {
        /// Travel time in ns below which the record is only noise.
        #[arg(long, value_parser = finite)]
        deep: f32,
        /// Start of the shallow window, in ns.
        #[arg(long, default_value_t = 10., value_parser = finite)]
        shallow_start: f32,
        /// End of the shallow window, in ns.
        #[arg(long, default_value_t = 40., value_parser = finite)]
        shallow_end: f32,
        /// How much of the profile the power is averaged over before it is
        /// compared: seconds (`10s`) or traces (`51`).
        #[arg(long, default_value = "10s")]
        traces: crate::filters::balance::Span,
        /// How much of the profile the reference (running median) spans:
        /// seconds (`400s`) or traces (`2001`).
        #[arg(long, default_value = "400s")]
        reference: crate::filters::balance::Span,
    },
    /// Measure the gain that levels the amplitude below the direct wave, and
    /// apply it with `gain`.
    ///
    /// The samples are split into bins from top to bottom, and each bin's
    /// level is the median absolute amplitude over all its samples and
    /// traces. The direct wave's ring-down is skipped, and the gain is the
    /// median decrease in level between neighbouring bins below it, in dB/ns.
    /// This is a display gain, not an attenuation estimate. If the amplitude
    /// grows with time, or no gain can be measured (e.g. too short a record),
    /// no gain is applied and the log says why. The number of bins can be
    /// given, e.g. `auto_gain(100)`.
    #[command(rename_all = "snake_case")]
    AutoGain {
        /// Number of vertical bins. At least 2.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_AUTOGAIN_N_BINS,
              value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(2..))]
        n_bins: usize,
    },
    /// Multiply the magnitude as a function of depth.
    ///
    /// This is most often used to correct for signal attenuation with
    /// time/distance. Gain is applied as: '10 ^(gain * twtt / 20)' (dB / ns)
    /// where gain is the given gain factor and twtt is the two-way travel
    /// time of the signal. Example: `gain(0.002)`.
    #[command(rename_all = "snake_case")]
    Gain {
        /// Gain factor, in dB/ns.
        #[arg(long)]
        factor: f32,
    },
    /// Migrate sample magnitudes in the horizontal and vertical distance
    /// dimension to correct hyperbolae in the data.
    ///
    /// The correction is needed because the GPR does not observe only what
    /// is directly below it, but rather in a cone that is determined by the
    /// dominant antenna frequency. Thus, without migration, each trace is the
    /// sum of a cone beneath it. Topographic Kirchhoff migration (in 2D)
    /// corrects for this in two dimensions.
    #[command(rename_all = "snake_case")]
    KirchhoffMigration2d,
    /// Run a log10 operation on the absolute values (log10(abs(data))),
    /// converting it to a logarithmic scale.
    ///
    /// This is useful for visualization. Before conversion, the data are
    /// added with the 1st percentile (absolute) value in the dataset to avoid
    /// log10(0) == inf.
    #[command(rename_all = "snake_case")]
    Abslog,
    /// Run a log10 operation on absolute values and then account for the
    /// sign.
    ///
    /// Values smaller than the set minimum magnitude are truncated to zero.
    /// E.g. with an exponent offset of 0: 1000 -> 3, -1000 -> -3, 0.001 -> 0.
    /// The argument specifies the exponent offset to apply to allow for
    /// values smaller than +-1 (e.g. 10e-1).
    #[command(rename_all = "snake_case")]
    Siglog {
        /// Exponent offset (log10 of the smallest magnitude kept).
        #[arg(long, default_value_t = crate::filters::siglog::DEFAULT_SIGLOG_MINVAL_LOG10)]
        minval_log10: f32,
    },
    /// Run `siglog` at a strength set from the data's own noise floor instead
    /// of a fixed one.
    ///
    /// The strength is `log10(noise) + offset`, where the noise floor is the
    /// median `log10|value|` over the whole radargram (zeros excluded). The
    /// same offset therefore gives the same result regardless of the
    /// recording's amplitude scale, which a fixed `siglog` strength cannot.
    /// More negative offsets keep more weak signal (and noise). The resolved
    /// strength is written to the processing log. Example:
    /// `adaptive_siglog(-1.7)`.
    #[command(rename_all = "snake_case")]
    AdaptiveSiglog {
        /// Offset from the noise floor, in log10 units.
        #[arg(long, default_value_t = crate::filters::siglog::DEFAULT_ADAPTIVE_SIGLOG_OFFSET)]
        offset: f32,
    },
    /// Combine the positive and negative phases of the signal into one
    /// positive magntiude.
    ///
    /// The assumption is made that the positive magnitude of the signal comes
    /// first, followed by an offset negative component. The distance between
    /// the positive and negative peaks are found, and then the negative part
    /// is shifted accordingly.
    #[command(rename_all = "snake_case")]
    Unphase,
    /// Make a copy of the data and topographically correct it.
    ///
    /// In the output, the data will be called
    /// "data_topographically_corrected". Note that the copying means any step
    /// run after this will not be reflected in
    /// "data_topographically_corrected". This is thus recommended to run
    /// last.
    #[command(rename_all = "snake_case")]
    CorrectTopography,
    /// Correct for the separation between the antenna transmitter and
    /// receiver.
    ///
    /// With the transmitter and receiver apart, a reflection travels two
    /// slant legs, and time zero (the air wave's arrival at the receiver)
    /// comes after the pulse left the transmitter. Depth is therefore not
    /// linear in travel time, least of all near the surface. This step
    /// resamples each trace so that each sample represents a consistent
    /// depth interval.
    ///
    /// Afterwards, `twtt` is the travel time a coincident transmitter and
    /// receiver would have recorded rather than the travel time between the
    /// pair, and the output declares that with `twtt:anchor_name =
    /// "twtt_normal_incidence"` and `antenna_separation_effective = 0`.
    ///
    /// `legacy` is the conversion before 0.7, which used the full separation
    /// where the geometry needs half and, after a zero correction, no
    /// separation at all. It exists to regenerate data processed with it,
    /// and picks made on its grid, exactly.
    ///
    /// `direct_velocity` is the velocity of the wave that time zero was
    /// picked on. The default is the speed of light in air, because the air
    /// wave arrives first. Pass the medium velocity to time it from the
    /// ground wave, as ImpDAR's `nmo` does. Only this step reads it: the
    /// depth axis of an uncorrected radargram always assumes air.
    /// Examples: `correct_antenna_separation(legacy)`,
    /// `correct_antenna_separation(slant, 0.168)`.
    #[command(rename_all = "snake_case")]
    CorrectAntennaSeparation {
        /// `slant` or `legacy`.
        #[arg(long, default_value = "slant")]
        method: crate::gpr::SeparationMethod,
        /// `slant` only: the direct wave's velocity, in m/ns.
        #[arg(long, default_value_t = crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
              value_parser = finite_positive)]
        direct_velocity: f32,
    },
    /// Multiply all values by a constant factor.
    ///
    /// This is useful e.g. for standardizing data between sensors and antenna
    /// frequencies. Example: `multiply(5)`.
    #[command(rename_all = "snake_case")]
    Multiply {
        /// Factor to multiply by. Must be finite and non-zero.
        #[arg(long, value_parser = finite_nonzero)]
        factor: f32,
    },
}

/// One `remove_traces` argument: a trace index or an inclusive range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceSelection {
    Index(usize),
    /// `start-end`, both included.
    Range(usize, usize),
}

impl FromStr for TraceSelection {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let index = |part: &str| {
            part.parse::<usize>().map_err(|_| {
                "expected a trace index such as `5` or a range such as `5-9`".to_string()
            })
        };
        match s.split_once('-') {
            None => index(s).map(TraceSelection::Index),
            Some((start, end)) => {
                let (start, end) = (index(start)?, index(end)?);
                if start > end {
                    return Err(format!("the range {s} runs backwards"));
                }
                Ok(TraceSelection::Range(start, end))
            }
        }
    }
}

fn finite_nonzero(s: &str) -> Result<f32, String> {
    let value: f32 = s.parse().map_err(|_| "expected a number".to_string())?;
    if value == 0. {
        Err("cannot be zero".into())
    } else if !value.is_finite() {
        Err("must be finite".into())
    } else {
        Ok(value)
    }
}

fn finite(s: &str) -> Result<f32, String> {
    let value: f32 = s.parse().map_err(|_| "expected a number".to_string())?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err("must be finite".into())
    }
}

fn finite_positive(s: &str) -> Result<f32, String> {
    let value: f32 = s.parse().map_err(|_| "expected a number".to_string())?;
    if !value.is_finite() {
        Err("must be finite".into())
    } else if value <= 0. {
        Err("must be above 0".into())
    } else {
        Ok(value)
    }
}

/// The end of a half-open index range: an index, or `-1` for "to the end".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct End(pub Option<u32>);

impl FromStr for End {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "-1" => Ok(End(None)),
            _ => s
                .parse::<u32>()
                .map(|v| End(Some(v)))
                .map_err(|_| "expected a non-negative index or -1 for the end".into()),
        }
    }
}

/// The clap root the steps hang off. Never shown to a user: it exists so
/// each step can be a subcommand.
#[derive(Debug, clap::Parser)]
#[command(
    name = "step",
    no_binary_name = true,
    disable_help_flag = true,
    disable_help_subcommand = true,
    disable_version_flag = true
)]
struct StepCommand {
    #[command(subcommand)]
    step: Step,
}

/// A step that parsed, with the call spelled out in full.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedStep {
    pub step: Step,
    /// Every argument by name, defaults included, e.g.
    /// `bandpass(low=0.1, high=0.9, q=0.707)`. This is what provenance
    /// should record: it stays true if a default changes later.
    pub canonical: String,
    pub span: Span,
}

/// Why a step list or a step was rejected, with where in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct StepError {
    pub message: String,
    pub span: Span,
}

impl StepError {
    /// The message with the offending part of `source` underlined.
    pub fn render(&self, source: &str) -> String {
        let start = source[..self.span.start].chars().count();
        let width = source[self.span.start..self.span.end]
            .chars()
            .count()
            .max(1);
        format!(
            "{}\n  {}\n  {}{}",
            self.message,
            source,
            " ".repeat(start),
            "^".repeat(width)
        )
    }
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for StepError {}

/// Step names that no longer exist, and what replaces them.
const RETIRED: &[(&str, &str)] = &[
    ("zero_corr_max_peak", "zero_corr(max_peak, trace, peak)"),
    ("normalize_horizontal_magnitudes", "dewow"),
];

/// Whether `name` is a registered step.
#[cfg(test)]
fn is_registered(name: &str) -> bool {
    StepCommand::command().find_subcommand(name).is_some()
}

/// Parse a whole step list.
pub fn parse_steps(source: &str) -> Result<Vec<ParsedStep>, StepError> {
    let raw = parse::parse_step_list(source).map_err(|e| StepError {
        message: e.message,
        span: e.span,
    })?;
    raw.iter().map(resolve).collect()
}

/// Check one tokenized step against the registry.
pub fn resolve(raw: &RawStep) -> Result<ParsedStep, StepError> {
    let root = StepCommand::command();
    let at_step = |message: String| StepError {
        message,
        span: raw.span,
    };

    let Some(command) = root.find_subcommand(&raw.name) else {
        if let Some((_, replacement)) = RETIRED.iter().find(|(old, _)| *old == raw.name) {
            return Err(at_step(format!(
                "`{}` was retired; use `{replacement}`",
                raw.name
            )));
        }
        // Hand the name to clap anyway, only for its "did you mean".
        let suggestion = StepCommand::try_parse_from([raw.name.as_str()])
            .err()
            .and_then(|e| match e.get(ContextKind::SuggestedSubcommand) {
                Some(ContextValue::String(s)) => Some(s.clone()),
                // Ordered by increasing similarity: the best is last.
                Some(ContextValue::Strings(s)) => s.last().cloned(),
                _ => None,
            });
        let hint = suggestion
            .map(|s| format!(" (did you mean `{s}`?)"))
            .unwrap_or_default();
        return Err(StepError {
            message: format!("unknown step `{}`{hint}", raw.name),
            span: raw.span,
        });
    };

    let arg_names: Vec<String> = command
        .get_arguments()
        .filter_map(|a| a.get_long().map(str::to_string))
        .collect();
    // A list argument (`remove_traces(0 1 5)`) takes every value from its
    // position on, and values after `name=` keep going to it.
    let is_list = |name: &str| {
        command.get_arguments().any(|a| {
            a.get_long() == Some(name) && matches!(a.get_action(), clap::ArgAction::Append)
        })
    };

    let mut argv = vec![raw.name.clone()];
    // The named argument that bare values continue, if it is a list.
    let mut named_list: Option<String> = None;
    let mut seen_named = false;
    for (i, arg) in raw.args.iter().enumerate() {
        let name = match &arg.key {
            Some(key) => {
                seen_named = true;
                named_list = is_list(key).then(|| key.clone());
                key.clone()
            }
            None if named_list.is_some() => named_list.clone().unwrap_or_default(),
            None if seen_named => {
                return Err(StepError {
                    message: "positional arguments must come before named ones".into(),
                    span: arg.span,
                })
            }
            None => match arg_names
                .get(i)
                .or_else(|| arg_names.last().filter(|n| is_list(n)))
            {
                Some(name) => name.clone(),
                None => {
                    return Err(StepError {
                        message: format!(
                            "`{}` takes at most {} argument(s) ({}), got {}",
                            raw.name,
                            arg_names.len(),
                            arg_names.join(", "),
                            raw.args.len()
                        ),
                        span: arg.span,
                    })
                }
            },
        };
        argv.push(format!("--{name}={}", arg.value));
    }

    let matches = root
        .clone()
        .try_get_matches_from(&argv)
        .map_err(|e| translate(&e, raw))?;
    let step = StepCommand::from_arg_matches(&matches)
        .map_err(|e| translate(&e, raw))?
        .step;

    let sub_matches = matches
        .subcommand_matches(&raw.name)
        .ok_or_else(|| at_step(format!("clap lost the step `{}`", raw.name)))?;
    step.validate().map_err(at_step)?;
    step.validate_sources(sub_matches).map_err(at_step)?;
    let unused = step.unused_args();
    for (name, setting) in &unused {
        let name = *name;
        if sub_matches.value_source(name) == Some(clap::parser::ValueSource::CommandLine) {
            let span = raw
                .args
                .iter()
                .enumerate()
                .find(|(i, a)| {
                    a.key.as_deref() == Some(name)
                        || (a.key.is_none() && arg_names.get(*i).map(String::as_str) == Some(name))
                })
                .map_or(raw.span, |(_, a)| a.span);
            return Err(StepError {
                message: format!("`{name}` has no effect with `{setting}`"),
                span,
            });
        }
    }
    let canonical_args: Vec<String> = arg_names
        .iter()
        .filter(|name| !unused.iter().any(|(unused, _)| unused == name))
        .filter_map(|name| {
            let values: Vec<String> = sub_matches
                .get_raw(name)?
                .map(|v| v.to_string_lossy().into_owned())
                .collect();
            Some(format!("{name}={}", values.join(" ")))
        })
        .collect();

    Ok(ParsedStep {
        step,
        canonical: if canonical_args.is_empty() {
            raw.name.clone()
        } else {
            format!("{}({})", raw.name, canonical_args.join(", "))
        },
        span: raw.span,
    })
}

/// Reword a clap error in step terms: no `--flags`, no "Usage:", and
/// pointing at the argument that caused it.
fn translate(error: &clap::Error, raw: &RawStep) -> StepError {
    let context_str = |kind: ContextKind| match error.get(kind) {
        Some(ContextValue::String(s)) => Some(s.clone()),
        Some(ContextValue::Strings(s)) => Some(s.join(", ")),
        _ => None,
    };
    // clap reports arguments as `--name <NAME>` or `--name=<NAME>`.
    let bare = |s: String| {
        s.trim_start_matches("--")
            .split(['=', ' '])
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let invalid_arg = context_str(ContextKind::InvalidArg).map(bare);
    let span = invalid_arg
        .as_ref()
        .and_then(|name| {
            raw.args
                .iter()
                .enumerate()
                .find(|(i, a)| {
                    a.key.as_deref() == Some(name.as_str())
                        || (a.key.is_none()
                            && StepCommand::command()
                                .find_subcommand(&raw.name)
                                .and_then(|c| c.get_arguments().nth(*i).and_then(|x| x.get_long()))
                                == Some(name.as_str()))
                })
                .map(|(_, a)| a.span)
        })
        .unwrap_or(raw.span);
    let step = &raw.name;
    let arg = invalid_arg.unwrap_or_default();

    let message = match error.kind() {
        ErrorKind::UnknownArgument => {
            // Ordered by increasing similarity: the best is last.
            let best = match error.get(ContextKind::SuggestedArg) {
                Some(ContextValue::String(s)) => Some(s.clone()),
                Some(ContextValue::Strings(s)) => s.last().cloned(),
                _ => None,
            };
            let hint = best
                .map(|s| format!(" (did you mean `{}`?)", bare(s)))
                .unwrap_or_default();
            format!("`{step}` has no argument `{arg}`{hint}")
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
            let value = context_str(ContextKind::InvalidValue).unwrap_or_default();
            let reason = error.source().map(|s| format!(": {s}")).unwrap_or_default();
            format!("invalid value `{value}` for `{arg}` in `{step}`{reason}")
        }
        ErrorKind::MissingRequiredArgument => {
            format!("`{step}` requires the argument `{arg}`")
        }
        ErrorKind::ArgumentConflict => format!("`{arg}` is given more than once in `{step}`"),
        _ => format!("`{step}`: {}", error.render()),
    };
    StepError { message, span }
}

impl Step {
    /// Reject argument combinations that clap cannot express.
    fn validate(&self) -> Result<(), String> {
        match self {
            Step::ZeroCorr {
                method: zero_corr::Method::Legacy,
                scope: zero_corr::Scope::Trace | zero_corr::Scope::Smooth,
                ..
            } => Err(
                "`zero_corr(legacy)` only has a global scope; for a per-trace correction, \
                 use another method, e.g. `zero_corr(coppens, trace)`"
                    .into(),
            ),
            Step::BalanceTraces {
                deep,
                shallow_start,
                shallow_end,
                ..
            } if shallow_end <= shallow_start => Err(format!(
                "the shallow window ends ({shallow_end} ns) before it starts ({shallow_start} ns)"
            )),
            Step::BalanceTraces {
                deep, shallow_end, ..
            } if deep < shallow_end => Err(format!(
                "`deep` ({deep} ns) is inside the shallow window, which ends at {shallow_end} ns"
            )),
            _ => Ok(()),
        }
    }

    /// Rejections that depend on *which* arguments were written, not only on
    /// their values.
    ///
    /// `ArgMatches` separates a value the caller wrote from one clap filled
    /// in from a default. That is the difference `subset` needs: every
    /// argument has a default, but a bare `subset` is meaningless, so at
    /// least one has to have been written out.
    fn validate_sources(&self, matches: &clap::ArgMatches) -> Result<(), String> {
        match self {
            Step::Subset { .. } => {
                const ARGUMENTS: [&str; 4] = ["min_trace", "max_trace", "min_sample", "max_sample"];
                let given = ARGUMENTS.iter().any(|name| {
                    matches.value_source(name) == Some(clap::parser::ValueSource::CommandLine)
                });
                if given {
                    Ok(())
                } else {
                    Err("`subset` needs at least one of `min_trace`, `max_trace`, \
                         `min_sample` and `max_sample`"
                        .into())
                }
            }
            _ => Ok(()),
        }
    }

    /// Arguments that the other arguments make meaningless, each with the
    /// setting responsible. Giving one is an error, and they are left out of
    /// the canonical form.
    fn unused_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Step::ZeroCorr { method, scope, .. } => {
                let by_method: &[&'static str] = match method {
                    zero_corr::Method::Legacy => &["sigma", "time_zero", "margin"],
                    zero_corr::Method::FirstBreak => &["factor"],
                    _ => &["factor", "sigma"],
                };
                let mut unused: Vec<(&'static str, String)> = by_method
                    .iter()
                    .map(|name| (*name, format!("method={method}")))
                    .collect();
                if *scope != zero_corr::Scope::Smooth {
                    unused.push(("window", format!("scope={scope}")));
                }
                unused
            }
            Step::CorrectAntennaSeparation {
                method: method @ crate::gpr::SeparationMethod::Legacy,
                ..
            } => vec![("direct_velocity", format!("method={method}"))],
            _ => Vec::new(),
        }
    }

    /// Run the step on `gpr`.
    ///
    /// Arguments are already typed and range-checked here; what is left to
    /// fail is what depends on the data, such as a subset out of bounds.
    pub fn apply(&self, gpr: &mut GPR) -> Result<(), Box<dyn Error>> {
        match self {
            Step::Subset {
                min_trace,
                max_trace,
                min_sample,
                max_sample,
            } => {
                *gpr = gpr.subset(
                    Some(*min_trace),
                    max_trace.0,
                    Some(*min_sample),
                    max_sample.0,
                )?;
            }
            Step::RemoveTraces { traces } => {
                let indices: Vec<usize> = traces
                    .iter()
                    .flat_map(|selection| match *selection {
                        TraceSelection::Index(i) => i..=i,
                        TraceSelection::Range(start, end) => start..=end,
                    })
                    .collect();
                gpr.remove_traces(&indices, true)?;
            }
            Step::RemoveEmptyTraces { strength } => gpr.remove_empty_traces(*strength)?,
            Step::RemoveStandstills {
                strength,
                min_duration,
            } => gpr.remove_standstills(*strength, *min_duration),
            Step::AverageTraces { window } => gpr.average_traces(*window)?,
            Step::ZeroCorr {
                method,
                scope,
                factor,
                sigma,
                time_zero,
                margin,
                window,
            } => gpr.zero_corr(
                &zero_corr::Settings {
                    method: *method,
                    scope: *scope,
                    sigma: *sigma,
                    time_zero: *time_zero,
                    margin: *margin,
                    window: *window as usize,
                },
                *factor,
            )?,
            Step::Bandpass { low, high, q } => gpr.bandpass(*low, *high, *q, true)?,
            Step::BandpassMhz { low, high, q } => gpr.bandpass(*low, *high, *q, false)?,
            Step::EquidistantTraces { step } => gpr.make_equidistant(*step),
            Step::ShiftCoordinates {
                along_track,
                altitude,
                cross_track,
            } => gpr.shift_coordinates(*along_track, *altitude, *cross_track)?,
            Step::Dewow { window, method } => gpr.dewow(*window, *method)?,
            Step::BackgroundRemoval { traces, method } => gpr.background_removal(*traces, *method),
            Step::RemoveTones {
                start,
                max_tones,
                traces,
                prominence,
            } => gpr.remove_tones(*start, *max_tones, *traces, *prominence),
            Step::BalanceTraces {
                deep,
                shallow_start,
                shallow_end,
                traces,
                reference,
            } => gpr.balance_traces(*deep, *shallow_start, *shallow_end, *traces, *reference),
            Step::AutoGain { n_bins } => gpr.auto_gain(*n_bins),
            Step::Gain { factor } => gpr.gain(*factor),
            Step::KirchhoffMigration2d => gpr.kirchhoff_migration2d(),
            Step::Abslog => gpr.abslog(),
            Step::Siglog { minval_log10 } => gpr.siglog(*minval_log10),
            Step::AdaptiveSiglog { offset } => gpr.adaptive_siglog(*offset)?,
            Step::Unphase => gpr.unphase(),
            Step::CorrectTopography => gpr.correct_topography(),
            Step::CorrectAntennaSeparation {
                method,
                direct_velocity,
            } => gpr.correct_antenna_separation(*method, *direct_velocity),
            Step::Multiply { factor } => gpr.multiply(*factor),
        }
        Ok(())
    }
}

/// Parse text that must be exactly one step, with any error rendered
/// against it.
pub fn parse_one(source: &str) -> Result<ParsedStep, String> {
    let mut parsed = parse_steps(source).map_err(|e| e.render(source))?;
    match parsed.len() {
        1 => Ok(parsed.remove(0)),
        n => Err(format!("expected one step, got {n}: {source}")),
    }
}

/// Split a step list into one string per step, each checked against the
/// registry.
///
/// `text` is either a comma-separated list or the path to a file with steps
/// on their own lines (`#` starts a comment). Each returned string is the
/// step exactly as written, which is what the rest of the pipeline and the
/// provenance carry.
pub fn split_step_list(text: &str) -> Result<Vec<String>, String> {
    let path = std::path::Path::new(text);
    let sources: Vec<String> = if path.is_file() {
        crate::tools::read_text(&path.to_path_buf())
            .map_err(|e| format!("Tried to read step file but failed: {e:?}"))?
    } else {
        vec![text.to_string()]
    };

    let mut out = Vec::new();
    for source in &sources {
        for parsed in parse_steps(source).map_err(|e| e.render(source))? {
            out.push(source[parsed.span.start..parsed.span.end].to_string());
        }
    }
    Ok(out)
}

/// Name and markdown description of every step, in registry order.
pub fn descriptions() -> Vec<(String, String)> {
    StepCommand::command()
        .get_subcommands()
        .map(|command| (command.get_name().to_string(), describe(command)))
        .collect()
}

/// Markdown documentation for every registered step: the source of
/// `steps.md`, which the staleness test regenerates.
#[cfg(test)]
fn markdown() -> String {
    let mut out = String::from(
        "<!-- Generated from src/steps/mod.rs; edit the doc comments there, then run\n     \
         UPDATE_STEPS_MD=1 cargo test --no-default-features -F cli steps_md -->\n\n\
         Below is the documentation for all steps in ridal\n",
    );
    for (name, description) in descriptions() {
        out.push_str(&format!("\n## {name}\n{description}"));
    }
    out
}

/// The processing steps page of the documentation, as Sphinx `step`
/// directives (`docs/_ext/ridal_steps.py`) so that each step renders like a
/// function in the Python reference. Regenerated with `steps.md`.
#[cfg(test)]
fn docs_markdown() -> String {
    let mut out = String::from(
        "<!-- Generated from src/steps/mod.rs; edit the doc comments there, then run\n     \
         UPDATE_STEPS_MD=1 cargo test --no-default-features -F cli steps_md -->\n\n\
         # Processing steps\n\n\
         Every processing step, generated from Ridal itself. `ridal steps --describe-all` \
         prints the same text.\n\n\
         A step is written as its name, optionally followed by its arguments in \
         parentheses. Arguments can be given in order, separated by spaces, or by name: \
         `bandpass(0.1 0.9)` and `bandpass(low=0.1, high=0.9)` are the same step. \
         Arguments that are left out take the defaults shown.\n",
    );
    // Built, or clap has not yet worked out how many values each argument takes.
    let mut steps = StepCommand::command();
    steps.build();
    for command in steps.get_subcommands() {
        let signature: Vec<String> = command
            .get_arguments()
            .map(|arg| {
                let name = arg.get_long().unwrap_or_default();
                // A `Vec` field, which takes any number of values.
                let many = matches!(arg.get_action(), clap::ArgAction::Append);
                let name = if many {
                    format!("{name}…")
                } else {
                    name.to_string()
                };
                match arg.get_default_values() {
                    [] if arg.is_required_set() => name,
                    [] => format!("[{name}]"),
                    values => format!(
                        "{name}={}",
                        values
                            .iter()
                            .map(|v| v.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                }
            })
            .collect();
        out.push_str(&format!(
            "\n```{{step}} {}({})\n",
            command.get_name(),
            signature.join(", ")
        ));
        if let Some(text) = command.get_long_about().or(command.get_about()) {
            out.push_str(&format!("{}\n", text.to_string().trim()));
        }
        let args: Vec<_> = command.get_arguments().collect();
        if !args.is_empty() {
            out.push_str("\n**Arguments**\n\n");
            for arg in args {
                let default = match arg.get_default_values() {
                    [] if arg.is_required_set() => " (required)".to_string(),
                    [] => String::new(),
                    values => format!(
                        " (default `{}`)",
                        values
                            .iter()
                            .map(|v| v.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                };
                out.push_str(&format!(
                    "- `{}`{default}: {}\n",
                    arg.get_long().unwrap_or_default(),
                    arg.get_help().map(|h| h.to_string()).unwrap_or_default()
                ));
            }
        }
        out.push_str("```\n");
    }
    out
}

/// One step's summary, detail and argument table, as markdown.
fn describe(command: &clap::Command) -> String {
    let mut out = String::new();
    if let Some(text) = command.get_long_about().or(command.get_about()) {
        out.push_str(&format!("{text}\n"));
    }
    let args: Vec<_> = command.get_arguments().collect();
    if !args.is_empty() {
        out.push_str("\n| argument | default | description |\n|---|---|---|\n");
        for arg in args {
            let default = match arg.get_default_values() {
                [] if arg.is_required_set() => "*required*".to_string(),
                [] => "".to_string(),
                values => values
                    .iter()
                    .map(|v| format!("`{}`", v.to_string_lossy()))
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                arg.get_long().unwrap_or_default(),
                default,
                arg.get_help().map(|h| h.to_string()).unwrap_or_default()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(source: &str) -> Result<ParsedStep, StepError> {
        parse_steps(source).map(|mut v| v.remove(0))
    }

    #[test]
    fn positional_and_named_arguments_mean_the_same_thing() {
        let a = one("bandpass(0.2 0.8 0.5)").unwrap();
        let b = one("bandpass(high=0.8, q=0.5, low=0.2)").unwrap();
        let c = one("bandpass(0.2, q=0.5, high=0.8)").unwrap();
        for other in [&b, &c] {
            assert_eq!(a.step, other.step);
            assert_eq!(a.canonical, other.canonical);
        }
        assert_eq!(
            a.step,
            Step::Bandpass {
                low: 0.2,
                high: 0.8,
                q: 0.5
            }
        );
    }

    #[test]
    fn the_canonical_form_spells_out_every_default() {
        assert_eq!(
            one("dewow").unwrap().canonical,
            "dewow(window=auto, method=median)"
        );
        assert_eq!(
            one("subset(0 300)").unwrap().canonical,
            "subset(min_trace=0, max_trace=300, min_sample=0, max_sample=-1)"
        );
        // And it parses back to the same step.
        let parsed = one("bandpass(q=2)").unwrap();
        assert_eq!(one(&parsed.canonical).unwrap().step, parsed.step);
    }

    #[test]
    fn negative_one_means_the_end_and_is_not_a_flag() {
        assert_eq!(
            one("subset(0 -1 0 500)").unwrap().step,
            Step::Subset {
                min_trace: 0,
                max_trace: End(None),
                min_sample: 0,
                max_sample: End(Some(500)),
            }
        );
    }

    #[test]
    fn subset_needs_one_argument_but_not_min_trace() {
        // #298: cropping the height is a natural subset on its own, and the
        // old required `min_trace` was only there to keep `subset()` from
        // being called bare.
        assert_eq!(
            one("subset(max_sample=1000)").unwrap().step,
            Step::Subset {
                min_trace: 0,
                max_trace: End(None),
                min_sample: 0,
                max_sample: End(Some(1000)),
            }
        );
        let err = one("subset").unwrap_err();
        assert!(
            err.message.contains("needs at least one"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_background_window_is_all_traces_or_an_odd_count() {
        let traces = |s: &str| match one(s).unwrap().step {
            Step::BackgroundRemoval { traces, .. } => traces,
            other => panic!("{other:?}"),
        };
        assert_eq!(traces("background_removal"), rolling::TraceWindow::All);
        assert_eq!(
            traces("background_removal(101)"),
            rolling::TraceWindow::Traces(101)
        );
        for (source, fragment) in [
            ("background_removal(100)", "odd number"),
            ("background_removal(1)", "at least 3"),
            ("background_removal(0)", "odd number"),
            ("background_removal(-1)", "expected `all`"),
            ("background_removal(all, mode)", "`median` or `mean`"),
            ("dewow(-3)", "positive number of nanoseconds"),
        ] {
            let err = one(source).unwrap_err();
            assert!(err.message.contains(fragment), "{source}: {}", err.message);
        }
    }

    #[test]
    fn normalize_horizontal_magnitudes_points_to_dewow() {
        let err = one("normalize_horizontal_magnitudes(0.3)").unwrap_err();
        assert!(err.message.contains("use `dewow`"), "{}", err.message);
    }

    #[test]
    fn errors_name_the_step_and_the_argument_in_step_terms() {
        for (source, fragments) in [
            (
                "dewo(5)",
                vec!["unknown step `dewo`", "did you mean `dewow`"],
            ),
            (
                "dewow(windw=5)",
                vec!["no argument `windw`", "did you mean `window`"],
            ),
            ("bandpass(hihg=0.5)", vec!["did you mean `high`"]),
            (
                "shift_coordinates(1 altitud=2)",
                vec!["did you mean `altitude`"],
            ),
            ("dewow(0)", vec!["invalid value `0` for `window`"]),
            ("dewow(abc)", vec!["invalid value `abc` for `window`"]),
            ("dewow(5 median 6)", vec!["at most 2 argument"]),
            ("dewow(window=5, window=6)", vec!["more than once"]),
            ("subset", vec!["needs at least one of `min_trace`"]),
            ("subset(0 -2)", vec!["for `max_trace`", "-1 for the end"]),
            (
                "bandpass(q=1, 0.2)",
                vec!["positional arguments must come before"],
            ),
        ] {
            let err = one(source).unwrap_err();
            for fragment in fragments {
                assert!(
                    err.message.contains(fragment),
                    "{source:?}: expected {fragment:?} in {:?}",
                    err.message
                );
            }
            assert!(!err.message.contains("--"), "{source:?}: {}", err.message);
        }
    }

    #[test]
    fn a_bad_argument_is_underlined_where_it_was_written() {
        let source = "dewow(10), bandpass(0.1, q=abc)";
        let err = parse_steps(source).unwrap_err();
        assert_eq!(&source[err.span.start..err.span.end], "q=abc");
        let rendered = err.render(source);
        assert!(
            rendered.ends_with("                         ^^^^^"),
            "{rendered}"
        );
    }

    #[test]
    fn every_documented_example_parses() {
        // Backticked snippets in a step's doc that call a registered step.
        let docs = markdown();
        let examples: Vec<&str> = docs
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|s| s.contains('(') && is_registered(s.split('(').next().unwrap()))
            .collect();
        assert!(examples.len() >= 5, "{examples:?}");
        for example in examples {
            let parsed =
                one(example).unwrap_or_else(|e| panic!("{example}: {}", e.render(example)));
            // The canonical form is what provenance will carry, so it has to
            // parse back to the same step.
            let again = one(&parsed.canonical).unwrap_or_else(|e| {
                panic!("{}: {}", parsed.canonical, e.render(&parsed.canonical))
            });
            assert_eq!(parsed.step, again.step, "{example} -> {}", parsed.canonical);
        }
    }

    #[test]
    fn a_list_argument_takes_the_remaining_values() {
        let expected = Step::RemoveTraces {
            traces: vec![
                TraceSelection::Index(0),
                TraceSelection::Index(1),
                TraceSelection::Range(5, 9),
            ],
        };
        for source in [
            "remove_traces(0 1 5-9)",
            "remove_traces(0, 1, 5-9)",
            "remove_traces(traces=0 1 5-9)",
        ] {
            assert_eq!(one(source).unwrap().step, expected, "{source}");
        }
        assert_eq!(
            one("remove_traces(0 1 5-9)").unwrap().canonical,
            "remove_traces(traces=0 1 5-9)"
        );
        assert!(one("remove_traces")
            .unwrap_err()
            .message
            .contains("requires"));
        assert!(one("remove_traces(9-5)")
            .unwrap_err()
            .message
            .contains("backwards"));
    }

    #[test]
    fn a_step_list_splits_into_steps_as_written() {
        // #10: the comma inside `subset(0, 200)` used to split the step.
        assert_eq!(
            split_step_list("subset(0, 200), dewow ,bandpass(0.1 q=2)").unwrap(),
            vec!["subset(0, 200)", "dewow", "bandpass(0.1 q=2)"]
        );
        let err = split_step_list("dewow bandpass").unwrap_err();
        assert!(err.contains("separated by commas"), "{err}");
        let err = split_step_list("dewow, bandpas").unwrap_err();
        assert!(err.contains("did you mean `bandpass`"), "{err}");
    }

    #[test]
    fn steps_without_arguments_reject_any() {
        assert_eq!(one("unphase").unwrap().canonical, "unphase");
        assert_eq!(one("unphase()").unwrap().step, Step::Unphase);
        assert!(one("unphase(1)").unwrap_err().message.contains("at most 0"));
    }

    #[test]
    fn multiply_rejects_factors_that_would_destroy_the_data() {
        for bad in ["multiply(0)", "multiply(inf)", "multiply(NaN)"] {
            assert!(one(bad).is_err(), "{bad}");
        }
        assert_eq!(
            one("multiply(5e0)").unwrap().step,
            Step::Multiply { factor: 5. }
        );
    }

    #[test]
    fn correct_antenna_separation_takes_the_direct_wave_velocity_second() {
        assert_eq!(
            one("correct_antenna_separation").unwrap().canonical,
            "correct_antenna_separation(method=slant, direct_velocity=0.2997)"
        );
        assert_eq!(
            one("correct_antenna_separation(slant, 0.168)")
                .unwrap()
                .step,
            Step::CorrectAntennaSeparation {
                method: crate::gpr::SeparationMethod::Slant,
                direct_velocity: 0.168,
            }
        );
        assert_eq!(
            one("correct_antenna_separation(legacy)").unwrap().canonical,
            "correct_antenna_separation(method=legacy)"
        );
        assert_eq!(
            one("correct_antenna_separation(legacy, 0.168)")
                .unwrap_err()
                .message,
            "`direct_velocity` has no effect with `method=legacy`"
        );
        for bad in ["0", "-0.1", "inf"] {
            let source = format!("correct_antenna_separation(slant, {bad})");
            assert!(one(&source).is_err(), "{source}");
        }
    }

    #[test]
    fn zero_corr_records_only_the_arguments_its_method_uses() {
        for (source, canonical) in [
            (
                "zero_corr",
                "zero_corr(method=coppens, scope=global, time_zero=onset, margin=auto)",
            ),
            (
                "zero_corr(max_peak, trace, margin=2.5)",
                "zero_corr(method=max_peak, scope=trace, time_zero=onset, margin=2.5)",
            ),
            (
                "zero_corr(legacy)",
                "zero_corr(method=legacy, scope=global, factor=1)",
            ),
            (
                "zero_corr(max_peak, trace, peak)",
                "zero_corr(method=max_peak, scope=trace, time_zero=peak, margin=auto)",
            ),
            (
                "zero_corr(coppens, smooth, window=101)",
                "zero_corr(method=coppens, scope=smooth, time_zero=onset, margin=auto, window=101)",
            ),
            (
                "zero_corr(first_break, trace, sigma=3)",
                "zero_corr(method=first_break, scope=trace, time_zero=onset, margin=auto, sigma=3)",
            ),
        ] {
            let parsed = one(source).unwrap();
            assert_eq!(parsed.canonical, canonical, "{source}");
            assert_eq!(one(&parsed.canonical).unwrap().step, parsed.step);
        }
    }

    #[test]
    fn zero_corr_says_how_to_migrate_and_what_does_not_apply() {
        for (source, fragment, underlined) in [
            (
                "zero_corr_max_peak",
                "use `zero_corr(max_peak, trace, peak)`",
                "zero_corr_max_peak",
            ),
            ("zero_corr(0.9)", "`zero_corr(legacy, factor=0.9)`", "0.9"),
            (
                "zero_corr(aic, factor=0.9)",
                "`factor` has no effect with `method=aic`",
                "factor=0.9",
            ),
            (
                "zero_corr(legacy, global, onset)",
                "`time_zero` has no effect with `method=legacy`",
                "onset",
            ),
            (
                "zero_corr(legacy, sigma=5)",
                "`sigma` has no effect with `method=legacy`",
                "sigma=5",
            ),
            (
                "zero_corr(legacy, trace)",
                "only has a global scope",
                "zero_corr(legacy, trace)",
            ),
            (
                "zero_corr(aic, both)",
                "`global`, `trace` or `smooth`",
                "both",
            ),
            ("zero_corr(margin=-1)", "non-negative", "margin=-1"),
            (
                "zero_corr(aic, trace, window=11)",
                "`window` has no effect with `scope=trace`",
                "window=11",
            ),
            (
                "zero_corr(legacy, smooth)",
                "only has a global scope",
                "zero_corr(legacy, smooth)",
            ),
            ("zero_corr(scope=smooth, window=1)", "window", "window=1"),
            (
                "zero_corr(time_zero=middle)",
                "`onset` or `peak`",
                "time_zero=middle",
            ),
            (
                "zero_corr(legacy, margin=2)",
                "`margin` has no effect with `method=legacy`",
                "margin=2",
            ),
        ] {
            let err = one(source).unwrap_err();
            assert!(
                err.message.contains(fragment),
                "{source:?}: expected {fragment:?} in {:?}",
                err.message
            );
            assert_eq!(
                &source[err.span.start..err.span.end],
                underlined,
                "{source}"
            );
        }
    }

    #[test]
    fn docs_steps_md_is_up_to_date() {
        // The documentation's steps page, kept in step with the registry the
        // same way `steps.md` is, and regenerated by the same command.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("reference")
            .join("steps.md");
        let expected = docs_markdown();
        if std::env::var_os("UPDATE_STEPS_MD").is_some() {
            // CI must check the committed file, never regenerate it.
            assert!(
                std::env::var_os("CI").is_none(),
                "UPDATE_STEPS_MD is set in CI"
            );
            std::fs::write(&path, &expected).unwrap();
        }
        // A Windows checkout may have converted the line endings.
        let actual = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            actual == expected,
            "docs/reference/steps.md is stale. Regenerate it with\n  \
             UPDATE_STEPS_MD=1 cargo test --no-default-features -F cli steps_md"
        );
    }

    #[test]
    fn steps_md_is_up_to_date() {
        // `steps.md` is generated so that the README's link keeps working;
        // this is what stops it drifting from the registry.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("steps.md");
        let expected = markdown();
        if std::env::var_os("UPDATE_STEPS_MD").is_some() {
            // CI must check the committed file, never regenerate it.
            assert!(
                std::env::var_os("CI").is_none(),
                "UPDATE_STEPS_MD is set in CI"
            );
            std::fs::write(&path, &expected).unwrap();
        }
        // A Windows checkout may have converted the line endings.
        let actual = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        assert!(
            actual == expected,
            "steps.md is stale. Regenerate it with\n  \
             UPDATE_STEPS_MD=1 cargo test --no-default-features -F cli steps_md"
        );
    }
}
