# Rendering images

:::{admonition} Not written yet
:class: note

`ridal render`, render profiles and horizontal scales.
:::

## Annotated figures

A figure is a radargram, or part of one, with axes, ticks and labels,
written as SVG or PNG. Everything about it is in a **figure spec**, a small
JSON document, so the same spec draws the same figure again:

```bash
ridal render processed.nc --figure spec.json -o figure.svg
```

```python
import ridal

ridal.render("processed.nc", "figure.png", figure={
    "extent": {"traces": [1200, 2400]},
    "x_axis": {"unit": "distance", "spacing": 50},
    "y_axis": {"unit": "depth", "spacing": 20},
})
```

`figure=` takes a dict or the path to a JSON file. The output's extension
picks the format.

Picks and derived layers are read from the project the radargram belongs
to, given with `--project DIR` (or `project=` in Python):

```bash
ridal render ridal_data/radargrams/line.nc --figure spec.json --project . -o figure.png
```

The project also supplies the radargram's elevation window for the
topographic view.

A server draws the same figures: `POST /api/v1/datasets/{radargram_id}/figure`
takes the spec as its body (see {doc}`../reference/http-api`), and the Python
client's `figure` method calls it (see {doc}`python-client`). There, the
picks and derived layers are the ones the account may see, and the profile
must be one the server knows by name.

**SVG** keeps the labels as text and the radargram as one embedded image, so
it can be edited in Inkscape or Illustrator and scales cleanly for print.
PowerPoint and Word (2019 and later, and Microsoft 365) insert it directly.
Google Docs and Slides cannot insert SVG at all; use PNG there. An SVG's text
is drawn with whatever fonts the viewer has: the figure asks for DejaVu Sans
and falls back to a generic sans-serif without it.

**PNG** works everywhere. Its text is drawn by Ridal with the DejaVu Sans font
built into it, so a PNG looks the same on every machine.

### The spec

Every field is optional; `{}` is the whole radargram, 16 cm wide at 300 dpi,
with distance and travel-time axes when the file has them.

```json
{
  "view": "standard",
  "profile": "default",
  "x_scale": 1.0,
  "extent": {"traces": [100, 400], "rows": [0, 600]},
  "size": {"width": 16, "unit": "cm", "dpi": 300},
  "x_axis": {"unit": "distance", "spacing": 25, "anchor": 0, "label": null},
  "y_axis": {"unit": "twtt", "spacing": null, "anchor": 0, "label": null},
  "style": {"font_size": 10, "color": "#000000", "background": "#ffffff"},
  "axes": true,
  "radargram": true,
  "picks": {"items": [{"layer": "bed"}], "width": 1.0, "opacity": 0.85},
  "derived": {"items": ["bed_median"], "width": 1.5, "opacity": 0.9, "fills": true},
  "revision_id": null
}
```

A misspelt field is an error rather than being ignored.

`view`
: `standard`, or `topo` for the topographically corrected view.

`profile`
: A render profile, as for `ridal render --profile`. Contrast comes from the
  whole radargram, so an excerpt looks as it does in the viewer.

`x_scale`
: Horizontal stretch, as the viewer's horizontal scale. The plot is
  `traces × x_scale` wide for every `rows` tall, so the height follows from
  the width, the extent and this.

`extent`
: The part of the radargram to draw, in the viewer's coordinates: `traces`
  is `[first, last]` trace and `rows` is `[top, bottom]` row, both fractional,
  where trace `t` covers `t` to `t + 1`. In the standard view a row is a
  sample. In the topographic view it is a row of the corrected image, which
  moves when the radargram's elevation window is edited, so a spec for that
  view can give `elevation: [top, bottom]` in metres instead. Anything left
  out is the whole radargram; bounds past the data are clipped to it, except
  the rows (or elevations) of the topographic view, which may reach above the
  highest surface or below the floor to leave empty space there.

`size`
: `width` is the whole figure's, labels included, in `cm`, `in` or `px`.
  `dpi` converts centimetres and inches to pixels, and point sizes (the
  font, lines) to pixels in every unit. An SVG is given its size in the same
  unit, so a word processor inserts it at the size it was made for. A figure
  can be at most 40 megapixels.

`x_axis`, `y_axis`
: `unit` is what the axis counts in:

  | Axis | Units |
  |---|---|
  | x | `trace`, `distance` (m), `time` (UTC time of day) |
  | y, standard view | `sample`, `twtt` (ns), `depth` (m) |
  | y, topographic view | `elevation` (m) |

  `spacing` is the distance between ticks, in that unit (seconds for time);
  without it, Ridal picks a round spacing that gives about six ticks. Ticks
  are at `anchor + k × spacing`: with the default anchor of 0, a 10 m spacing
  gives 10, 20, 30 m wherever the excerpt starts. `label` replaces the axis
  title; `""` draws none.

`style`
: `font_size` in points, `color` for text, ticks and the frame, and
  `background`, or `null` for a transparent one. Colours are hex, `#rrggbb`.

`axes`
: `false` writes the radargram alone as PNG, with no margins, and a `.json`
  file beside it that says what it shows: the extent, and each axis's value at
  every pixel column and row. That is enough to draw it in matplotlib with
  `imshow(..., extent=...)` or `pcolormesh`.

`radargram`
: `false` leaves the radargram out, so only the picks and derived layers are
  drawn, over the background.

`picks`
: Which picked lines to draw. Each entry in `items` selects by `user`,
  `layer`, both or neither: `{"layer": "bed"}` is everyone's bed picks,
  `{"user": "erik"}` all of one person's, `{"layer": ""}` the lines with no
  layer, and `{}` all of them. With no
  items, the default, no picks are drawn. Each line is drawn in its layer's
  colour, `width` points wide.

  Picks are drawn where the viewer draws them: picks made on an earlier
  revision are carried to this one. Picks that cannot be carried, including
  those that do not say which revision they were drawn on (imported from
  older tools), are drawn as stored, as the viewer's contributor overlay
  draws them, with a warning.

`derived`
: Which derived layers to draw, by id, each in its own colour. With `fills`,
  a range fill between two drawn layers is drawn under them, as in the
  viewer. A derived *attribute*, such as a count, is not a position and
  cannot be drawn.

`revision_id`
: When set, the figure is only drawn from that revision of the radargram.
  Trace and row numbers can point at a different stretch of a reprocessed
  radargram, so a spec that names its revision is refused rather than quietly
  drawing the wrong part.

### Ticks sit where their coordinate is

Distance along a profile is not even in trace number: where the survey slowed
down, more traces cover each metre. Ridal draws every trace as recorded and
puts each distance tick at the trace where that distance was reached, so 10 m
ticks can be unevenly spaced on the page. A standstill, where the distance
stands still for many traces, gets one tick, where it was first reached.
Time ticks work the same way.
