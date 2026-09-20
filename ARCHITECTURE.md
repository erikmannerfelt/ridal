# Ridal web server architecture

> **Scope:** this document describes only the optional `server` cargo
> feature — the web server and browser GUI under `src/server/` (`ridal
> gui` / `ridal server start`). It is not an architecture overview of
> Ridal as a whole, which is primarily a GPR processing CLI/library; see
> [`README.md`](README.md) for that. A CLI-only build (`cargo build
> --no-default-features -F cli`) never links Axum, MiniJinja, or blake3,
> and none of this applies to it.

This is the narrative companion to the module docs in `src/server/` —
run `cargo doc --no-deps -F cli,server --open` for the in-code map, with
intra-doc links between every type mentioned below. This document is
where the *reasoning* lives; the module docs are where the *contract*
for each piece lives, kept next to the code it describes so it can't
drift as easily as prose.

## What this is

A read-only viewer for radargrams Ridal has already processed into its
NetCDF output format: an index page listing every discovered radargram
(grouped, with a map of each group's track), and a chunked pan/zoom
viewer per radargram, similar in spirit to a tile-based map viewer but
serving amplitude renders instead of map tiles. Two launch modes share
one implementation:

```console
ridal gui <FILE-OR-DIRECTORY>          # local, ephemeral port, opens a browser
ridal server start <FILE-OR-DIRECTORY> # persistent, 127.0.0.1:8000 by default
```

Both accept a single processed `.nc` file or a directory to scan
recursively.

## Request lifecycle

1. **Startup** (`src/server/launch.rs`) discovers every radargram under
   the given root *eagerly*, not lazily per request — the target catalog
   size is on the order of 100 files, and eager construction turns a
   broken file into a clear startup warning instead of a request-time
   surprise. This builds one shared `AppState` (`src/server/app.rs`).
2. **Discovery** (`src/server/catalog.rs`) walks the directory (or
   accepts a single file), recognising processed output via
   `io::inspect_ridal_netcdf` — a metadata-only recogniser that lives
   *outside* `src/server/` and outside the `server` feature entirely, so
   CLI-only code (`process`, `batch`) can use it too. Duplicate
   radargram IDs resolve deterministically (newest `processing_datetime`
   wins, ties broken by path order), always with a warning, never a
   silent drop.
3. **Routing** (`src/server/routes.rs`) handles every HTTP request. It
   composes the catalog and render-service layers rather than containing
   new logic itself — no NetCDF, catalog, or rendering code belongs
   here. Page HTML comes from `src/server/templates.rs` (MiniJinja,
   embedded via `include_str!`); CSS/JS/images come from
   `src/server/assets.rs` (embedded via `include_bytes!`). A deployed
   binary needs no frontend directory alongside it — everything is
   compiled in.
4. **Rendering** a chunk or overview acquires a permit from
   `AppState::render_permits` (bounded by `--n-workers`), then runs on a
   `spawn_blocking` thread against the matching radargram's
   `RenderService`. See [Render pipeline](#render-pipeline) below.
5. **Tracks** (`src/server/track.rs`) are extracted and simplified for
   both the index page's per-group map and the viewer's cursor readout.

The one rule every submodule below `app`/`routes`/`launch`/`templates`
follows: **no dependency on Axum, MiniJinja, or other HTTP/template
types.** The render pipeline in particular is plain Rust, unit-tested
against synthetic arrays with no HTTP server anywhere near the tests.

## Identity model

Four concepts that are easy to conflate and load-bearing to keep apart
(`src/identity.rs`, `src/server/catalog.rs`):

| Concept | Type | Stable across reprocessing? |
|---|---|---|
| Radargram identity | `RadargramId` | yes |
| Display label | `DisplayName` | n/a — cosmetic only |
| Group name / id | `GroupName` / `GroupId` | n/a — cosmetic only |
| Processed revision | `RevisionId` | **no** — changes every run |

`RadargramId` and `GroupId` are validated ASCII slugs (`[a-z0-9_-]`, ≤128
characters, no reserved names, never starting with `_` or `-`).
`RevisionId` is a blake3 fingerprint of `(radargram_id,
processing_datetime)`, deliberately excluding path, filesystem
timestamps, filesize, and display name — moving or renaming a processed
file does not invalidate its cache; reprocessing does.

`RadargramId` and the recognition logic live outside the `server`
feature specifically so the CLI's `process`/`batch` commands can use the
same identity types (`--radargram-id`, `--display-name`, `--group-name`,
`--group-id`) without linking anything server-only.

A radargram with no group gets its own "Ungrouped" section on the index
page — same map, same card grid, not a lesser presentation — via a
reserved id (`_none`, `NO_GROUP_ID` in `app.rs`) that can never collide
with a real slug, since real slugs can't start with `_`. The same
underscore-prefix convention is used for the metadata dialog's synthetic
entries (`__revision_id`, `__shape`, `__start_stop_datetime`). This is
an implicit convention rather than a typed one — if slug validation
rules ever change, grep for these sentinels first.

## Render pipeline

Fixed order, enforced by module structure rather than just convention:
**source amplitude → dataset view → resample → normalize → colormap →
encode.** Everything lives under `src/server/render/`.

- **Geometry** (`grid.rs`) is pure, with no I/O: `ViewerRaster` is the
  source array at 1:1 — **the viewer does not resample** — `ChunkGrid`
  divides it into addressable 256×256 chunks, and `OverviewSpec`
  describes a whole-radargram thumbnail (~512 px wide), which does
  downscale.

  The viewer raster used to be capped at 8192×4096, with anything
  larger scaled down to fit. That silently cost a 12187-trace radargram
  a third of its trace resolution *and* a third of its sample
  resolution, since one scale factor was applied to both axes. The cap
  was bounding client cost — every chunk is a decoded 256×256 bitmap in
  the browser — which is now handled by loading chunks only as the view
  reaches them (see Frontend). Removing it took the server from 320 to
  720 chunks on that file but cold render time only from 34.3 s to
  37.9 s: total work is dominated by reading the source array either
  way, and 1:1 chunks each read exactly one storage chunk instead of a
  window spanning several.
- **Reading** (`renderer.rs`, via `source.rs`) never materializes a
  full-resolution image. A chunk reads exactly its source window; an
  overview reads the source in bounded-size horizontal bands (a 64 MB
  budget by default), since an overview's *input* is the entire
  radargram regardless of how small the output is — banding this
  dropped peak memory on the largest file in the test corpus from
  268 MB to 159 MB with render time unchanged.

  A band's read is wider than the band itself, and `overview_rows_per_band`
  has to reserve for **both** of the things that widen it or the budget is
  a number rather than a bound: `resample::halo`, which is what a Lanczos
  kernel reaches beyond the band on each side (`3 × scale`, so ~144 rows at
  a typical 24× overview), and `AmplitudeSource::vertical_read_overhead`,
  which is the shear span for a topographically corrected source and zero
  for every other. Reserving only the second let the two Lanczos profiles
  read past the budget.
- **Resampling** (`resample.rs`) offers four methods, each required to
  degrade gracefully to the exact raw sample at a true 1:1 footprint —
  the same behaviour a naive box filter has, and a real bug (see below)
  when a method fails to have it:
  - `Mean` — area-weighted, NaN-aware, weight-renormalizing. The
    default. Nearest-neighbour is deliberately excluded; it preserves
    this data's high-frequency noise badly.
  - `Peak` — largest value in the footprint, unweighted. Exists because
    radar traces oscillate around zero, so averaging a downsampled
    footprint cancels signed amplitude toward zero.
  - `Lanczos` — windowed-sinc, applied as two separable 1-D passes (a
    2-D kernel would be tens of thousands of taps per output pixel at a
    typical overview downsample ratio). Still a *linear* filter, so it
    has the same cancellation problem as `Mean` on signed oscillating
    data.
  - `LanczosRectified` — Lanczos on `|amplitude|`. Removes the
    cancellation while keeping proper anti-aliasing, unlike `Peak`'s
    upward bias. **Rectification is gated per axis** on that axis
    actually downsampling (`step > 1.0`); an earlier version rectified
    unconditionally, which silently defeated any sign-dependent
    profile's stretch even at native resolution, since every value
    reaching the colormap was already non-negative regardless of the
    source's true sign. Any future nonlinear or order-sensitive
    resampling method must have this same graceful-degradation property
    or it will reproduce that bug.
- **Profiles** (`profile.rs`) are the one server-defined, non-free-form
  configuration surface — never client-defined, per the explicit
  warning against unbounded client-driven render work. Four built-ins:

  | Profile | Transform | Resampling | Notes |
  |---|---|---|---|
  | `default` | Linear | Mean | 1–99% quantile |
  | `positive` | Positive (asymmetric) | LanczosRectified | biases toward positive returns, clips negative toward black |
  | `abslog` | `log10\|A\|` | LanczosRectified | sign-agnostic by construction, so rectifying changes nothing about what it means |
  | `high-contrast` | Linear | Mean | 5–95% quantile |

  `positive`'s asymmetry is why `RenderProfile`'s amplitude transform is
  an enum rather than a boolean: it needs a different domain for
  *statistics* (percentile bounds from `|x|`) than for *display* (the
  signed value, so negatives can clip). `colormap.rs`'s
  `to_stats_domain`/`to_display_domain` keep that split explicit.
- **Statistics** (`stats.rs`) estimates percentile bounds **once per
  revision+profile**, never per chunk — per-chunk estimation would let
  adjacent chunks normalize differently and turn every chunk boundary
  into a visible seam. Sampling reads 128 runs of 16 contiguous traces
  each (a fixed seed derived from the revision ID, so limits are
  reproducible across restarts): whole, contiguous traces rather than
  scattered pixels, both because a trace carries the full vertical
  structure needed to represent the source wavelet's contribution in
  correct proportion, and because contiguous reads stay inside a
  handful of HDF5 storage chunks — measured at ~7.6× faster than
  evenly-strided single-trace reads.
- **`RenderService`** (`service.rs`) is the entry point everything above
  is reached through: resolve amplitude limits (cached separately from
  images) → check the in-memory cache → render on a miss → insert →
  return. Cache keys fold in `RevisionId` plus every profile field that
  affects pixels, so a reprocessed file or a changed profile can never
  return a stale image.

## Concurrency and resource bounds

- Rendering runs on `spawn_blocking` threads, not tokio's async workers,
  under a semaphore permit sized by `--n-workers`. The permit is
  acquired *before* spawning, so a client that disconnects while queued
  never starts a render at all — one already in flight cannot be
  cancelled, since `spawn_blocking` tasks aren't cancellable, but it
  completes into the cache rather than being wasted work.
- Permit acquisition times out (30 s) into a `503` with a `Retry-After`
  header rather than queueing without limit.
- Each radargram's `RenderService` sits behind a `Mutex` held for the
  whole read-and-render, so two chunks of the *same* radargram never
  render concurrently — deliberately not split further, since the
  `netcdf` crate serialises its C calls behind its own global lock
  regardless, so splitting would only buy concurrency *across*
  radargrams, not within one. That same `Mutex` plus the cache re-check
  on entry is also what makes concurrent requests for one chunk
  generate it only once — a queued request finds the result already
  cached rather than re-rendering, which is pinned by a test rather
  than implemented as separate single-flight machinery.
- **netcdf-c/HDF5 is not thread-safe.** A global lock serialises every
  netcdf call, which is a hard ceiling on read parallelism no amount of
  tokio tuning lifts. Tests use `#[serial_test::serial(netcdf)]` plus
  retry; a residual ~6% `Netcdf(-101)` flake is a known, documented
  symptom of this.

## What was measured, and what it settled

`scripts/bench_server.py` benchmarks the server over real HTTP against a
real catalog — cold vs. warm separately, since they differ by orders of
magnitude. On a release build, the largest radargram in the test
catalog (12187×3678):

- Chunks (the viewer's actual unit of work) cost **2–6 ms cold, 0.2 ms
  warm.** Pan and zoom are already far faster than perceptible.
- The one expensive request is a **cold overview at ~1.8 s** — this is
  index first-paint cost, not a viewer responsiveness problem.
- The warm/cold ratio is **~9000×**, which is why persisting renders
  across restarts (an on-disk cache) matters more than any further
  rendering optimisation.

Measured again when the viewer cap was removed, on the same file: a
cold overview costs 13.9 s, the full 720-chunk grid 37.7 s, and the
360 chunks an opening viewport actually requests 24.0 s (debug build,
ratios only). Per-chunk marginal cost is ~38 ms against a ~10 s fixed
cost for opening and estimating amplitude limits, so viewport culling
saves 36% here — and proportionally far more the longer the radargram,
which is what makes an uncapped raster affordable.

**This settles that multiresolution server-side `(z, x, y)` tiling is
not currently justified and should not be started speculatively.** The
viewer's horizontal-scale control is a pure client-side transform for
exactly this reason — it re-lays existing overlays rather than asking
the server for a different render, which is cheap and, per the above,
sufficient. If real usage ever shows otherwise, tiling would replace
that control rather than extend it, and should be justified by new
measurements, not assumed from first principles.

## Frontend

No build step: Leaflet is vendored (not loaded from a CDN, so the
viewer works offline — a realistic deployment for field use) via
`scripts/vendor_leaflet.sh`, and all first-party CSS/JS is embedded the
same way as the templates. **First-party assets must not live under
`assets/vendor/`** — the vendor script does `rm -rf` on that directory,
so anything placed there is silently deleted on the next Leaflet
refresh.

Page-specific JavaScript lives in `assets/index.js` / `assets/viewer.js`
(shared helpers in `assets/app.js`), not inline in the templates. Fetch
calls go through `RIDAL.fetchJson`, which surfaces the server's
structured error envelope (`{"error": {"code", "message"}}`) instead of
a bare `.then(r => r.json())` silently proceeding with a malformed
object on failure.

## Known constraints and gaps

Durable, load-bearing decisions rather than oversights:

- **Ridal never terminates TLS, and the guardrails are keyed on the
  bind address because of it.** Behind a TLS-terminating proxy the
  server sees plain HTTP on loopback, which is correct and safe, so
  "is this connection TLS?" always answers no and is useless as a
  check. A loopback bind is therefore trusted; a public one refuses to
  serve writes with no accounts configured, and refuses password
  logins without `--allow-insecure-login`. The reverse proxy is the
  supported way to serve this remotely (#131).
- **Sessions are a signed cookie, with no server-side table.**
  `blake3::keyed_hash` over `user|version|expiry`, verified by
  constant-time `blake3::Hash` comparison against a 32-byte key.
  The credential version in the cookie is what makes a stateless
  session revocable: changing a password, role or download scope bumps
  it on the account and the outstanding cookie stops verifying. A
  project with no `users.json` has not opted into any of this and
  behaves exactly as it did before authentication existed.
  `ridal server start` keeps the key in `ridal_data/session.key`;
  `ridal gui` generates one at startup and never writes it down, so an
  offline session ends with the server and a survey directory that is
  zipped and shared carries no secret out with it (#187).
- **A project's state is one directory, and `ridal.toml` is not in
  it.** Everything Ridal owns lives under `ridal_data/`; the marker
  stays at the project root so the project root remains the directory
  the user points Ridal at, and every relative path in the marker
  resolves against the directory holding it. This is what lets a
  project be initialized in a survey directory that is already full of
  the user's files without colliding with them, and what makes "back
  this up" and "delete this project" single operations. The layout is
  recorded as `format_version` in the marker rather than inferred from
  which files exist, so a future move is detectable: an older Ridal
  refuses a newer project instead of writing into the wrong places
  (#187).
- **Amplitude limits are global per revision+profile**, which is what
  keeps chunks seamless — a radargram with strongly varying gain
  down-profile cannot be locally renormalised without breaking that
  guarantee. Per-region normalisation and seamlessness are mutually
  exclusive under the current design.
- **Edge chunks are returned at their true size, not padded** to
  256×256 — padding was tried and left a visible border of dead pixels.
  A client consuming the chunk API must compute the valid extent itself;
  it isn't currently advertised in a response header or manifest
  endpoint.
- **`RenderServiceConfig::source_cache_mb` is not wired to anything.**
  It's reserved for a deferred HDF5-chunk-aligned source-read cache and
  is not even exposed as a CLI flag today — always its default value.

Not yet implemented, in rough priority order given the measurements
above:

1. **On-disk render cache**, keyed on `RevisionId` so reprocessing
   invalidates it correctly. Now the highest-value remaining item — see
   [What was measured](#what-was-measured-and-what-it-settled).
2. **Digitization** — picking, editing, and persisting interpreted
   reflector layers in the viewer, with user-editable layer names and
   colours (a fixed server-side vocabulary will not survive contact
   with real interpretation work). This is the actual point of building
   a viewer at all; everything above is scaffolding for it. Needs a
   write path and a real persistence/authorship model, which nothing
   built so far requires.
3. **User-defined render profiles**, named/validated/persisted
   server-side rather than accepted as free-form per-request
   parameters — the latter is explicitly rejected, since
   `?min=…&max=…&contrast=…` on every chunk request would make each one
   a distinct, uncacheable render variant and let a client trivially
   thrash the cache.

## Topographic correction

`DatasetView::Topographic` (#168) is a render-time-only vertical shear of
the `data` array, applied by `render::topo::TopoSource` — a decorator
over `AmplitudeSource`, exactly like every other stage of the [render
pipeline](#render-pipeline): it reports a taller, sheared shape and
assembles each output row from the appropriately shifted source row(s),
so chunks, overview banding, `ridal render`, the GUI image download and
even the amplitude-limits sampler (which deliberately does *not* read
through it — see below) all work unchanged. Nothing is precomputed or
stored, and no interpretation is ever saved in the corrected coordinate
space — distinct from `gpr.rs::correct_topography`'s `data_topocorr`,
which writes a NetCDF product on a slightly different (`height /
max_depth`) vertical scale, by design (see `topo.rs`'s module docs).

- **Geometry** (`TopoGeometry`, resolved by `topo::resolve_topo_geometry`
  from the `elevation`/`depth` axes) is the one place the numbers are
  computed: `dz` (median positive diff of `depth`), `E_top` (max
  effective elevation) and a per-trace `shift`, memoized per
  `RenderService` and re-resolved only when the requested elevation range
  changes. `routes.rs` resolves it (via `RenderService::topo_raster_height`)
  *before* building a chunk/overview's geometry — a corrected-view chunk
  below the source's own row count is perfectly valid in the taller
  raster, and routes built from the source shape the way every other view
  is would 404 it forever.
- **Amplitude limits are always sampled from the standard source**,
  regardless of which view was requested: the distribution a shear
  relocates is unchanged by relocating it, so resampling through
  `TopoSource` would read its NaN wedges into the percentile estimate and
  shift contrast every time the checkbox is toggled.
- **Erroneous elevations** (GPS spikes) never silently inflate the
  raster, and the two directions are guarded differently, because they
  are different problems. `elevation_max` caps a trace's *surface*: a
  surface above it is clamped down, so an upward spike is flattened to
  the cap rather than lifting the whole raster's top to meet it.
  `elevation_min` is the *floor of the rendered raster*: nothing below it
  is drawn, which bounds the view from beneath without moving any trace
  off its true position. Both live in `overrides.json` and are edited
  from the catalog's properties dialog — a property of the survey, not of
  whoever is viewing it. A *missing* elevation is the one case that is
  interpolated from neighbours (a trace with no elevation has no vertical
  position at all, so "leave it alone" is not available), and is counted
  in the diagnostics. A spread that looks like spikes (full span far
  exceeding the 1-99th percentile span) is flagged in the geometry
  response and surfaced as a viewer warning, never auto-corrected.
- **Unavailability says what can fix it.** `TopoUnavailableCause`
  separates a file that cannot support the view (no `elevation`, no
  `depth` — permanent, reported quietly as a disabled checkbox
  explaining itself on hover) from a configured window that excludes its
  own data (`topo_window_invalid` — somebody's edit, fixable, and given a
  visible warning naming the reason). One code for both is what made a
  bad window look like an unsupported file.
- **The sub-sample shift is a windowed-sinc fractional delay**, not a
  two-tap linear blend. Linear interpolation is a low-pass filter whose
  strength depends on the fractional shift, scaling amplitude by
  `sqrt((1-f)^2 + f^2)` — 1.00 at `f = 0`, 0.71 at `f = 0.5` — and since
  `f` sweeps `[0, 1)` as the surface rises and falls, that 29% swing
  lands across traces as vertical banding and its average as an overall
  darkening. Measured at a 30% peak-to-peak contrast swing on a real
  profile before the fix, 6-10% after. See
  `topo::SHIFT_KERNEL_HALF_WIDTH`.
- **The catalog's own index overviews stay `Standard`** — only the
  viewer offers the corrected view, and its checkbox is disabled with the
  unavailability reason as its `title` when a radargram lacks usable
  axes, both from an on-load availability check and from
  `ridal render --topo` failing the same way on the command line.

## Derived layers and reducers (#205–#210)

A **derived item** is a named Rhai expression over the project's picked
layers, e.g. `median(bed)` or `std(concatenate(bed, bed_no_temperate))`.
Whether it is a *layer* or an *attribute* is inferred, never declared: an
expression that yields a position is a derived layer (a line, available as
depth, TWTT and sample number); anything else is a derived attribute (a
per-position number exported in its own unit). The evaluator lives in
`interp::derive` (pure, no I/O) and the document model in `project::derived`
(`ridal_data/derived/derived.json`).

### Reducers turn multi-valued geometry into one line per user

A reflector is supposed to be a function of trace, but a layer may be drawn
in several pieces, or a fold may have two limbs. `Layer::reducer` decides
how several picked values for one user at one position collapse to one:
`shallowest` (the project default), `deepest`, `median`, `mean`. The default
is shallowest because stray picks on multiples and ringing lie *below* the
true reflector, so the minimum depth is the defensible choice — for a folded
reflector it keeps only the upper limb. Reducers apply **at evaluation
time**; stored picks are never rewritten, so changing one is
non-destructive and reversible.

### Exclusivity groups

`LayerSet::groups` declares sets of layers that cannot all hold a value for
one user at one position. Two layers conflict iff they share a group, and
membership is deliberately not transitive. When one user holds values for
two conflicting layers at a position, *every* layer involved becomes NaN for
that user there, so group evaluation order cannot matter. A group naming an
undefined layer is reported, never dropped.

### Units

Every layer value is converted into the expression's unit (`meters`,
`nanoseconds`, or `samples`, positive **down**) before the expression runs,
and a position result is converted back to the other two. Sample numbers may
be fractional. An attribute result is exported in its own unit and never
converted.

A fourth unit, `dimensionless`, exists for counts, ratios and flags. A
*position* may not be declared dimensionless — a position is a depth, and a
depth has a unit — which `Unit::allows` enforces at evaluation time, where
the inferred kind is known. (`validate` cannot: it has no layer vocabulary,
so it cannot know an expression's kind.)

### Who sees a result computed from whose picks

Three rules, and they are the whole permission model for derived results:

1. **Anyone may see a result computed from their own picks.** It is a
   function of data they already have, so it needs nobody's permission. This
   is `Audience::OwnPicks`, the default.
2. **A cross-user result is an admin decision**, recorded as
   `Audience::Released` on the item. Releasing needs `Role::Admin`, not the
   `Role::Operator` that *authoring* an item needs, because releasing
   publishes other contributors' work in aggregate.
3. **An operator sees the cross-user result either way**, so a consensus can
   be defined and watched while picking is still open, without pickers seeing
   each other's work — which is the bias the study design exists to avoid.

`audience` is deliberately separate from `scope`: `scope` decides who can see
that an item *exists*, `audience` decides *whose data feeds it*. A download
that mixes audiences evaluates once per audience and serves each item only
from its own evaluation, because serving both from one wider evaluation and
filtering afterwards is how a consensus leaks into an unreleased item.

`DownloadScope::Results` sits **below** `Picks` in the scope ladder, and is
the one rung where "more" inverts: an aggregate over many contributors
discloses less than any single contributor's raw picks. It is what expresses
"you may have the consensus but not the individual interpretations", and
without it that setting is unreachable — the lowest scope permitting a result
also permitted every pick behind it.

**Which velocity model the expressions assume:** the depth axis of a
processed radargram, i.e. the single `medium_velocity` the file was processed
with. A derived expression in `meters` therefore inherits that velocity, and
changing it means reprocessing the radargram; the picks themselves are in
trace/sample space and are unaffected.

### Permissions

An expression is evaluated over the picks the caller may see: an operator
(or a project that never opted into authentication) gets the full consensus,
an ordinary picker only their own picks. Defining a project-wide item needs
the operator role; a private item belongs to one user and any signed-in user
may keep one. Results have their own download scope, separate from picks.

A worked example — the layers, exclusivity group and seven expressions used
to reproduce the Mannerfelt et al. (2026) consensus — is in
`assets/examples/dronbreen-20250327-DAT_0066_A1_1/`.

## The layer panel, fills and the expression editor (#209)

The viewer's controls for all of this live in `assets/panel.js`, a small
`L.Control` subclass rather than `L.control.layers`. The built-in is a flat
checkbox list that cannot group derived items apart from picked layers, label
an item "(your picks)", or add one contributor toggle without doubling every
row — so the subclass is less code than working around it. Panel order is
draw order.

- **Own layers are on by default; derived items are off.** A new expression
  is likelier to be wrong than the layers it is built from, so `show` starts
  false. An item the caller may not see is absent from `GET /api/v1/derived`
  entirely — `scope` is applied server-side before the panel ever sees it.
- **One "show all contributors" toggle**, not a per-layer variant. It is
  offered only when the server says so (`can_see_others`), never inferred
  from a role string. Other contributors' lines are fetched through
  `GET /api/v1/datasets/{id}/contributors`, which applies the caller's own
  visibility; the ungated `.../interpretations/{user}` route (#212) is
  deliberately not used.
- **A project-wide item read as a personal number says so.** When an
  `OwnPicks` item is shown to someone who cannot see cross-user results, its
  label carries "(your picks)". Silently showing a personal evaluation under
  a name like "Consensus" is wrong in a way nobody can see, which is the
  failure this phase most needs to avoid.

### Range fills

A `fill_to` on a derived position draws a matplotlib `fill_between` band
toward another item. It is filled **per trace interval** and split wherever
the two bounds cross, so a fill can never render as a bowtie; it **breaks at
a NaN gap** on either side rather than bridging it; and it is drawn in its
own Leaflet pane **behind every line**, so it is visible with both bound
lines toggled off. It is drawn only when the caller can see both bounds — a
fill against an invisible bound would disclose that bound's position exactly
— and only in the radargram viewer, where a depth range has meaning.

### The expression editor

The editor previews a line on the radargram as you type, without saving:
`POST /api/v1/datasets/{id}/derived/preview` evaluates the expression over
the same picks the caller may see and returns the inferred kind and unit.
That live line is the best guard against a sign or unit mistake, and an
invalid expression clears it rather than leaving a stale line pretending to
be current. Highlighting is hand-rolled for the twenty-token grammar (layer
ids, built-ins, numbers, `if`/`else`, `NaN`) rather than vendoring Prism, and
autocomplete is a `<datalist>` fed from the layer vocabulary and the built-in
list. `layers_unusable_in_expressions` is shown beside it: a legacy id with a
hyphen parses as a minus and would otherwise fail with no hint that the id
was the problem.

`scripts/panel_harness.py` drives these through headless Chromium and a
same-origin iframe harness; see its module doc for the virtual-time and
request-counting traps.
