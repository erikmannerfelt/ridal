# File formats

## Input formats

Ridal reads three families of impulse GPR data. Each recording is several
files with one name and different extensions, and any of them can be given as
the input: Ridal finds the others beside it, whatever the case of their
extensions.

| Format | Header | Data | Positions |
|---|---|---|---|
| Malå RAMAC (`ramac`) | `.rad` | `.rd3` | `.cor` |
| Sensors & Software pulseEKKO (`pulseekko`) | `.hd` | `.dt1` | `.gp2` |
| GSSI (`gssi`) | `.DZT` | `.DZT` | `.DZG` |

`ridal formats` prints the same table, and `ridal.all_format_descriptions`
holds it in Python. A path without an extension works too when only one
format's header sits beside it.

The positions file is required: a radargram without positions cannot be
placed on a map, and Ridal refuses to read it. For RAMAC, `--cor` (or `cor=`
in Python) reads positions from a `.cor` file with another name. GSSI and
pulseEKKO positions are read from the NMEA sentences (`$GPGGA`) in their
sidecar files.

Ridal reads but does not yet write these formats; see
{doc}`../background/scope`.

### What is read from the header

The header gives the number of samples, the time window, the sampling
frequency, the antenna and its separation, and the interval between traces.
Two of these are sometimes missing or wrong, and can be given instead:

Antenna frequency
: Read from the antenna's name in the header (`25 MHz RTA=11` is 25 MHz).
  When the name gives no frequency, reading fails with "Could not read
  frequency from the antenna field" until `--override-antenna-mhz` gives one.
  `dewow` uses it for its default window.

Antenna separation
: The distance between transmitter and receiver, in metres, read from the
  RAMAC and pulseEKKO headers. A GSSI header does not record it, so it is 0
  unless `--override-antenna-separation` gives it. It matters for
  `correct_antenna_separation` and for depths near the surface; see
  {doc}`coordinates`.

The medium velocity is not read from any file: it is `--velocity`, 0.168 m/ns
(ice) by default.

In the GUI, the **Process…** dialog and **Add raw data in bulk…** take the
same two overrides (see {doc}`gui`).

### Positions to traces

The positions file holds fixes for some of the traces, not all of them.
Positions between two fixes are interpolated linearly in trace number, and
positions before the first fix or after the last are held at that fix, since
nothing says where the radar went. Times beyond the fixes are extrapolated at
the trace interval instead, because the radar kept recording.

Positions are projected into a CRS on reading, and elevations come from the
positions file unless a DEM replaces them; see {doc}`coordinates`.

## Ridal's NetCDF output

Processing writes one NetCDF-4 file per radargram, following the CF
conventions (1.7) where they apply, so it opens in xarray, Panoply, MATLAB
or anything else that reads NetCDF:

```python
import xarray as xr

ds = xr.open_dataset("DAT_0066_A1.nc")
ds["data"].plot(yincrease=False)
```

### Dimensions

`x`
: Traces, along the profile. `x` is also a coordinate: the zero-based trace
  index.

`y`
: Samples, down each trace. `y` is the zero-based sample index.

`y2`
: Rows of the topographically corrected copy, only when
  `correct_topography` has run, indexed like `y`.

### Variables

`data(y, x)`
: The amplitude, as 32-bit floats, compressed and chunked in tiles of 256 ×
  256 samples so that a part of it can be read without reading all of it.
  Its `units` is `mV`, but after gain or log steps the values are no longer
  millivolts; the processing log says what was done to them.

`twtt(y)`
: Two-way travel time of each sample, in ns, counted from time zero (see
  below). Negative above time zero when a zero correction kept a margin.

`depth(y)`
: Depth of each sample, in m, at the constant `medium_velocity`.

`distance(x)`
: Distance along the profile, in m, from the first trace.

`time(x)`
: Time of each trace, in seconds since 1970-01-01 UTC.

`easting(x)`, `northing(x)`
: Position in the projected CRS, in m.

`longitude(x)`, `latitude(x)`
: The same position in WGS84 degrees.

`elevation(x)`
: Elevation of the surface at each trace, in m, from the positions file or
  from a DEM.

`projected_crs`
: An empty variable whose attributes describe the CRS, as CF grid-mapping
  attributes and as WKT (`crs_wkt`). `data` points at it through its
  `grid_mapping` attribute, which is how GDAL and QGIS find the CRS.

`twtt_crop`, `twtt_time_zero`
: Scalars, in ns, that place the samples on the instrument's original clock:
  how much of the start of the recording was cropped away, and where time
  zero was found on that clock. See below.

`data_topocorr(y2, x)`, `elevation_topocorr(y2)`
: Only after `correct_topography`: a copy of `data` shifted so that each row
  is one elevation, and that elevation, in m.

### Global attributes

Measurement
: `antenna`, `antenna_frequency` (MHz), `antenna_separation` (m),
  `vertical_sampling_frequency` (MHz), `time_interval` (s between traces),
  `frequency_steps`, `medium_velocity` (m/ns), `start_datetime`,
  `stop_datetime`, `total_distance` (m), `crs` and `elevation_correction`
  (`None`, or the DEM the elevations were sampled from). Each quantity with a
  unit has a `_unit` attribute beside it.

`antenna_separation_effective`
: The separation the travel times still include: the recorded one, or 0
  after `correct_antenna_separation`, which converts the travel times to
  those of a coincident transmitter and receiver.

Processing
: `processing_steps` (the steps as run, with every argument), `processing_log`
  (what each step did, with timings), `original_filepaths` and
  `ridal_version`.

Identity
: `ridal_radargram_id` (the stable id that picks are tied to),
  `ridal_display_name`, `ridal_group_name` and `ridal_group_id` when given,
  and `ridal_processing_datetime`.

Recipe
: `ridal_recipe` is a JSON description of everything that went into the
  file: the raw files with their hashes, the CRS, DEM, velocity, overrides
  and steps. `ridal_recipe_hash` is its hash, and names the revision:
  processing the same raw files the same way gives the same revision.

`ridal_user_metadata_json`
: What `--metadata key=value` (or `metadata=` in Python) added, as JSON.

Group and display names are written as given, so they may be non-ASCII
(Drønbreen). Everything Ridal writes itself is ASCII. Some readers garble a
non-ASCII attribute, for example xarray with the `h5netcdf` engine.

### Time zero and travel time

Time zero is where travel time is counted from: the arrival of the direct
wave at the receiver. A raw recording starts some time before
that, so its first samples are not travel time at all. A zero correction
(`zero_corr`) finds the direct wave, crops the samples before it, and makes
`twtt` count from it.

Two scalars keep that traceable on the recording's own clock:

- `twtt_crop`: where sample 0 is on the original recording, that is how much
  was cropped, by a zero correction or by `subset`.
- `twtt_time_zero`: where time zero is on the same clock.

The travel time of sample 0 is `twtt_crop - twtt_time_zero`: zero when the
correction cropped exactly at time zero, negative when it kept a margin above
it. A `twtt_time_zero` of exactly 0 means time zero was never located: no zero
correction has run, and `twtt` counts from whenever the instrument started
sampling. Picks can still be carried between such a radargram and another
processing of the same recording, because both are on the recording's clock.

`twtt` carries `anchor_name = "twtt_normal_incidence"` once
`correct_antenna_separation` has run, to say that its times are those of a
coincident transmitter and receiver.

## Other outputs

Track CSV
: `-t`/`--track` writes the positions as `trace_n,easting,northing,altitude`,
  one row per trace, in the radargram's CRS.

Images
: `-r` and `ridal render` write JPEG or PNG images, and annotated figures as
  PNG or SVG; see {doc}`rendering`.

Interpretations and points
: Picks are stored as [gprinterp](https://docs.rs/gprinterp) JSON documents
  and exported as GeoJSON or CSV points; see {doc}`interpretation` and
  {doc}`../reference/project-files`.
