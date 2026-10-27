# FAQ and troubleshooting

If your problem is not described here, search the
[issues](https://github.com/erikmannerfelt/ridal/issues) or open a new one
with the command you ran, the full message and the output of
`ridal --version`.

## Installing and reading files

**"PROJ (projinfo) cannot be found / is not installed"**
: The CRS you asked for is not a WGS84 UTM zone (which Ridal handles on its
  own), so it needs PROJ. Either install PROJ (`proj-bin` on Debian and
  Ubuntu) or use a UTM zone such as `--crs EPSG:32633` (see
  {doc}`../guide/coordinates`).

**"GDAL (gdallocationinfo) cannot be found / is not installed"**
: Sampling a DEM needs GDAL, so either install it (`gdal-bin` on Debian and
  Ubuntu) or process without `--dem`.

**"Unsupported input extension"**
: Ridal reads Malå RAMAC, pulseEKKO, GSSI and IceRadar files (see
  {doc}`../guide/file-formats`), and the input has to be the header, data or
  positions file of one of them.

**"No positions for …"**
: The positions file (`.cor`, `.gp2` or `.DZG`) is missing beside the data.
  `--cor` reads a positions file with another name, and positions from any
  other source can be given as a coordinate CSV (see
  {ref}`coordinate-csv`).

**"Could not read frequency from the antenna field … Try using the antenna MHz override"**
: The header names the antenna in a way that Ridal does not recognize. Give
  the frequency with `--override-antenna-mhz 100` (`override_antenna_mhz=` in
  Python, or **Antenna frequency** in the GUI's dialogs).

**The antenna separation is wrong or 0**
: GSSI headers do not record the separation, and some other headers record
  it wrongly, so give it with `--override-antenna-separation`. It matters
  mostly for depths near the surface (see {doc}`../guide/coordinates`).

**I used `rsgpr`**
: Ridal was called `rsgpr` before February 2026, and its interfaces changed
  completely in version 0.5 (see
  [issue #82](https://github.com/erikmannerfelt/ridal/issues/82)).

## Processing

**Depths look wrong**
: Check, in this order, 1) that time zero is right (the `zero_corr` line of
  the processing log says where it was picked), 2) the velocity
  (`--velocity`, which defaults to 0.168 m/ns, a typical value for most
  glacier ice), and 3) the antenna separation, which `ridal info` shows (see
  {doc}`../guide/coordinates`).

**The radargram is stretched where the survey stopped**
: A time-triggered radar keeps recording while it stands still. Adding
  `remove_standstills` removes the standstills, and `equidistant_traces`
  gives an even spacing (see {doc}`../guide/processing`).

**A step did nothing**
: Several steps measure something from the data and leave the data
  unchanged when they cannot, instead of failing; `auto_gain` does so when
  the amplitude does not fall with depth, for example. The processing log
  says why (see {doc}`../guide/processing`).

**"No radargram ID was supplied"**
: This is not an error, but a note that the output's file name was used as
  its id. Give an id with `--radargram-id` (or `--id-pattern` in a batch)
  when file names repeat across a survey (see {doc}`../guide/python` for
  what the id is used for).

**Non-ASCII names come out garbled in Python**
: Group and display names are written as given, so they may contain
  non-ASCII characters, and some readers (such as xarray with the `h5netcdf`
  engine) garble a non-ASCII attribute. Another engine reads it correctly.

## The GUI

**A radargram is not listed**
: The catalog lists Ridal's own `.nc` files below the served directory. A
  file processed by a Ridal old enough to predate radargram ids is listed as
  needing reprocessing and has to be processed again from the raw data,
  while a radargram marked **Unlisted** in **Edit properties** is
  deliberately left out of listings.

**"was created by an older Ridal, which kept project state … directly in the project directory"**
: The project predates `ridal_data/`, and running the `ridal project
  migrate` command that the message gives moves it.

**I cannot pick**
: Picking needs a project (`ridal project init`) and, on a server, an account
  with the `picker` role, and the viewer's toolbar says which is missing.

**I cannot see other people's picks**
: This is deliberate, since a `picker` only sees their own picks so that the
  interpretations stay independent (see {doc}`../guide/interpretation`).

**The picks are not where I drew them**
: The radargram was most likely processed again after you picked. Picks are
  carried to the new processing, and the banner above the radargram says
  what had to be moved and how far (see {doc}`../guide/interpretation`).

**Saving picks is refused**
: A line doubles back, two lines in one layer overlap, or a line overlaps
  one in an exclusive layer, and the viewer marks where. If the layer really
  should allow it, allow overhangs or turn off the duplicate warning on the
  layer (see {doc}`../guide/interpretation`).

**The topographic correction is grayed out**
: The radargram has no usable elevations or depths, and hovering over the
  checkbox says which. If the view is instead stretched by a GPS spike, set
  a **Surface cap** in **Edit properties** (see
  {doc}`../guide/coordinates`).
