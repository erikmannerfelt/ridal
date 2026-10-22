# The browser GUI

Ridal has a web interface for looking at processed radargrams, for picking
reflectors in them and, on a server, for processing and managing them. The
interface is the same whether it runs on your own computer with `ridal gui`
or for a team with `ridal server` (see {doc}`../deploy/index`).

## Starting it

```bash
ridal gui path/to/project
```

`ridal gui` serves one directory (or one `.nc` file) on your own computer
only, prints the address to open, and stops when you press Ctrl+C. Without a
path, it serves the project it finds by searching upward from the current
directory, or the current directory itself if there is none.

Every processed Ridal `.nc` file below the directory is found and listed.
Picks need somewhere to be saved, which is a **project** made with
`ridal project init` (see {doc}`../getting-started/quickstart`), so without
one, radargrams can be looked at but not picked.

Radargrams are rendered by the server in tiles, and only the parts that are
being looked at, so a radargram that is larger than what the browser could
hold is no problem. Files processed by a Ridal old enough to predate
radargram ids are listed as needing reprocessing.

The menu at the top left leads to the pages described below and to this
documentation, and the **?** beside a heading opens the part of the
documentation that describes it.

## The catalog

The first page lists every radargram in its **group**, which is the survey,
campaign or site given with `--group-name` when it was processed (or later
in the GUI). Each group has a map of where its radargrams were recorded, and
clicking a radargram opens it in the viewer.

- **Render profile**, at the top, decides how every image on the page and in
  the viewer is drawn (see {doc}`rendering`).
- **More info** under a radargram shows its id, size, track length, file and
  processing date, and whether it has been picked.
- **Hide all** / **Show all** and **Hide map** / **Show map** on a group
  hide or show its radargrams and its map on this page. Which way the page
  opens is set under **Radargrams in the catalog** and **Maps in the
  catalog** in your settings, with a default for the project in its
  settings. A map is only loaded once it is shown and scrolled to, so hiding
  the maps makes a large catalog quicker to open on a slow connection.
  **Hide where all are unlisted** hides only the maps of groups whose
  radargrams are all unlisted.
- **Download** on a group, or **Download all**, gives the picked and derived
  layer points of every radargram in it as one file, and their tracks as
  GeoJSON.
- **Edit → Edit properties** gives a radargram a name and a group of the
  project's own without changing its file, so that reprocessing does not lose
  them. The same dialog marks a radargram as **Unlisted**, which leaves it
  out of listings and merged downloads (although it can still be opened by
  anyone who knows its address), and sets its horizontal scale and the
  bounds of its topographic correction. **Edit group** renames a group.
- **Add data**, for an `operator`, uploads a processed radargram, makes a new
  one from raw data, or adds the raw data of a whole survey at once (see
  [below](#adding-raw-data-in-bulk)).

A file over 64 MiB, such as a large radargram or DEM, is uploaded in chunks.
A chunk that fails is sent again from where the server got to, so a dropped
connection costs seconds rather than the whole upload. If the upload stops
anyway, or the page is closed or reloaded, choosing the same file again goes
on from where it stopped, for up to a day.

## The viewer

The viewer shows one radargram with a map of where it was recorded below it.
Scrolling or pinching zooms, and dragging pans.

The line under the controls reports what is under the cursor: the trace
number, the distance along the profile, the two-way travel time, the depth,
and the elevation of that point (i.e. the surface elevation of the trace
minus the depth). The values of listed derived attributes are added to the
same line.

The display controls only change how the radargram is drawn:

Render profile
: As in the catalog (see {doc}`rendering`), listed as built-in, the
  project's and your own. **Edit profile…** opens the profile editor (see
  [tuning a profile in the viewer](rendering.md#tuning-a-profile-in-the-viewer)).

Horizontal scale
: Stretches or squeezes the radargram horizontally, from 0.25× to 8×.

Contrast, Brightness
: Adjust the image on screen only, while downloads use the render profile as
  it is.

Smooth when zoomed in
: Blends between samples when zoomed in past one screen pixel per sample.
  When it is off, each sample is drawn as a block, which shows whether an
  edge in the image comes from the data or from the drawing.

Show radargram
: Unticked, only the picks and derived layers are drawn, which helps to see
  a pick in a colour close to the reflector it follows. It is ticked again
  whenever the page loads.

Topographic correction
: Draws each trace at its elevation, so that the surface follows the terrain
  (see {doc}`coordinates`). **Recentre** fits the view to the traces on
  screen again if a pan has drifted into the empty space above or below.

**Metadata** shows the file's attributes, including the processing steps and
their log (see {doc}`processing`). The layer panel at the right of the
radargram lists the layers and toggles each of them, as well as other
contributors' picks (see {doc}`interpretation`).

### The map

The map follows the cursor, with a marker showing where on the ground the
trace under the cursor was recorded. It shows the radargram's track and the
other radargrams of its group, on satellite imagery or on a project's own
basemaps, together with any overlays the project defines (see
{doc}`../reference/project-files`). The bars between the radargram, the map
and the trace view can be dragged to resize them.

### The trace view

**Trace view** opens a panel beside the radargram with one trace drawn as a
wiggle, at the same height as the radargram. Clicking the radargram chooses
the trace, and **+** and **−** change how wide the wiggle is drawn.

### Downloads

The viewer's **Download** menu has:

- **Picked layer points** and **Derived layer points**, i.e. the layers as
  points in GeoJSON or CSV (see {doc}`interpretation`);
- **Raw picks (gprinterp)**, which are your picks as stored;
- **Track (GeoJSON)**, which holds the positions of the traces;
- **Rendered image**, which is the whole radargram as PNG or JPEG in the
  current render profile;
- **Figure…**, an annotated figure (see [Figures](#figures));
- **Radargram (NetCDF)**, which is the processed file itself.
- **Raw data (zip)**, the files it was processed from, as they were uploaded,
  when the project has them. A radargram merged from several recordings has
  one numbered folder per recording, in the order they were merged.

What each account may download is decided by its download scope (see
{doc}`../deploy/accounts`).

### Edit

**Edit properties** is the same dialog as in the catalog. For an `operator`,
**Replace with a new version…** uploads a reprocessed file for the
radargram, and **Process…** processes it again from its raw data (see
{doc}`processing`). Picks follow to the new version in both cases.

## Other pages

**Layers**
: The layers that picks are drawn in, exclusivity groups, derived items and
  the pick manager (see {doc}`interpretation`).

**Processing**
: Processing profiles, raw data that are not processed yet, DEMs and recent
  batches (see {doc}`processing`). **Download** beside raw data that is not
  processed yet gives its files as a zip, and beside a DEM gives its file.

**Project settings**
: Your own preferences, such as the render profile and horizontal scale that
  radargrams open with, and, for an `admin`, the project's description,
  basemaps, overlays, members, access and history (see
  {doc}`../deploy/accounts`).

## Adding raw data in bulk

An `operator` can add the raw recordings of a whole survey at once with
**Add data → Add raw data in bulk…** in the catalog (or **Add raw data in
bulk…** on the Processing page). It is for many recordings; **Add data →
New radargram from raw data…** in the catalog is for one radargram, looked
at before it is added.

1. **Drop the files**, every file of each recording (`.rad`, `.rd3` and
   `.cor` for RAMAC, for example). They are grouped into recordings by
   name and uploaded one at a time.
2. **Check the radargrams.** Recordings made less than the threshold apart
   (ten minutes unless changed), with the same antenna and settings, are
   merged into one radargram. The timeline shows one row per radargram:
   click a gap inside a row to split it there, or the chip at the start of
   a row to join it to the one before. Click a recording to see what it
   is, to join its radargram with the one before or after it, or to not
   use it. A join you make holds whatever the threshold, even with **Never
   merge**, and its gap is drawn solid rather than dashed; click it to undo
   it. The map shows the tracks in the same colors. Above the timeline,
   each antenna the recordings name is listed with what its name is read
   as. Give a frequency or separation there where the files do not say it,
   or say it wrongly: it applies to every recording with that antenna,
   before they are merged and named, and is kept with each radargram's raw
   data, so processing it later uses it too. A recording whose antenna
   gives no frequency is left out until one is given.
3. **Name them** with an id pattern and a display name pattern, as
   `ridal batch-process --id-pattern` takes them (see
   {doc}`cli`), and type over any single one. An id that already exists in
   the project is refused; choose another. Choose the catalog group they
   all go in: one that exists, a new one, or none.
4. **Choose what happens next**: process them now with a processing
   profile, put them in a profile to process later, or keep only the raw
   data for someone to process one by one. Raw data that is not processed
   yet is listed on the Processing page, where it can be processed or
   deleted.

**Create** stores each radargram's raw data in turn. If it stops halfway,
what was stored stays, listed on the Processing page as not processed yet,
and planning again reports those ids as taken.

## Figures

**Download → Figure…** in the viewer makes an annotated figure of the
radargram: a part of it, with axes, ticks and labels, and the picks and
derived layers you choose drawn over it. It needs the `derived` download
scope, as the rendered image does.

- **Extent.** The dialog opens on what the viewer shows. **Current view**
  takes it again, **Draw rectangle** closes the dialog until you drag a box
  on the radargram (a banner on the map says so; Escape or **Cancel**
  stops), and **Whole radargram** takes all of it. The four fields show the
  extent in the axis units and can be typed into. In the topographic view
  the figure is of the corrected view, in elevation, and the top may be
  typed above the highest surface to leave room above the radargram.
- **Size.** The width is the whole figure's, labels included, in
  centimeters, inches or pixels, at the DPI given; the height follows from
  the extent and the horizontal scale, which starts at the viewer's.
- **Axes and style** are those of a figure spec (see {doc}`rendering`):
  units, tick spacing and anchor, titles, font size, colors, and a
  transparent background. **Radargram** off draws only the layers.
- **Layers.** Off until **Show layers** is ticked. **Picks** lists the
  contributors whose picks you may see,
  with a box per layer they picked (and one for lines with no layer);
  **Derived** lists the derived layers. Each group has its own line width
  and opacity, and picks are drawn over the derived layers. Picks that
  cannot be carried to this revision are drawn as stored, as the viewer
  draws other contributors' picks (dashed there), and the dialog says so.
  Lines that run past the edge of this revision are cut where they cross
  it.

The preview is the figure itself, rendered smaller, and follows the
controls as they change; for an extent of more than a few million samples
it waits for **Update preview**. **Download** writes PNG or SVG, and
**Download spec**, at the left, the figure spec as JSON, to draw the same figure again
with `ridal render --figure`, `ridal.render` or the API.

The dialog remembers its settings in this browser for the next figure in
the project, and the extent for this radargram until the tab is closed.
