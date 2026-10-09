<!-- Generated from src/steps/mod.rs; edit the doc comments there, then run
     UPDATE_STEPS_MD=1 cargo test --no-default-features -F cli steps_md -->

Below is the documentation for all steps in ridal

## subset
Subset the data in x (traces) and/or y (samples).

Indices are zero-based and the end is exclusive; `-1` means "to the end". Clip to the first 500 samples: `subset(0 -1 0 500)`. Clip to the first 300 traces: `subset(0 300)`. At least one argument is required; the rest keep their defaults, so `subset(max_sample=1000)` crops the height on its own.

| argument | default | description |
|---|---|---|
| `min_trace` | `0` | First trace to keep |
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

## remove_standstills
Replace each stretch recorded while the radar stood still with its median trace.

A time-triggered radar keeps recording when it stops, and in a standstill each trace repeats the one before it. This is found from the radar data alone, without the positions, by how coherent neighbouring traces are below the direct wave. Each stretch is scored as a robust z against the rest of the profile, and a stretch is a standstill when it reaches `strength` and lasts at least `min_duration`, given in seconds of recording (`5s`) or traces (`25`). Seconds use the trace interval in the file header, which `average_traces` keeps up to date.

The log lists every standstill with its traces, duration and strength, and the highest strength elsewhere, so the threshold can be judged against both. Each median trace keeps the time and position of the middle of its stretch, or of the first or last trace for a standstill at the start or end of the profile, so that interpretations made before this step still carry over.

Run it early, before the filters. After a running `background_removal` it missed the longest standstills and found false ones, and after `dewow` it missed a weak one on 100 MHz data. Run it before `equidistant_traces` too, which it can replace when the positions are poor. Standstills that together make up more than half the profile are not found. Examples: `remove_standstills`, `remove_standstills(6)`, `remove_standstills(min_duration=20)`.

So far this has only been tested on Malå data (25, 100 and 800 MHz). With the defaults it found every standstill identified by eye there except one weak 100 MHz one, which `remove_standstills(7)` found. Check the result before relying on it for other instruments.

| argument | default | description |
|---|---|---|
| `strength` | `8` | How far a standstill must stand out from the rest of the profile, as a robust z. Lower finds more |
| `min_duration` | `5s` | The shortest standstill: seconds (`5s`) or traces (`25`) |

## average_traces
Average traces in a given window.

The coordinate information is picked from the middle averaged trace. Example: `average_traces(3)`.

| argument | default | description |
|---|---|---|
| `window` | *required* | Number of traces to average |

## zero_corr
Move time zero to where the direct wave starts, and crop what came before it.

`method` decides how each trace's direct wave is found, and so what the traces are aligned on. `coppens` takes the steepest rise of the smoothed energy ratio, or the rise of a weaker leading lobe that runs into it out of the noise. `first_break` takes the first sample more than `sigma` noise standard deviations out of the noise, and mostly agrees with `coppens`. `aic` splits the record where it best divides into noise and signal, which puts it at the start of a gradual rise, often a sample earlier. `max_peak` takes the direct wave's largest value with the sign that most traces' largest value has, and survives noisy or corrupted first samples best. `legacy` is the pre-0.7 threshold on the mean trace, and also subtracts the mean of what it crops. All but `legacy` look for the direct wave around the first strong arrival, and none of them depend on the amplitude scale. The onset methods only accept an onset where the signal stays out of the noise for most of the next quarter period, so isolated early samples do not start the direct wave.

`time_zero` says which feature of the direct wave time zero goes on, `onset` or `peak`, whichever method aligned the traces. When the method finds the other feature, time zero moves by the median distance between the two over the traces, so every method means the same time zero by default.

`scope` is `global`, one time zero from the mean trace; `trace`, one per trace; or `smooth`, one per trace from the running median of the per-trace picks over `window` traces, for a time zero that drifts slowly and would otherwise gain the scatter of single picks. Per-trace picks that stray from their neighbours by more than three quarters of a period, and whose distance to the other end of their own direct wave is also unusual, are replaced, and the bottom is trimmed so that no trace is zero-padded.

`margin` keeps some record above time zero, the same amount in every trace, and the travel times of those samples are negative. `auto` keeps back to where the direct wave starts: nothing with `time_zero=onset`, and the start of the wavelet with `time_zero=peak`. Examples: `zero_corr(coppens, trace)`, `zero_corr(max_peak, trace)`, `zero_corr(coppens, smooth, window=101)`, `zero_corr(max_peak, trace, peak)`, `zero_corr(coppens, margin=5)`, `zero_corr(first_break, sigma=4)`, `zero_corr(legacy, factor=0.9)`.

| argument | default | description |
|---|---|---|
| `method` | `coppens` | `coppens`, `first_break`, `aic`, `max_peak` or `legacy` |
| `scope` | `global` | `global`, `trace` or `smooth` |
| `time_zero` | `onset` | Where on the direct wave time zero goes: `onset` or `peak` |
| `margin` | `auto` | How much record to keep above time zero: `auto`, back to where the direct wave starts, or a number of nanoseconds |
| `factor` | `1` | `legacy` only: multiplier on the first-rise threshold; lower picks earlier |
| `sigma` | `5` | `first_break` only: how many noise standard deviations count as signal |
| `window` | `51` | `smooth` only: how many traces the running median of the picks spans |

## bandpass
Apply a zero-phase bandpass filter to each trace individually.

The given frequencies are normalized (0: 0Hz, 1: Nyquist). Example (with default values): `bandpass(0.1 0.9)`.

A high-pass and a low-pass section (Butterworth at the default `q`) are run forward and then backward along the trace, so reflections keep their shape and position. The two passes square the response: each cutoff is where the amplitude has fallen to half (-6 dB).

| argument | default | description |
|---|---|---|
| `low` | `0.1` | Lower cutoff, as a fraction of the Nyquist frequency |
| `high` | `0.9` | Upper cutoff, as a fraction of the Nyquist frequency |
| `q` | `0.707` | Filter strength (quality factor). Must be above 0 |

## bandpass_mhz
Apply a zero-phase bandpass filter to each trace individually, with the frequencies in MHz.

Example: `bandpass_mhz(100 800)`. Filters as `bandpass` does, so each cutoff is at -6 dB.

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

## dewow
Remove slow drift ("wow") from each trace by subtracting the running median or mean of the samples around each sample.

This is a zero-phase high-pass that works on each trace separately. `auto` makes the window two periods of the antenna's nominal frequency, which removes drift slower than that and keeps the wavelet. A window much shorter than a period removes the signal itself. The median is the default because the mean is pulled by the strong direct wave and leaves an artefact below it, and a median over only one period distorts the wavelet. Examples: `dewow`, `dewow(10)` for a 10 ns window, `dewow(method=mean)`.

| argument | default | description |
|---|---|---|
| `window` | `auto` | `auto`, two periods of the antenna frequency, or a window in nanoseconds |
| `method` | `median` | `median` or `mean` |

## background_removal
Remove what the traces share at the same sample, such as antenna ringing and horizontal banding, by subtracting the median or mean trace.

`traces` is `all`, one background for the whole radargram, or an odd number of traces for a running background centred on each trace, which follows ringing that changes along the profile. Anything horizontal and as long as the window is removed too, including a flat bed or the direct wave, so a running window should be much longer than any flat reflector worth keeping. The median keeps a reflector found in fewer than half the traces of the window intact; the mean spreads a fraction of it into every trace. Examples: `background_removal`, `background_removal(501)`, `background_removal(all, mean)`.

| argument | default | description |
|---|---|---|
| `traces` | `all` | `all`, or an odd number of traces for a running background |
| `method` | `median` | `median` or `mean` |

## remove_tones
Remove narrowband interference ("tones") that is measured below the deepest reflection and subtracted from the whole trace.

A continuous interferer can appear in every trace as a sinusoid of constant amplitude from top to bottom, whose frequency drifts slowly along the profile and whose phase jumps from trace to trace. It draws stacks of hyperbola-like stripes where neighbouring traces happen to agree in phase, and averaging traces turns it into a slow, regular rise and fall of power along the profile.

`start` is the travel time in ns below which the record is taken to be only noise. There, each trace's spectrum is averaged over `traces` neighbouring traces, and up to `max_tones` peaks that stand `prominence` dB above the median of the spectrum within 50 MHz of them (wider for a short noise window) are taken as tones. Their amplitudes and phases are then fitted in each trace on its own, and only those sinusoids are subtracted, from every sample. Anything above `start` that is not at a picked frequency is left as it was.

The tones are sinusoids in the samples as recorded, so run this first: before `average_traces`, which blurs a tone whose phase jumps between traces, and before `correct_antenna_separation`, which resamples the traces. If the record is too short below `start`, nothing is removed and the log says so. Examples: `remove_tones(150)`, `remove_tones(150, max_tones=2)`.

So far this has only been tested on Malå ProEx 800 MHz data. Check the result before relying on it for other instruments or antennas.

| argument | default | description |
|---|---|---|
| `start` | *required* | Travel time in ns below which the record is only noise |
| `max_tones` | `5` | Most tones to remove from each trace. At least 1 |
| `traces` | `201` | How many traces the spectra are averaged over to find the tones |
| `prominence` | `6` | How far a tone must stand above the surrounding spectrum, in dB |

## balance_traces
Even out slow rises and falls of amplitude along the profile, such as bright vertical bands ("godrays"), without evening out real bright or dark zones.

The gain is measured in two windows: a shallow one from `shallow_start` to `shallow_end`, where the layering should look the same along the profile, and from `deep` to the bottom, where the record should be only noise. In each, a trace's power averaged over `traces` neighbouring traces is compared with its running median over `reference` traces, and the trace is scaled towards that median. Between the two windows the gain changes smoothly with depth, and it is constant above and below them. Nothing between the windows is measured, so a reflector there keeps its brightness. All times are travel times in ns.

Changes shorter than `traces` are too short to be measured, and changes longer than `reference` are kept. Both are seconds of recording, with an `s` (`10s`), or a number of traces (`51`). Seconds use the trace interval in the file header, which `average_traces` keeps up to date and GPS timestamps do not affect; give traces if that interval is missing or wrong, or after `equidistant_traces`, after which a trace is a distance and not a time. On a profile shorter than `reference`, the reference is the median of the whole profile.

Amplitudes are no longer comparable along the profile afterwards, only within each stretch of it. If a window is outside the record, or seconds cannot be converted to traces, nothing is changed and the log says so. Examples: `balance_traces(150)`, `balance_traces(150, shallow_start=5, shallow_end=30)`, `balance_traces(150, traces=51, reference=2001)`.

So far this has only been tested on Malå ProEx 800 MHz data. Check the result before relying on it for other instruments or antennas.

| argument | default | description |
|---|---|---|
| `deep` | *required* | Travel time in ns below which the record is only noise |
| `shallow_start` | `10` | Start of the shallow window, in ns |
| `shallow_end` | `40` | End of the shallow window, in ns |
| `traces` | `10s` | How much of the profile the power is averaged over before it is compared: seconds (`10s`) or traces (`51`) |
| `reference` | `400s` | How much of the profile the reference (running median) spans: seconds (`400s`) or traces (`2001`) |

## auto_gain
Measure the gain that levels the amplitude below the direct wave, and apply it with `gain`.

The samples are split into bins from top to bottom, and each bin's level is the median absolute amplitude over all its samples and traces. The direct wave's ring-down is skipped, and the gain is the median decrease in level between neighbouring bins below it, in dB/ns. This is a display gain, not an attenuation estimate. If the amplitude grows with time, or no gain can be measured (e.g. too short a record), no gain is applied and the log says why. The number of bins can be given, e.g. `auto_gain(100)`.

| argument | default | description |
|---|---|---|
| `n_bins` | `100` | Number of vertical bins. At least 2 |

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

With the transmitter and receiver apart, a reflection travels two slant legs, and time zero (the air wave's arrival at the receiver) comes after the pulse left the transmitter. Depth is therefore not linear in travel time, least of all near the surface. This step resamples each trace so that each sample represents a consistent depth interval.

The `slant` grid keeps the sample interval: one sample is `v * dt / 2` deep, starting from the depth of the first sample's travel time, so the output's `depth` coordinate is the depth of each sample exactly. Up to 0.8.1 its spacing was taken from the data and came out slightly larger, and `depth` fell short by a fraction of a percent.

Afterwards, `twtt` is the travel time a coincident transmitter and receiver would have recorded rather than the travel time between the pair, and the output declares that with `twtt:anchor_name = "twtt_normal_incidence"` and `antenna_separation_effective = 0`.

`legacy` is the conversion before 0.7, which used the full separation where the geometry needs half and, after a zero correction, no separation at all. It exists to regenerate data processed with it, and picks made on its grid, exactly.

`direct_velocity` is the velocity of the wave that time zero was picked on. The default is the speed of light in air, because the air wave arrives first. Pass the medium velocity to time it from the ground wave, as ImpDAR's `nmo` does. Only this step reads it: the depth axis of an uncorrected radargram always assumes air. Examples: `correct_antenna_separation(legacy)`, `correct_antenna_separation(slant, 0.168)`.

| argument | default | description |
|---|---|---|
| `method` | `slant` | `slant` or `legacy` |
| `direct_velocity` | `0.2997` | `slant` only: the direct wave's velocity, in m/ns |

## multiply
Multiply all values by a constant factor.

This is useful e.g. for standardizing data between sensors and antenna frequencies. Example: `multiply(5)`.

| argument | default | description |
|---|---|---|
| `factor` | *required* | Factor to multiply by. Must be finite and non-zero |
