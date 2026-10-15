# FAQ and troubleshooting

If your problem is not here, search the
[issues](https://github.com/erikmannerfelt/ridal/issues) or open one, with
the command you ran, the full message, and `ridal --version`.

## Installing and reading files

**"PROJ (projinfo) cannot be found / is not installed"**
: The CRS you asked for is not a WGS84 UTM zone, which Ridal handles on its
  own, so it needs PROJ. Install it (`proj-bin` on Debian and Ubuntu), or use
  a UTM zone, such as `--crs EPSG:32633`. See
  {doc}`../guide/coordinates`.

**"GDAL (gdallocationinfo) cannot be found / is not installed"**
: Sampling a DEM needs GDAL. Install it (`gdal-bin` on Debian and Ubuntu),
  or process without `--dem`.

**"Unsupported input extension"**
: Ridal reads Malå RAMAC, pulseEKKO and GSSI files; see
  {doc}`../guide/file-formats`. Give the header, data or positions file of
  one of them.

**"No such file or directory (os error 2)" for a file that exists**
: Usually the positions file (`.cor`, `.gp2` or `.DZG`) is missing beside
  the data. Ridal needs it, and the message does not yet say so
  ([#13](https://github.com/erikmannerfelt/ridal/issues/13)). For RAMAC,
  `--cor` reads one with another name.

**"Could not read frequency from the antenna field … Try using the antenna MHz override"**
: The header names the antenna in a way Ridal does not recognise. Give the
  frequency with `--override-antenna-mhz 100` (`override_antenna_mhz=` in
  Python, or **Antenna frequency** in the GUI's dialogs).

**The antenna separation is wrong or 0**
: GSSI headers do not record it, and some others record it wrongly. Give it
  with `--override-antenna-separation`. It matters for depths near the
  surface; see {doc}`../guide/coordinates`.

**I used `rsgpr`**
: Ridal was called `rsgpr` before February 2026, and its interfaces changed
  completely in version 0.5; see
  [issue #82](https://github.com/erikmannerfelt/ridal/issues/82).

## Processing

**Depths look wrong**
: Check, in this order: that time zero is right (the `zero_corr` line of the
  processing log says where it was picked); the velocity (`--velocity`,
  0.168 m/ns for ice by default); and the antenna separation (`ridal info`
  shows it). See {doc}`../guide/coordinates`.

**The radargram is stretched where the survey stopped**
: A time-triggered radar keeps recording while standing still. Add
  `remove_standstills`, and `equidistant_traces` for an even spacing. See
  {doc}`../guide/processing`.

**A step did nothing**
: Several steps measure something from the data and leave it unchanged when
  they cannot, rather than failing: `auto_gain` when the amplitude does not
  fall with depth, for example. The processing log says why. See
  {doc}`../guide/processing`.

**"No radargram ID was supplied"**
: Not an error: the output's file name was used as its id. Give an id with
  `--radargram-id`, or `--id-pattern` in a batch, when file names repeat
  across a survey. See {doc}`../guide/python` for what the id is for.

**Non-ASCII names come out garbled in Python**
: Group and display names are written as given, so they may be non-ASCII.
  Some readers, such as xarray with the `h5netcdf` engine, garble a
  non-ASCII attribute; another engine reads it correctly.

## The GUI

**A radargram is not listed**
: The catalog lists Ridal's own `.nc` files below the served directory. A
  file processed by a Ridal old enough to predate radargram ids is listed as
  needing reprocessing: reprocess it from the raw data. A radargram marked
  **Unlisted** in **Edit properties** is left out of listings.

**"was created by an older Ridal, which kept project state … directly in the project directory"**
: The project predates `ridal_data/`. Run the `ridal project migrate`
  command the message gives.

**I cannot pick**
: Picking needs a project (`ridal project init`), and on a server an account
  with the `picker` role. The viewer's toolbar says which is missing.

**I cannot see the others' picks**
: That is deliberate: a `picker` sees only their own, so that
  interpretations stay independent. See {doc}`../guide/interpretation`.

**The picks are not where I drew them**
: The radargram was processed again after you picked. Picks are carried to
  the new processing, and the banner above the radargram says what had to be
  moved and how far. See {doc}`../guide/interpretation`.

**Saving picks is refused**
: A line doubles back, two lines in one layer overlap, or a line overlaps
  one in an exclusive layer. The viewer marks where. Allow overhangs or turn
  off the duplicate warning on the layer if that is what it is; see
  {doc}`../guide/interpretation`.

**The topographic correction is greyed out**
: The radargram has no usable elevations or depths; hovering over it says
  which. If it is stretched by a GPS spike instead, set a **Surface cap** in
  **Edit properties**. See {doc}`../guide/coordinates`.
