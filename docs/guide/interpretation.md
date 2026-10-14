# Interpretation and layers

:::{admonition} Not written yet
:class: note

Picking reflectors in the viewer, and everything that decides what a pick means:

- **Projects**: where picks are saved, and who can see whose.
- **Layers**: defining the layers a project is picked into.
- **Reducers**: how several picks by one person at one position become one value, and why `shallowest` is the default.
- **Overhangs**: why a layer must have one depth per position by default, when to allow overhangs (crevasse walls, water-body outlines), and why a layer that allows them is exported as its picked vertices rather than at even spacing.
- **Duplicates**: when more than one value at a position is pointed out.
- **Exclusivity groups**: layers that cannot both hold a value at one position, what the viewer refuses while drawing, and why values that still conflict become `NaN`.
- **Derived items**: combining layers with {doc}`../reference/expressions`, derived layers versus derived attributes, the layer panel and expression editor, and who may see a result computed from other people's picks.
- **Exporting**: level 2 points as GeoJSON or CSV, from the browser or with `ridal interp export`.
:::

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

**Download** fetches the stored picks file, and needs the `picks` download
scope, for one's own picks as well (see {doc}`../deploy/accounts`).
**Delete** removes one's own picks on a radargram, on its current or an older revision, and cannot be undone. It
refuses if the picks were saved again after the page was loaded.

Files in the project's `interpretations/` directory that cannot be read, such
as a file whose name is not a valid account name, are listed under **Files
that could not be read** for an operator, and are left out of the counts.
