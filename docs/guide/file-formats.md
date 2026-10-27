# File formats

## Input formats

Ridal reads four families of impulse GPR data. A recording of the first
three consists of several files that share a name but have different
extensions, and any one of them can be given as the input, since Ridal finds
the others beside it regardless of the case of their extensions. An IceRadar
recording is a single file (see [below](#iceradar)).

| Format | Header | Data | Positions |
|---|---|---|---|
| Malå RAMAC (`ramac`) | `.rad` | `.rd3` | `.cor` |
| Sensors & Software pulseEKKO (`pulseekko`) | `.hd` | `.dt1` | `.gp2` |
| GSSI (`gssi`) | `.DZT` | `.DZT` | `.DZG` |
| Blue System Integration IceRadar (`iceradar`) | `.h5`/`.hdf5` | the same file | the same file |

`ridal formats` prints the same table, and `ridal.all_format_descriptions`
holds it in Python. A path without an extension also works, as long as only
one format's header sits beside it.

Positions are required, because a radargram without positions cannot be
placed on a map, and Ridal refuses to read a recording without them. GSSI and
pulseEKKO positions are read from the NMEA sentences (`$GPGGA`) in their
sidecar files, and IceRadar positions from the GPS fields stored with each
trace. Positions from anywhere else, for any format, can be given as
a coordinate CSV, either beside the recording as `<stem>.coords.csv` or with
`--cor` (see {ref}`coordinate-csv`).

Ridal reads but does not yet write these formats (see
{doc}`../background/scope`). Multi-channel GSSI files are refused for now;
a single-channel file whose last trace was cut short is read without it.

### What is read from the header

The header gives the number of samples, the time window, the sampling
frequency, the antenna and its separation, and the interval between traces.
Two of these are sometimes missing or wrong, and can then be given
explicitly:

Antenna frequency
: Read from the antenna's name in the header, so that `25 MHz RTA=11` is
  read as 25 MHz. When the name contains no frequency, reading fails with
  "Could not read frequency from the antenna field" until
  `--override-antenna-mhz` gives one. The frequency is used by `dewow` for
  its default window, among other things.

Antenna separation
: The distance between the transmitter and the receiver (in meters), read
  from the RAMAC and pulseEKKO headers. GSSI and IceRadar files do not record
  it, so it is 0 for them unless `--override-antenna-separation` gives it
  (and for IceRadar, Ridal warns that it was not given). The separation
  matters for `correct_antenna_separation` and for depths near the surface
  (see {doc}`coordinates`).

Date
: GPS positions in pulseEKKO and IceRadar files carry only a time of day,
  and the date comes from the logger's or the acquisition PC's clock, which
  can be wrong. `--override-date YYYY-MM-DD` gives the UTC date instead.
  It is refused for formats whose positions carry their own dates, and for
  positions from a coordinate CSV.

The medium velocity is not read from any file; it is given with
`--velocity`, which defaults to 0.168 m/ns (a typical value for most glacier
ice).

In the GUI, the **Process…** dialog and **Add raw data in bulk…** take the
antenna overrides (see {doc}`gui`), but not yet the date.

(iceradar)=
### IceRadar

An IceRadar file is HDF5 holding one or more *lines*. Each line is a
sequence of traces, stored one per group with the digitizer settings and the
GPS fix of that trace. Two versions of the acquisition software are known,
with differently named fields, and Ridal reads both. There is no public
description of the format, so what follows is what Ridal assumes.

Lines
: A file of one line is read like any other recording. A file of several
  needs `--lines` (`lines=` in Python), and fails with a list of its lines
  and their trace counts until it is given: one line (`--lines 3`), several
  merged in the order given (`--lines 0,2-4`), or `--lines all`. Lines are
  merged as several files are, so lines recorded with different settings,
  such as a different record length, are refused with the reason. The
  output of one line of a multi-line file is named `<stem>_line_<N>.nc`.
  `batch-process` does not split a file into lines yet, and refuses a file
  of several.

Antenna
: IceRadar files record neither the antenna frequency nor the antenna
  separation. `--override-antenna-mhz` is required, and the separation is
  0 m with a warning unless `--override-antenna-separation` gives it.

Date
: The GPS gives the time of day in UTC. The date is the acquisition PC's,
  which is local time and may be off by hours, so Ridal warns that it used
  it. A file whose PC clock was at its reset value (1 January 2000) is
  refused until `--override-date` gives the date. A line that runs past
  midnight UTC continues into the next day.

Positions
: Positions are read as north and east, since the files do not record the
  hemisphere, and one version names the longitude `Long_ W` for data from
  east of Greenwich. A trace's fix is used only when the file marks it valid
  and every field is shaped as GPS gives it, since some files hold shifted
  fields in traces marked valid. Traces without a usable fix are placed by
  interpolation, as between the fixes of any other format.

Traces
: Samples are converted from volts to millivolts. Traces that captured
  nothing (a stacking count of 0) are left out. When the record length
  changes within a line, the traces are aligned on the trigger and the
  shorter ones are padded with zeros to the longest. The time window starts
  at the start of the record, before the trigger, so time zero is found by
  `zero_corr` as for the other formats.

Autosaves
: The software keeps a copy of the first part of a recording as
  `<name>.hdf5.x1`. Ridal refuses it and names the file it belongs to.

### From positions to traces

The positions file holds fixes for some of the traces, not for all of them.
Positions between two fixes are interpolated linearly in trace number, and
positions before the first fix or after the last are held at that fix, since
nothing says where the radar went there. Times beyond the fixes are instead
extrapolated with the interval between traces, because the radar kept
recording at that rate.

The positions are projected into a coordinate reference system (CRS) when
the file is read, and the elevations come from the positions file unless a
DEM replaces them (see {doc}`coordinates`).

## Ridal's NetCDF output

Processing writes one NetCDF-4 file per radargram, following the CF
conventions (version 1.7) where they apply, so that it opens in xarray,
Panoply, MATLAB or anything else that reads NetCDF:

```python
import xarray as xr

ds = xr.open_dataset("DAT_0066_A1.nc")
ds["data"].plot(yincrease=False)
```

### Dimensions

`x`
: Traces along the profile. `x` is also a coordinate, which holds the
  zero-based trace index.

`y`
: Samples down each trace, with `y` holding the zero-based sample index.

`y2`
: Rows of the topographically corrected copy, indexed like `y`, which only
  exists when `correct_topography` has been run.

### Variables

`data(y, x)`
: The amplitude as 32-bit floats, compressed and chunked in tiles of
  256 × 256 samples so that a part of it can be read without reading the
  whole array. Its `units` attribute is `mV`, but after gain or log steps
  the values are no longer millivolts, and the processing log says what was
  done to them.

`twtt(y)`
: The two-way travel time of each sample (in ns), counted from time zero
  (see [below](#time-zero-and-travel-time)). It is negative above time zero
  when a zero correction kept a margin there.

`depth(y)`
: The depth of each sample (in m), at the constant `medium_velocity`.

`distance(x)`
: The distance along the profile (in m) from the first trace.

`time(x)`
: The time of each trace, in seconds since 1970-01-01 UTC.

`easting(x)`, `northing(x)`
: The position in the projected CRS (in m).

`longitude(x)`, `latitude(x)`
: The same position in WGS84 degrees.

`elevation(x)`
: The elevation of the surface at each trace (in m), from the positions file
  or from a DEM.

`projected_crs`
: An empty variable whose attributes describe the CRS, both as CF
  grid-mapping attributes and as WKT (`crs_wkt`). `data` points at it
  through its `grid_mapping` attribute, which is how GDAL and QGIS find the
  CRS.

`twtt_crop`, `twtt_time_zero`
: Two scalars (in ns) that place the samples on the instrument's original
  clock, by saying how much of the start of the recording was cropped away
  and where time zero was found on that clock (see
  [below](#time-zero-and-travel-time)).

`data_topocorr(y2, x)`, `elevation_topocorr(y2)`
: Only present after `correct_topography`: a copy of `data` shifted so that
  each row is one elevation, and the elevation of each row (in m). The copy
  spans the whole elevation range, so it is larger than `data` itself. Ridal
  never reads it: its corrected views are drawn from `data`.

### Global attributes

Measurement
: `antenna`, `antenna_frequency` (MHz), `antenna_separation` (m),
  `vertical_sampling_frequency` (MHz), `time_interval` (s between traces),
  `frequency_steps`, `medium_velocity` (m/ns), `start_datetime`,
  `stop_datetime`, `total_distance` (m), `crs` and `elevation_correction`
  (`None`, or the DEM that the elevations were sampled from, by `ridal
  process --dem` or by `ridal coords --dem`). Each quantity with a unit has a
  `_unit` attribute beside it.

`time_source`
: Only present when the trace times were made up by `ridal coords` from a
  start time and a trace interval (see {ref}`ridal-coords`). Such times order
  the traces, but say nothing about when the radar stood still.

`antenna_separation_effective`
: The antenna separation that the travel times still include, which is the
  recorded one before `correct_antenna_separation` and 0 after it, since the
  step converts the travel times to those of a coincident transmitter and
  receiver.

Processing
: `processing_steps` (the steps as run, with every argument),
  `processing_log` (what each step did, with timings), `original_filepaths`
  and `ridal_version`.

Identity
: `ridal_radargram_id` (the stable id that picks are tied to),
  `ridal_display_name`, `ridal_group_name` and `ridal_group_id` when they
  are given, and `ridal_processing_datetime`.

Recipe
: `ridal_recipe` is a JSON description of everything that went into the file,
  i.e. the raw files with their hashes, the CRS, the DEM, the velocity, any
  overrides and the steps. `ridal_recipe_hash` is its hash and names the
  revision, so processing the same raw files the same way gives the same
  revision.

`ridal_user_metadata_json`
: What `--metadata key=value` (or `metadata=` in Python) added, as JSON.

Group and display names are written as given, so they may contain non-ASCII
characters (such as in Drønbreen), while everything that Ridal generates
itself is ASCII. Some readers garble a non-ASCII attribute, for example
xarray with the `h5netcdf` engine.

### Time zero and travel time

Time zero is the point that travel time is counted from, which is the
arrival of the direct wave at the receiver. A raw recording starts some time
before that, so its first samples do not represent travel time at all. A
zero correction (`zero_corr`) finds the direct wave, crops the samples before
it, and makes `twtt` count from it.

Two scalars keep this traceable on the recording's own clock: `twtt_crop`
says where sample 0 is on the original recording (i.e. how much was cropped,
either by a zero correction or by `subset`), and `twtt_time_zero` says where
time zero is on the same clock.

The travel time of sample 0 is therefore `twtt_crop - twtt_time_zero`, which
is zero when the correction cropped exactly at time zero and negative when it
kept a margin above it. A `twtt_time_zero` of exactly 0 means that time zero
was never located: no zero correction has been run, and `twtt` counts from
whenever the instrument started sampling. Picks can still be carried between
such a radargram and another processing of the same recording, because both
are on the recording's own clock.

Once `correct_antenna_separation` has been run, `twtt` has the attribute
`anchor_name = "twtt_normal_incidence"`, which says that its times are those
of a coincident transmitter and receiver.

## Other outputs

Track CSV
: `-t`/`--track` writes the positions as `trace_n,easting,northing,altitude`
  with one row per trace, in the radargram's CRS.

Images
: `-r` and `ridal render` write JPEG or PNG images, and annotated figures as
  PNG or SVG (see {doc}`rendering`).

Interpretations and points
: Picks are stored as [gprinterp](https://docs.rs/gprinterp) JSON documents
  and exported as GeoJSON or CSV points (see {doc}`interpretation` and
  {doc}`../reference/project-files`).
