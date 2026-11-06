# File formats

## Input formats

Ridal reads six families of impulse GPR data. A recording of the first
three consists of several files that share a name but have different
extensions, and any one of them can be given as the input, since Ridal finds
the others beside it regardless of the case of their extensions. An IceRadar
or VIRL recording is a single file (see [IceRadar](#iceradar) and
[VIRL](#virl) below), and a SkyHub recording's positions are found by the
names SkyHub and the software that cuts its recordings give them (see
[SkyHub](#skyhub)).

| Format | Header | Data | Positions |
|---|---|---|---|
| Malå RAMAC (`ramac`) | `.rad` | `.rd3` | `.cor` |
| Sensors & Software pulseEKKO (`pulseekko`) | `.hd` | `.dt1` | `.gp2` |
| GSSI (`gssi`) | `.DZT` | `.DZT` | `.DZG` |
| Blue System Integration IceRadar (`iceradar`) | `.h5`/`.hdf5` | the same file | the same file |
| VIRL, as the VIRL-7 (`virl`) | `.DSR` | the same file | the same file |
| SPH Engineering SkyHub, with a Radsys zGPR (`skyhub`) | `.sgy` | the same file | `-position.csv`, a `.txt`, or the same file |

`ridal formats` prints the same table, and `ridal.all_format_descriptions`
holds it in Python. A path without an extension also works, as long as only
one format's header sits beside it.

Positions are required, because a radargram without positions cannot be
placed on a map, and Ridal refuses to read a recording without them. GSSI and
pulseEKKO positions are read from the NMEA sentences (`$GPGGA`) in their
sidecar files, IceRadar positions from the GPS fields stored with each
trace, VIRL positions from the `$GPGGA` sentence in each trace's header, and
SkyHub positions from the files described [below](#skyhub). Positions from anywhere else, for any format, can be given as
a coordinate CSV, either beside the recording as `<stem>.coords.csv` or with
`--cor` (see {ref}`coordinate-csv`).

Ridal reads but does not yet write these formats (see
{doc}`../background/scope`). A GSSI file of several channels is read one
channel at a time (see [below](#gssi)), and one whose last trace was cut
short is read without it. So is a pulseEKKO file whose header counts one
trace more than its data file holds, as an interrupted recording can leave
it, with a warning saying so.

### What is read from the header

The header gives the number of samples, the time window, the sampling
frequency, the antenna and its separation, and the interval between traces.
Two of these are sometimes missing or wrong, and can then be given
explicitly, either for a whole run with `--override KEY=VALUE`, or for
one recording in a sidecar beside it, `<stem>.ridal.toml`, which
`ridal override` writes (see {doc}`../reference/sidecar`). The keys are the
same in both. The option takes precedence over the sidecar, and the sidecar
over the header.

Antenna frequency
: Read from the antenna's name in the header, so that `25 MHz RTA=11` is
  read as 25 MHz. When the name contains no frequency, reading fails with
  "Could not read frequency from the antenna field" until
  `--override antenna_mhz=MHZ` or a sidecar's `antenna_mhz` gives one. The frequency is used by `dewow` for
  its default window, among other things.

Antenna separation
: The distance between the transmitter and the receiver (in meters), read
  from the RAMAC and pulseEKKO headers. GSSI, IceRadar, VIRL and SkyHub files do not
  record it, so it is 0 for them, with a warning, unless
  `--override antenna_separation_m=M` or a sidecar's `antenna_separation_m`
  gives it. A sidecar is the way to give recordings of one survey different
  separations. The separation matters for
  `correct_antenna_separation` and for depths near the surface (see
  {doc}`coordinates`).

Date
: GPS positions in pulseEKKO and IceRadar files carry only a time of day,
  and the date comes from elsewhere: a pulseEKKO file's header, where in
  older files it was typed by hand, and an IceRadar line's acquisition PC
  clock, which is local time. Either can be wrong.
  `--override date=YYYY-MM-DD` gives the UTC date instead, for a run, and
  `date` in a recording's {doc}`sidecar <../reference/sidecar>` for that
  recording. Both are refused for formats whose positions carry their own
  dates, and for positions from a coordinate CSV.

  GSSI positions carry their own dates, but an older GPS receiver that
  does not know about a GPS week-number rollover reports dates a multiple
  of 1024 weeks (19.6 years) too early. When a `.DZG` date is that far
  before the date the `.DZT` was created, Ridal moves it forward by the
  rollovers and warns that it did.

The medium velocity is not read from any file; it is given with
`--velocity`, which defaults to 0.168 m/ns (a typical value for most glacier
ice).

In the GUI, the **Process…** dialog and **Add raw data in bulk…** take the
antenna overrides (see {doc}`gui`), but not yet the date.

(ramac)=
### Malå RAMAC

A recording is three files with one name: the text header `.rad`, the
traces `.rd3` and the positions `.cor`.

Header
: `KEY:VALUE` lines, of which Ridal reads the number of samples
  (`SAMPLES`), the sampling frequency (`FREQUENCY`), the time window
  (`TIMEWINDOW`), the antenna (`ANTENNAS`), the antenna separation
  (`ANTENNA SEPARATION`), the interval between traces (`TIME INTERVAL`) and
  the number of traces (`LAST TRACE`). The antenna frequency is the number
  before `MHz` in the antenna's name.

Samples
: 16-bit little-endian integers, trace after trace, scaled to millivolts
  for a range of ±50 V, as RGPR does. The 32-bit `.rd7` of newer systems
  is not read yet.

Positions
: One line per fix in the `.cor`: the trace number (from 1), the date, the
  time, the latitude and `N` or `S`, the longitude and `E` or `W`, and the
  altitude, separated by whitespace. The time is read as UTC. A line that
  does not parse, such as one with too few columns or an impossible time,
  is skipped.

(pulseekko)=
### Sensors & Software pulseEKKO

A recording is three files with one name: the text header `.hd`, the
traces `.dt1` and the positions `.gp2`.

Header
: `KEY = VALUE` lines, of which Ridal reads the number of samples
  (`NUMBER OF PTS/TRC`), the time window (`TOTAL TIME WINDOW`), the antenna
  frequency (`NOMINAL FREQUENCY`), the antenna separation
  (`ANTENNA SEPARATION`), the interval between traces
  (`TRACE INTERVAL (s)`) and the number of traces (`NUMBER OF TRACES`). The
  sampling frequency follows from the number of samples and the time
  window.

Samples
: Each trace in the `.dt1` is a 128-byte trace header, which Ridal skips,
  then the samples as 16-bit little-endian integers. A recording that was
  interrupted can end without its last trace, though the header counts it:
  it is then read without that trace, with a warning. A data file that
  lacks more than one trace is refused.

Positions
: The `.gp2` holds a `$GPGGA` sentence for some of the traces, each after
  the trace number (from 1). Only the first line of a trace is used. GGA
  gives only the time of day, so the date comes from the file's `Date=`
  line, which in older files was typed by hand; it can be given instead
  (see Date above).

(gssi)=
### GSSI

Channels
: A GSSI file can hold several *channels*, recorded side by side: one per
  antenna, or one per frequency of a dual-frequency antenna. Each channel
  has its own header, with its own time window, and the traces are stored
  scan by scan, one trace of every channel in turn. A file of one channel
  is read like any other recording. A file of several needs `--channel N`
  (`channel=` in Python), numbered from 1 as GSSI numbers them, and fails
  with a list of its channels until it is given. Channels are never merged:
  each is its own radargram, named `<stem>_ch_<N>.nc`, so a file of two
  channels is processed in two runs. The same channel is read from every
  file given. `batch-process` makes each channel its own output, unless
  `--channel N` reads one (see {doc}`cli`). `ridal info` describes the
  channel given. In the GUI, the ingest planner makes each channel its own
  recording, and **New radargram from raw data…** asks which channel to
  read (see {doc}`gui`).

Antenna
: The frequency is read from the antenna's name. A dual-frequency antenna
  names both frequencies (`300/800D`) and records the higher one on
  channel 1, so channel 1 of a `300/800D` file is read as 800 MHz and
  channel 2 as 300 MHz. GSSI files do not record the antenna separation,
  which is 0 m with a warning unless given; the two channels of a
  dual-frequency antenna may have different separations, which a sidecar
  gives each channel with `[channels.<N>]`.

Date
: The `.DZG` positions carry their own dates. Dates a GPS week-number
  rollover behind the `.DZT`'s creation date are corrected, as described
  under Date above.

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
  `batch-process` makes each line of a file of several its own recording:
  one output per line, or with `--merge`, consecutive lines (and the lines
  of consecutive files) merged in time as files are. `{line}` names them in
  an id pattern (see {doc}`cli`).

Antenna
: IceRadar files record neither the antenna frequency nor the antenna
  separation. `--override antenna_mhz=MHZ` (or a sidecar's `antenna_mhz`)
  is required, and the separation is 0 m with a warning unless
  `--override antenna_separation_m=M` (or a sidecar's
  `antenna_separation_m`) gives it. A sidecar can give a line of a multi-line file its own values
  (`[lines.<N>]`).

Date
: The GPS gives the time of day in UTC. The date is the acquisition PC's,
  which is local time and may be off by hours, so Ridal warns that it used
  it. A file whose PC clock was at its reset value (1 January 2000) is
  refused until `--override date=YYYY-MM-DD` or the sidecar's `date` gives the date. A line that runs past
  midnight UTC continues into the next day.

Positions
: Positions are read as north and east, since the files do not record the
  hemisphere, and one version names the longitude `Long_ W` for data from
  east of Greenwich. A trace's fix is used only when the file marks it valid
  and every field is shaped as GPS gives it, since some files hold shifted
  fields in traces marked valid. Traces without a usable fix are placed by
  interpolation, as between the fixes of any other format. A
  coordinate CSV ({ref}`coordinate-csv`) for a file of several lines needs a
  `line` column, and trace N in it is the file's `location_N`.

Traces
: Samples are converted from volts to millivolts. Traces that captured
  nothing (a stacking count of 0) are kept, as the zeros they hold, so that
  trace N is the file's `location_N`, and their GPS fixes are used;
  `remove_empty_traces`, the first step of the default profile, leaves them
  out. When the record length
  changes within a line, the traces are aligned on the trigger and the
  shorter ones are padded with zeros to the longest. The time window starts
  at the start of the record, before the trigger, so time zero is found by
  `zero_corr` as for the other formats.

Autosaves
: The software keeps a copy of the first part of a recording as
  `<name>.hdf5.x1`. Ridal refuses it and names the file it belongs to.

(virl)=
### VIRL

The VIRL radars were built by Evgeny Vasilenko and used by the Universidad
Politécnica de Madrid on glaciers in Svalbard, the Antarctic and elsewhere.
Ridal reads the `.DSR` files of the VIRL-7. A file is one recording, a
sequence of records of one size, one per trace: a 112-byte text header,
then the trace as 16-bit samples. There is no public description of the
format, so what follows is what Ridal assumes, from the files of one
campaign (2014).

What must be given
: The files record neither the sampling frequency, nor the date, nor the
  antenna frequency, nor the antenna separation. A recording is refused
  until the first three are given in its sidecar (see
  {doc}`../reference/sidecar`), as `sampling_mhz`, `date` and
  `antenna_mhz`. Any of them can instead be given for a whole run, with
  `--override` (`--override sampling_mhz=200`), but the date at least
  usually differs between recordings. The separation is 0 m with a warning
  unless given. For a survey whose settings did not
  change, one command writes them all, a day at a time since the date
  differs:

  ```bash
  ridal override day1/*.DSR --set sampling_mhz=200 --set date=2014-04-05 \
      --set antenna_mhz=25 --set antenna_separation_m=11.8
  ```

  In the GUI, **New radargram from raw data…** asks for the antenna
  frequency, the sampling frequency and the date before it uploads a
  `.DSR` file, and **Add raw data in bulk…** takes the frequencies by
  antenna and one date for every recording (see {doc}`gui`).

  The sampling frequency scales every time and depth, so take it from the
  survey's notes, not from a guess. For the 2014 campaign it is 200 MHz
  (5 ns), which reproduces the published ice thicknesses.

Positions
: Each trace's header holds the GPS's last `$GPGGA` sentence, so several
  traces share one fix. A position is kept at the first trace of each fix
  and the traces between are interpolated. A sentence whose checksum is
  wrong, as a dropped character makes it, is skipped. A recording whose GPS
  gave only one fix is placed at it, every trace at one time, with a
  warning. A recording that runs past midnight UTC continues into the next
  day.

Traces
: Samples are a 14-bit converter's, stored as offset binary, and are read
  as their difference from its middle value (8192). The time window starts
  at the start of the record, so time zero is found by `zero_corr` as for
  the other formats. The window is long (20 µs in 2014), so most of it can
  be noise below the bed, and `auto_gain` may then apply none.

(skyhub)=
### SkyHub

SPH Engineering's SkyHub is the onboard computer of a common UAV radar
setup. It records a Radsys zGPR, such as the Zond Aero LF (`zAeroLF`), as
SEG-Y. Ridal reads these files and refuses any other SEG-Y, since `.sgy` is
a generic extension: a SkyHub file is told by its textual header, which
names SkyHub and the zGPR. Unlike the standard, the file is little-endian,
with 32-bit integer samples.

A recording comes in one of two layouts.

As SkyHub writes it
: `<stamp>-gpr.sgy` with `<stamp>-position.csv`, and a `<stamp>-system.log`
  that Ridal does not read. Either the `.sgy` or the `-position.csv` can be
  given as the input. The CSV is matched to the traces by its `zGPR:Trace`
  column, and gives each trace a time to the millisecond. Its RTK columns
  are used when any row has RTK, and its standalone positions otherwise. The
  GNSS updates less often than traces are recorded, so a position repeated
  over several rows is kept at its first and the traces between are
  interpolated in time.

Cut into parts
: `<stem>_001.sgy`, `<stem>_002.sgy`, ... are parts of one recording, with
  one `<stem>.txt` of trace number, latitude, longitude and altitude for
  the whole recording. Each part is a recording of its own, positioned by
  its traces' numbers in the whole recording.

Without either file, the positions in the trace headers are read. They are
stored to only about a metre.

What must be given
: The files do not record the antenna frequency, and the zGPR takes
  antennas of several frequencies, so it is never assumed: give
  `antenna_mhz` in a sidecar or with `--override antenna_mhz=MHZ`. The
  antenna separation is 0 m with a warning unless given.

Altitudes
: The altitudes are the aircraft's, not the ground's. Only the RTK
  altitudes are absolute (in a vertical datum the files do not name); the
  others are relative to take-off, and need `altitude_offset_m` (see
  {ref}`altitude-offset`) to become elevations. Without a DEM (`--dem`),
  which gives the ground's elevations in their place, processing warns
  about both. `ridal info` and the processing log say which positions were
  used.

The samples
: The first sample of every trace is a marker, not data, and is cropped
  off as it is read. The `Pulse delay` of the textual header is a hardware
  setting tuned per antenna and unit to keep the direct wave at the top of
  the window, not a time, so it is only reported, by `ridal info`, with the
  rest of the radar's settings and the share of samples at the zGPR's
  saturation, which the direct wave often reaches.

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
