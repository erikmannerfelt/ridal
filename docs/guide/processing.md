# Processing and filters

Processing turns a raw recording into a radargram that can be read, by
putting time zero where the signal starts, removing what is not reflected
signal, and making deep reflections about as visible as shallow ones. In
Ridal, processing is a list of **steps** that are run in order. Each step is
described in full, with its arguments, in {doc}`../reference/steps`, while
this page is about choosing them.

## Writing steps

A step is written as its name, optionally followed by arguments in
parentheses, which can be given either in order or by name:

```text
zero_corr
bandpass(0.1 0.9)
bandpass(low=0.1, high=0.9)
zero_corr(coppens, scope=trace)
```

The same list works everywhere: comma-separated after `--steps`, one step per
line in a steps file (where `#` starts a comment) or in the GUI's steps box,
or as a Python list. `ridal steps` lists every step, and
`ridal steps --describe-all` describes them.

What each step did is written into the output. `processing_steps` holds the
steps with every argument, defaults included, and `processing_log` holds what
each step found and did, such as the time zero it picked or the gain it
measured. **Metadata** in the viewer shows both, as do `ncdump -h` and the
attributes of the dataset in xarray. Reading the log after a first
processing is a good habit, since several steps measure something from the
data and report what they found.

## The default profile

`--default` (`default=True` in Python, or **Replace with default steps** in
the GUI) runs:

```text
remove_empty_traces
zero_corr
correct_antenna_separation
dewow
background_removal
auto_gain(100)
```

The default profile is meant to give a readable radargram from most data
without any choices, since every step in it either measures what it needs
from the data or does nothing harmful when there is nothing to do.
`--default-with-topo` adds `correct_topography` at the end.

The default is a starting point and not a recommendation for publication.
Typical changes are to add `remove_standstills` and `equidistant_traces` for
time-triggered data, a `bandpass` for noisy data, and migration.

## The families of steps

The families are listed roughly in the order they are usually run.

### Removing and reorganizing traces

`remove_empty_traces`
: Removes traces with no signal in them, and should be run first.

`subset`, `remove_traces`
: Crop the radargram to a range of traces or samples, or remove particular
  traces by their index. For example, `subset(max_sample=1000)` cuts off a
  long and noisy bottom of the record.

`remove_standstills`
: Replaces each stretch recorded while the radar stood still with one trace.
  A time-triggered radar keeps recording when the survey stops, so a long
  stop becomes a long smear of identical traces. It should be run before the
  filters, which change the trace similarity that it looks for.

`average_traces`
: Averages neighboring traces, for fewer traces and less random noise.

`equidistant_traces`
: Resamples the traces onto an even spacing along the track, so that the
  horizontal axis is distance rather than time. Migration assumes even
  spacing, so this step should come before it.

### Time zero

`zero_corr`
: Finds the direct wave, crops what was recorded before it, and makes travel
  time count from it. Nearly every other step and every depth depends on time
  zero being right, so this step comes early. The default method (`coppens`)
  and scope (`global`, i.e. one time zero for the whole radargram) suit most
  data. When time zero drifts along the profile, `scope=trace` or
  `scope=smooth` finds one time zero per trace, and `max_peak` copes best
  with noisy or corrupted first samples.

### Geometry

`correct_antenna_separation`
: Resamples each trace so that the samples are evenly spaced in depth, while
  accounting for the distance between the transmitter and the receiver. It
  matters most for low frequencies, where the antennas are meters apart (see
  {doc}`coordinates`), and should be run after `zero_corr`.

`shift_coordinates`
: Moves the positions to where the radar was, when the GNSS antenna was
  mounted elsewhere.

`kirchhoff_migration2d`
: Collapses diffraction hyperbolas into the points that cause them and moves
  dipping reflectors to their true positions. It uses the medium velocity,
  the elevations and the mean trace spacing, so it should be run after
  `zero_corr`, `correct_antenna_separation` and `equidistant_traces`, and
  only when the velocity is a fair approximation of the medium. Migration is
  also slow on large radargrams.

`correct_topography`
: Writes a topographically corrected copy of the data into the file. The
  viewer can show the same view without this step (see {doc}`coordinates`).

### Frequency filters

`dewow`
: Removes slow drift ("wow") within each trace, which is a low-frequency
  offset that shifts whole stretches of a trace to one side of zero. It is
  almost always useful, and its default window is two periods of the antenna
  frequency.

`bandpass`, `bandpass_mhz`
: Keep a band of frequencies and remove those above and below it, for noise
  outside the antenna's band. `bandpass` takes the band as fractions of the
  Nyquist frequency and `bandpass_mhz` takes it in MHz, such as
  `bandpass_mhz(50 200)` around a 100 MHz antenna. Both filters are
  zero-phase, so reflections stay where they are.

### Removing coherent noise

`background_removal`
: Subtracts what the traces have in common at each travel time, such as
  antenna ringing and horizontal banding. It also removes any truly
  horizontal reflector that is as long as its window, so when a flat bed or a
  lake surface is worth keeping, a running window (`background_removal(501)`)
  that is much longer than the flat stretch is the better choice.

`remove_tones`
: Removes narrowband interference, i.e. a sinusoid in every trace from a
  nearby transmitter, which appears as stripes or hyperbola-like patterns. It
  should be run first, on the samples as recorded. So far, it has only been
  tested on Malå ProEx 800 MHz data.

`balance_traces`
: Evens out slow brightening and darkening along the profile, such as bright
  vertical bands, while leaving real bright or dark zones intact. So far, it
  has only been tested on Malå ProEx 800 MHz data.

### Gain

The signal weakens with depth through geometric spreading and attenuation, so
deep reflections are faint compared to shallow ones.

`auto_gain`
: Measures how fast the amplitude falls off with travel time and applies the
  gain that levels it out (with `gain`). It is a display gain and not an
  estimate of attenuation.

`gain`
: Applies an exponential gain of a given number of dB per ns. It is the
  better choice when radargrams that are compared should have the same gain,
  since `auto_gain` measures a separate gain for each radargram.

`multiply`
: Multiplies everything by a constant, for example to put different
  instruments on the same scale.

### Display transforms

`siglog`, `adaptive_siglog`, `abslog`, `unphase`
: Compress or fold the amplitude for viewing, as a signed logarithm, the
  logarithm of the magnitude, or the two phases of a wavelet combined. They
  change the values that are written to the file. The same compression is
  available at render time instead (the `siglog-*` render profiles; see
  {doc}`rendering`), which keeps the file's amplitudes and lets each viewer
  choose, so that is usually the better option unless the data are meant to
  be analyzed in their compressed form.

## Choosing a velocity

`--velocity` converts travel time to depth, and migration uses it too. Its
default of 0.168 m/ns is a typical value for most glacier ice, but not all
ice (and not all glacier ice) has that velocity. Approximate values for some
common media are:

| Medium | Velocity (m/ns) |
|---|---|
| Air | 0.30 |
| Most glacier ice | 0.168 |
| Dry snow and firn | 0.19–0.24, depending on density |
| Dry sand | 0.12–0.15 |
| Wet sediments | 0.06–0.10 |
| Fresh water | 0.033 |

One velocity applies to the whole radargram (see {doc}`coordinates`).

## Processing in the browser

In a project served with `ridal gui` or `ridal server`, an `operator` can
also process radargrams from the GUI (see {doc}`gui`).

A **processing profile**, made on the **Processing** page, is a list of steps
and a velocity that many radargrams share. A radargram whose processing no
longer matches its profile, because either the profile or its raw data
changed, is marked as *stale*, and **Process out-of-date radargrams**
reprocesses every stale radargram in the profile.

**Process…** on a radargram processes it, with a profile or with steps of
its own, and shows the result before anything is replaced, so that it can be
compared with **Show current** before **Apply and close**. A radargram's own
steps before the profile are limited to `subset` and `remove_traces`, which
are meant for cropping that belongs to one recording rather than to the
whole profile.

## Reprocessing and picks

Picks are tied to a radargram's id rather than to one processing of it. When
a radargram is processed again, picks drawn on the earlier processing are
carried onto the new one, for example across a zero correction that moved
time zero, a `subset`, removed standstills or a different antenna separation
correction. The viewer says when picks could not be carried exactly, and
steps that only change amplitudes, such as filters and gain, do not move
picks at all.
