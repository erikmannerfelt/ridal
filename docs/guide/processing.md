# Processing and filters

Processing turns a raw recording into a radargram that can be read: it puts
time zero where the signal starts, removes what is not reflected signal, and
makes deep reflections as visible as shallow ones. In Ridal, processing is a
list of **steps** run in order. Each step is described in full, with its
arguments, in {doc}`../reference/steps`; this page is about choosing them.

## Writing steps

A step is its name, optionally followed by arguments in parentheses, given in
order or by name:

```text
zero_corr
bandpass(0.1 0.9)
bandpass(low=0.1, high=0.9)
zero_corr(coppens, scope=trace)
```

The same list works everywhere: comma-separated after `--steps`, one per line
in a steps file (with `#` comments) or in the GUI's steps box, or as a Python
list. `ridal steps` lists every step, and `ridal steps --describe-all`
describes them.

What each step did is written into the output: `processing_steps` holds the
steps with every argument, defaults included, and `processing_log` what each
one found and did, such as the time zero it picked or the gain it measured.
**Metadata** in the viewer shows them, as do `ncdump -h` and the attributes
of the dataset in xarray. Read the log after a first processing: several
steps measure something from the data and say what.

## The default profile

`--default` (`default=True` in Python, **Replace with default steps** in the
GUI) runs:

```text
remove_empty_traces
zero_corr
correct_antenna_separation
dewow
background_removal
auto_gain(100)
```

It is meant to give a readable radargram from most data without any choices:
every step either measures what it needs from the data or does nothing
harmful when there is nothing to do. `--default-with-topo` adds
`correct_topography` at the end.

The default is a starting point, not a recommendation for publication.
Typical changes are adding `remove_standstills` and `equidistant_traces` for
time-triggered data, a `bandpass` for noisy data, and migration.

## The families of steps

Roughly in the order they are usually run:

### Removing and reorganising traces

`remove_empty_traces`
: Removes traces with no signal in them. Run it first.

`subset`, `remove_traces`
: Crop the radargram to a range of traces or samples, or remove particular
  traces, by index. `subset(max_sample=1000)` cuts off a long noisy bottom.

`remove_standstills`
: Replaces each stretch recorded while the radar stood still with one trace.
  A time-triggered radar keeps recording when the survey stops, and a long
  stop is a long smear of identical traces. Run it before the filters, which
  change what it looks for.

`average_traces`
: Averages neighbouring traces, for fewer traces and less random noise.

`equidistant_traces`
: Resamples the traces onto even spacing along the track, so the horizontal
  axis is distance rather than time. Migration assumes even spacing.

### Time zero

`zero_corr`
: Finds the direct wave, crops what was recorded before it, and makes travel
  time count from it. Nearly every other step and every depth depends on
  time zero being right, so this comes early. The default method (`coppens`)
  and scope (`global`, one time zero for the whole radargram) suit most data.
  When time zero drifts along the profile, `scope=trace` or `scope=smooth`
  finds one per trace. `max_peak` copes best with noisy or corrupted first
  samples.

### Geometry

`correct_antenna_separation`
: Resamples each trace so that the samples are evenly spaced in depth,
  accounting for the distance between transmitter and receiver. It matters
  most for low frequencies, where the antennas are metres apart; see
  {doc}`coordinates`. Run it after `zero_corr`.

`shift_coordinates`
: Moves the positions to where the radar was, when the GNSS antenna was
  elsewhere.

`kirchhoff_migration2d`
: Collapses diffraction hyperbolas into the points that cause them, and
  moves dipping reflectors to their true positions. It uses the medium
  velocity, the elevations and the mean trace spacing, so run it after
  `zero_corr`, `correct_antenna_separation` and `equidistant_traces`, and only
  when the velocity is a fair approximation. Migration is slow on large
  radargrams.

`correct_topography`
: Writes a topographically corrected copy of the data into the file. The
  viewer can show the same view without it; see {doc}`coordinates`.

### Frequency filters

`dewow`
: Removes slow drift ("wow") within each trace, a low-frequency offset that
  shifts whole stretches of a trace to one side of zero. Almost always
  useful. By default its window is two periods of the antenna frequency.

`bandpass`, `bandpass_mhz`
: Keep a band of frequencies and remove those above and below it, for noise
  outside the antenna's band. `bandpass` takes the band as fractions of the
  Nyquist frequency, `bandpass_mhz` in MHz, such as `bandpass_mhz(50 200)`
  around a 100 MHz antenna. Both are zero-phase, so reflections stay where
  they are.

### Removing coherent noise

`background_removal`
: Subtracts what the traces have in common at each travel time: antenna
  ringing and horizontal banding. It also removes any truly horizontal
  reflector as long as its window, so with a flat bed or a lake surface
  worth keeping, use a running window (`background_removal(501)`) much
  longer than the flat stretch.

`remove_tones`
: Removes narrowband interference, a sinusoid in every trace from a nearby
  transmitter, seen as stripes or hyperbola-like patterns. Run it first, on
  the samples as recorded. So far tested on Malå ProEx 800 MHz data.

`balance_traces`
: Evens out slow brightening and darkening along the profile, such as bright
  vertical bands, while leaving real bright or dark zones. So far tested on
  Malå ProEx 800 MHz data.

### Gain

The signal weakens with depth, through spreading and attenuation, so deep
reflections are faint next to shallow ones.

`auto_gain`
: Measures how fast the amplitude falls off with travel time and applies the
  gain that levels it, with `gain`. It is a display gain, not an estimate of
  attenuation.

`gain`
: Applies an exponential gain of a given number of dB per ns. Use it when
  radargrams that are compared should have the same gain, which `auto_gain`
  measures for each one separately.

`multiply`
: Multiplies everything by a constant, to put different instruments on one
  scale.

### Display transforms

`siglog`, `adaptive_siglog`, `abslog`, `unphase`
: Compress or fold the amplitude for viewing: a signed logarithm, the
  logarithm of the magnitude, or the two phases of a wavelet combined. They
  change the values written to the file. The same compression is available
  at render time instead (the `siglog-*` render profiles; see
  {doc}`rendering`), which keeps the file's amplitudes and lets each viewer
  choose. Prefer that unless the data are meant to be analysed in the
  compressed form.

## Choosing a velocity

`--velocity` (0.168 m/ns, ice, by default) converts travel time to depth, and
migration uses it. Approximate values for common media:

| Medium | Velocity (m/ns) |
|---|---|
| Air | 0.30 |
| Ice | 0.168 |
| Dry snow and firn | 0.19–0.24, by density |
| Dry sand | 0.12–0.15 |
| Wet sediments | 0.06–0.10 |
| Fresh water | 0.033 |

The velocity applies to the whole radargram; see {doc}`coordinates`.

## Processing in the browser

On a project served with `ridal gui` or `ridal server`, an `operator` can
process radargrams from the GUI instead (see {doc}`gui`).

A **processing profile**, made on the **Processing** page, is a list of steps
and a velocity that many radargrams share. A radargram whose processing no
longer matches its profile, because the profile or its raw data changed, is
marked *stale*, and **Process out-of-date radargrams** reprocesses every stale
one.

**Process…** on a radargram processes it, with a profile or with steps of its
own, and shows the result before it replaces anything: compare it with
**Show current**, then **Apply and close**. A radargram's own steps before
the profile are limited to `subset` and `remove_traces`, for cropping that
belongs to one recording rather than to the profile.

## Reprocessing and picks

Picks are tied to a radargram's id, not to one processing of it. When a
radargram is processed again, picks drawn on the earlier processing are
carried onto the new one: through a zero correction that moved time zero, a
`subset`, removed standstills or a different antenna separation correction.
The viewer says when picks could not be carried exactly. Steps that only
change amplitudes, such as filters and gain, do not move picks at all.
