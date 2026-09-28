<!-- Generated from src/cli.rs; edit the doc comments there, then run
     UPDATE_CLI_MD=1 cargo test --no-default-features -F cli,server cli_md -->

# CLI reference

Every command and option of `ridal`, generated from the program itself. `ridal <command> --help` prints the same text.

## `ridal process`

```{program} ridal process
```

Process one or more GPR profiles into one final output

```console
$ ridal process [OPTIONS] <INPUTS>...
```

**Arguments**

```{option} <INPUTS>
Input header/data path(s). Explicit paths are preferred, but glob patterns are also expanded

Required.
```

**Options**

```{option} -v <VELOCITY>, --velocity <VELOCITY>
Velocity of the medium in m/ns. Defaults to the typical velocity of ice

Default: `0.168`.
```

```{option} -c <COR>, --cor <COR>
Load a separate ".cor" file (RAMAC only). If not given, it will be searched for automatically
```

```{option} -d <DEM>, --dem <DEM>
Correct elevation values with a DEM
```

```{option} --crs <CRS>
Which coordinate reference system to project coordinates in
```

```{option} -t <TRACK>, --track <TRACK>
Export the location track to CSV. If no value is given, a sidecar path is derived from the output path
```

```{option} --default
Process with the default profile
```

```{option} --default-with-topo
Process with the default profile plus topographic correction
```

```{option} --steps <STEPS>
Processing steps to run, separated by commas. Can also be a filepath to a newline-separated step file
```

```{option} -o <OUTPUT>, --output <OUTPUT>
Output filename or directory. Defaults to the first input with a ".nc" extension
```

```{option} -q, --quiet
Suppress progress messages
```

```{option} -r <RENDER>, --render <RENDER>
Render an image of the profile and save it to the specified path. If no path is given, a JPG sidecar is used
```

```{option} --render-profile <RENDER_PROFILE>
Render profile for --render: a built-in name, or a path to a TOML file. Distinct from the *processing* profile selected by --default and --steps
```

```{option} --render-width <RENDER_WIDTH>
Output width in pixels for --render. Defaults to one pixel per trace
```

```{option} --no-export
Don't export an nc file
```

```{option} --override-antenna-mhz <OVERRIDE_ANTENNA_MHZ>
Override the antenna center frequency (in MHz) from file metadata
```

```{option} --override-antenna-separation <OVERRIDE_ANTENNA_SEPARATION>
Override the antenna separation (in m) from file metadata
```

```{option} --metadata <KEY=VALUE>
Add user metadata as key=value. Repeatable
```

```{option} --radargram-id <RADARGRAM_ID>
Stable, unique identifier for this radargram (lowercase ASCII, digits, '-', '_'). Defaults to the output file stem if not given
```

```{option} --display-name <DISPLAY_NAME>
Human-readable display label. Purely cosmetic: has no identity semantics
```

```{option} --group-name <GROUP>
Human-readable name of the group this radargram belongs to (survey, campaign, location), for catalog grouping. Non-ASCII is accepted and written unchanged. Ridal writes everything it generates itself as ASCII, but some NetCDF readers (for example xarray with the h5netcdf engine) will garble a non-ASCII value. A stable URL/filesystem-safe id is derived from this automatically unless --group-id overrides it. `--group` is a supported alias
```

```{option} --group-id <GROUP_ID>
Explicit override for the group's id, when the id automatically derived from --group-name is not the one wanted
```

## `ridal batch-process`

```{program} ridal batch-process
```

Batch-process one or more GPR profiles into many outputs

```console
$ ridal batch-process [OPTIONS] --output <OUTPUT> <INPUTS>...
```

**Arguments**

```{option} <INPUTS>
Input header/data path(s). Explicit paths are preferred, but glob patterns are also expanded

Required.
```

**Options**

```{option} -o <OUTPUT>, --output <OUTPUT>
Output directory. Must already exist

Required.
```

```{option} -v <VELOCITY>, --velocity <VELOCITY>
Velocity of the medium in m/ns. Defaults to the typical velocity of ice

Default: `0.168`.
```

```{option} -c <COR>, --cor <COR>
Load a separate ".cor" file (RAMAC only). If not given, it will be searched for automatically
```

```{option} -d <DEM>, --dem <DEM>
Correct elevation values with a DEM
```

```{option} --crs <CRS>
Which coordinate reference system to project coordinates in
```

```{option} -t <TRACK>, --track <TRACK>
Export location tracks to CSV in the given directory
```

```{option} --default
Process with the default profile
```

```{option} --default-with-topo
Process with the default profile plus topographic correction
```

```{option} --steps <STEPS>
Processing steps to run, separated by commas. Can also be a filepath to a newline-separated step file
```

```{option} -q, --quiet
Suppress progress messages
```

```{option} -r <RENDER>, --render <RENDER>
Render images into the given directory
```

```{option} --render-profile <RENDER_PROFILE>
Render profile for --render: a built-in name, or a path to a TOML file. Distinct from the *processing* profile selected by --default and --steps
```

```{option} --render-width <RENDER_WIDTH>
Output width in pixels for --render. Defaults to one pixel per trace
```

```{option} --no-export
Don't export nc files
```

```{option} --merge <MERGE>
Merge neighboring chronological profiles that are closer than the given threshold (e.g. "10 min"). Incompatible neighbors remain separate outputs
```

```{option} --override-antenna-mhz <OVERRIDE_ANTENNA_MHZ>
Override the antenna center frequency (in MHz) from file metadata
```

```{option} --override-antenna-separation <OVERRIDE_ANTENNA_SEPARATION>
Override the antenna separation (in m) from file metadata
```

```{option} --metadata <KEY=VALUE>
Add user metadata as key=value. Repeatable
```

```{option} --group-name <GROUP>
Human-readable name of the group all outputs in this batch belong to (survey, campaign, location), for catalog grouping. Non-ASCII is accepted and written unchanged; see `ridal process --help` for the reader caveat. Applied uniformly; radargram IDs and display names are still derived per-output since an explicit single value would collide. `--group` is a supported alias
```

```{option} --group-id <GROUP_ID>
Explicit override for the group's id, when the id automatically derived from --group-name is not the one wanted
```

## `ridal info`

```{program} ridal info
```

Show metadata/location information for one or more GPR profiles

```console
$ ridal info [OPTIONS] <INPUTS>...
```

**Arguments**

```{option} <INPUTS>
Input header/data path(s). Explicit paths are preferred, but glob patterns are also expanded

Required.
```

**Options**

```{option} --json
Emit JSON instead of human-readable text
```

```{option} -v <VELOCITY>, --velocity <VELOCITY>
Velocity of the medium in m/ns. Defaults to the typical velocity of ice

Default: `0.168`.
```

```{option} -c <COR>, --cor <COR>
Load a separate ".cor" file (RAMAC only). If not given, it will be searched for automatically
```

```{option} -d <DEM>, --dem <DEM>
Correct elevation values with a DEM
```

```{option} --crs <CRS>
Which coordinate reference system to project coordinates in
```

```{option} --override-antenna-mhz <OVERRIDE_ANTENNA_MHZ>
Override the antenna center frequency (in MHz) from file metadata
```

```{option} --override-antenna-separation <OVERRIDE_ANTENNA_SEPARATION>
Override the antenna separation (in m) from file metadata
```

## `ridal steps`

```{program} ridal steps
```

Inspect available processing steps

```console
$ ridal steps [OPTIONS]
```

**Options**

```{option} --describe-all
Show descriptions for all steps
```

```{option} --describe <DESCRIBE>
Show the description for one step
```

```{option} --default
Show the default processing pipeline
```

```{option} --json
Emit JSON instead of human-readable text
```

## `ridal formats`

```{program} ridal formats
```

Inspect supported formats

```console
$ ridal formats [OPTIONS]
```

**Options**

```{option} --json
Emit JSON instead of human-readable text
```

## `ridal render`

```{program} ridal render
```

Render a processed radargram to an image

```console
$ ridal render [OPTIONS] <INPUT>
```

**Arguments**

```{option} <INPUT>
Processed .nc file to render

Required.
```

**Options**

```{option} -o <OUTPUT>, --output <OUTPUT>
Output image path. The extension picks the encoding (.png or .jpg); if omitted, a sidecar beside the input is used
```

```{option} --profile <PROFILE>
Render profile: a built-in name, or a path to a TOML file

Default: `default`.
```

```{option} --width <WIDTH>
Output width in pixels. Defaults to one pixel per trace; larger than the trace count is not upsampled to
```

```{option} --quality <QUALITY>
JPEG quality, 1-100. Ignored for PNG
```

```{option} --topo
Render the topographically corrected view instead of the standard one (#168). Fails with a clear reason rather than falling back to a standard render when the file lacks usable `elevation`/`depth` axes
```

```{option} -q, --quiet
Suppress progress messages
```

## `ridal interp`

```{program} ridal interp
```

Work with interpretations (picked layers) of processed radargrams

```console
$ ridal interp <COMMAND>
```

### `ridal interp export`

```{program} ridal interp export
```

Derive the level 2 point product from a level 1 interpretation

```console
$ ridal interp export [OPTIONS] --output <OUTPUT> <RADARGRAM> <INTERPRETATION>
```

**Arguments**

```{option} <RADARGRAM>
The processed radargram (.nc) the interpretation was drawn on

Required.
```

```{option} <INTERPRETATION>
The level 1 interpretation (a gprinterp JSON document)

Required.
```

**Options**

```{option} -o <OUTPUT>, --output <OUTPUT>
Where to write the level 2 product. The format follows the extension: ".geojson"/".json" for GeoJSON, ".csv" for CSV

Required.
```

```{option} --spacing <SPACING>
Point spacing along the ground track. A distance in metres ("5", "2.5"), "auto" to derive one from the radargram's own trace spacing, "per-trace" for one point per native trace, or "vertices" for the picked vertices exactly as drawn.

Spacing is always measured in metres along the track, never in traces: trace spacing varies with survey speed, so a fixed trace stride produces unevenly spaced ground positions.

Default: `auto`.
```

```{option} --crs <CRS>
CRS for the output geometry. WGS84 by default, which is what RFC 7946 requires of GeoJSON. Accepts "native" for the radargram's own projected CRS, or any CRS string PROJ understands.

Note that projected GeoJSON is not portable: readers that follow RFC 7946 will interpret the coordinates as degrees. Native easting/northing are always present as properties regardless.
```

```{option} --user <USER>
The author recorded on every exported point. This command is not tied to a server account, so it is a label rather than an authenticated identity

Default: `default`.
```

## `ridal project`

```{program} ridal project
```

Create and inspect Ridal projects

```console
$ ridal project <COMMAND>
```

### `ridal project init`

```{program} ridal project init
```

Create a project so interpretations have somewhere to live

```console
$ ridal project init [OPTIONS] [PATH]
```

**Arguments**

```{option} <PATH>
Directory to create the project in. Created if it does not exist

Default: `.`.
```

**Options**

```{option} --name <NAME>
Human-facing project name. Cosmetic
```

### `ridal project info`

```{program} ridal project info
```

Show what a project contains

```console
$ ridal project info [PATH]
```

**Arguments**

```{option} <PATH>
A path inside the project. The project is found by searching upwards

Default: `.`.
```

### `ridal project migrate`

```{program} ridal project migrate
```

Move a project created by an older Ridal into its data directory

```console
$ ridal project migrate [OPTIONS] [PATH]
```

**Arguments**

```{option} <PATH>
The project directory, the one holding ridal.toml

Default: `.`.
```

**Options**

```{option} --dry-run
Print what would move, without moving anything
```

### `ridal project user`

```{program} ridal project user
```

Manage who may use the project's server

```console
$ ridal project user <COMMAND>
```

#### `ridal project user add`

```{program} ridal project user add
```

Create an account and print a one-time invite link

```console
$ ridal project user add [OPTIONS] <NAME>
```

**Arguments**

```{option} <NAME>
The account name. Lowercase letters, digits, '-' and '_'; it is used as a filename inside the project

Required.
```

**Options**

```{option} --role <ROLE>
What they may do: viewer, picker, operator or admin. Each level includes the ones below it

Default: `picker`.
```

```{option} --download <DOWNLOAD>
What they may download: none, results, picks, derived or all

Default: `all`.
```

```{option} --path <PATH>
A path inside the project. The project is found by searching upwards

Default: `.`.
```

#### `ridal project user add-bulk`

```{program} ridal project user add-bulk
```

Create several accounts and print their invite links or passwords

```console
$ ridal project user add-bulk [OPTIONS] --count <COUNT>
```

**Options**

```{option} --prefix <PREFIX>
Generate names as prefix-01, prefix-02, and so on

Default: `student`.
```

```{option} --random-names
Draw names from a fixed pool of friendly usernames instead of the prefix. Fails if fewer unused names remain than were requested
```

```{option} --count <COUNT>
Number of accounts to create

Required.
```

```{option} --role <ROLE>
What they may do: viewer, picker, operator or admin

Default: `picker`.
```

```{option} --download <DOWNLOAD>
What they may download: none, results, picks, derived or all

Default: `all`.
```

```{option} --passwords
Generate shared passwords instead of one-time invite links
```

```{option} --i-know-what-i-am-doing
Required acknowledgement for generated shared passwords
```

```{option} --out <OUT>
Where password mode writes `name<TAB>password` lines. They are never printed to the terminal, which is often captured in a log; hand the file out and then delete it

Default: `passwords.txt`.
```

```{option} --path <PATH>
A path inside the project. The project is found by searching upwards

Default: `.`.
```

#### `ridal project user list`

```{program} ridal project user list
```

List the accounts and what each may do

```console
$ ridal project user list [PATH]
```

**Arguments**

```{option} <PATH>
A path inside the project. The project is found by searching upwards

Default: `.`.
```

#### `ridal project user set`

```{program} ridal project user set
```

Change someone's role or download scope

```console
$ ridal project user set [OPTIONS] <NAME>
```

**Arguments**

```{option} <NAME>
Required.
```

**Options**

```{option} --role <ROLE>
New role: viewer, picker, operator or admin
```

```{option} --download <DOWNLOAD>
New download scope: none, results, picks, derived or all
```

```{option} --path <PATH>
A path inside the project. The project is found by searching upwards

Default: `.`.
```

#### `ridal project user reset`

```{program} ridal project user reset
```

Issue a fresh invite link, for a password reset or a lost one

```console
$ ridal project user reset [OPTIONS] <NAME>
```

**Arguments**

```{option} <NAME>
Required.
```

**Options**

```{option} --path <PATH>
A path inside the project. The project is found by searching upwards

Default: `.`.
```

#### `ridal project user remove`

```{program} ridal project user remove
```

Remove an account. Their interpretations are kept

```console
$ ridal project user remove [OPTIONS] <NAME>
```

**Arguments**

```{option} <NAME>
Required.
```

**Options**

```{option} --path <PATH>
A path inside the project. The project is found by searching upwards

Default: `.`.
```

## `ridal gui`

```{program} ridal gui
```

Open a local browser GUI for one radargram or a directory of them

```console
$ ridal gui [OPTIONS] [PATH]
```

**Arguments**

```{option} <PATH>
A single processed .nc file, or a directory to scan recursively. Omitted, Ridal serves the project found by searching upwards from here, or this directory if there is none
```

**Options**

```{option} --cache-memory-mb <CACHE_MEMORY_MB>
In-memory cache budget for encoded chunk/overview images, in MB
```

```{option} --n-workers <N_WORKERS>
Number of worker threads for CPU-heavy rendering
```

```{option} --read-only
Serve a project without accepting any writes
```

```{option} --open-browser
Open a browser after starting (off by default, matching `ridal server start`)
```

## `ridal server`

```{program} ridal server
```

Run the web server explicitly (for remote or persistent deployment)

```console
$ ridal server <COMMAND>
```

### `ridal server start`

```{program} ridal server start
```

Start the HTTP server

```console
$ ridal server start [OPTIONS] <PATH>
```

**Arguments**

```{option} <PATH>
A single processed .nc file, or a directory to scan recursively

Required.
```

**Options**

```{option} --host <HOST>
Bind address. Loopback by default; binding elsewhere is explicit because Ridal does not terminate TLS, so a non-loopback bind needs a TLS-terminating reverse proxy in front of it (see `--allow-insecure-login`)

Default: `127.0.0.1`.
```

```{option} --port <PORT>
Bind port. A stable default rather than an OS-assigned ephemeral port, since this mode is for persistent/remote deployment

Default: `8000`.
```

```{option} --open-browser
Open a browser after starting (off by default in this mode)
```

```{option} --read-only
Serve a project without accepting any writes. Caps every caller at the "viewer" role, whatever their account says
```

```{option} --allow-insecure-login
Accept password logins while bound to a non-loopback address.

Ridal does not terminate TLS, so a password sent to a non-loopback address travels in the clear unless something in front of it is doing so. Use this only when you know what that something is; the supported arrangement is to bind loopback behind a TLS-terminating reverse proxy.
```

```{option} --cache-memory-mb <CACHE_MEMORY_MB>
In-memory cache budget for encoded chunk/overview images, in MB
```

```{option} --n-workers <N_WORKERS>
Number of worker threads for CPU-heavy rendering
```
