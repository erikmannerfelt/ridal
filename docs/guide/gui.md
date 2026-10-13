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
   it. The map shows the tracks in the same colours.
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
