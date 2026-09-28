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
