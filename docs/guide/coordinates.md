# Coordinates, CRS and DEMs

Every trace in a processed radargram has a position (`easting` and
`northing`, and the same position as `longitude` and `latitude`) and a
surface `elevation`, and every sample has a `depth`. This page describes
where these values come from and how they can be corrected.

## Positions

The positions come from the recording's own positions file (`.cor`, `.gp2`
or `.DZG`), which holds GNSS fixes for some of the traces, and Ridal
interpolates them to every trace (see {doc}`file-formats`), or from a
coordinate CSV given in its place (see {ref}`coordinate-csv`). `ridal info`
summarizes them with the number of fixes, the time span, the track length and
the elevation range.

Three steps correct the positions during processing (see
{doc}`../reference/steps`):

`shift_coordinates`
: The GNSS antenna is rarely on the radar itself, but rather on a pole, on
  the snowmobile towing the radar, or to one side of it. This step shifts
  every position along the track, across it, and up or down to where the
  radar was.

`remove_standstills`
: A time-triggered radar keeps recording when it stops, which stretches the
  radargram horizontally wherever the survey paused. This step replaces each
  standstill with a single trace, and with `gps_uncertainty`, the positions
  are also used to decide what was a standstill.

`equidistant_traces`
: Resamples the traces onto an even spacing along the track, so that each
  trace is the same distance from the next.

(coordinate-csv)=
### Positions from a CSV

Positions from another source, such as a better GNSS or a survey that only
noted where each line started and ended, are given as a coordinate CSV. It
has one header line, and its columns may come in any order:

| Column | Required | Meaning |
|---|---|---|
| `trace` | yes | The trace number, counted from 0 |
| `time` | yes | ISO 8601 with a UTC offset, such as `2025-03-27T12:10:20Z` or `2025-03-27T14:10:20+02:00` |
| `lon`, `lat` | yes | WGS84 longitude and latitude in degrees |
| `alt` | no | Elevation in meters, used as given |
| `time_source` | no | `recorded`, `interpolated` or `synthetic` |
| `alt_source` | no | `recorded`, `interpolated` or `dem` |

```text
trace,time,lon,lat,alt
0,2025-03-27T12:10:20Z,16.64838,78.10189,715.2
250,2025-03-27T12:11:05Z,16.65120,78.10342,721.8
```

The rows need not cover every trace: traces between two rows are
interpolated, as between the fixes of a `.cor`. Traces must increase from
row to row, and times must not decrease. Lines starting with `#` are
comments. Without an `alt` column, `--dem` is required, and the elevations
are sampled from the DEM.

Ridal uses a CSV in two ways:

- Beside the recording, named after it (`DAT_0066_A1.coords.csv` for
  `DAT_0066_A1.rad`), it is used in place of the recording's own positions
  file, which is then ignored.
- With `--cor FILE` (`cor=` in Python), for a single recording.

`--cor` also takes a format's own positions file with another name, such as
another `.cor`. When several recordings are processed or merged at once,
`--cor` takes a directory instead, which holds a `<stem>.coords.csv` or
positions file for each recording. Which file was read is part of the
processing recipe, so a new positions file gives a new revision.

The two source columns say where each value came from, and are written by
`ridal coords` (below). A `synthetic` time in any row marks the radargram's
times as made up: the output then has a `time_source` attribute, and
`remove_standstills` does not use the GPS for it. A `dem` altitude marks its
elevations as sampled from a DEM, which `elevation_correction` names.

(ridal-coords)=
### Writing the CSV with `ridal coords`

`ridal coords` (`ridal.coords` in Python) writes a recording's coordinate
CSV, one row per trace, as
`<stem>.coords.csv` beside it (or `-o FILE`, or in `--output-dir`). It fills
in what is not known, and says in the file's opening comments and in each
row where every value came from:

Positions
: From `--positions FILE`, a CSV with `trace` and `lon,lat` and optionally
  `alt` and `time`; from `--start lon,lat[,alt]` and `--end lon,lat[,alt]`,
  the first and last trace; from `--track FILE`, a GNSS track (below); or,
  with none of these, from the recording's own positions file. Traces
  between known ones are interpolated by trace number, which is exact for a
  wheel-triggered recording and assumes a steady speed for a time-triggered
  one. Positions in another CRS are given as `x,y` with `--src-crs`.

Times
: From the given positions if they have times, else from the recording's
  own positions file. When neither has any, or with `--start-time` or
  `--time-interval`, the times are made up as the start time plus the trace
  interval (the header's unless `--time-interval` gives another), and marked
  `synthetic`. Without `--start-time`, they start at 1970-01-01, with a
  warning. A time without a UTC offset is taken as UTC, with a warning.

Altitudes
: From `--dem` before anything else, then from the given positions, then
  from the recording's own positions file.

Track
: `--track FILE` is a CSV with `time` and `lon,lat` (or `x,y`), and
  optionally `alt`, such as a separate GNSS receiver's log, where other
  columns are ignored. Each trace gets the track's position at its time on
  the recording's clock, which comes from its own positions file (or from
  `--start-time`). When the two clocks differ, `--time-offset` gives the
  seconds to add to the recording's to get the track's, and the CSV's times
  are then the track's. Traces before the track's first fix or after its
  last are held at its ends, with a warning, and if no trace falls within
  the track at all, the clocks are almost certainly apart and `ridal coords`
  refuses. Since a track carries no trace numbers, one track can position a
  whole day of recordings: `ridal coords *.rad --track day.csv`.

For example, a recording whose positions file was lost, but whose start and
end points and start time are in a field notebook:

```bash
ridal coords DAT_0066_A1.rad --start 16.6484,78.1019 --end 16.6612,78.1034 \
  --start-time 2025-03-27T12:10:20Z --dem svalbard_dem.tif
ridal process DAT_0066_A1.rad --default
```

With no position options, `ridal coords` writes the recording's own
positions, interpolated to every trace: a template to correct by hand, or a
way to read a `.cor` anywhere else. It takes several recordings at once in
that case and with `--track`, writing a CSV for each. It refuses to replace a CSV that exists
unless given `--overwrite`.

## Coordinate reference systems

The positions are projected into a coordinate reference system (CRS) when the
file is read. Without `--crs` (`crs=` in Python), Ridal uses the WGS84 UTM
zone of the first position, which is for example `EPSG:32633` in central
Svalbard. The CRS is written both to the output's `crs` attribute and to its
`projected_crs` variable.

`--crs` takes a WGS84 UTM zone without needing any other software, written
either as `EPSG:32633` (`326xx` for the northern hemisphere and `328xx` for
the southern) or as `WGS84 UTM Zone 33N`. Any other CRS, such as
`EPSG:25833` or a PROJ string, needs PROJ's `projinfo` and `cs2cs` on the
`PATH` (see {doc}`../getting-started/installation`).

The CRS should be set explicitly when radargrams from one survey cross a UTM
zone boundary, so that they all share one CRS. A project can do the same for
every radargram processed on its server with `[processing] crs` in
`ridal.toml` (see {doc}`../reference/project-files`).

Exported points (see {doc}`interpretation`) are in WGS84 by default and in
the radargram's own CRS with `--crs native`, and the projected easting and
northing are written as properties on every point either way.

## Elevations from a DEM

The elevations in a positions file are those of the GNSS antenna, and
handheld or uncorrected GNSS elevations are often several meters off.
`--dem` (`dem=` in Python) replaces them with the surface elevation sampled
from a digital elevation model (DEM) at each fix:

```bash
ridal process DAT_0066_A1.rad --default --dem svalbard_dem.tif
```

The DEM can be a raster that GDAL reads, in any CRS, since Ridal samples it
with GDAL's `gdallocationinfo`, which therefore has to be on the `PATH`. By
default (and on a webserver), it must be a **GeoTIFF** (`.tif`); see
[#487](https://github.com/erikmannerfelt/ridal/pull/487) for the motivation
why. Other formats can be used locally by setting `RIDAL_DEM_GDAL_DRIVERS`
to the GDAL driver short names, comma- or space-separated (for example
`RIDAL_DEM_GDAL_DRIVERS=VRT`).
GDAL 3.10 and later sample it bilinearly; older versions can only take the
nearest cell, so the elevations step from one cell to the next along the
track. Ridal then warns: `ridal process` when it samples, `ridal gui` and
`ridal server start` when they start, and a site's **Site settings** page to
its server administrators. The output's `elevation_correction` attribute
names the DEM that was used, and adds "nearest cell" when it was sampled that
way. Its recipe records it too, so it is another revision than one sampled
bilinearly, and the **Processing** page lists it as older processing until
it is reprocessed with a newer GDAL. On a server, a DEM is added once on the
**Processing** page and then chosen in each radargram's **Process…**
dialog.

A DEM gives the surface at the time it was made rather than at the time of
the survey, so on a glacier that has thinned since, every sampled elevation
is too high by the amount of thinning.

## Depth

The `depth` of a sample is computed from its two-way travel time `twtt` and
the medium velocity `v` (`--velocity`, which defaults to 0.168 m/ns, a
typical value for most glacier ice), with one velocity for the whole
radargram.

With the transmitter and receiver at the same place, the depth is simply
`v · twtt / 2`. In practice they are usually apart by the antenna separation
`s`, so a reflection travels down one slanted leg and up another, and time
zero (the direct wave's arrival at the receiver) comes `s / c` after the pulse
left the transmitter, where `c` is the speed of light in air. Ridal accounts
for both effects, so that `depth` is the depth of the reflector directly
below the midpoint between the antennas. Both effects are large for
low-frequency antennas: with the 25 MHz antennas used on Drønbreen, which are
6.2 m apart, `v · twtt / 2` would put a reflector that is 5 m deep at 4.2 m,
and one that is 50 m deep at 48.4 m.

`correct_antenna_separation` (part of the default profile) goes one step
further and resamples each trace so that the samples are evenly spaced in
depth, `v · dt / 2` apart. Afterwards, `twtt` is the travel time that a
coincident transmitter and receiver would have recorded, which is what
migration assumes, and the output states this with
`antenna_separation_effective = 0`.

A single velocity is an approximation, since firn, wet snow, water and
temperate ice all have different velocities, and Ridal does not model these
variations. The velocity can instead be changed by reprocessing with another
`--velocity`. Picks are stored on the radargram's traces and samples rather
than as depths, so they follow to the reprocessed radargram and are then at
its new depths.

## Topographic correction

A radargram is normally drawn with the surface as a flat line along its top.
A topographic correction instead shifts each trace down by how far its surface
lies below the highest surface in the radargram, so that each row of the
image is one elevation and the reflectors are drawn with their true shape.
The elevation of a sample is then the surface `elevation` minus its `depth`.

There are two ways to get a topographic correction:

In the viewer and `ridal render`
: The **Topographic correction** checkbox in the viewer, `ridal render
  --topo`, and figures with `"view": "topo"` all correct the radargram while
  drawing it, from its `elevation` and `depth`. Nothing is stored, and picks
  are always drawn and saved in the uncorrected radargram's trace and sample
  numbers, so they line up in both views.

In the file
: The `correct_topography` step writes a corrected copy, `data_topocorr`,
  beside `data` in the NetCDF file (see {doc}`file-formats`). Since it is a
  copy, later steps do not affect it, so it should be run last.
  `--default-with-topo` is the default profile followed by this step.

The first way is usually the right one. The stored copy spans the whole
elevation range, so it more than doubles the size of the file, and nothing in
Ridal reads it. It is there for tools outside Ridal that should read a
corrected array without computing one.

Elevations come from GNSS or from a DEM, and a single bad fix can lift a
trace's surface far above its neighbors, which would stretch the corrected
view. Two bounds in a radargram's **Edit properties** dialog guard against
this: the **Surface cap** is the highest a trace's surface may be (higher
surfaces are pulled down to it), and the **Floor** is the lowest elevation
that is drawn. When a radargram has no `elevation` or no `depth`, the
checkbox is disabled and says why.
