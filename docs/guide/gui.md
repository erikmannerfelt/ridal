# The browser GUI

:::{admonition} Not written yet
:class: note

`ridal gui`: the catalogue, the radargram viewer, the map, the trace view and display settings.
:::

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
   it. The map shows the tracks in the same colours. Above the timeline,
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
  centimetres, inches or pixels, at the DPI given; the height follows from
  the extent and the horizontal scale, which starts at the viewer's.
- **Axes and style** are those of a figure spec (see {doc}`rendering`):
  units, tick spacing and anchor, titles, font size, colours, and a
  transparent background. **Radargram** off draws only the layers.
- **Layers.** **Picks** lists the contributors whose picks you may see,
  with a box per layer they picked (and one for lines with no layer);
  **Derived** lists the derived layers. Each group has its own line width
  and opacity, and picks are drawn over the derived layers. Picks that
  cannot be carried to this revision are drawn as stored, as the viewer
  draws other contributors' picks, and the dialog says so.

The preview is the figure itself, rendered smaller, and follows the
controls as they change; for an extent of more than a few million samples
it waits for **Update preview**. **Download** writes PNG or SVG, and
**Download spec** the figure spec as JSON, to draw the same figure again
with `ridal render --figure`, `ridal.render` or the API.

The dialog remembers its settings in this browser for the next figure in
the project, and the extent for this radargram until the tab is closed.
