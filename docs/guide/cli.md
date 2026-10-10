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

`-r` and `-t` on their own put the images and tracks in the output
directory, or in the directory given after them. With `--no-export`, `-o` is
optional and those go in the current directory.

### Naming the outputs

Each output's radargram id comes from its first input's file name, unless
`--id-pattern` gives a pattern for it. The outputs are then named after their
ids:

```bash
ridal batch-process "data/*.rd3" --merge "10 min" --default -o output/ \
  --group-name "Drønbreen" \
  --id-pattern "{group}-2026-{freq}mhz-{n:02, per=freq}" \
  --display-name-pattern "Drønbreen 2026, {freq} MHz, line {n}"
```

| Placeholder | Value |
|---|---|
| `{stem}` | the first input's file stem: `dat_0142_a1` |
| `{n}` | a counter over the outputs, in the order they were recorded, from 1 |
| `{freq}` | the antenna frequency in MHz |
| `{nfiles}` | how many inputs were merged into the output |
| `{date}` | the date of the first trace, `YYYYMMDD` |
| `{group}` | the group id, from `--group-name` or `--group-id` |

`{n:02}` pads a number to two digits, and so do `{freq:04}` and
`{nfiles:02}`. The counter takes options after a comma:

- `{n, from=15}` starts at 15.
- `{n, from=next}` continues after the highest number among the radargrams
  already in the output directory whose ids match the pattern.
- `{n, per=freq}` counts each frequency separately, so the example above
  gives `dronbreen-2026-100mhz-01`, `dronbreen-2026-100mhz-02` and
  `dronbreen-2026-800mhz-01`. `per=freq+date` counts each frequency on each
  day.

An id may only hold lowercase ASCII letters, digits, `-` and `_`, so the text
around the placeholders is checked when the pattern is read: write `mhz`, not
`MHz`. Every id is worked out before anything is processed, and two outputs
that would get the same one are an error.

`--display-name-pattern` takes the same placeholders and any text, plus `{id}`
and `{group_name}`. Its `{n}` is the number in the output's id, so line 7 is
`…-07`, unless it is given `from=` or `per=` of its own.

## Rendering an image

`-r` renders the processed profile as a JPEG beside the output. A file that is
already processed can be rendered on its own:

```bash
ridal render processed.nc -o profile.png
```

See {doc}`rendering` for render profiles and scales.
