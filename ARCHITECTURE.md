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
**source amplitude → dataset view → source transform → resample →
normalize → colormap → encode.** Everything lives under
`src/server/render/`.

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
  warning against unbounded client-driven render work. Seven built-ins,
  each `siglog-*` sitting next to the profile it is the log view of:

  | Profile | Source transform | Display transform | Resampling | Notes |
  |---|---|---|---|---|
  | `default` | None | Linear | Mean | 1–99% quantile |
  | `siglog-default` | SigLog | Linear | Mean | the `default` view on log-compressed source |
  | `positive` | None | Positive (asymmetric) | LanczosRectified | biases toward positive returns, clips negative toward black |
  | `siglog-positive` | SigLog | Positive | LanczosRectified | `positive`'s tuning on log-compressed source; it displays the rectified envelope, so it keeps `positive`'s reducer |
  | `abslog` | None | `log10\|A\|` | LanczosRectified | sign-agnostic by construction, so rectifying changes nothing about what it means |
  | `high-contrast` | None | Linear | Mean | 5–95% quantile |
  | `siglog-high-contrast` | SigLog | Linear | Mean | the `high-contrast` view on log-compressed source |

  A profile has **two** transforms, and their order is the reason
  `siglog-*` needs no new resampler. `source_transform` is applied to
  each source sample *before* resampling; `transform` maps the already
  resampled value into the display domain afterwards. The `siglog-*`
  profiles set `SourceTransform::SigLog` — the processing step's own
  `(log10|A| − offset).max(0)·sign(A)` — so the compression happens
  before whatever the base profile's reducer does, exactly reproducing
  the order a `siglog` processing step would have. Each `siglog-*` also
  keeps its base profile's resampling method, and that is load-bearing
  for `siglog-positive`: `positive` displays the rectified envelope, so
  it filters `|siglog(A)|` with `LanczosRectified`. Reducing that with
  `Mean` would average the signed siglog values back toward zero, which
  `Positive`'s black level then clips to an almost entirely black
  overview — while "run `siglog`, then render with `positive`" looks
  right, because it rectifies.

  Compressing *before* the reducer is what keeps an overview meaningful;
  a post-resample log would compute `siglog(mean(A))`, and a downsampled
  footprint of oscillating signed data averages toward zero, which the
  log then truncates to a flat image. The same split is why limits are
  sampled in the source-transform domain (`stats.rs`).

  There is deliberately no `siglog-abslog`: `abslog` is already a log
  transform, so its siglog view would be a log of a log rather than a
  distinct picture.

  `positive`'s asymmetry is why `RenderProfile`'s display transform is
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

**The trace view (#181)** is a canvas beside the radargram, toggled by
`#trace-toggle` and fed by `GET /api/v1/datasets/{id}/traces/{trace}`,
which returns the raw `data[:, trace]` column (no render profile — the
processed amplitudes already carry their gain). Rendering was treated as
expensive, so the panel is click-driven: a click on the radargram selects
the trace under it, and the panel does not follow the cursor. Its vertical
axis is the radargram's *visible* sample window, converted per row through
`shiftAt` at the selected trace, so it stays aligned in the topographically
corrected view rather than being disabled there. A black vertical line is
drawn on the radargram at the selected trace (in its own
`radargram-trace` pane) so it is obvious where the panel is reading from;
it is removed when the panel closes and re-projected through
`RIDAL_REDRAW_TRACE` after a topographic or horizontal-scale change. Two
behaviours keep the panel feeling attached to the map: a zoom animates its
sample window over the same 250 ms as Leaflet's own CSS zoom (from the
`zoomanim` event's target centre/zoom, since `getBounds()` is still the old
view until `zoomend`), and dragging the canvas pans the radargram
vertically so the two scroll together.

**The viewer's two drag handles (#199).** `#map` and `#trace-view` share
`.radar-region`; `#trace-resizer` sizes them against each other, and
`#split-resizer` sizes the whole region against `#overview-map`. Both
`.layout` and `.radar-region` are `flex-wrap: nowrap` on a wide screen and
stack only through the narrow-screen media query, so a handle can never be
hidden by the state its own drag created — the feedback loop that made #199
unrecoverable, including after a browser zoom out. `.radar-region` carries
a CSS `min-width` (the radargram alone, or radargram + trace handle + trace
panel + gutters when the trace is open) that `#split-resizer`'s clamp reads
back, so shrinking the region can only take the trace to its minimum and
never push it onto a row of its own. Each clamp reserves the panes' minimums
plus the 1px borders that `box-sizing: content-box` keeps outside the
flex-basis, and both flex gaps; a double-click resets that handle's split.
Both handles set `touch-action: none` (and a widened `::before` hit area),
without which a touchscreen claims the drag for scrolling and the pointer
events never arrive. `#trace-resizer` keeps its 8px in flow when hidden
(`visibility`, not `display`) so hiding it cannot change whether the trace
panel wraps below the radargram.

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
- **Bulk teaching accounts have two deliberately different paths.** Invite
  batches create one single-use link per account, so each student chooses a
  password without the administrator knowing it. A second batch mode can create
  random passwords for small workshops, but it requires an explicit per-request
  acknowledgement, warns more strongly for picker and operator accounts, and
  refuses administrator accounts. Generated passwords are returned once and
  only their Argon2id hashes are stored; the browser's print/CSV result is the
  administrator's responsibility to protect (#202).
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
Whether it is a **layer** or an **attribute** is inferred, never declared: an
expression that yields a position is a derived layer (a line, available as
depth, TWTT and sample number); anything else is a derived attribute (a
per-position number exported in its own unit). The evaluator lives in
`interp::derive` (pure, no I/O) and the document model in `project::derived`
(`ridal_data/derived/derived.json`).

There are exactly two kinds, matching the two words the UI uses. An earlier
`length`/`scalar` split was never acted on by any branch — the only decisions
are "is it a layer" — and the stored `unit` already distinguishes a thickness
from a count, so the extra terms only reached the UI and confused it. The
`percentile(a, p)` built-in is the order statistic at `floor(p/100 * (n-1))`;
it deliberately does not interpolate, so it always returns a value a
contributor actually picked (see the Mannerfelt consensus).

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
*layer* may not be declared dimensionless — a layer is a depth, and a depth
has a unit — which `Unit::allows` enforces at evaluation time, where the
inferred kind is known. (`validate` cannot: it has no layer vocabulary, so it
cannot know an expression's kind.)

### Two engines, one function table

Kind inference and evaluation are **two Rhai engines over parallel type
tables**: evaluation binds a layer as a `UserArray` and a reference to another
derived item as a plain `f64`; inference binds both as a `Kinded`, which
carries the kind and whether the value is still per-user.

That split is what makes `median(bed)` inferable without any picks, and it is
also the standing hazard: **a function registered on one engine and not the
other makes them disagree about what is a valid expression**, and the
disagreement is silent in the worst direction. Inference runs when an item is
*saved*; evaluation when it is *read*. An expression that infers but cannot
evaluate saves cleanly, reports its kind in the editor, and then fails on
every read afterwards.

So every element-wise helper has a scalar twin (`clamp`, `shallowest`,
`deepest`, `where`), the mixed array/scalar forms exist on both engines, and
inference refuses an array-valued result exactly as evaluation does —
`bed - temperate_ice` is a length *per contributor*, not an item;
`median(bed) - median(temperate_ice)` is. Rhai's standard library already
supplies scalar `min`, `max`, `abs` and `is_nan`, so those need only a
`Kinded` mirror.

`inference_and_evaluation_accept_the_same_expressions` enforces this over a
table of expressions. **Add a row whenever a function is registered** — the
test exists to catch the class, not the three instances that prompted it.

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

### Where derived items are managed, and why in two places

The viewer's layer panel is the **main** place to create, edit and delete a
derived item, because that is where the expression's effect is visible: the
live preview only exists next to a radargram. It is not the **only** place.
A project's derived items are project state, and a project should be
manageable without opening a radargram, so the `/layers` page lists and edits
them too. The two are one dialog and one save path: `RIDAL.derivedEditor`
in `app.js` builds the form and does the `PUT`, and each page supplies what
it alone can — the viewer passes a `preview` callback that draws a line,
`/layers` passes none and the dialog says there is no radargram to preview
against.

Both editors send only the items the caller can see, and `PUT
/api/v1/derived` merges: items the caller cannot see are preserved, an
incoming id that collides with one is refused, and a delete that would orphan
another item's reference is refused naming the dependent. That is why the
client never has to fetch or resend the invisible partition, which it cannot
see by design.

### The panel's disclosure

The panel is a `<details>` closed by default — the same pattern the header
menu uses, so it opens and closes and takes keyboard focus with no
JavaScript. Unlike `.site-menu` it deliberately does **not** close on an
outside click — the close-on-outside handler in `app.js` is bound to
`details.site-menu`, and the panel is not one — because the map is what is
being looked at while layers are toggled, and a click there must not fold the
panel away mid-task. A separate control hides the panel entirely for a viewer
reading the image, and that choice persists for the session in
`sessionStorage`; the toggle has its own hover treatment because it is small
and easy to overlook.

`audience` is a permission control, not a display setting, and the editor
labels it as publishing every contributor's picks in aggregate ("Visible to
everyone"). It is shown only to a caller who may set it (`can_release`), and
the server's `Role::Admin` check on `Audience::Released` remains the real
gate; a 403 is shown verbatim rather than folded into a generic failure.

### Listed vs shown, and the used-by counter

A derived item has two independent viewer-facing flags. `show` is whether it
is **drawn** when the viewer opens; `listed` is whether it appears in the
viewer's layer panel **at all**. The distinction exists for intermediate
layers: a layer that exists only as an input to another derived item should
not clutter the panel, so it is unlisted while staying fully usable in
expressions and fully visible on the `/layers` page. The editor names both
plainly ("Show in the viewer's layer list", "Draw it when the viewer opens"),
because the earlier single "Show by default" left it unclear how to take a
layer out of the list. The panel's own count is the number of layer rows it
shows -- picked layers plus listed derived layers -- and not the contributor
toggle.

The `/layers` page shows a **used-by** count per item: how many other derived
items reference it in an expression. It is computed server-side over the
*whole* stored set, so an invisible dependent is counted too -- otherwise the
count would read zero and the server's refusal to delete the item would look
arbitrary. Only the count leaves the server, never who depends on it.

### Downloading derived layers

The download menus offer **Picked layer points** and **Derived layer points**.
They share the spacing, format and coordinate options, but not their shape.

Both sample the radargram's own **arc-distance grid**, built once from the
radargram start (distance 0 by construction) at exact multiples of the step,
so a 5 m export gives 0, 5, 10 m for every layer and every user. A layer emits
the grid nodes inside its span; "per-trace" is the integer-trace axis and
"vertices" is the line as drawn.

**Picked layer points** are long: one point per picked line per position, each
row tagged with its layer. **Derived layer points** are wide: one point per
position, and every visible derived item -- layer or attribute -- is a property
of it. A derived layer is a depth, so it is written in all three vertical units
(`thickness_m`, `thickness_ns`, `thickness_samples`); a derived attribute is
written in its own unit (`thickness_user_std_m`, or the bare id when
dimensionless). A thickness and a cts depth therefore share a row, with the
statistics that summarise them beside them. A property name is reserved
against the point's own fields and against every other item's: `easting`,
`northing` or `trace` as a dimensionless attribute, or any id whose `<id>_m`
is `distance_m`, is refused at save -- while the author can still choose
another id -- and refused again at export if it reached the store another
way. "Include unlisted" brings in items
marked `listed: false`, off by default to match the viewer panel. The
single-radargram route is `GET /api/v1/datasets/{id}/derived/level2`; the merged
group/catalog menus select the same path with `derived=true` on the existing
`level2` route, so one dialog serves both. A merged derived file's CSV columns
are the union across its radargrams, with missing values empty -- the shape
`assets/interp/.../expected_consensus.csv` uses.

An admin also gets **For every user** on picked layer points: one file with
every contributor's points, each row still tagged with its user. It is an
`Role::Admin` decision (the server checks it, not just the checkbox), because
it discloses individual picks in aggregate.
