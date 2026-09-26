<!-- Generated from src/steps/mod.rs; edit the doc comments there, then run
     UPDATE_STEPS_MD=1 cargo test --no-default-features -F cli steps_md -->

Below is the documentation for all steps in ridal

## subset
Subset the data in x (traces) and/or y (samples).

Indices are zero-based and the end is exclusive; `-1` means "to the end". Clip to the first 500 samples: `subset(0 -1 0 500)`. Clip to the first 300 traces: `subset(0 300)`.

| argument | default | description |
|---|---|---|
| `min_trace` | *required* | First trace to keep |
| `max_trace` | `-1` | Trace to stop before, or -1 for the last one |
| `min_sample` | `0` | First sample to keep |
| `max_sample` | `-1` | Sample to stop before, or -1 for the last one |

## remove_traces
Manually remove trace indices, for example in case they are visually deemed bad.

Remove the first two traces: `remove_traces(0 1)`. Inclusive ranges are allowed too: `remove_traces(0 5-9)`.

| argument | default | description |
|---|---|---|
| `traces` | *required* | Trace indices or inclusive ranges (`5-9`) to remove |

## remove_empty_traces
Remove all traces that appear empty.

Recommended to be run as the first filter if required! The strength threshold (mean absolute trace value) can be tweaked. Example: `remove_empty_traces(2)`.

| argument | default | description |
|---|---|---|
| `strength` | `1` | Mean absolute trace value below which a trace counts as empty |

## average_traces
Average traces in a given window.

The coordinate information is picked from the middle averaged trace. Example: `average_traces(3)`.

| argument | default | description |
|---|---|---|
| `window` | *required* | Number of traces to average |

## zero_corr_max_peak
Shift the location of the zero return time by finding the maximum row value.

The peak is found for each trace individually.

## zero_corr
Shift the location of the zero return time by finding the first row where data appear.

The correction can be tweaked to allow more or less data, e.g. `zero_corr(0.9)`.

| argument | default | description |
|---|---|---|
| `threshold_multiplier` | `1` | Multiplier on the first-rise threshold; lower picks earlier |

## bandpass
Apply a bandpass Butterworth filter to each trace individually.

The given frequencies are normalized (0: 0Hz, 1: Nyquist). Example (with default values): `bandpass(0.1 0.9)`.

| argument | default | description |
|---|---|---|
| `low` | `0.1` | Lower cutoff, as a fraction of the Nyquist frequency |
| `high` | `0.9` | Upper cutoff, as a fraction of the Nyquist frequency |
| `q` | `0.707` | Filter strength (quality factor). Must be above 0 |

## bandpass_mhz
Apply a bandpass Butterworth filter to each trace individually, with the frequencies in MHz.

Example: `bandpass_mhz(100 800)`.

| argument | default | description |
|---|---|---|
| `low` | *required* | Lower cutoff, in MHz |
| `high` | *required* | Upper cutoff, in MHz |
| `q` | `0.707` | Filter strength (quality factor). Must be above 0 |

## equidistant_traces
Make all traces equidistant by resampling them in a fixed horizontal grid.

Unless provided, the step size is determined from the median moving velocity. Other step sizes in m can be given, e.g. `equidistant_traces(2.)` for 2 m.

| argument | default | description |
|---|---|---|
| `step` |  | Distance between traces, in m. Determined from the data if left out |

## shift_coordinates
Shift trace coordinates along the track.

Useful if the location data were collected away from the GPR antenna. Edge coordinates are clamped to the min/max bounds of the original data. Example for moving the location data (along-track) forward 3 m (if the GPR is ahead of the GNSS), down 2 m (GNSS mounted on a pole) and (cross-track) right 1 m (GNSS mounted on the left): `shift_coordinates(3 -2 1)`

| argument | default | description |
|---|---|---|
| `along_track` | *required* | Along-track shift in m; positive is forward |
| `altitude` | `0` | Vertical shift in m; positive is up |
| `cross_track` | `0` | Cross-track shift in m; positive is right |

## normalize_horizontal_magnitudes
Normalize the magnitudes of the traces in the horizontal axis.

This removes or reduces horizontal banding. The uppermost samples of the trace can be excluded, either by sample number (integer; e.g. `normalize_horizontal_magnitudes(300)`) or by a fraction of the trace (float; e.g. `normalize_horizontal_magnitudes(0.3)`).

| argument | default | description |
|---|---|---|
| `skip_first` | `0` | Samples to exclude at the top: a count, or a fraction in [0, 1) |

## dewow
Remove values that are systematic across all traces.

The samples are split into consecutive blocks of `window` rows, and each block's mean over all traces is subtracted. The last 1 to `window` samples are left unchanged. Example: `dewow(10)`.

| argument | default | description |
|---|---|---|
| `window` | `5` | Height of each block, in samples |

## auto_gain
Automatically determine the best gain factor and apply it.

The data are binned vertically and the mean absolute deviation of the values is used as a proxy for signal attenuation. The median attenuation in decibel volts is given to the gain filter. The amounts of bins can be given, e.g. `auto_gain(100)`.

| argument | default | description |
|---|---|---|
| `n_bins` | `100` | Number of vertical bins |

## gain
Multiply the magnitude as a function of depth.

This is most often used to correct for signal attenuation with time/distance. Gain is applied as: '10 ^(gain * twtt / 20)' (dB / ns) where gain is the given gain factor and twtt is the two-way travel time of the signal. Example: `gain(0.002)`.

| argument | default | description |
|---|---|---|
| `factor` | *required* | Gain factor, in dB/ns |

## kirchhoff_migration2d
Migrate sample magnitudes in the horizontal and vertical distance dimension to correct hyperbolae in the data.

The correction is needed because the GPR does not observe only what is directly below it, but rather in a cone that is determined by the dominant antenna frequency. Thus, without migration, each trace is the sum of a cone beneath it. Topographic Kirchhoff migration (in 2D) corrects for this in two dimensions.

## abslog
Run a log10 operation on the absolute values (log10(abs(data))), converting it to a logarithmic scale.

This is useful for visualization. Before conversion, the data are added with the 1st percentile (absolute) value in the dataset to avoid log10(0) == inf.

## siglog
Run a log10 operation on absolute values and then account for the sign.

Values smaller than the set minimum magnitude are truncated to zero. E.g. with an exponent offset of 0: 1000 -> 3, -1000 -> -3, 0.001 -> 0. The argument specifies the exponent offset to apply to allow for values smaller than +-1 (e.g. 10e-1).

| argument | default | description |
|---|---|---|
| `minval_log10` | `0` | Exponent offset (log10 of the smallest magnitude kept) |

## adaptive_siglog
Run `siglog` at a strength set from the data's own noise floor instead of a fixed one.

The strength is `log10(noise) + offset`, where the noise floor is the median `log10|value|` over the whole radargram (zeros excluded). The same offset therefore gives the same result regardless of the recording's amplitude scale, which a fixed `siglog` strength cannot. More negative offsets keep more weak signal (and noise). The resolved strength is written to the processing log. Example: `adaptive_siglog(-1.7)`.

| argument | default | description |
|---|---|---|
| `offset` | `-1.7` | Offset from the noise floor, in log10 units |

## unphase
Combine the positive and negative phases of the signal into one positive magntiude.

The assumption is made that the positive magnitude of the signal comes first, followed by an offset negative component. The distance between the positive and negative peaks are found, and then the negative part is shifted accordingly.

## correct_topography
Make a copy of the data and topographically correct it.

In the output, the data will be called "data_topographically_corrected". Note that the copying means any step run after this will not be reflected in "data_topographically_corrected". This is thus recommended to run last.

## correct_antenna_separation
Correct for the separation between the antenna transmitter and receiver.

The consequence of antenna separation is that depths are slightly exaggerated at low return-times before correction. This step averages samples so that each sample represents a consistent depth interval.

Afterwards, `twtt` is the travel time a coincident transmitter and receiver would have recorded rather than the travel time between the pair, and the output declares that with `twtt:anchor_name = "twtt_normal_incidence"` and `antenna_separation_effective = 0`.

## multiply
Multiply all values by a constant factor.

This is useful e.g. for standardizing data between sensors and antenna frequencies. Example: `multiply(5)`.

| argument | default | description |
|---|---|---|
| `factor` | *required* | Factor to multiply by. Must be finite and non-zero |
