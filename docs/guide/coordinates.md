# Coordinates, CRS and DEMs

Every trace in a processed radargram has a position (`easting` and
`northing`, and the same position as `longitude` and `latitude`) and a
surface `elevation`, and every sample has a `depth`. This page describes
where these values come from and how they can be corrected.

## Positions

The positions come from the recording's own positions file (`.cor`, `.gp2`
or `.DZG`), which holds GNSS fixes for some of the traces, and Ridal
interpolates them to every trace (see {doc}`file-formats`). `ridal info`
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

The DEM can be any raster that GDAL reads, in any CRS, since Ridal samples it
with GDAL's `gdallocationinfo`, which therefore has to be on the `PATH`.
GDAL 3.10 and later sample it bilinearly; older versions take the nearest
cell. The output's `elevation_correction`
attribute names the DEM that was used. On a server, a DEM is added once on
the **Processing** page and then chosen in each radargram's **Process…**
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
