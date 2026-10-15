# Coordinates, CRS and DEMs

Every trace in a processed radargram has a position (`easting`, `northing`,
and the same in `longitude` and `latitude`), a surface `elevation`, and every
sample a `depth`. This page is about where those come from and how to
correct them.

## Positions

Positions come from the recording's own positions file (`.cor`, `.gp2` or
`.DZG`), which holds GNSS fixes for some of the traces. Ridal interpolates
them to every trace; see {doc}`file-formats`. `ridal info` summarises them:
how many fixes, the time span, the track length and the elevation range.

Three steps correct positions during processing (see
{doc}`../reference/steps`):

`shift_coordinates`
: The GNSS antenna is rarely on the radar: on a pole, on the snowmobile
  towing it, or to one side. This shifts every position along the track,
  across it and up or down, to where the radar was.

`remove_standstills`
: A time-triggered radar keeps recording when it stops, which stretches the
  radargram horizontally wherever the survey paused. This replaces each
  standstill with one trace. With `gps_uncertainty`, the positions help
  decide what was a standstill.

`equidistant_traces`
: Resamples the traces onto an even spacing along the track, so that each
  trace is the same distance apart.

## Coordinate reference systems

Positions are projected into a CRS when the file is read. Without `--crs`
(`crs=` in Python), Ridal uses the WGS84 UTM zone of the first position: for
example `EPSG:32633` in central Svalbard. The CRS is written to the output's
`crs` attribute and its `projected_crs` variable.

`--crs` takes a WGS84 UTM zone without any other software, written as
`EPSG:32633` (`326xx` north, `328xx` south) or `WGS84 UTM Zone 33N`. Any
other CRS, such as `EPSG:25833` or a PROJ string, needs PROJ's `projinfo` and
`cs2cs` on the `PATH` (see {doc}`../getting-started/installation`).

Set the CRS explicitly when radargrams from one survey cross a UTM zone
boundary, so that they all share one. A project can do the same for every
radargram processed on its server with `[processing] crs` in `ridal.toml`
(see {doc}`../reference/project-files`).

Exported points (see {doc}`interpretation`) are in WGS84 by default and in
the radargram's own CRS with `--crs native`; projected easting and northing
are written on every point either way.

## Elevations from a DEM

The elevations in a positions file are those of the GNSS antenna, and
handheld or uncorrected GNSS elevations are often off by metres. `--dem`
(`dem=` in Python) replaces them with the surface elevation sampled from a
digital elevation model at each fix:

```bash
ridal process DAT_0066_A1.rad --default --dem svalbard_dem.tif
```

The DEM can be any raster GDAL reads, in any CRS; Ridal samples it with
GDAL's `gdallocationinfo` (bilinear where GDAL supports it), which therefore
has to be on the `PATH`. The output's `elevation_correction` attribute names
the DEM. On a server, a DEM is added once on the **Processing** page and then
chosen in each radargram's **Process…** dialog.

A DEM gives the surface at the time it was made, not at the time of the
survey. On a glacier that has thinned since, every elevation is too high by
the change.

## Depth

The `depth` of a sample is computed from its two-way travel time `twtt` and
the medium velocity `v` (`--velocity`, 0.168 m/ns for ice by default), with
one velocity for the whole radargram.

With the transmitter and receiver together, depth is `v · twtt / 2`. They are
usually apart, by the antenna separation `s`. A reflection then travels down
one slanted leg and up another, and time zero, the direct wave's arrival at
the receiver, comes `s / c` after the pulse left the transmitter (`c` being
the speed of light in air). Ridal accounts for both: `depth` is the depth of
the reflector directly below the midpoint between the antennas. Both effects
are large for low-frequency antennas. With the 25 MHz antennas used on
Drønbreen, 6.2 m apart, `v · twtt / 2` would put a reflector 5 m deep at
4.2 m, and one 50 m deep at 48.4 m.

`correct_antenna_separation` (in the default profile) goes one step further
and resamples each trace so that the samples are evenly spaced in depth,
`v · dt / 2` apart. Afterwards `twtt` is the travel time a coincident
transmitter and receiver would have recorded, which is what migration
assumes, and the output says so with
`antenna_separation_effective = 0`.

A single velocity is an approximation. Firn, wet snow, water and temperate
ice all have different velocities, and Ridal does not model them: change the
velocity by reprocessing with another `--velocity`. Picks are stored on the
radargram's traces and samples rather than as depths, so they follow to the
reprocessed radargram, at its new depths.

## Topographic correction

A radargram is drawn with the surface flat along its top. A topographic
correction instead shifts each trace down by how far its surface lies below
the highest surface in the radargram, so that each row is one elevation and
the reflectors are drawn at their true shape. The elevation of a sample is
the surface `elevation` minus its `depth`.

There are two ways to get one:

In the viewer and `ridal render`
: The **Topographic correction** checkbox in the viewer, `ridal render
  --topo`, and figures with `"view": "topo"` all correct the radargram while
  drawing it, from its `elevation` and `depth`. Nothing is stored, and picks
  are always drawn and saved in the uncorrected radargram's trace and sample
  numbers, so they line up in both views.

In the file
: The `correct_topography` step writes a corrected copy, `data_topocorr`, to
  the NetCDF file beside `data` (see {doc}`file-formats`). It is a copy, so
  steps after it do not reach it: run it last. `--default-with-topo` is the
  default profile followed by it.

Elevations come from GNSS or a DEM, and one bad fix can lift a trace's
surface far above its neighbours, which would stretch the corrected view. Two
bounds, set in a radargram's **Edit properties** dialog, guard against that:
the **Surface cap** is the highest a trace's surface may be (higher surfaces
are pulled down to it), and the **Floor** is the lowest elevation drawn.
When a radargram has no `elevation` or no `depth`, the checkbox is disabled
and says why.
