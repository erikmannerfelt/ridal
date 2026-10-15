# Interpretation and layers

Interpretation in Ridal is drawing lines along reflectors in the viewer:
**picks**. Each line belongs to a **layer**, such as `bed` or `surface`, and
each person's picks are kept apart, so that several people can interpret the
same radargram independently and their picks can be combined afterwards.

## Picking in the viewer

Picking needs a project (see {doc}`../getting-started/quickstart`), and on a
server an account with the `picker` role or above (see
{doc}`../deploy/accounts`).

1. Choose the **Layer** in the toolbar above the radargram.
2. **Add line**, then click along the reflector. Each click adds a vertex;
   **Undo vertex** removes the last one, and clicking a vertex of the line
   being drawn removes it.
3. Finish the line with **Finish line** or a double-click on the last point.
   Escape abandons it.
4. **Save**. Nothing is stored until then, and the viewer warns before
   leaving a page with unsaved picks.

Click a saved line to select it. Then drag a vertex to move it, or onto its
neighbour to remove it; click a vertex to split the line there or remove the
vertex; click the small handle between two vertices to add one; click the
line elsewhere to split it; and drop an end onto the end of another line in
the same layer to join them. **Delete line** removes it and the **Layer**
beside it moves it to another layer. **Editing help** repeats this, and turns
the tooltips on vertices off.

**Hide picks** hides the lines to look at the radargram underneath. The layer
panel at the right toggles each layer, and the derived layers.

Picks are stored in the radargram's own trace and sample numbers, not as
depths or coordinates. They are drawn in the right place in the
topographically corrected view and at any horizontal scale, and they follow
the radargram when it is processed again (see
[Reprocessing](#reprocessing-and-picks)).

## Projects and whose picks are shown

A project keeps one file of picks per person per radargram, in
`ridal_data/interpretations/` (see {doc}`../reference/project-files`).
Picks belong to the person who drew them: nobody can change another's, not
even an administrator.

With `ridal gui` on your own computer, there are no accounts and everything
is saved as the user `default`. On a server with accounts, each person picks
under their own name. A `picker` sees only their own picks, so that each
interpretation is independent of the others. An `operator` or `admin` with
the `picks` download scope can also show everyone else's with the layer
panel's contributor toggle.

## Layers

A project's layers are defined on the **Layers** page by an `operator`, and
stored in `layers.json` (see {doc}`../reference/project-files`). A layer has:

- an **ID**, written into every pick drawn in it. It cannot be changed once
  picks use it, and must be a valid name in expressions: lowercase letters,
  digits and `_`, starting with a letter.
- a **Name** and **Description**, for display only, which can be changed at
  any time.
- a **Colour**.
- whether it allows **Overhangs**, its **reducer**, and whether it warns of
  **duplicates**, described below.

Picks with a layer that is not defined, such as picks imported from another
project, are listed under **Labels in use with no definition** and drawn
without a colour. Defining a layer with that id adopts them.

Choose layers for what the interpreters should decide, not only for what
is there. A layer for "bed, clearly visible" and one for "bed, uncertain"
records the confidence of each pick, and a derived item can combine them
afterwards.

### One depth per position

A layer is a function of position: for each person, it has one depth at each
trace. That is what lets a layer be exported at even spacing, compared
between people and combined into a consensus. Three rules keep it so, both
while drawing, where the viewer marks the problem, and when saving, which is
refused until it is resolved:

- a line may not double back, to an earlier trace (an **overhang**);
- two lines in one layer may not cover the same traces, beyond the one trace
  where one ends and the next begins;
- a person may not pick two different depths in one layer at one trace (a
  **duplicate**).

A line drawn from right to left is not an overhang; only a change of
direction is.

### Overhangs

Some features do double back: a crevasse wall, the outline of a water body,
an overturned fold. **Allow overhangs** on a layer lets its lines double
back. Such a layer has no single depth at a position, so it cannot be
resampled to even spacing: it is always exported as the vertices as picked,
whatever spacing is asked for, and the export says so.

### Duplicates and reducers

Turning off `warn_on_duplicates` (in `layers.json`) declares a layer
multi-valued: one person may pick it at several depths at one position, for
example where a reflector is drawn in overlapping pieces. Such values, and
those of a layer that allows overhangs, are reduced to one per person per
position by the layer's **reducer** whenever the layer is used in a derived
item:

`shallowest`
: The shallowest value. The default, because stray picks on multiples and
  antenna ringing lie below the true reflector. For a fold,
  it keeps the upper limb.

`deepest`, `median`, `mean`
: The deepest value, or the median or mean of them.

The project's `default_reducer` applies to layers that do not name their
own. Reducers apply when the picks are used; the stored picks are never
changed, so changing a reducer can always be undone.

### Exclusivity groups

Some layers cannot both be present at one position for one person, such as
"bed, cold ice above" and "bed, temperate ice above": a person who picked
the bed decided which it is. An **exclusivity group**, defined on the
**Layers** page, says so. While drawing, the viewer marks and refuses a line
in one layer of a group over traces that a line in another layer of the same
group already covers, and saving refuses it too.

Picks that conflict anyway, such as picks drawn before the group was
defined, are not guessed between: at a position where one person has values
in two layers of a group, every one of those layers is `NaN` for that person
there. A layer can be in several groups, and only layers that share a group
conflict.

## Derived items

A **derived item** is a named expression over the layers, evaluated over
everyone's picks at each position. `median(bed)` is the median bed depth
over all contributors, a consensus line; `count(bed)` is how many of them
picked the bed there; `median(bed) - median(surface)` is a thickness. The
language and every function are in {doc}`../reference/expressions`.

What an expression gives decides what the item is:

Derived layer
: A position, such as `median(bed)`. It is drawn on the radargram as a line,
  in depth, travel time and sample number, and exported like a picked layer.

Derived attribute
: Any other number, such as a count or a spread. It is not drawn, but its
  value under the cursor is shown in the viewer's readout, and it is
  exported in its own unit beside the derived layers.

Derived items are made and edited in the viewer's layer panel, where the
expression is previewed on the radargram as it is typed, and on the
**Layers** page. A project-wide item needs the `operator` role; anyone who
can pick can keep private items of their own. An item can be **shown** when
the viewer opens or not, and **listed** in the layer panel or not, which
keeps intermediate items out of the way. A derived layer can be filled
towards another, to shade the range between two lines.

### Who sees a result computed from other people's picks

A consensus is computed from other people's picks, so it is a way to see
them. Three rules decide who sees what:

1. Anyone may see an item computed from their own picks. That is how a
   project-wide item is evaluated for a `picker`, and its label then says
   "(your picks)".
2. An `operator` or `admin` sees an item computed from everyone's picks, so
   that a consensus can be defined and followed while picking is still
   going on, without the pickers seeing each other's work.
3. An `admin` can **release** an item, so that everyone sees it computed from
   everyone's picks. Releasing publishes everyone's work in aggregate, which
   is why it needs the `admin` role.

The `results` download scope lets a person download released results
without being able to download anyone's individual picks (see
{doc}`../deploy/accounts`).

## Exporting

Picks are stored as lines in trace and sample numbers, which is the
interpretation itself (level 1). Exported, they become **points** along the
ground track, with positions and depths (level 2):

- **Picked layer points**: one point per picked line per position, each
  tagged with its layer and user. An `admin` can export every contributor's
  in one file.
- **Derived layer points**: one point per position, with every visible
  derived layer and attribute as a column.

Both are in the viewer's **Download** menu, for one radargram, and in the
catalog for a group or the whole project. The spacing of the points is:

| Spacing | Points |
|---|---|
| Auto | Evenly spaced, from the radargram's own trace spacing. |
| 1, 5, 10, 25 m | Evenly spaced, in metres along the track, at 0, 5, 10 m … from the start of the radargram, the same for every layer and person. |
| Every trace | One per trace. |
| Picked vertices only | The vertices as drawn. |

Spacing is always measured along the ground, not in traces, because trace
spacing varies with the survey speed.

The format is GeoJSON in WGS84, GeoJSON in the radargram's own CRS (which
most GIS software reads, but which strict GeoJSON readers will take for
degrees), or CSV. Every point has its radargram and revision, layer, user,
trace and sample, distance along the track, two-way travel time, depth,
easting and northing, and longitude and latitude. A CSV has the columns:

```text
radargram_id,revision_id,layer,line_index,point_index,feature_id,trace,sample,distance_m,twtt_ns,depth_m,easting,northing,longitude,latitude,crs,antenna_separation_effective_m,twtt_anchor,user
```

From the command line, `ridal interp export` does the same for one
interpretation file:

```bash
ridal interp export ridal_data/radargrams/line66.nc \
  ridal_data/interpretations/line66/default.gprinterp.json -o bed.csv --spacing 5
```

See {doc}`../reference/cli` for its options.

## Reprocessing and picks

Picks are tied to a radargram's id, and drawn on one revision of it: one
processing. When the radargram is processed again, they are **carried** to
the new revision through the clock and positions they share: across a new
zero correction, a `subset`, removed standstills or a different correction
for the antenna separation. A banner in the viewer says when picks had to be
moved or could not be carried exactly, and what moved. Picks that cannot be
carried at all are drawn where they were stored.

The stored picks stay as drawn until they are saved on the new revision, and
the version that saving replaces is kept.

## The pick manager

The **Pick manager** at the bottom of the **Layers** page lists stored
interpretations by radargram and contributor. Anyone who can pick sees their
own; an `operator` or `admin` sees everyone's. It shows counts, never the
picks themselves, so an operator can follow how picking is going without
being able to read picks their download scope does not allow.

Each row is one person's picks on one radargram:

- **Layers**: the layers they picked, with the number of lines in each.
- **Lines (vertices)**: picked lines, and in brackets the vertices they are made of.
- **Coverage**: the share of the radargram's traces with at least one picked line over them, from the first to the last trace of each line. A radargram picked in parts shows how much is left. Hovering over a layer shows that layer's coverage alone.
- **State**:
  - *Current*: drawn on the radargram as it is now.
  - *Older revision*: drawn before the radargram was reprocessed. The viewer shows these carried onto the current revision, and coverage is measured the same way, marked *(carried)*. Picks that cannot be carried are marked *(not carried)* and measured as they are stored: against the revision they were drawn on if its axes were kept, otherwise against the current one, where the viewer draws them.
  - *Ignored*: the radargram is ignored, so not served; lifting the ignore brings these back.
  - *No radargram*: no radargram with this id is served, and these were not archived by a removal, which means its file was moved or deleted on the server.
  - *Archived*: kept when the radargram was removed, with the time of the removal.

For an operator, the contributor chips above the table say on how many
radargrams each person has picked; clicking one filters to them. The table can
also be filtered by layer, state and radargram, and sorted by any column with
a heading button.

**Download** fetches the stored picks file, archived ones included, and needs
the `picks` download scope, for one's own picks as well (see
{doc}`../deploy/accounts`). **Delete** removes one's own picks on a
radargram and cannot be undone.

An `admin` cannot delete someone else's picks, but can **Archive** them: they
leave the radargram, so they are no longer shown, counted or exported, and are
listed as *Archived* with the time. This is also where a radargram's picks go
when the radargram is removed. **Purge** on an archived row permanently
deletes it, or every row archived at the same time. Archiving and purging are
both recorded in the project's audit log.

Delete and Archive refuse if the picks were saved again after the page was
loaded.

Files in the project's `interpretations/` directory that cannot be read, such
as a file whose name is not a valid account name, are listed under **Files
that could not be read** for an operator, and are left out of the counts.
