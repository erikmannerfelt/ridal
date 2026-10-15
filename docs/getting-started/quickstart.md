# Quickstart

This page goes from a raw recording to a processed radargram open in the
browser. It assumes the `ridal` command is installed (see
{doc}`installation`). The examples use a Malå recording, `DAT_0066_A1.rad`
with its `.rd3` and `.cor` beside it; any supported format works the same way
(see {doc}`../guide/file-formats`).

## 1. Look at the file

```console
$ ridal info DAT_0066_A1.rad
Input:		DAT_0066_A1.rad
Format:		ramac (Malå RAMAC format)
...
Samples:	736
Traces:		3548
Time window:	2241.852 ns
Antenna MHz:	25
Antenna sep:	6.2 m
...
CRS:		EPSG:32633
Track length:	2532.382 m
```

`info` reads the header and the positions without processing anything. It is
the quickest way to see whether Ridal reads a file as you expect: the antenna
frequency and separation, the time window, and where and when it was
recorded.

## 2. Process it

```bash
ridal process DAT_0066_A1.rad --default
```

`--default` runs the default processing profile: it removes empty traces,
moves time zero to the direct wave, corrects for the antenna separation,
removes slow drift (dewow) and horizontal banding (background removal), and
applies a gain. The result is `DAT_0066_A1.nc`, a NetCDF file beside the
input. {doc}`../guide/processing` explains each step and when to choose
others.

Add `-r` to also write a JPEG of the radargram beside it, for a first look
without a browser:

```bash
ridal process DAT_0066_A1.rad --default -r
```

## 3. Make a project

A project is a directory where Ridal keeps what it adds to your data, such as
picks and layer definitions. Make one, and put the processed file in it:

```bash
ridal project init my_survey
ridal process DAT_0066_A1.rad --default -o my_survey/ridal_data/radargrams/
```

Processed files can be anywhere below the project directory; the
`ridal_data/radargrams/` directory is where files uploaded through the browser
go.

## 4. Open it in the browser

```bash
ridal gui my_survey
```

Ridal prints the address it serves on, such as
`http://127.0.0.1:37951/p/default/`; open it, or add `--open-browser` to have
Ridal open it for you. The port is any free one unless `--port` names one.

The catalog lists the radargram with a map of where it was recorded. Click it
to open the viewer, where you can zoom and pan, see the trace, travel time
and depth under the cursor, and pick reflectors. Stop the server with
Ctrl+C.

## Where next

- {doc}`../guide/cli` for processing many files at once and naming the
  results.
- {doc}`../guide/gui` and {doc}`../guide/interpretation` for the viewer and
  picking.
- {doc}`../guide/python` to do the same from Python.
