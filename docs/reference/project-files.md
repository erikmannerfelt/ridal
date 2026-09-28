# Project files

A **project** is a directory with a `ridal.toml` in it. `ridal project init`
creates one. Everything Ridal writes goes in a single `ridal_data/` directory
beside it, so your own files are never touched and it is always clear what
belongs to Ridal.

```text
my_survey/
  line_01.nc ...                  your processed files, untouched
  ridal.toml                      marks the project, and holds its settings
  ridal_data/                     everything Ridal owns
    .gitignore                    keeps the cache and secrets out of git
    interpretations/
      <radargram_id>/
        <user>.gprinterp.json     one interpretation per person
    layers/
      layers.json                 the project's layers
    derived/
      derived.json                derived items
    preferences/
      <user>.json                 one person's display preferences
    users.json                    accounts and access policy
    session.key                   signs login cookies
    overrides.json                corrected radargram and group metadata
    revisions/, revisions.json    earlier revisions of replaced radargrams
    audit.json                    who added or removed radargrams, and when
    radargrams/                   radargrams uploaded through the browser
    cache/                        derived data; safe to delete
```

Several of these only appear once they are needed. For example, a project
that has never had accounts has no `users.json` or `session.key`.

## What to keep

`interpretations/`, `layers/` and `derived/` are **authored**: people made
them, and nothing can recreate them. Back them up, and keep them in version
control if the project is in a git repository. The `.gitignore` that
`ridal project init` writes already leaves them in.

`cache/` is **derived**. Deleting it is always safe; Ridal rebuilds it. It
contains a `CACHEDIR.TAG`, so backup tools that honour the convention skip it.

`users.json` and `session.key` are **secrets**. Ridal creates both readable
by their owner only, and the `.gitignore` keeps them out of git. Deleting
`session.key` signs everyone out.

:::{note}
Projects made with Ridal 0.6 kept these entries directly beside `ridal.toml`.
Ridal refuses to open such a project rather than showing it as empty.
`ridal project migrate` moves it into `ridal_data/`.
:::

## `ridal.toml`

Every key except `format_version` is optional. Relative paths are relative to the directory that
holds `ridal.toml`.

```toml
[project]
name = "Drønbreen 2025"
format_version = 1

[radargrams]
roots = ["processed/", "/mnt/archive/2024/"]
max_bytes = 53687091200

[cache]
dir = "/var/cache/ridal/dronbreen"

[render]
default_profile = "default"
default_xscale = 2.0

[export]
default_spacing = "10"
default_format = "geojson"

[map]
default_basemap = "local-orthophoto"
built_in_basemap = true

[[basemaps]]
id = "local-orthophoto"
name = "Orthophoto"
url = "https://example.org/tiles/{z}/{x}/{y}.png"
attribution = "Example tile provider"

[[overlays]]
id = "stakes"
name = "Mass balance stakes"
url = "https://example.org/stakes.geojson"
name_field = "Stake"
```

### `[project]`

`name`
: A display name for the project.

`format_version`
: The layout the project is written in. Written by `ridal project init`;
  do not change it by hand. Ridal refuses to open a project with a version
  it does not know.

`data_dir`
: Where Ridal keeps its data. Default: `ridal_data`.

### `[radargrams]`

`roots`
: Directories searched for processed radargrams, in addition to the
  directory being served, which is always searched. An absolute path lets a
  project serve an archive it does not contain. Radargrams uploaded through
  the browser go into the first relative entry. `ridal project init` sets
  it to `["ridal_data/radargrams"]`.

`max_bytes`
: How large the project may grow before uploads are refused, in bytes.
  Default: 50 GiB.

### `[cache]`

`dir`
: Where derived data is cached. Useful when the project is on a network
  share and the cache should be on local disk. Default: `cache`, inside the
  data directory.

### `[render]`

`default_profile`
: The render profile used when a request does not name one.

`default_xscale`
: The horizontal stretch a radargram opens with. Default: `1.0`.

### `[export]`

`default_spacing`
: The point spacing export dialogs start with: `auto`, `per-trace`,
  `vertices`, or a distance in metres. Default: `auto`.

`default_format`
: The format export dialogs start with: `geojson` (WGS84), `geojson-native`
  (the radargram's own CRS) or `csv`. Default: `geojson`.

### `[map]`

`default_basemap`
: The `id` of the basemap that people who have not chosen one see. Default:
  the first one offered.

`built_in_basemap`
: Whether to offer Ridal's built-in ESRI World Imagery basemap. Default:
  `true`.

### `[[basemaps]]`

Extra tiled basemaps. Each entry is its own `[[basemaps]]` table.

`id`
: A stable identifier. Required.

`name`
: The name shown in the map's layer control. Required.

`url`
: An XYZ tile URL with `{z}`, `{x}` and `{y}` (or `{-y}`). Required.

`attribution`, `attribution_url`
: The credit line, and where it links to.

`tile_size`
: Tile size in pixels. Default: `256`.

`max_zoom`
: The deepest zoom level the provider serves. Default: `18`.

`zoom_offset`
: Added to the map's zoom level before requesting a tile, for providers
  whose tiling differs from the standard one. Default: `0`.

`subdomains`
: The letters `{s}` in the URL cycles through, such as `"abc"`.

### `[[overlays]]`

GeoJSON files that every map can draw on top of the basemap, switched off by
default. The browser fetches them directly.

`id`
: A stable identifier. Required.

`name`
: The name shown in the map's layer control. Required.

`url`
: Where the GeoJSON is fetched from. Required.

`name_field`
: The feature property used as a popup's heading.

`description_field`
: The feature property used as a popup's body. It may contain HTML.

`color`
: The colour features are drawn in, as a hex code.

## `layers.json`

The project's layers, edited on the **Layers** page of the GUI. See
{doc}`../guide/interpretation` for what the settings mean.

```json
{
  "schema": "ridal-layers",
  "schema_version": "1",
  "default_reducer": "shallowest",
  "layers": [
    {"id": "surface", "name": "Surface", "color": "#e6194b"},
    {"id": "bed", "name": "Glacier bed", "color": "#3cb44b", "groups": ["bed-kind"]},
    {"id": "bed_no_temperate", "name": "Bed, no temperate ice", "groups": ["bed-kind"]},
    {"id": "crevasse", "name": "Crevasse wall", "allow_overhangs": true}
  ],
  "groups": [
    {"id": "bed-kind", "name": "Bed", "members": ["bed", "bed_no_temperate"]}
  ]
}
```

Each layer has:

`id`
: A stable identifier, written into every pick that uses the layer.
  Required.

`name`
: The display name. Safe to change at any time. Required.

`color`, `description`
: How the layer is drawn and described in the GUI.

`allow_overhangs`
: Whether a line may double back, so that it has more than one depth at one
  position. Default: `false`.

`reducer`
: How several values from one person at one position become one:
  `shallowest`, `deepest`, `median` or `mean`. Default: the project's
  `default_reducer`, which itself defaults to `shallowest`.

`warn_on_duplicates`
: Whether to point out more than one value at one position. Default: `true`.

`groups`
: Exclusivity groups the layer belongs to. A layer is also in a group if
  the group lists it in `members`.

## Interpretations

Each interpretation is a [gprinterp](https://docs.rs/gprinterp) document: a
GeoJSON-like file whose coordinates are trace and sample numbers in the
radargram, with each line's layer in `properties.label`. There is one file
per person per radargram. Accounts can be deleted without losing them, since
interpretations are stored under the person's name, not their account.
