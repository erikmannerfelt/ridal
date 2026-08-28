# Web server and browser GUI for processed radargrams (#115)

Implements the tracking issue #115 and its six sub-issues (#116–#123): a
Rust-native, dependency-free web application for browsing and visualising
radargrams processed by ridal.

**Scope:** 34 commits, 41 files (+11 767 lines) excluding vendored
Leaflet. 219 unit/integration tests pass. Everything sits behind a
`server` cargo feature, so a CLI-only build is unchanged.

---

## What this adds

### Two launch modes (#120)

```console
ridal gui <FILE-OR-DIRECTORY>          # local, ephemeral port, opens a browser
ridal server start <FILE-OR-DIRECTORY> # persistent, 127.0.0.1:8000 by default
```

Both take either a single processed `.nc` file or a directory to scan
recursively. Both share one server implementation.

### Persistent identity metadata (#116, #117)

Four concepts that were previously conflated, now kept apart:

| Concept | Attribute | Stable across reprocessing? |
|---|---|---|
| Radargram identity | `ridal_radargram_id` | yes |
| Display label | `ridal_display_name` | n/a (cosmetic) |
| Group name / id | `ridal_group_name`, `ridal_group_id` | n/a (cosmetic) |
| Processed revision | derived `RevisionId` | **no** — changes every run |

Radargram and group IDs are validated ASCII slugs (`[a-z0-9_-]`, ≤128
chars, no reserved names). Display names and group *names* are free-form
Unicode; their IDs are derived by slugification, which transliterates
ø/æ/å so `--group-name "Drønbreen"` yields the id `dronbreen`.

`RevisionId` is a blake3 fingerprint of `(radargram_id,
processing_datetime)`, deliberately excluding path, filesystem
timestamps, filesize and display name — so moving or renaming a file does
not invalidate its cached renders, but reprocessing does.

New CLI flags: `--radargram-id`, `--display-name`, `--group-name`
(aliased as `--group`), `--group-id`, on both `process` and `batch`.

### Catalog discovery (#122, #123)

`io::inspect_ridal_netcdf` is a reusable, metadata-only recogniser that
lives *outside* `src/server/` so non-server code can use it. Discovery
walks a directory, skips hidden/cache dirs, tolerates unreadable
candidates as warnings rather than aborting, and resolves duplicate
radargram IDs deterministically (newest `processing_datetime` wins, ties
broken by path order, always with a warning).

Group names that disagree across members of one group id resolve by the
same rule — reused, not reinvented.

### Streaming renderer (#118)

Source amplitudes → dataset view → float-domain resampling →
normalisation → colormap → encode. Never materialises a full-resolution
image. Chunks are 256×256, addressed by deterministic
Leaflet-compatible `(x, y)`; overviews are ~512 px wide.

Two resampling methods: area-weighted mean (NaN-aware, weight-renormalising)
and peak. Nearest-neighbour is deliberately excluded — it preserves this
data's high-frequency noise badly.

### Render profiles and caching (#119)

Four server-defined profiles, all typed (`RenderProfile`), never
client-defined:

- `default` — 1–99 % quantile, linear
- `positive` — asymmetric stretch biased toward positive returns (see below)
- `abslog` — genuine `log10|A|`
- `high-contrast` — 5–95 % quantile

Amplitude limits are estimated **once per revision+profile** from
fixed-seed sampled percentiles and reused for every chunk and the
overview — which is what keeps adjacent chunks seamless. Sampling reads
whole traces in short contiguous runs; benchmarked at ~1 GB, that is
~7.6× faster than evenly-strided single-trace reads because each run
stays inside a handful of HDF5 storage chunks.

An in-memory, byte-bounded LRU cache (`--cache-memory-mb`) keys on
`(revision, view, profile)` plus the object descriptor.

### Index and viewer (#121)

**Index:** lazily-loaded overview thumbnail per entry, one Leaflet map
per group showing every member's track, a render-profile switcher, and
bidirectional hover highlighting between a card and its track on the map.

**Viewer:** chunked Leaflet radargram with `L.CRS.Simple`, a synchronised
overview map, clickable sibling tracks, a cursor readout, a horizontal
scale control, a resizable split, and a metadata dialog.

### Cursor sync, done exactly

The obvious way to simplify a track — sample vertices evenly by
*distance*, then look a cursor position up by *trace fraction* — only
works when traces are evenly spaced. Any standstill desynchronises the
two and the error accumulates along the profile: **up to 140 m** on a
real Drønbreen line with an 18-trace standstill.

This implementation stores each retained vertex's source trace index
directly, so lookup is exact at every vertex and monotone in trace index
regardless of spacing. Douglas–Peucker simplification runs in
`(trace_index × speed_scale, easting, northing)` — 3-D, not 2-D, because
a standstill on an otherwise straight line adds no *geometric* deviation
and plain 2-D simplification collapses it away, silently reintroducing
the exact problem this module exists to avoid.

A regression test pins both halves against real assets: the trace-indexed
method stays within 2 m, and the distance-indexed one demonstrably does
not.

### Cursor readout

```
trace 1234 / 2529 · 1043.2 m · TWTT 512.3 ns · depth 43.1 m
```

Distance, TWTT and depth are served as arrays from `/axes`, not
reconstructed client-side: `return_time_to_depth` applies a Pythagorean
antenna-separation correction and clamps near the surface, so depth is
nonlinear and cannot be derived from a step size without duplicating
physics into JavaScript.

### Frontend, with no build step

Leaflet is vendored (BSD-2-Clause, refreshable via
`scripts/vendor_leaflet.sh`) rather than loaded from a CDN. All CSS/JS is
first-party and embedded in the binary via `include_bytes!`. Production
needs no Node, no dev server, no assets directory. A full design token
set drives a real light/dark theme.

### API surface

```
GET /                                                    index
GET /view/{radargram_id}                                 viewer
GET /api/v1/health
GET /api/v1/profiles
GET /api/v1/datasets
GET /api/v1/datasets/{id}
GET /api/v1/datasets/{id}/track
GET /api/v1/datasets/{id}/axes
GET /api/v1/datasets/{id}/attributes
GET /api/v1/groups/{group_id}/tracks
GET /api/v1/datasets/{id}/views/{view}/overview
GET /api/v1/datasets/{id}/views/{view}/chunks/{profile}/{x}/{y}
```

Errors use a stable envelope: `{"error": {"code", "message"}}`.

---

## Choices that may challenge future development

These are the decisions most likely to constrain or annoy whoever works
here next. Listed because they are load-bearing, not because they are
regrettable.

### 1. `--n-workers` is accepted but inert

The flag exists on both `gui` and `server start`, is parsed, and is
stored in `RenderServiceConfig` — **and then read by nothing.** It was
added in anticipation of the bounded-concurrency work (see Phase 1 of the
roadmap) that this PR does not include. It should either be wired up or
removed before release;
shipping a flag that silently does nothing is worse than not having it.

### 2. Rendering is synchronous inside async handlers

`overview_image` and `chunk_image` render on the tokio worker thread,
holding that radargram's `Mutex`, with no `spawn_blocking`.
`render_overview` reads the **entire** source array. `loading="lazy"`
plus the browser's per-origin connection limit bound this in practice,
and the cache makes repeat loads free, but first paint on a large catalog
will be slow and can occupy tokio workers. This is the single most
important thing to fix next.

### 3. netcdf-c/HDF5 is not thread-safe

A global `libnetcdf_lock` serialises *every* netcdf call. This is a hard
ceiling on read parallelism that no amount of tokio tuning will lift.
Tests use `#[serial_test::serial(netcdf)]` plus `#[test_retry::retry]`;
a residual ~6 % `Netcdf(-101)` flake remains and is documented. Any future
concurrency work must treat netcdf reads as a serialised resource.

### 4. Amplitude limits are global per revision+profile

This is what makes chunks seamless, but it means a radargram with
strongly varying gain down-profile cannot be locally normalised without
breaking that guarantee. Per-region normalisation and seamlessness are
mutually exclusive under the current design.

### 5. The `positive` profile reads amplitude asymmetrically

`positive` derives its percentile bounds from `|x|` (skipping the first
50 sample rows, i.e. the direct wave) and then stretches the *signed*
data against those bounds, so negative returns fall below the black level
and clip. That asymmetry is the point of the profile, and it has two
consequences the codebase now carries:

- `RenderProfile.abslog: bool` had to become an `AmplitudeTransform`
  enum, because `Positive` needs a different domain for *statistics*
  (`|x|`) than for *display* (signed `x`) — one bool could not express
  that. Two functions, `to_stats_domain` and `to_display_domain`, now
  exist where one used to.
- The mean is the wrong reducer for it. Radar traces oscillate about
  zero, so averaging a downsampled footprint cancels them and the black
  level clips the remainder away — `positive` overviews rendered almost
  entirely black until `ResamplingMethod::Peak` was added. Peak and Mean
  agree exactly at a 1:1 footprint, so this only affects downsampled
  views. **Any future profile that reads amplitude asymmetrically must
  make the same choice.**

### 6. The x-scale control is a pure client-side transform

Changing horizontal scale re-lays the existing overlays; it never asks
the server for a different render. Cheap and responsive, but it means
there is no multiresolution pyramid and no formal relationship between
Leaflet zoom and raster resolution. #121 explicitly leaves that door open
for a future server-side `(z, x, y)` tiling scheme; adopting one would
replace this control rather than extend it.

### 7. Edge chunks are returned at their true size, not padded

A rightmost/bottommost chunk covers fewer than 256 px, and the server
returns an image of exactly that size; `chunkBounds` places it there.
The alternative (padding to 256) leaves a visible border of dead pixels.
**Any client consuming the chunk API must compute the valid extent
itself** — it is not currently advertised in a response header or a
manifest endpoint, which is a gap worth closing if a second client ever
appears.

### 8. First-party assets must live outside `assets/vendor/`

`scripts/vendor_leaflet.sh` does `rm -rf` on that directory. Anything
first-party placed there is silently deleted on the next Leaflet
refresh. The `embedded_asset!` macro's path handling encodes this, and
its doc comment says why. The repo-root logo needed a *second* macro arm
(`embedded_repo_asset!`) for the same reason.

### 9. Page-specific JavaScript is inline in the templates

Shared constants live in `assets/app.js`, but per-page logic is inline in
`index.html.jinja` and `viewer.html.jinja`. This was a deliberate
no-build-step tradeoff; it will not scale past roughly the current size.

### 10. No authentication, anywhere

`server start` binds loopback by default and binding elsewhere is an
explicit flag, but there is no authN/authZ of any kind. Deploying this
beyond localhost requires a reverse proxy that provides it.

---

## Not implemented

### Deferred — worth doing later

| Item | Issue | Notes |
|---|---|---|
| **On-disk render cache** | #119 | Explicitly deferred by the issue itself. When added, it **must** key on the revision ID so a reprocess invalidates it, and needs a documented cache location and an enablement flag. The in-memory LRU already keys correctly, so this is additive. |
| **Bounded concurrency and cancellation** | #119, #120 | Collapse identical concurrent render misses so N requests generate an item once; bound per-request work; support cancellation. `--n-workers` exists for exactly this. |
| **`spawn_blocking` for renders** | — | See "challenges" #2. Should land alongside bounded concurrency. |
| **Topographically corrected view** | #118 | `DatasetView` is an enum with one variant precisely so this can be added without reshaping the API. |
| **Multiresolution / server-side (z,x,y) tiles** | #118, #121 | Named as a future optimisation *if* representative radargrams show it is needed. Not yet measured. |
| **Digitization and labelling categories** | #115 | Out of scope for a read-only v1, and the largest remaining piece of work — see Phase 4, where it belongs together with gprinterp development. |
| **Reading ridal NetCDF back into a full `GPR`** | #123 | Only metadata-only inspection exists. `identity::resolve_*` already takes an `inherited` tier that is always `None` today, shaped for this. |
| **Config-file support for render settings** | #119 | The issue names "CLI, configuration file, API, and future GUI controls" sharing one typed `RenderProfile`. CLI and API share it; there is no config file yet. |
| **Frontend fetch error handling** | #121 | Page scripts do bare `.then(r => r.json())`. A failed `/track` leaves the map silently empty. Index thumbnails *do* degrade visibly. This is the most substantive of the deferred UI items. |
| **Viewer prev/next navigation within a group** | #121 | Group membership is already computed server-side, so this stays cheap. |
| **Placeholder panels for the topo view and digitization** | #121 | Would keep the eventual digitization layout from being a retrofit. |
| **Multiple named basemaps** | — | Deliberately parked; currently Esri World Imagery only. Wants a proper config for names and attribution rather than a hardcoded second entry. |
| **Windows/macOS verification of the server feature** | — | Pure Rust, so it should work; CI builds/tests it on Linux only and says so. |

### Should **not** be pursued

| Item | Why not |
|---|---|
| **Client-defined render profiles** | #120 warns explicitly against unbounded client-driven render work. Accepting arbitrary limits/transforms from a query string turns every request into an uncacheable, unbounded server job. Profiles must stay server-defined and enumerable. |
| **Nearest-neighbour resampling** | #118 is explicit that it preserves this data's high-frequency noise poorly. Adding it as an option would produce visibly wrong-looking radargrams and invite bug reports that are not bugs. |
| **Per-chunk amplitude limit estimation** | Superficially attractive (better local contrast, no full-radargram sampling pass) and definitively wrong: adjacent chunks would normalise differently and every chunk boundary would become a visible seam. |
| **Folding display name or group into the revision fingerprint** | #117 lists what must *not* change a revision. Renaming a radargram would needlessly invalidate every cached tile. The current split is load-bearing — keep it. |
| **Loading Leaflet from a CDN** | #120 requires production to work without a separate asset pipeline; a CDN also breaks offline/field use, which is a realistic deployment for this tool. Vendoring costs one script and a licence file. |
| **Padding edge chunks back to 256×256** | Tried; leaves a visible gray border of dead pixels past the radargram's real extent. If a future client genuinely needs fixed-size tiles, advertise the valid extent in a header instead of re-introducing the padding. |
| **A generic caching framework** | Each cache's read condition, invalidation and serialisation should stay visible at its own call site. The current two caches (limits, encoded images) are not similar enough to share an abstraction without hiding the decisions that matter. |
| **`--group-id` without `--group-name`** | Already rejected in code: an id exists only to give a name a URL-safe form, so a name-less id has nothing to attach to. Do not "fix" this by allowing it. |

---

## Roadmap to a fully robust implementation

Phases 1–2 harden what already exists and are the ones that should block
a production deployment. Phase 3 is small. **Phase 4 (digitization) is
the largest by a wide margin and is the actual goal** — a read-only
viewer is scaffolding for it — so it is the natural boundary for a
separate PR, or several. Phases 5–6 are incremental, and Phase 6 should
not be started without measurements justifying it.

### Phase 1 — Concurrency and resource bounds

The current design is correct but unbounded. This phase is what makes it
safe to point at a 100-file catalog on a shared machine.

1. Move rendering off the async executor (`spawn_blocking`, or a
   dedicated rayon pool sized by `--n-workers`).
2. **Wire up `--n-workers`, or delete it.** Do not ship it inert.
3. Single-flight: collapse concurrent misses for the same
   `(revision, view, profile, object)` so N simultaneous requests render
   once. #119 names this explicitly.
4. Bound per-request work and honour client disconnects — a user
   scrolling fast should not queue hundreds of doomed renders.
5. Backpressure: a bounded render queue that returns 503 rather than
   growing without limit.

*Exit criterion:* a 100-radargram catalog loads without saturating tokio
workers, and `ab -c 50` against the chunk route degrades gracefully.

### Phase 2 — Persistence and invalidation

6. On-disk cache keyed on revision ID, with a documented location and an
   explicit enablement flag (#119).
7. A cache-eviction and size-budget policy for disk, mirroring the
   in-memory byte budget.
8. Verify invalidation end-to-end: reprocess a file, confirm every
   derived artefact is regenerated and none is served stale.

*Exit criterion:* a server restart does not re-render anything a previous
run already produced, and a reprocess invalidates exactly what it should.

### Phase 3 — Robustness of the client

9. Fetch wrappers that surface the structured error envelope instead of
   failing silently (#121).
10. Extract per-page JS from the templates now that there is enough of
    it to justify the move — still no build step.
11. Advertise chunk valid extents via a header or a grid-manifest
    endpoint, so the client stops re-deriving geometry the server
    already knows.

### Phase 4 — Digitization *(the point of the whole thing)*

Everything above makes the viewer robust; this is what makes it *useful*.
A read-only viewer is a means to an end — the end is picking bed and
internal reflectors and getting those picks back out as data. This phase
is the largest of the six and is the natural boundary for a separate PR,
or several.

12. **Interactive digitization in the viewer:** pick, edit and delete
    reflector traces on the radargram; snap to trace index (the
    trace-indexed track model already guarantees a pick's position is
    exact, which is precisely why it was built that way).
13. **Labelling categories** (#115) — bed, internal layers, surface,
    uncertain — as a typed, server-defined vocabulary rather than
    free-form strings, for the same reason render profiles are
    server-defined.
14. **Persistence and a write path.** This is the first time the server
    stops being read-only, and it changes the threat model completely:
    it needs an ownership/authorship model, concurrent-edit semantics,
    and a storage format that is not the processed NetCDF itself
    (picks must survive reprocessing, which by definition changes the
    revision ID).
15. **gprinterp development in step with it.** gprinterp is the intended
    consumer of `ridal_radargram_id` and the reason that ID is stable
    across reprocessing. The interchange format between the two — how a
    pick made against one revision is carried forward to the next — has
    to be designed jointly, not bolted on afterwards. Doing the viewer
    side first and inferring the contract later is the main risk in this
    phase.
16. Topographically corrected view as a second `DatasetView` — needed
    for digitization to be interpretable, not just cosmetic.

### Phase 5 — Remaining capability

17. Config-file support so render settings are shared by CLI, config,
    API and GUI (#119's stated goal).
18. Viewer prev/next within a group; placeholder panels for the
    digitization layout.
19. Multiple named basemaps with proper attribution config.

### Phase 6 — Measure before optimising

20. Benchmark first paint and pan/zoom latency on a genuinely large
    radargram and a genuinely large catalog.
21. **Only if that shows a need:** multiresolution server-side `(z,x,y)`
    tiles with a formal Leaflet-zoom-to-resolution mapping (#118, #121).
    This replaces the client-side x-scale transform rather than
    extending it, so it should not be started speculatively.

### Cross-cutting, throughout

- Keep the netcdf serialisation lock in mind: it is the hard ceiling on
  read parallelism, and the ~6 % `Netcdf(-101)` test flake is a symptom
  worth revisiting if it worsens.
- Two pre-existing, unrelated test failures remain in this environment
  (`dem::tests::test_no_gdal_failure`, `coords::tests::test_projinfo_to_wkt`);
  neither is caused by this branch and both are documented.
- Verify against the real `assets/mala` fixtures, not just synthetic
  data. Every substantive bug in this branch — the 140 m cursor desync,
  the stretched edge chunks, the black `positive` overviews — was found
  by looking at real data, and two of them were invisible under the
  `default` profile.
