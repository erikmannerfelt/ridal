# Changelog

What changed in each release. {doc}`upgrading` says what to do about the
changes that affect existing data, scripts or servers.

## 0.8.0

### Python client

- **`ridal.client`**, an HTTP client for a Ridal server, `ridal gui` or a
  site (#328, #344), described in {doc}`../guide/python-client`. It reads
  the catalog, axes and picks, downloads radargrams and level 2 products,
  uploads and replaces radargrams, and saves and promotes picks. `plan()`
  sorts a directory of processed files into new, unchanged, safe, risky and
  legacy, asking the server what replacing each would do to its picks, and
  `apply()` uploads them. Install it with `pip install "ridal[client]"`;
  `ridal[geo]` adds `to_pandas()` and `to_geopandas()`.
- `plan()` and the new `download_radargrams()` work on several files at
  once (#357). `apply()` stays one file at a time.
- The Rust extension is now `ridal._ridal`, inside a pure-Python `ridal`
  package (#344). Every public name is still available as `ridal.<name>`.

### API tokens

- **API tokens** let a script act as an account without a password (#194,
  #342), described in {doc}`../deploy/accounts`. A token is limited to the
  projects it is granted, with a role and download ceiling in each, and
  never reaches a site's own routes. Create them on the site's Settings
  page (#354) or with `ridal site token`.

### Processing

- New step {step}`remove_standstills`, which finds where a time-triggered
  radar stood still from the radar data alone and replaces each stop with
  its median trace (#327, #351). It is not in the default profile.
- Traces past the last `.cor` fix (or the first) get times that advance at
  the trace interval, instead of all sharing the time of that fix (#350).
  Positions are still held. This applies to Malå, pulseEKKO and GSSI, and
  lets picks carry to the very end of such a profile.
- `ridal info` and `ridal.info()` read a processed `.nc` and report its
  identity and revision, or why a legacy file needs reprocessing (#11,
  #340).
- Merging refuses files with different antenna separation, effective
  antenna separation or medium velocity (#151). `batch-process --merge`
  starts a new group at such a file instead.

### Interpretation

- `only()` and `without()` choose contributors by username in derived
  expressions, as in `median(without(bed, "anna"))` (#326, #355). See
  {doc}`../reference/expressions`.
- A layer with a name of several words can be added: its id is now derived
  as an expression identifier, and a refused save keeps its error visible
  (#348, #349).
- The overlap explanation toast has a close button, and closes itself once
  the overlap is gone (#352).

### Server

- A replace **preflight**: what replacing a radargram would do to its
  picks, from the new file's axes alone, without uploading it (#331, #339).
- `GET /api/v1/openapi.json` publishes the schemas of the responses the
  client reads, including the picks document (#338, #339, #356). Health
  reports the server's version.
- `…/axes/gprinterp` returns the axes block a saved picks document needs,
  so picks saved by a script can be carried onto a later revision (#344).
- A level 2 export is bounded by its total point count, not just one grid
  (#135, #335), and group and catalog exports no longer block other
  requests while they run (#136, #336).
- `ridal gui --port` chooses the port (#343).
- `ridal server start` and `ridal gui` shut down cleanly on SIGTERM as well
  as Ctrl+C.
- An unreadable radargram file is reported as one, rather than as a
  radargram that declares no axes or has changed on disk (#325).
- External tools no longer inherit open NetCDF files, which made a file
  just written fail to reopen with `Netcdf(-101)` (#129, #324).

### Documentation

- Running a server as a systemd service, in {doc}`../deploy/server` (#304).

## 0.7.1

### Processing

- `zero_corr(coppens)`, the default method, picks a weak leading lobe of
  the direct wave consistently and no longer searches before the noise
  window (#310). On quiet records whose picks used to jump between the lobe
  and the wave, time zero moves by a few samples and `smooth` is no longer
  blocky.
- {step}`subset` needs only one argument, so `subset(max_sample=1000)`
  crops the height on its own (#298).

### Viewer

- A **Display** menu with contrast and brightness sliders, which holds the
  render profile and horizontal scale (#303). The sliders adjust only what
  the browser shows: no new render, and downloads are unaffected.
- **Smooth when zoomed in** can be switched off, which removes the seam
  that appeared at chunk edges at high zoom (#305, #313).
- **Open radargrams zoomed out**, a personal setting (#315).

### Catalog

- A project description: a short one on the site's project list and a
  long one, in Markdown, at the top of the catalog (#285).
- A list of groups at the top of the catalog to jump between them (#309).
- **Hide all** / **Show all** for a group's radargrams, with a personal and
  a project default (#314). Each group's controls sit beneath its name
  (#320).
- More info shows each radargram's track length (#319).
- The fixed "Radargram catalog" heading is gone (#318).

### GUI

- A **Documentation** link in the menu, and `?` links to the relevant page
  beside layers, exclusivity groups, derived items, the viewer and accounts
  (#293).
- "New derived item" replaces "New derived expression", with a hint that
  says what an item is (#294).
- Wide tables scroll sideways on a narrow screen instead of widening the
  page, and the derived item editor fits a phone (#295).

### Server

- Overviews are coloured before they are shrunk, so they look like the
  radargram does at full resolution (#300). `seismic` overviews were nearly
  white before, and `siglog-positive` ones too bright.
- Overviews are kept on disk under `ridal_data/cache/overviews/`, so a
  restart does not rebuild them (#180).
- Overview builds have their own limit, radargram files are opened when
  needed rather than all kept open, and freed memory is returned (#301,
  #306). A server with 120 radargrams went from 2.5 GB to under 100 MB at
  rest.
- Upload temporaries left by a crash are skipped by the catalog and
  removed at startup (#302).

### Documentation

- The 0.6.0 changelog covers what that release added (#311).

## 0.7.0

### Breaking changes

- **Sites** ({doc}`../deploy/sites`, #214, #286). `ridal server start` now
  serves a *site*: one server hosting many projects behind one list of
  accounts. It no longer accepts a project directory. Project accounts and
  `ridal project user` are gone; a project's `users.json` now holds only
  memberships. Every project URL moved under `/p/{key}/` and
  `/api/v1/projects/{key}/`, including under `ridal gui`, which serves its
  project as a site of one at `/p/default/`.
- **The default processing profile changed**, and so does the output of
  `ridal process --default` and `ridal.process(default=True)`. It is now
  `remove_empty_traces`, `zero_corr`, `correct_antenna_separation`, `dewow`,
  `background_removal` and `auto_gain`.
- **`zero_corr`** is one step with a `method` and a `scope` (#262). The
  default method is `coppens`; the pre-0.7 behaviour is `zero_corr(legacy)`.
  `zero_corr_max_peak` is retired in favour of
  `zero_corr(max_peak, trace, peak)`.
- **Travel time to depth** accounts for the antenna separation correctly
  (#263). `correct_antenna_separation` takes a `method` (`slant` by default,
  or `legacy`) and a `direct_velocity`.
- **`dewow`** is a per-trace running median or mean (#265). The old `dewow`
  was a coarse background removal. `normalize_horizontal_magnitudes` is
  retired in favour of `dewow`, and `background_removal` is new.
- **`bandpass`** is zero-phase, run forward and backward (#267). Each cutoff
  is now at −6 dB rather than −3 dB.
- **`auto_gain`** measures the gain below the direct wave's ring-down (#268).
- **Python 3.10 wheels are no longer built**; wheels are built for Python
  3.11 to 3.14 (#201).

### Processing

- All processing steps are defined in one typed registry (#85, #260).
  Arguments are checked before processing starts, and a retired step name
  says what replaces it. {doc}`../reference/steps` is generated from it.
- New steps: {step}`multiply` (#222), {step}`background_removal` (#265),
  {step}`adaptive_siglog` (#255), and {step}`remove_tones` and
  {step}`balance_traces` for interference in 800 MHz Malå ProEx data (#287).
- `zero_corr` falls back to `aic` when a record has no noise before the
  direct wave, instead of failing (#269).

### Rendering

- Render profiles can use colormaps, including `seismic` and
  `siglog-seismic` (#251), and `siglog` source preprocessing (#223).
- `siglog` adapts to each radargram's noise floor (#255).
- Python can render the topographically corrected view:
  `ridal.render(topo=...)`, and `render_topo=` on `ridal.process` and
  `ridal.batch_process` (#289).

### GUI and interpretation

- **Derived layers and attributes** (#205–#210, #216): items computed from
  everyone's picks, such as `median(bed)`, written in a small
  {doc}`expression language <../reference/expressions>`. Each derived item is
  evaluated on its own, so a broken one fails alone (#278), and whole
  numbers are accepted in expressions (#270, #277). Derived points have a new
  format (#219), and derived attributes appear in the cursor readout (#281).
- **Exclusivity groups** (#252, #253): layers that may not overlap, enforced
  when picking and saving.
- A trace view (#221), a button to hide the radargram (#233), and a reworked
  picking and violation feedback (#249).
- Contributor counts in the catalog and the layer panel (#279), and groups
  listed alphabetically by name (#284).
- The GUI asks before splitting or joining a line (#235), checks overlap
  from the first vertex of a new line (#290), lets a derived layer's colour
  be changed (#234), and offers range-fill targets again (#243).
- The image download dialog defaults to a width the server accepts (#137).

### Server

- Quick creation of many accounts at once, with invite links or generated
  passwords (#224), now `ridal site account add-bulk`.
- `--cache-memory-mb` is one budget for the whole server rather than per
  radargram (#288).
- Errors caused by something outside Ridal say who can fix them (#250).
- Site and project histories are JSON Lines files (`audit.jsonl`).

### Documentation

- This site (#275).

## 0.6.1

- `ridal.process` and `ridal.batch_process` accept radargram and group
  identity arguments (#218).

## 0.6.0

The first release with the browser GUI, interpretation, accounts and
`ridal render`.

### Browser GUI

- A web server and browser GUI for processed radargrams (#125).
  {doc}`../guide/gui` opens the catalog, grouped by survey with a track map,
  and the viewer with its cursor readout and render-profile menu.
- Catalog metadata can be overridden without reprocessing — display name,
  group and **Unlisted** (#155) — and external radargram roots can be served
  read-only with the project on top (#158).
- Radargrams can be added and removed from the browser (#161), replaced with
  a new revision (#165), and picks drawn on an earlier revision are shown on
  the current one (#163, #164).
- A topographically corrected view, in the viewer and as a download (#168,
  #169).
- More basemaps than ESRI World Imagery, and a basic overlay manager (#177,
  #185).
- Display settings: theme, pick visibility, download defaults and the
  project's default horizontal scale (#141, #143, #166, #176).

### Interpretation

- Pick layers in the browser and export a level 2 point product (#132), with
  the layer vocabulary defined in the project. See
  {doc}`../guide/interpretation`.
- `ridal project init`, `info` and `migrate`, with `ridal.toml` at the root
  and everything Ridal owns in `ridal_data/` (#187, #188), described in
  {doc}`../reference/project-files`.
- Time zero, effective antenna separation and the `twtt` anchor are recorded
  in the file and carried into exports (#149, #157).
- Files from older Ridal versions are recognised and refused with a clear
  reprocess message instead of being silently ignored (#189).

### Accounts

- A multi-user server: authentication, per-user interpretations and
  project-wide permissions (#139), described in {doc}`../deploy/accounts`.

### CLI

- `ridal render` renders a processed radargram to an image from the command
  line, through the same pipeline as the GUI (#138), described in
  {doc}`../guide/rendering`.

### Fixes

- `GPRLocation::distances` and `velocities` gained the missing square root
  (#127).
- HDF5 no longer locks a file against the process that just wrote it (#154),
  and the macOS build works against HDF5 2.x (#128).
- The catalog and its render services sit behind one lock (#150).
