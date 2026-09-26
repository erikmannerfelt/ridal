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
    /// first 300 traces: `subset(0 300)`.
    #[command(rename_all = "snake_case")]
    Subset {
        /// First trace to keep.
        #[arg(long)]
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
    /// Shift the location of the zero return time by finding the maximum row
    /// value.
    ///
    /// The peak is found for each trace individually.
    #[command(rename_all = "snake_case")]
    ZeroCorrMaxPeak,
    /// Shift the location of the zero return time by finding the first row
    /// where data appear.
    ///
    /// The correction can be tweaked to allow more or less data, e.g.
    /// `zero_corr(0.9)`.
    #[command(rename_all = "snake_case")]
    ZeroCorr {
        /// Multiplier on the first-rise threshold; lower picks earlier.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_ZERO_CORR_THRESHOLD_MULTIPLIER)]
        threshold_multiplier: f32,
    },
    /// Apply a bandpass Butterworth filter to each trace individually.
    ///
    /// The given frequencies are normalized (0: 0Hz, 1: Nyquist). Example
    /// (with default values): `bandpass(0.1 0.9)`.
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
    /// Apply a bandpass Butterworth filter to each trace individually, with
    /// the frequencies in MHz.
    ///
    /// Example: `bandpass_mhz(100 800)`.
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
    /// Normalize the magnitudes of the traces in the horizontal axis.
    ///
    /// This removes or reduces horizontal banding. The uppermost samples of
    /// the trace can be excluded, either by sample number (integer; e.g.
    /// `normalize_horizontal_magnitudes(300)`) or by a fraction of the trace
    /// (float; e.g. `normalize_horizontal_magnitudes(0.3)`).
    #[command(rename_all = "snake_case")]
    NormalizeHorizontalMagnitudes {
        /// Samples to exclude at the top: a count, or a fraction in [0, 1).
        #[arg(long, default_value = "0")]
        skip_first: SkipFirst,
    },
    /// Remove values that are systematic across all traces.
    ///
    /// The samples are split into consecutive blocks of `window` rows, and
    /// each block's mean over all traces is subtracted. The last 1 to
    /// `window` samples are left unchanged. Example: `dewow(10)`.
    #[command(rename_all = "snake_case")]
    Dewow {
        /// Height of each block, in samples.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_DEWOW_WINDOW,
              value_parser = clap::value_parser!(u32).range(1..))]
        window: u32,
    },
    /// Automatically determine the best gain factor and apply it.
    ///
    /// The data are binned vertically and the mean absolute deviation of the
    /// values is used as a proxy for signal attenuation. The median
    /// attenuation in decibel volts is given to the gain filter. The amounts
    /// of bins can be given, e.g. `auto_gain(100)`.
    #[command(rename_all = "snake_case")]
    AutoGain {
        /// Number of vertical bins.
        #[arg(long, default_value_t = crate::gpr::DEFAULT_AUTOGAIN_N_BINS)]
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
    /// The consequence of antenna separation is that depths are slightly
    /// exaggerated at low return-times before correction. This step averages
    /// samples so that each sample represents a consistent depth interval.
    ///
    /// Afterwards, `twtt` is the travel time a coincident transmitter and
    /// receiver would have recorded rather than the travel time between the
    /// pair, and the output declares that with `twtt:anchor_name =
    /// "twtt_normal_incidence"` and `antenna_separation_effective = 0`.
    #[command(rename_all = "snake_case")]
    CorrectAntennaSeparation,
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

/// How many samples at the top of each trace to leave out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SkipFirst {
    Samples(usize),
    /// Fraction of the trace height, in [0, 1).
    Fraction(f32),
}

impl SkipFirst {
    fn samples(self, height: usize) -> usize {
        match self {
            SkipFirst::Samples(n) => n,
            SkipFirst::Fraction(f) => (height as f32 * f) as usize,
        }
    }
}

impl FromStr for SkipFirst {
    type Err = String;
    /// An integer is a sample count and anything with a decimal point is a
    /// fraction, so `1` and `1.0` mean different things on purpose.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(n) = s.parse::<usize>() {
            return Ok(SkipFirst::Samples(n));
        }
        match s.parse::<f32>() {
            Ok(f) if (0.0..1.0).contains(&f) => Ok(SkipFirst::Fraction(f)),
            Ok(_) => Err("a fraction must be at least 0 and below 1".into()),
            Err(_) => Err("expected a sample count or a fraction in [0, 1)".into()),
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
    let canonical_args: Vec<String> = arg_names
        .iter()
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
            Step::AverageTraces { window } => gpr.average_traces(*window)?,
            Step::ZeroCorrMaxPeak => gpr.zero_corr_max_peak(),
            Step::ZeroCorr {
                threshold_multiplier,
            } => gpr.zero_corr(Some(*threshold_multiplier)),
            Step::Bandpass { low, high, q } => gpr.bandpass(*low, *high, *q, true)?,
            Step::BandpassMhz { low, high, q } => gpr.bandpass(*low, *high, *q, false)?,
            Step::EquidistantTraces { step } => gpr.make_equidistant(*step),
            Step::ShiftCoordinates {
                along_track,
                altitude,
                cross_track,
            } => gpr.shift_coordinates(*along_track, *altitude, *cross_track)?,
            Step::NormalizeHorizontalMagnitudes { skip_first } => {
                let skip = skip_first.samples(gpr.height()) as isize;
                gpr.normalize_horizontal_magnitudes(Some(skip));
            }
            Step::Dewow { window } => gpr.dewow(*window),
            Step::AutoGain { n_bins } => gpr.auto_gain(*n_bins),
            Step::Gain { factor } => gpr.gain(*factor),
            Step::KirchhoffMigration2d => gpr.kirchhoff_migration2d(),
            Step::Abslog => gpr.abslog(),
            Step::Siglog { minval_log10 } => gpr.siglog(*minval_log10),
            Step::AdaptiveSiglog { offset } => gpr.adaptive_siglog(*offset)?,
            Step::Unphase => gpr.unphase(),
            Step::CorrectTopography => gpr.correct_topography(),
            Step::CorrectAntennaSeparation => gpr.correct_antenna_separation(),
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
        assert_eq!(one("dewow").unwrap().canonical, "dewow(window=5)");
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
    fn an_integer_skips_samples_and_a_decimal_skips_a_fraction() {
        let skip = |s: &str| match one(s).unwrap().step {
            Step::NormalizeHorizontalMagnitudes { skip_first } => skip_first,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            skip("normalize_horizontal_magnitudes(300)"),
            SkipFirst::Samples(300)
        );
        assert_eq!(
            skip("normalize_horizontal_magnitudes(0.3)"),
            SkipFirst::Fraction(0.3)
        );
        // The old parser's range check could never fire; this one does.
        assert!(one("normalize_horizontal_magnitudes(1.5)").is_err());
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
            ("dewow(5 6)", vec!["at most 1 argument"]),
            ("dewow(window=5, window=6)", vec!["more than once"]),
            ("subset", vec!["requires the argument `min_trace`"]),
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
