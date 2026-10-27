# Changelog

What changed in each release. {doc}`upgrading` says what to do about the
changes that affect existing data, scripts or servers.

## 0.11.0

### Positions and coordinates

- **Coordinates are placed correctly** (#480, #481). Converting UTM back
  to WGS84 was up to 4.6 m off in latitude (4.4 m on Svalbard), so every
  processed file's `latitude`/`longitude`, and every elevation sampled
  from a DEM, was for a point that far north of the trace. Both directions
  now use Krüger's series, as PROJ does, and agree with `cs2cs` to 1 µm.
- **A coordinate CSV** (#13, #479): `--cor` (`cor=` in Python) reads a
  Ridal coordinate CSV (`trace`, `time`, `lon`, `lat`, optionally `alt`)
  for any format, and a `<stem>.coords.csv` beside a recording is used
  before its own positions file. `--cor DIR` finds one per recording. A
  recording without positions gets an error saying what to do.
- **`ridal coords`** (#482) writes that CSV from partial positions: a few
  points, `--start`/`--end`, a GNSS `--track` (with `--time-offset`), or
  the recording's own file, with `--dem` for elevations. Synthetic times
  are marked (`time_source`), and `remove_standstills` does not use the
  GPS for them. In Python it is `ridal.coords()` (#483), and a
  `.coords.csv` uploaded in the GUI belongs to its recording (#484).
- **DEMs with GDAL before 3.10** are sampled at the nearest cell instead
  of failing with a broken pipe (#461). The revision records it, the
  Processing page lists those radargrams as older processing, and the
  server warns its admin at start and in Site settings (#462, #463).
  `cs2cs` no longer deadlocks on about 10 000 points.

### Processing

- **GSSI travel times and depths are correct** (#506). The time window is
  the header's `range`, not `range - position`, so depths were off by
  `range / (range - position)`. A cut-short last trace is dropped instead
  of failing the file, and multi-channel DZT files are refused.
- Recordings with different sample counts are no longer merged, and a
  merge refuses instead of panicking (#507).
- **`equidistant_traces`** resamples unless every step already matches
  (it skipped uneven tracks whose trace count happened to match), and no
  longer extrapolates a trace past the end of the track (#511).
- **`abslog`** takes its offset from the whole array instead of about ten
  values, and errors instead of panicking on all-NaN data (#512).
- **`correct_topography` must be the last step**, and run once; other
  step lists are refused (#513).
- **Small radargrams** (#7, #502): a step that leaves no traces or no
  samples fails, `subset` refuses an empty range, and steps that need a
  minimum size warn and leave the data unchanged instead of panicking.
  `equidistant_traces` makes at most 4× the traces it was given.
- **Malformed files** error instead of panicking in the Malå, GSSI and
  pulseEKKO readers (#489).
- `correct_antenna_separation(method=legacy)` is documented as keeping
  the recorded sample interval, about 2 % off its real grid (#508).

### Picks

- **Picks sit at pixel centres**, as gprinterp's specification defines
  (#509, #520). Stored picks are not migrated, so picks made before this
  appear half a pixel up and to the left; they export the same depths.
  Figure ticks and picks move half a pixel to the true centres.
- **Picks stay on their horizon when time zero moves** (#470, #471, with
  gprinterp 0.5.0): they are carried by the recording clock where both
  revisions have one, and the carry report says how far time zero moved.
- **Level 2 export is much faster**, and the single-radargram route no
  longer blocks a server worker (#510).

### Render profiles and figures

- **Project render profiles** (#183, #476): an operator keeps profiles
  beside the built-ins, offered everywhere a profile is chosen, through
  `/api/v1/render-profiles` or the Python client. `[render]
  hidden_profiles` hides built-ins from the lists.
- **Personal render profiles** (#477): anyone signed in keeps up to 20 of
  their own (`personal-…`), offered only to them; `[render]
  personal_profiles = false` turns them off for a project.
- **A profile editor** in the viewer (Display → Edit profile…) with a live
  preview (#478). "My render profiles" and an operator's "Render profiles"
  section are in the settings.
- **Figures** get a `halo` style, so a transparent figure reads on light
  and dark pages (#495), and `x_axis.reverse`, `zero_at` and `descending`
  to mirror a figure and count its labels down (#496, #503).
- **Slow renders stop** (#473, #475): an overview or figure that runs for
  more than 2 minutes answers `503 render_timed_out`, a figure stops when
  its download is cancelled, figures no longer hold up the viewer's
  tiles, and running renders are listed under "Running" in the site
  settings and in the heartbeat. The topographic view of a long,
  steep profile renders in seconds instead of timing out (#472, #474).
- The Kroppbreen example (`scripts/render_kroppbreen.py`) downloads its
  data from Zenodo and draws the figure on the front page with the
  built-in renderer (#493, #504).

### Python

- **`ridal.write_netcdf`** writes a radargram processed elsewhere as a
  Ridal file, to pick in the GUI, and `ridal.impdar.from_radardata` maps
  an ImpDAR `RadarData` onto it (#468, #469).
- The Python client is checked against every operation of the HTTP API
  (#454, #455).

### Server and GUI

- **Large uploads resume** (#396, #456, #457). Raw files, DEMs and
  radargrams too large for one request, or behind a proxy that limits a
  request, are sent in chunks through `/api/v1/uploads` that survive a
  dropped connection, a retry or a reload of the page.
- **Download raw data and DEMs** from the browser: a radargram's raw data
  as one zip, and a project's DEMs under their uploaded names (#460,
  #464).
- **The HTTP reference is generated** from an OpenAPI description of every
  route, also served at `/api/v1/openapi.json`, with each route's
  permission tested (#330, #449–#453).
- **Site settings** are linked from a project's menu, and **Maps in the
  catalog** can hide only groups where everything is unlisted
  (`show_group_maps = "listed"`) (#446, #447, #459).
- An unknown `?profile=` on a page falls back to the next choice instead
  of returning 400. Dialogs whose Close discarded edits now say Cancel.
- The processing page fits a narrow screen (#465, #466).

### Security

- **Cross-site writes are refused** (#514): a POST, PUT, PATCH or DELETE
  that a browser marks as cross-site or same-site gets 403
  `cross_site_request`, and `ridal gui` answers only to localhost,
  127.0.0.1 and \[::1\] (421 `unexpected_host`).
  `--allow-cross-site-requests` on `gui` and `server start` turns both
  off.
- **Sign-ins are throttled** (#515): failed sign-ins to one name double
  the wait before the next, from 1 s to 5 min (429 `too_many_attempts`
  with `Retry-After`). The reverse-proxy guide has an nginx `limit_req`
  example for limits per address.
- **Security headers and a Content Security Policy** (#516, #517): every
  response forbids framing by other sites and sends `Referrer-Policy:
  same-origin` and `nosniff`; pages only run Ridal's own scripts, and a
  project's basemap and overlay hosts are allowed automatically.
- **Uploaded DEMs are opened only as GeoTIFF** (#487), so a VRT cannot
  make the server read local files or fetch URLs. On the command line,
  `RIDAL_DEM_GDAL_DRIVERS` widens the list.
- Headings in a project description no longer take attributes, which
  allowed script in them (#486).
- CI fails on any RustSec advisory against a dependency, and the existing
  ones are cleared (#488, #523).

## 0.10.0

### Annotated figures

- **Figures with axes** (#142). A figure spec, a small JSON document,
  describes an excerpt of a radargram with axes, ticks and labels, and
  renders it as SVG or PNG: `ridal render FILE --figure spec.json`, or
  `ridal.render(..., figure=...)` in Python (#425). It sets the view,
  extent, horizontal scale, physical size and DPI, the units, spacing and
  anchor of each axis, and the style. Ticks sit at their coordinate's true
  position, so distance ticks are uneven where the speed varied. With
  `axes: false` it writes the bare raster and a `.json` sidecar for
  plotting elsewhere. See {doc}`../guide/rendering`.
- **Picks and derived layers in figures** (#427). With `--project DIR` (or
  `project=`), a spec draws picks by user and layer and derived layers by
  id, with range fills, carried to the revision drawn and moved with the
  topographic view, as the viewer draws them.
- **Figures from a server** (#428): `POST /api/v1/datasets/{id}/figure`,
  byte-identical to a local render of the same file, and
  `Client.figure()` in the Python client. Picks that could not be drawn as
  stored are reported as `FigureWarning`s.
- **Download → Figure…** in the viewer (#429) opens a dialog beside a live
  preview: the current view, the whole radargram or a dragged rectangle,
  every option of the spec, and the picks and derived layers to draw. It
  downloads PNG, SVG or the spec, which reproduces the SVG through
  `ridal render`. Distances can be in kilometres (`distance_km`).

### Picks

- **A pick manager** on the Layers page (#178, #433) lists every stored
  interpretation as statistics: lines per layer, the share of traces
  covered, and whether it is current, on an older revision, ignored,
  without a radargram, or archived. An admin can **archive** someone
  else's picks, **purge** archived ones (#162) and **restore** them
  (#435, #443), and anyone with the `picks` download scope can download
  the rows shown as one zip (#443). In the Python client it is
  `interpretation_summaries()`.
- **Contributors' picks are carried** to the revision being viewed, as a
  person's own are (#434, #440). Picks that cannot be carried are drawn
  dashed where they were stored, and the layer panel says whose were
  refused, cut or moved. The same holds for a person's own picks in a
  read-only viewer.
- **A line past the edge of a radargram is cut there** when carried,
  rather than dropped whole (#440, with gprinterp 0.4.0). A cut of more
  than half a trace makes the carry `partial`, and `CarryReport` gains
  `clipped`.
- **Derived layers are evaluated over carried picks** (#440), so a
  consensus is computed over the lines the viewer draws. Picks that cannot
  be carried still count as stored, and the editor, figures and downloads
  say which they are.
- Picks without anchors still carry after their file is replaced on disk:
  a revision's axes are kept as soon as it is discovered (#440), and kept
  axes nothing needs any more are swept 30 days after their revision
  ended. A file replaced on disk now supersedes the revision before it.

### Viewer and catalog

- **The catalog loads much faster** (#444). Tracks are read once and
  cached under `ridal_data/cache/tracks/`, group maps send tracks at map
  resolution and are only created when scrolled near, pages and scripts
  are gzipped, and scripts and styles are revalidated with an ETag rather
  than downloaded again. On a 714-radargram project, opening the catalog
  went from 21 track requests and 2.1 s of track reads to one request and
  a 60 KB page, and a server with a warm cache starts in half the time.
- **Hide map / Show map** on each group, with **Maps in the catalog** in
  My settings and Project settings (`[catalog] show_group_maps`) (#444).
- **An Edit menu in the viewer** (#431, #438), with the catalog card's
  Edit properties, Replace with a new version… and Process…/Attach raw
  data….
- **Help where steps are written** (#423, #438): every steps box colours
  the steps, shows the signature and arguments of the step at the caret
  with a link to its documentation, and marks unknown steps with
  suggestions, and steps that may not run before a profile's.
- The menu reads **Radargram catalog**, **Layers**, **Processing**,
  **Project settings** and **All projects** in a project, and **Projects**
  and **Site settings** on site pages (#426, #438).
- Saving a changed render profile in the settings no longer leaves the
  menu leading back to the old one through `?profile=` (#432, #438).
- The viewer's profile, scale and topographic correction controls show
  what the server rendered after a reload, not the browser's last values
  (#438).
- **Hide radargram** is now a **Show radargram** checkbox in the Display
  menu (#439, #442).

### Processing

- **`unphase` is retired** (#437, #442). It shifted the data by an
  arbitrary amount: its peak searches ranked samples by magnitude and
  picked the wrong one. Naming it now fails with a message pointing at the
  `abslog` render profile; files already processed with it still open.
- The help of `correct_topography` and `--default-with-topo` now says the
  step is rarely needed: the viewer, `ridal render --topo` and figures draw
  the corrected view from `data`, and the stored copy more than doubles
  the file (#436, #442).
- `ridal.process()` accepts `render=True` and `track=True`, deriving the
  same paths as `ridal process -r` and `--track` without a value (#93,
  #442).

### Documentation

- Every page that was a placeholder is written (#441): a quickstart, the
  data model, GPR in brief, guides to Python, processing, the browser GUI,
  interpretation, rendering, coordinates and file formats (with the full
  NetCDF schema), three worked examples, and the FAQ, citing and
  contributing pages.

## 0.9.0

### Processing on the server

- **A server processes raw data** (#382). A project keeps the raw
  recordings each radargram is made from, and an operator processes them on
  the server, looks at the result, and makes it the radargram. Each job runs
  as a process of its own behind one site-wide queue, bounded by
  `--processing-jobs`, `--processing-memory-mb`, `--processing-threads`,
  `--processing-timeout`, `--max-queued-per-user` and
  `--max-queued-per-project` (#387, #388, #392, #402). See
  {doc}`../guide/gui` and {doc}`../guide/python-client`.
- **Process… in the viewer** reprocesses a radargram from its raw data as a
  draft, step by step, with its overview beside the one that is served, and
  applies it with the same report of what happens to picks as a replace
  (#392). **Attach raw data…** gives a served radargram its raw data (#394).
- **Adding raw data.** The catalog's **Add data** menu holds "Add processed
  radargram…", "New radargram from raw data…" for one radargram (#394), and
  "Add raw data in bulk…" (#408, #413). The bulk page groups the files into
  recordings, plans them as radargrams on a timeline where gaps can be split
  and joined, and names them with id and display-name patterns before
  anything is processed (#406). Raw data that is not processed yet is listed
  on the Processing page, where it can be processed or deleted (#407, #413).
- **Processing profiles**: named, versioned processing that many radargrams
  share (#389). The **Processing page** and a badge on each catalog card say
  which radargrams are current, stale (and why), on custom steps, or not
  processed (#393, #399). **Process N out-of-date…**, for a profile or a
  group, applies the profile as a batch (#395, #399); a batch report says
  whose picks did not carry, and whoever started it is told when it ends
  (#401). Applying a draft whose steps differ from its profile offers to make
  them the profile's next version (#400).
- **DEMs** are project resources, uploaded once and linked per radargram
  (#398). A project may set the CRS its radargrams are processed in
  (`[processing] crs`, #404).
- Antenna frequency and separation overrides belong to a radargram's raw
  data, so a batch keeps them, and they can be set in the browser, per
  antenna in a bulk upload (#409, #415).
- Who is processing a radargram is shown in the viewer, the Process dialog
  and the catalog (#403).
- Drafts are kept for `--draft-ttl` after they were last looked at, within
  `--draft-cache-mb` per project (#397).
- The Python client covers all of it: raw data, sources, DEMs, drafts,
  profiles, batches, ingest plans, and `process()` for one radargram from
  its files (#405, #406).

### Processing

- A processed file records what it was made from (`ridal_recipe`): the
  hashes of its raw files, its steps with their versions, CRS, DEM,
  velocity and overrides. Its revision id is derived from that, so
  processing the same data the same way again gives the same revision
  (#383). Files from earlier versions keep their ids.
- `remove_standstills(gps_uncertainty=...)` uses the GPS to rule out
  standstills the radar data alone would find, and to add the ones it misses
  (#410). Without the argument the step is unchanged.
- `correct_antenna_separation(slant)` resamples onto exactly the grid the
  file's coordinates describe; depths were 0.32 % short before (#380).
- Picks are carried correctly between a revision corrected for antenna
  separation and one that is not, instead of shifting by a few samples
  (#370, #381).
- `batch-process` names its outputs with `--id-pattern` and
  `--display-name-pattern` (#368, #385), and `--dry-run` shows what it
  would do (#386). A bare `--render` or `--track` writes into the output
  directory instead of being ignored, and `--no-export` no longer needs
  `--output` (#384).
- The `.nc` records the antenna frequency it was processed as, and the
  viewer shows it with the trace spacing and survey speed (medians over the
  track) (#412, #416).

### Viewer and catalog

- A radargram can have its own horizontal scale, set in its properties,
  which comes before a person's own and the project's (#411, #417).
- Tapping a vertex offers **Split here** and **Remove vertex**, and deleting
  a line asks first (#371, #375, #378).
- A clicked sibling track on the viewer's map turns orange (#366). Groups
  stay shown or hidden across the catalog's own reloads (#367). A catalog
  with no groups loses its redundant heading and menu (#369, #373, #374,
  #377).
- "Take out" and other deletes no longer report "response was not valid
  JSON" (#409, #413).

### Server

- **Logging.** `ridal gui` and `ridal server start` write a log to stderr,
  filtered by `RUST_LOG`: warnings that were dropped before, one line per
  request naming its route but not the caller, processing jobs, and a
  heartbeat every `--heartbeat-seconds` (#282, #418). See
  {doc}`../deploy/server`.
- Site settings has a **Server** section for server administrators: the
  heartbeat's numbers, and the processing queue with a Cancel button for
  any job (#391, #419).
- Uploads no longer hold up the rest of a project while their body
  arrives: raw data (#396, #413), radargrams and replacements (#414, #420).
- GeoJSON exported in a projected CRS declares it, so GDAL, QGIS and
  GeoPandas place it correctly (#372, #376).

## 0.8.1

### Python client

- `apply()` stops at the first sign that the server is down, rather than
  trying every remaining file against it (#361, #362). The file it was on is
  `failed`, and every later file that would have been uploaded is
  `not_attempted`, a new action. Planning again picks up where it stopped.
  `apply(..., stop_on_outage=False)` keeps trying every file, as before.
- `502` and `504` raise `Unavailable`, as `503` does, so a proxy in front of
  a server that is down counts as an outage (#362).

### Viewer

- Tapping the selected line anywhere offers to split it there, at the
  tapped trace (#360, #364). It used to deselect the line; tapping off it
  still does. Two-vertex lines can now be split too.
- A **Show tooltips on vertices** checkbox under "Editing help" hides the
  "Drag to move" and "Add vertex here" tooltips, and is remembered per
  person (#363, #364). My settings has the same checkbox.

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
