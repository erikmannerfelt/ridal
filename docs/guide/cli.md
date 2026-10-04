# Using the CLI

Every command has built-in help:

```bash
ridal -h
ridal process -h
```

The full list of commands and flags is in the {doc}`../reference/cli`.

## Inspecting a file

`ridal info` shows a file's metadata and a summary of its location data:

```bash
ridal info DAT_001_A1.rd3
```

Add `--json` for output that other tools can read.

## Processing

Process a file with the default profile:

```bash
ridal process DAT_001_A1.rd3 --default
```

Or choose the steps yourself, either inline or from a file:

```bash
ridal process DAT_001_A1.rd3 --steps "zero_corr,dewow,auto_gain"
ridal process DAT_001_A1.rd3 --steps steps.txt
```

A steps file has one step per line, and comments are allowed:

```text
subset(1 100) # Comments are supported!
zero_corr
dewow

correct_topography
```

Every step and its arguments are listed in {doc}`../reference/steps`, and
`ridal steps` prints the same list.

The output is a NetCDF file with the same name as the input and an `.nc`
suffix, written beside the input unless `-o`/`--output` says otherwise.

## Many files at once

`ridal batch-process` takes a glob pattern. `--merge` joins files that were
recorded close together in time:

```bash
ridal batch-process "data/*.rd3" --merge "10 min" --default -o output/
```

Files are only joined when they agree on what the merged file states once
for all its traces: the CRS, antenna frequency, time window, antenna
separation and medium velocity. A file that differs in any of them starts a
new output instead.

## Rendering an image

`-r` renders the processed profile as a JPEG beside the output. A file that is
already processed can be rendered on its own:

```bash
ridal render processed.nc -o profile.png
```

See {doc}`rendering` for render profiles and scales.
