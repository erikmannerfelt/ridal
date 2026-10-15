# Examples

These are worked examples that go from raw data to results, using only Ridal
and, in the Python examples, xarray and matplotlib.

## A survey, from raw files to points

This example starts from a day of 100 MHz profiles in `raw/`, recorded as
many files with short breaks between them, which a team will pick. It is
worth looking at one file first, to check that the antenna, the separation
and the positions are read as expected:

```bash
ridal info raw/DAT_0140_A1.rd3
```

With `--dry-run`, the batch shows how the files will be merged and named
without processing anything:

```bash
ridal batch-process "raw/*.rd3" --merge "10 min" -o survey/ \
  --group-name "Drønbreen" \
  --id-pattern "{group}-{date}-{freq}mhz-{n:02}" \
  --display-name-pattern "Drønbreen {date}, {freq} MHz, line {n}" \
  --dry-run
```

The project is then made and the files processed into it, with the default
profile plus standstill removal, which time-triggered data usually need:

```bash
ridal project init survey
ridal batch-process "raw/*.rd3" --merge "10 min" -o survey/ridal_data/radargrams/ \
  --group-name "Drønbreen" \
  --id-pattern "{group}-{date}-{freq}mhz-{n:02}" \
  --display-name-pattern "Drønbreen {date}, {freq} MHz, line {n}" \
  --steps "remove_empty_traces,remove_standstills,zero_corr,correct_antenna_separation,dewow,background_removal,auto_gain"
ridal gui survey
```

In the browser, the layers (such as `surface` and `bed`) are defined on the
**Layers** page before picking starts. To share the picking with others, the
project is instead served from a server with accounts (see
{doc}`../deploy/index`).

When the picking is done, **Download all → Picked layer points** in the
catalog gives the picks on every radargram as points in one file, at an even
spacing along the track; an `admin` can include everyone's. See {doc}`../guide/interpretation`.

## A radargram in matplotlib, against elevation

This example processes a file in memory and draws it against elevation, with
the topographic correction done by xarray from the file's own axes:

```python
import matplotlib.pyplot as plt
import ridal

ds = ridal.process(
    "DAT_0066_A1.rad",
    default=True,
    return_dataset=True,
    return_dataset_format="xarray",
)

# The elevation of every sample: its trace's surface minus its depth.
data = ds["data"].assign_coords(z=ds["elevation"] - ds["depth"])

fig, ax = plt.subplots(figsize=(10, 4))
data.plot.pcolormesh(
    x="distance", y="z", ax=ax, cmap="gray", robust=True, add_colorbar=False
)
ax.set_xlabel("Distance (m)")
ax.set_ylabel("Elevation (m)")
ax.set_title(ds.attrs["ridal_radargram_id"])
fig.savefig("line66.png", dpi=200)
```

The same code works on a processed file opened with `xarray.open_dataset`.
For a figure with picks drawn on it, see the annotated figures in
{doc}`../guide/rendering`.

## A consensus from several interpreters

In this example, several people have picked the bed of the same radargrams
independently. Parts of the bed are hard to see, so the project has two
layers for it, which one person cannot both pick at the same place:

```json
{
  "schema": "ridal-layers",
  "schema_version": "1",
  "default_reducer": "shallowest",
  "layers": [
    {"id": "bed", "name": "Bed, clear", "color": "#3cb44b", "groups": ["bed-kind"]},
    {"id": "bed_unclear", "name": "Bed, unclear", "color": "#ffe119", "groups": ["bed-kind"]}
  ],
  "groups": [
    {"id": "bed-kind", "name": "Bed", "members": ["bed", "bed_unclear"]}
  ]
}
```

An `operator` then adds derived items on the **Layers** page:

| ID | Expression | Unit | What it is |
|---|---|---|---|
| `bed_consensus` | `median(concatenate(bed, bed_unclear))` | meters | The median bed depth over everyone, a layer. |
| `bed_count` | `count(concatenate(bed, bed_unclear))` | dimensionless | How many people picked the bed at each position. |
| `bed_spread` | `nmad(concatenate(bed, bed_unclear))` | meters | How much they disagree. |
| `bed_clear_share` | `count(bed) / max(count(concatenate(bed, bed_unclear)), 1.0)` | dimensionless | The share of the picks that were clear. |

`bed_consensus` is drawn on the radargram as a line, while the others are
numbers that are shown under the cursor when they are listed. All four are
exported together with **Derived layer points**, with one row per position.
While picking is still going on, only an `operator` sees the consensus, and
an `admin` releases it to everyone once the picking is done (see
{doc}`../guide/interpretation` and {doc}`../reference/expressions`).
