# Using Python

```bash
pip install ridal
```

The `ridal` package runs the same reader, processing steps and renderer as
the command line, in-process. Every function takes paths as strings or
`pathlib.Path`. The full signatures are in {doc}`../reference/python`; this
page shows how the functions fit together. To talk to a Ridal server instead,
see {doc}`python-client`.

## Inspecting a file

`ridal.info` returns what `ridal info` prints, as a list with one dict per
input:

```python
import ridal

info = ridal.info("DAT_0066_A1.rad")[0]
info["metadata"]["antenna_mhz"]   # 25.0
info["location"]                  # CRS, start and stop, track length, ...
```

## Processing

`ridal.process` takes the steps as a list, as a comma-separated string, or
`default=True` for the default profile (see {doc}`processing`):

```python
ridal.process("DAT_0066_A1.rad", default=True)
ridal.process("DAT_0066_A1.rad", steps=["zero_corr", "dewow", "auto_gain"],
              output="processed/line66.nc")
```

It writes a NetCDF file, beside the input unless `output` says otherwise, and
returns its path. `ridal.all_steps` lists the step names and
`ridal.all_step_descriptions` describes each one.

The other arguments are those of `ridal process`: `velocity`, `crs`, `dem`,
`cor`, `override_antenna_mhz`, `override_antenna_separation`, `track` and
`render`, with `render_profile`, `render_width` and `render_topo` for the
image. Several inputs given together are merged into one output.

## Working with the result in xarray

With `return_dataset=True`, nothing is written and the processed radargram
is returned instead. `return_dataset_format="xarray"` makes it an
`xarray.Dataset` (xarray must be installed):

```python
ds = ridal.process(
    "DAT_0066_A1.rad",
    default=True,
    return_dataset=True,
    return_dataset_format="xarray",
)
```

The dataset has the same variables, coordinates and attributes as the
NetCDF file (see {doc}`file-formats`): `data` on the dimensions `y` (sample)
and `x` (trace), with `twtt` and `depth` along `y` and `distance`, `time`,
`easting`, `northing` and `elevation` along `x`.

```python
import matplotlib.pyplot as plt

ds["data"].plot(x="distance", y="depth", yincrease=False, cmap="gray",
                robust=True)
plt.show()

# The trace closest to 1 km along the profile.
trace = ds["data"].isel(x=int(abs(ds["distance"] - 1000).argmin()))
```

`distance` cannot be used as an index with `sel`: where the survey stood
still, many traces share one distance.

A processed file opens the same way with `xarray.open_dataset`, so the two
are interchangeable. The default `return_dataset_format`, `"xarray_dict"`,
is the same content as a plain dict, for when xarray is not installed.

`ridal.read` returns a file as it is recorded, with no processing, in the
same forms. Its `twtt` is not yet counted from time zero (see
{doc}`file-formats`).

```python
raw = ridal.read("DAT_0066_A1.rad", return_dataset_format="xarray")
```

## Many files at once

`ridal.batch_process` is `ridal batch-process`: each output goes in the
directory given as `output`, which must exist, and the paths are returned.
`merge` joins recordings made close together in time:

```python
from pathlib import Path

outputs = ridal.batch_process(
    sorted(Path("raw").glob("*.rad")),
    output="processed",
    default=True,
    merge="10 min",
    group_name="Drønbreen",
    id_pattern="{group}-{date}-{freq}mhz-{n:02}",
)
```

See {doc}`cli` for how files are merged and what the pattern placeholders
mean.

## Identity arguments

A processed radargram has an id, which picks and URLs refer to, and
optionally a display name and a group. They are written into the NetCDF
file, and the catalog in the GUI groups and labels radargrams by them.

`radargram_id`
: A stable, unique id: lowercase ASCII letters, digits, `-` and `_`. Without
  it, the output file's stem is used, lowercased. Give one when the file
  names are not unique across a survey, such as `DAT_0001` from every day of
  fieldwork. Picks are tied to it, so do not change it after people have
  started picking.

`radargram_name`
: A label for people to read, such as `"Drønbreen 2025, line 66"`. It has no
  meaning beyond display and can be changed freely. The CLI calls it
  `--display-name`.

`group_name`, `group_id`
: The survey, campaign or site the radargram belongs to. The id is derived
  from the name unless `group_id` gives it.

`metadata`
: Anything else, as a JSON-serializable dict, kept in the file as
  `ridal_user_metadata_json`.

In `batch_process`, `group_name` and `group_id` apply to every output, and
`id_pattern` and `display_name_pattern` name each one.

## Rendering

`ridal.render` writes an image of a processed file, with the same renderer
as the GUI, so the same profile gives the same image:

```python
ridal.render("line66.nc", "line66.png")
ridal.render("line66.nc", "line66.jpg", profile="siglog-default", width=2000)
ridal.render("line66.nc", "figure.svg", figure={"y_axis": {"unit": "depth"}})
```

See {doc}`rendering` for profiles and figure specs.

## Errors

Arguments that do not fit together, such as both `steps` and `default=True`,
or an unknown step, raise `ValueError` before anything is read. A file that
cannot be read or processed raises `RuntimeError` with Ridal's message.
