# Web GUI implementation progress (#115)

Working log for the web server / browser GUI implementation. Updated after
each milestone lands. Branch: `feature/web-gui`, local only (no push, no PR,
per read-only remote access for this run).

Plan: see the artifact shared in-conversation (revision 3, chunk-benchmark
update). Milestone numbering matches that plan.

## Status

| Milestone | Status | Commit |
|---|---|---|
| M0: dependency smoke test + vendor Leaflet | done | `d150314` |
| Fixtures: MALA assets added to repo | done | `a929025` |
| M1: identity metadata (#116) | done | `1c3c04b` |
| M2: inspection + revision identity (#123, #117) | done (revision identity deferred, see below) | - |
| M3: catalog + track (#122 + new) | done | `4fd2e7a` |
| M4: streaming renderer (#118) | done | `33ab2e6` |
| M5: render service + cache (#119) | done | `f75bc6e` |
| M6: HTTP app + launch modes (#120) | done | `ac753a8` |
| M7: index, viewer, sync, x-scale (#121 + new) | done, but **incompletely** -- corrected in M7b | `8a99193` |
| M7b: thumbnails, design tokens, Nordic slugs | done | `cfda633`, `2aa2261` |
| M8: concurrency hardening | not attempted (stretch, see below) | - |
| M7c: viewer refinement round (see below) | done | `b63dbcf`, `290ea53`, `3fea81c`, `91023ed`, `2b43d8d` |

## Gate applied before every commit

```
cargo fmt --check
cargo clippy --no-default-features -F cli,server -- -D warnings --no-deps
cargo test  --no-default-features -F cli,server
```

**Known baseline failure**: `dem::tests::test_no_gdal_failure` fails in this
container (confirmed pre-existing and environment-specific: reproduced
against the unmodified manifest before any changes; CI on `main` is green).
Expected test count going forward: **N passed, 1 known failure** where N
grows as tests are added. A *new* failure beyond this one stops the run.

## M0 findings

- `gh` CLI works but is not on `PATH` in this container; full path is
  `~/.nix-profile/bin/gh`.
- Git identity was not configured. Set repo-locally (not global):
  `Erik Schytt Mannerfelt <33550973+erikmannerfelt@users.noreply.github.com>`.
  This would have silently blocked every commit overnight if unnoticed.
- **axum 0.8 route bug in the original plan**: `.../chunks/{x}/{y}.jpg` fails
  to register — "Only one parameter is allowed per path segment". Routes
  drop the extension; format is carried by `Content-Type` instead
  (`.../chunks/{profile}/{x}/{y}`, no suffix). Applies to all M6/M7 routes
  with a trailing extension on a parameterized segment.
- Verified the `L.CRS.Simple` chunk-bounds formula from the plan against a
  real headless-Chromium screenshot: `[[-(y*C+C), x*C], [-(y*C), x*C+C]]`
  places chunk (0,0) upper-left with no transposition or mirroring.
- Leaflet 1.9.4 vendored via `scripts/vendor_leaflet.sh`, per-file SHA-256
  pinned, BSD-2-Clause `LICENSE-leaflet` included.
- New Cargo dependencies (`axum`, `tokio`, `minijinja`, `blake3`, `walkdir`,
  `lru`, `webbrowser`, `toml`, `tracing[-subscriber]`) sit behind a `server`
  feature. Verified `cargo tree -F python` resolves zero axum/tokio/hyper,
  so the Python wheel is unaffected.

## M1 findings

- `assets/mala/*` triplets committed raw rather than pre-zipped: git already
  deflates blobs in the packfile (~1.45-1.9x measured on the `.rd3` files,
  matching plain `gzip -9`), and `unzip` is not installed in this container.
- `resolve_display_name`'s first draft had a real bug, caught by its own
  test: an *explicit* empty override (`--display-name ""`) must clear an
  inherited name, not fall through to it. Fixed to distinguish "flag not
  passed" (`None`, falls through) from "flag passed empty" (`Some("")`,
  clears).
- Verified live against both `assets/mala` fixtures: stem-fallback + warning
  message, explicit `--radargram-id`/`--display-name`/`--group`, rejection
  of an invalid explicit ID, and the new 256x256 HDF5 chunking all behave as
  designed. Confirmed with `netCDF4.Dataset(...)['data'].chunking()`.
- There is no "inherited" identity tier yet, by design: Ridal has no code
  path that reads its own processed `.nc` back in as input (#123 explicitly
  scopes full NetCDF-to-GPR reading as future work beyond metadata-only
  inspection). `identity::resolve_*()` already accept an `inherited`
  parameter so wiring this in later is not a redesign -- currently always
  called with `None`.

## M2 findings

- **Real, previously-unexplained bug found and fixed**: `netcdf-c`/HDF5 is
  not thread-safe for concurrent `open`/`create` across tests. The existing
  `test_save_netcdf` carried a `#[test_retry::retry]` with a comment
  "randomly fails sometimes. Unclear why" (added 2026-03-13) -- almost
  certainly this. Adding 6 new tests that also touch `netcdf::create`/`open`
  multiplied the concurrent-call surface enough to make the race reproduce
  on *every* run instead of occasionally, which is what surfaced it.
  Fixed by tagging every netcdf-touching test with
  `#[serial_test::serial(netcdf)]` (a new named lock group, kept separate
  from the existing PATH-mutation `#[serial]` tests in `dem.rs`/`coords.rs`).
  Confirmed clean across 4 consecutive full-suite runs after the fix, vs.
  failing on 3 of 4 runs before it. This will matter a lot more from M3
  onward, once catalog discovery opens many NetCDF files per test.
- `inspect_ridal_netcdf()` added to `io.rs` (unconditional, not gated behind
  `server`): metadata-only recognition via `ridal_version` +
  `ridal_processing_datetime` (legacy unprefixed fallback per M1's
  decision), typed `RidalNetcdfKind::{NotRidal, Supported}`. A malformed
  `ridal_radargram_id` or missing/non-2D `data` variable is reported as
  `NotRidal` rather than an error, per #123's stated first-implementation
  simplicity allowance.
- **Deviation from the plan**: `RevisionId`/`FastRevisionFingerprintV1` are
  deferred to M3, not implemented here as originally scoped. Reason: they
  need `blake3`, which is gated behind the `server` feature (cache
  invalidation is a server-only concern), while `inspect_ridal_netcdf`
  needs to stay available unconditionally in `io.rs` per #123 ("should be
  placed outside src/server/"). Computing the fingerprint from metadata the
  inspector already returns is a pure function that fits naturally in
  `src/server/catalog.rs` once that module exists, without requiring
  `io.rs` to depend on `blake3` for CLI-only builds.
- 7 new tests, all `#[allow(dead_code)]`-annotated pending M3's catalog
  wiring them in (the same transitional pattern used for `identity.rs` in
  M1).

## M3 findings

- **`src/server/track.rs` (new)**: `Track::from_location()` fixes the
  PFA_website cursor-sync bug directly. `format_radargrams.py` samples
  vertices evenly by *distance*; `digitize.js` indexes them by *trace
  fraction*; any standstill desynchronizes the two (measured up to 140m on
  a real Drønbreen line). The fix stores each retained vertex's real trace
  index, so lookups stay exact regardless of vertex spacing.
- **Real algorithmic gap found while building this, not just a test
  artifact**: plain 2D Douglas-Peucker is blind to velocity. A standstill
  sitting on an otherwise-straight line adds zero geometric deviation, so
  2D simplification collapses it away -- which silently reintroduces the
  exact bug class this module exists to fix, since trace-index
  interpolation between the two straight-line endpoints then implies
  constant speed straight through the standstill. **Fixed** by simplifying
  in 3D -- `(trace_index * speed_scale, easting, northing)` -- so a
  non-uniform-speed stretch shows up as a genuine chord deviation and gets
  a retained vertex. `speed_scale` is the segment's own average m/trace.
- Added a regression test that processes both real `assets/mala` fixtures
  at the exact standstill windows identified during planning
  (`subset(1200 1700 0 400)` on 2022, `subset(2400 2800 0 400)` on 2025)
  and checks *both* halves of the fix on real data in one place: the new
  method stays under the plan's 2m bound, and a faithful reimplementation
  of PFA's actual distance-indexed approach demonstrably does not.
- **`src/server/catalog.rs` (new)**: recursive discovery via `walkdir`,
  deterministic path-sorted ordering, duplicate-radargram-ID resolution
  (newest `processing_datetime` wins, path order breaks ties, collision
  retained as a warning), one bad candidate doesn't abort discovery. Group
  resolves from `ridal_group` if present, else the catalog-relative parent
  directory as a fallback slug.
- Implements the `RevisionId`/`FastRevisionFingerprintV1` deferred from M2
  (see M2 findings above for why it lives here, not in `io.rs`).
- **Second real bug found and fixed**: the hidden/cache-directory exclusion
  rule (skip dot-prefixed dirs) was being applied to the walk *root*
  itself, not just its descendants. `tempfile::tempdir()` names its
  directories `.tmpXXXXXX` -- dot-prefixed -- so every catalog test against
  a tempdir silently discovered **zero** entries until root was exempted
  from its own exclusion rule. Caught immediately by the test suite (7 of
  12 catalog tests failed with `entries.len() == 0`), not left latent.
  Worth remembering for `ridal gui`/`ridal server start` too: a real user
  could plausibly point the server at a dot-prefixed directory.
- `io.rs`'s `inspect_ridal_netcdf()` and helpers move from unconditional
  `#[allow(dead_code)]` to `#[cfg_attr(not(feature = "server"), allow(dead_code))]`,
  since `catalog.rs` is their first real (non-test) caller and it only
  exists under the `server` feature. More honest than a blanket allow.
- The M2 netcdf flake (see below) reappeared once under the *added* load of
  M3's tests before the M2 fix was strengthened -- see the updated M2 entry
  below for what changed.

## M2 fix, revisited during M3

The `#[serial_test::serial(netcdf)]` fix from M2 measurably reduced but did
**not** fully eliminate the netcdf-c/HDF5 concurrency flake once more
netcdf-touching tests were added in M3: stress-testing showed roughly 1
failure in 10 full-suite runs (down from ~3 of 4 before any fix), always
`NetCDF: HDF error` when reopening a file the same test had just written.
Serial execution (`--test-threads=1`) was 100% clean across 3 runs,
confirming this is concurrency-related, not a logic bug.

**Fix strengthened**: added `#[test_retry::retry]` alongside the existing
serial tag on all netcdf-touching tests, matching the codebase's own
pre-existing precedent for this exact issue (`test_save_netcdf` already
carried retry with a "randomly fails, unclear why" comment before this
session). Confirmed 0 unexpected failures across 25 stress-test runs after
combining both. This is believed to be an inherent limitation of the
statically-linked netcdf-c/HDF5 build in this environment, not something
fixable at the Rust call-site level within this session's scope --
worth a closer look with more time (e.g. checking whether `netcdf-sys`'s
lock actually wraps every FFI entry point, or whether a threadsafe HDF5
build is available).

## M4 findings

- Built the full pipeline: `render/grid.rs` (chunk/overview geometry),
  `render/resample.rs` (area-weighted mean, NaN-aware), `render/profile.rs`
  (the 3 built-in profiles), `render/stats.rs` (sampled amplitude limits),
  `render/colormap.rs` (normalize + grayscale + encode), `render/renderer.rs`
  (ties it together), `source.rs` (windowed netcdf reads). 169 tests, 168
  passing (1 opt-in `#[ignore]`d real-asset check).
- **No new bugs found this milestone** -- every test passed on its first
  real run, including the seam-consistency test and both orientation
  guards. Credit goes to M0's early verification of the Leaflet bounds
  formula and M1's chunk-size decision: both were exactly right when
  finally exercised for real here.
- **Verified against real acquisition data, not just synthetic fixtures.**
  An opt-in test (`manual_visual_check_against_real_asset`, run with
  `--ignored`) processes a real MALA asset, estimates its amplitude limits
  (came out to -1093.7..1161.6 on the 2022 asset), and renders an overview
  plus two chunks. All three visually inspected: the overview shows a
  recognizable direct-wave band and a plausible arcing subsurface
  reflector; the direct-wave-band chunk shows the same banding at full
  resolution; a mid-radargram chunk shows clean noise-like clutter texture
  with no transposition, mirroring, tiling, or corruption artifacts.
- **Deferred, deliberately, within #118's own stated scope**:
  - The `ridal server render overview|chunk` diagnostic CLI commands --
    these need the CLI/HTTP wiring that belongs in M6, not before it.
  - The LRU block-alignment read cache from the plan's §05 (one HDF5-chunk
    read serving all render chunks inside it). `source.rs`'s *correctness*
    property (never loading a full radargram) does not depend on this --
    it's a pure performance layer that belongs with M5's caching
    infrastructure rather than duplicated ahead of it.
  - Multilevel `(z, x, y)` tiling: explicitly out of scope for the entire
    first milestone per #115/#118, not specific to this session.

## Deviations from the plan so far

- Route paths drop file extensions (axum constraint, M0). Plan will be
  updated to match before M6.
- `RevisionId`/`FastRevisionFingerprintV1` moved from M2 to M3 (see above).
- Diagnostic CLI render commands moved from M4 to M6 (see M4 findings).

## M5 findings

- Built `render/service.rs`: `RenderVariantId`/`RenderObjectKey` (blake3
  over revision ID + view + `profile.cache_key_fragment()` from M4 +
  version constants), `ByteBoundedCache` (wraps `lru::LruCache` with
  manual byte tracking, since the `lru` crate itself is item-count-
  bounded, not byte-bounded), and `RenderService` composing `SourceReader`
  + `Renderer` (both M4) with the cache in the required order.
- No new bugs this milestone either -- all 7 new tests passed first try,
  including the one that actually matters most: reusing sampled limits
  across multiple chunks in the same variant (verified by asserting
  `limits_cache.len() == 1` after rendering two different chunks).
- **Deliberate scope decision, not a shortfall**: `RenderService`'s
  concurrency policy (bound rendering via `--n-workers` +
  `spawn_blocking`, keep reads serialized behind netcdf's lock) is
  *documented* in the module but not yet *enforced* by concrete async
  primitives. Reasoning: `&mut self` methods already serialize read
  access correctly for now, and there is no concurrent caller yet to
  bound -- that's HTTP request handling, which is M6. Wiring a semaphore
  around nothing untested seemed worse than deferring it to where it's
  actually exercised.
- `--source-cache-mb` remains a documented placeholder in
  `RenderServiceConfig` -- the read cache it configures was deferred from
  M4 and is deferred again, now explicitly to M6/M8 once real HTTP load
  exists to justify it.

## M6 findings

- Built `app.rs` (AppState + router), `routes.rs` (handlers), `templates.rs`
  + `templates/*.jinja` (MiniJinja base/index/viewer/error shells),
  `assets.rs` (embedded Leaflet via `include_bytes!`), `launch.rs`
  (`ridal gui` / `ridal server start`, sharing one `serve()`).
  `AppState::build()` opens every catalog entry's `RenderService` eagerly
  at startup (matches #122/#123's ~100-file target scale) so a broken
  file is a clear startup warning, not a request-time surprise.
- API versioned under `/api/v1`: health, profiles, datasets,
  dataset detail, overview, chunks. Structured JSON error envelope
  throughout, correctly distinguishing structurally-invalid input (400)
  from legitimately-absent (404) per #118's explicit requirement --
  covered by a comprehensive route test that checks every status code and
  error `code` field, not just "the request succeeded."
- **Real bug caught immediately by that same test suite**: the viewer
  template's `{{ radargram_id | tojson }}` needs minijinja's `"json"`
  feature, which isn't enabled -- every single `/view/{id}` request
  500'd. Fixed by interpolating the (already-validated,
  `[a-z0-9_-]`-only) radargram ID directly instead of adding a whole
  crate feature for one substitution.
- **Verified end-to-end against real data, beyond what the plan strictly
  asked for.** Processed both `assets/mala` fixtures into a real
  two-radargram catalog (one with a display name, one in a group),
  started `ridal server start` for real (not a test harness), and drove
  it with headless Chromium exactly as in M0. Confirmed: the index page
  lists both radargrams with correct effective labels and group; the
  viewer page for the 2022 asset loads all 80 real chunks
  (10x8, matching `ceil(2529/256) x ceil(1988/256)` exactly) with zero
  console errors; screenshots show actual recognizable GPR structure, not
  placeholder or corrupted output.
- **A false claim from earlier in this session, found and corrected**: I
  had reported `cargo check -F python` as clean during the mid-session
  audit. It actually fails to build the *binary* target
  (`main.rs` calls `cli::main()`, which is gated behind
  `#[cfg(feature = "cli")]`). Traced this all the way down: it reproduces
  identically with every uncommitted M6 change stashed away, and the same
  gating already existed on `main` at `05b9611`, before this session
  touched anything. It's harmless in practice -- `maturin` only packages
  the `cdylib` for the Python wheel, never the `[[bin]]` target, and
  `cargo check -F python --lib` (what actually matters) is clean. Not
  fixed, since it's unrelated to #115 and pre-existing; flagging the
  correction here rather than letting a wrong claim stand uncorrected.
- Updated `.github/workflows/rust.yml`: the Linux job now builds and
  tests `-F cli,server`. Windows/macOS deliberately left at `-F cli`
  only -- the server feature is pure Rust so it should work there in
  principle, but that has not been verified on those platforms, and I
  did not want to claim a cross-platform guarantee I hadn't checked.
  Also updated `AGENTS.md` (gitignored, invisible to `git status`, but
  kept current for local dev).
- Diagnostic CLI commands (`ridal server render overview|chunk`) from
  #118, still deferred -- not because of new information, just genuinely
  lower priority than the actual serving path, and the renderer they'd
  wrap is already fully tested via M4's unit tests.

## M7 findings

> **Correction (see M7b below).** This milestone was reported complete
> when it was not. The index page's overview thumbnails -- an explicit
> #121 acceptance criterion -- were never implemented, and the CSS token
> set promised in the plan's §07 was never written. The user spotted the
> missing styling from a screenshot; auditing from there found the
> thumbnails too. Everything described below is accurate; it was just
> incomplete, and I should have checked the acceptance criteria off
> individually rather than declaring the milestone done.

- `track.rs` gained `read_track_from_netcdf()`, which reconstructs a
  temporary `GPRLocation` from the file's `easting`/`northing`/`time`/`crs`
  variables and calls the same, already-tested `Track::from_location()`
  -- verified byte-for-byte identical (trace indices, lon, lat to 1e-9)
  between reading a file back and extracting from the in-memory `GPR`
  that produced it.
- New routes: `.../datasets/{id}/track`, `.../groups/{group}/tracks`
  (one bad sibling is skipped, not fatal), `.../datasets/{id}/attributes`
  (the complete raw NetCDF global attribute set).
- Viewer gained: an overview map (own track + clickable grey sibling
  tracks), cursor-sync (a client-side JS port of `Track::locate_trace`,
  checked against the exact standstill/corner scenario the Rust suite
  covers and found to agree exactly), a horizontal x-scale control
  (0.25x-4x, pure `imageOverlay` bounds rewrite, no server round-trip),
  and a Metadata button opening a `<dialog>` with every attribute.
- Index page now groups entries by `ridal_group`, each group getting its
  own auto-fit Leaflet map showing every member's track.
- Verified end-to-end against a real two-radargram grouped catalog,
  served for real (not a test harness): both `/track` endpoints return
  correct Svalbard coordinates, the viewer's overview map shows the
  actual glacier with the track following the real valley on ESRI
  imagery, the index group map shows both 2022 and 2025 survey tracks
  together auto-fit to bounds, and both pages loaded with zero console
  errors across separate Chromium runs. Screenshots inspected directly,
  not just asserted programmatically.
- **A third occurrence of the netcdf-c concurrency flake, now fully
  characterized rather than newly discovered.** Extended stress testing
  (16 full-suite runs across M7's verification) found 1 failure in
  `server::catalog::tests::duplicate_radargram_ids_resolve_to_the_newest`
  with the identical `Netcdf(-101)` error already seen in M2/M3, despite
  both `#[test_retry::retry]` and `#[serial_test::serial(netcdf)]` being
  present. Traced `test_retry`'s retry count: hardcoded to 3, not
  configurable, so 3 consecutive attempts all hit the same contention
  window this one time. This is consistent with -- not new evidence
  against -- the existing pattern: `test_save_netcdf` has tolerated the
  same underlying flake via retry alone for months. Given retry+serial
  already reduced the observed rate roughly 20-30x versus before that
  fix, and further mitigation (e.g. forcing global `--test-threads=1`)
  would slow the whole suite for a ~6% residual rate on one test, this is
  documented rather than further engineered around. If it recurs
  noticeably, the next lever to pull is a cross-process file lock instead
  of `serial_test`'s in-process one -- untried because there was no
  evidence it was needed.

## M7b findings: closing the gaps M7 left open

Prompted by the user noticing missing styling in a screenshot. Auditing
from there found more than they had spotted.

- **Index overview thumbnails, never implemented** (#121: "approximately
  512 px overview" per entry; `loading="lazy"` named as the mechanism
  bounding initial render work). The `/overview` endpoint had been built
  and tested in M6 -- the index page just never used it. Entries are now
  cards with the overview image, emitted from one minijinja macro so the
  grouped and ungrouped sections cannot drift.
- **`object-fit: contain`, not `cover`** -- a correction to my own first
  draft of the plan. `OverviewSpec` scales by width only, so thumbnail
  aspect equals the radargram's own and varies enormously (the two
  fixtures are 2529x1988 and 3548x695). `cover` would crop most of a long
  profile away, and cropping a scientific preview misrepresents the data.
  Visibly confirmed in the screenshots: the wide profile letterboxes.
- **The CSS token set promised in plan §07, never written.** There were
  zero custom properties; three disconnected `<style>` blocks with
  hand-repeated values instead. Now one first-party `assets/app.css`.
- **Dark mode was declared but broken** -- `color-scheme: light dark` with
  every colour hardcoded light. Now every colour is a token with both
  values. One deliberate exception, `--color-viewer-bg`, stays dark in
  both themes: a light letterbox around a grayscale radargram destroys
  perceived contrast.
- **`app.css`/`app.js` live beside `vendor/`, not inside it**, because
  `scripts/vendor_leaflet.sh` does `rm -rf` on that directory. Verified by
  re-running the vendor script: both first-party files survive. The
  `embedded_asset!` macro's path is now relative to `assets/` with the
  `vendor/` prefix at each call site, and its doc comment says why.
- **The metadata dialog was always implemented** -- the user's doubt came
  from a `<dialog>` being invisible until `showModal()`, which a page-load
  screenshot can never show. Closed the real gap (I had only verified the
  endpoint and DOM presence, never the click path) by temporarily patching
  the template to auto-open it, screenshotting the populated dialog
  against a real radargram, then reverting and confirming a clean
  `git diff`.
- **The ø pet peeve surfaced a genuine bug.** `sanitize_to_slug("Drønbreen")`
  returned `"dr-nbreen"`; `"Ålesund"` returned `"lesund"`. Fixed with a
  narrow ø/æ/å transliteration ahead of the charset filter, matching the
  convention the repo's own asset filenames already use by hand. A test
  pins the property that makes it safe for existing catalogs: `Drønbreen`
  and `Dronbreen` now produce the *same* slug, so correcting a filename's
  spelling does not change its radargram ID.
- Processing datetimes are formatted `YYYY-MM-DD HH:MM` for display. The
  raw `to_rfc3339()` value wrapped mid-token in a narrow card. The stored
  string is untouched -- the revision fingerprint (#117) hashes it.

## M7c findings: viewer refinement round (real-usage feedback)

The first pass of feedback after M7b landed -- five commits, all local to
`feature/web-gui`.

- **`positive` render profile** (`b63dbcf`). PFA_website's own "abslog"
  turned out not to be a log transform at all: `normalize()` estimates
  percentile bounds from `|amplitude|` (skipping the direct-wave band)
  and stretches the *signed* value against those bounds, biasing the
  display toward positive returns. `RenderProfile.abslog: bool` became
  an `AmplitudeTransform` enum (`Linear`/`AbsLog`/`Positive`), because
  `Positive` needs a different domain for percentile estimation (`|x|`)
  than for the displayed pixel (signed `x`) -- something a single bool
  couldn't express, and genuinely two different functions
  (`to_stats_domain` vs `to_display_domain`) rather than one reused for
  both, unlike the two pre-existing profiles.
- **X-scale centring + distance/TWTT/depth readout** (`290ea53`).
  Changing horizontal scale used to re-lay the profile from its left
  edge, silently scrolling you away from whatever you were looking at;
  now the view re-centers on the same trace. New `/api/v1/datasets/{id}/axes`
  route serves distance/twtt/depth (each degrading independently to
  `null` for fixtures that never wrote it -- depth and distance are both
  nonlinear, so they must be served as arrays, not reconstructed
  client-side from a step size).
- **Metadata dialog presentation** (`290ea53`, `2b43d8d`). Moved from a
  raw attribute dump to a curated view: prettified labels
  (`ridal_processing_datetime` -> "Processing datetime"), `*_unit`
  attributes merged into their value's parenthetical instead of a
  separate row, every float rounded to 4dp on top of fixing the
  underlying `f32`->`f64` widening artifact at its root
  (`f32_to_f64_exact` recovers the shortest round-trip decimal, so
  `0.168_f32` no longer reaches the dialog as `0.16799999773502350`),
  start/stop datetime merged into one row, a synthetic Shape row, and a
  curated identity/acquisition/processing/everything-else order. The
  banner collapsed to a single Radargram ID row
  ("dronbreen-2022 (Rev. 493a1cc; processed ...)"); Group, Processed and
  Shape moved into the dialog. **Correction, caught in the next feedback
  round: the full revision checksum did not actually move into the
  dialog** -- `revision_id` is a server-computed fingerprint
  (`RevisionId::fingerprint_v1`), never written as a file attribute, so
  it was never in `dataset_attributes`'s `raw` map to begin with; the
  abbreviated banner form is currently the only place it appears at all.
  See "Open feedback" below. Processing
  steps/log got their own `<details>`, with the log's tab-separated
  per-step detail broken onto its own line (it used to collapse into an
  unreadable run-on paragraph, since HTML collapses whitespace).
- **Frosted popups, two-way hover highlight, header logo** (`3fea81c`).
  Sibling/group track popups gained an overview thumbnail and
  PFA_website's frosted `backdrop-filter` background, ported through the
  theme tokens (plus the `-webkit-` prefix PFA's own CSS omits) so it
  works in dark mode too. Index cards and their matching map tracks now
  highlight each other on hover in both directions, sharing one
  hover/popup-open flag (`RIDAL.bindTrackHighlight`) so the two
  highlight sources don't fight over the layer's weight when one ends
  before the other. `images/logo.svg` now shows beside the wordmark in
  the shared header via a new `embedded_repo_asset!` macro arm (the logo
  lives at the repo root, not under `src/server/assets/`); `logo.png`
  is served as `/favicon.ico`, previously unhandled entirely.
- **Group name/ID split** (`2b43d8d`, the largest of the five: 13 files).
  A group previously had one field doing double duty as both the
  display heading and the URL/API key, forcing a group's name into the
  same ASCII-slug rules as its id. Applied #116's radargram-id/
  display-name pattern one level up: `GroupName` (free-form Unicode,
  mirrors `DisplayName`) plus a derived `GroupId` slug (via
  `sanitize_to_slug`, so `--group-name "Drønbreen"` derives id
  "dronbreen" for free), with `--group-id` as an explicit override.
  `--group` keeps working as an alias for `--group-name`. Conflicting
  names across entries sharing one group id resolve exactly like a
  duplicate radargram id already does (newest `processing_datetime`
  wins, ties broken by path order, with a `CatalogWarning` either way)
  -- reused, not reinvented, via a new `Catalog::group_names` map.
- Verified end-to-end against both real `assets/mala` fixtures processed
  with `--group-name "Drønbreen"`: exported attributes, catalog
  discovery, the index heading, and the viewer's "(Group: Drønbreen)"
  all show the Unicode name, while `/api/v1/groups/dronbreen/tracks` and
  `data-group` use the derived ASCII id. Zero console errors across a
  headless-Chromium pass over the index and viewer pages (including the
  metadata dialog, verified with the same temporarily-auto-open-then-
  revert trick M7b used, confirmed via `git diff` afterward). Dark theme
  verified statically via the CSS token diff rather than a screenshot --
  this container's headless Chromium build did not honor
  `--blink-settings=preferredColorScheme` or `--force-dark-mode`, and
  the earlier M7b round already established that the token mechanism
  itself renders correctly in both themes.
- Test suite run 5x across this round's commits (4 clean, 1 hitting the
  documented `Netcdf(-101)` flake on a different test than before --
  consistent with a shared-lock concurrency flake shifting location as
  new `#[serial_test::serial(netcdf)]` tests were added, not a
  regression); `cargo fmt --check` and `cargo clippy -F cli,server`/
  `-F cli -- -D warnings` both clean throughout.

## Open feedback for a future round (logged only, not yet implemented)

Collected from real-usage feedback after M7c shipped. Deliberately not
acted on now -- logged so the next round has it, not lost to context.

- **Full revision checksum is missing from the metadata dialog entirely**
  (see the M7c correction above): `revision_id` is a server-side
  fingerprint, not a file attribute, so `dataset_attributes` never sees
  it. Needs a synthetic entry injected the same way Shape already is
  (`routes.rs::build_metadata_entries` takes `shape` as a side input;
  it would need `revision_id` passed in alongside it).
- **Add `8x` to the horizontal-scale dropdown** (currently
  0.25x/0.5x/1x/2x/4x, per M0's planning defaults). One line in
  `viewer.html.jinja`'s `<select id="xscale-select">`.
- **`Original filepaths` can be arbitrarily long** (many merged inputs,
  each a long path) and currently renders as one plain comma-joined
  metadata row. Needs its own `<details>` (like Processing steps/log)
  rather than assuming it stays short.
- **Index page: a card can already be highlighted (`.card:target`, blue
  rim, from the viewer's `/#card-{id}` back-link) when its track is also
  hover-highlighted from the group map** -- the two treatments look
  identical, so hovering such a card's track visibly does nothing. Needs
  a distinct visual state for hover vs. target (or a state that composes
  visibly with target).
- **Map tooltip readability**: the frosted/translucent popup background
  is hard to read, especially over dark satellite imagery -- and
  PFA_website has the same problem for the same reason (a translucent
  background can't guarantee contrast against arbitrary imagery
  underneath). Needs a design that doesn't depend on what's beneath it
  (e.g. a solid or near-solid background, or a text shadow/outline).
- **Popup thumbnail should be part of the link**, so clicking the image
  itself (not just the label text) navigates to the radargram.
- **Index card fields `Path` and `Processed` should move behind some
  "more info" affordance** to declutter the card -- unclear yet whether
  that's a `<details>`, a popup, or something else; needs a design
  decision, not just an implementation.
- **Ability to change the default render profile from the index page**
  (i.e. per-thumbnail or global, without visiting each viewer) was
  requested but how to store that choice is unresolved -- a URL query
  param, `localStorage`, or a server-side per-session/per-user setting
  all have different tradeoffs (shareability vs. persistence vs. needing
  no server state) that need deciding before implementation.

## Run status: M0-M7 done (M7 completed by M7b, refined in M7c), M8 not attempted

The plan's explicit target was "the first user-visible milestone (index +
viewer + map sync)" with "M8 hardening as optional stretch." That target
is reached, with M7's gaps closed in M7b. Branch not pushed anywhere, per
the read-only remote access for this run.

**M8 (concurrency hardening) was not attempted.** It is explicitly a
stretch goal in the plan, and its main components (collapsing identical
concurrent render misses, bounding per-request work, cancellation) need a
real concurrent caller to bound -- which now exists (M6's HTTP layer) but
building and testing that safely deserved fresh attention rather than
being rushed at the end of a long session. Good next increment, not a
gap in what was promised.

**Process note for whoever picks this up next**: gate every commit with
the three commands under "Gate applied before every commit" above, run
the test suite at least 3-6x before trusting a green result (this session
found several real bugs and three occurrences of one real flake this way
that a single run would have missed every time), and verify any claim
about a build/test configuration by actually running it, even one you
believe you already checked earlier in the same session -- this session
had to self-correct exactly that once, in M6.

## Open questions for the user

(none blocking implementation; see plan artifact §11 for the two
soft-default questions from planning -- x-scale step values and the
amplitude-sampling budget -- which were never answered and so were
implemented at their proposed defaults: 0.25x/0.5x/1x/2x/4x and 64 MB /
128 runs of 16 traces respectively. Both are one-line changes if the
defaults turn out wrong.)

Also unresolved, unrelated to this branch: `test_projinfo_to_wkt`
(coords.rs) is a second pre-existing flake, already flagged in its own
code comment, confirmed to still occur during this session's stress
testing. Not touched -- out of scope for #115.

## Deliberately deferred from M7b (user deselected these)

Offered and declined, recorded so they are not silently lost:

- **Viewer prev/next navigation within a group** (plan §07). Group
  membership is already computed server-side, so this stays cheap.
- **Reserved placeholder panels** for the topographically corrected view
  and labelling categories (plan §07), which would keep the v2 layout from
  being a retrofit.
- **Fetch error handling in the frontend.** Plan §07 promised "fetch
  wrappers that surface the structured API error envelope"; the page
  scripts still do a bare `.then(r => r.json())`, so a failed `/track` or
  `/groups/{g}/tracks` request silently leaves the map empty with no
  explanation. The index thumbnails *do* degrade visibly (an `error`
  listener swaps in "no preview"), but the track fetches do not. This is
  the most substantive of the three and the one worth doing first.
- **Splitting page JS into per-concern files.** Partially addressed --
  shared constants now live in `assets/app.js` -- but the per-page logic
  is still inline in the templates.

## Known follow-up worth flagging

`overview_image` and `chunk_image` (`routes.rs`) render synchronously
inside the async handler, holding that radargram's `Mutex`, with no
`spawn_blocking`. `render_overview` reads the *entire* source array. Until
M7b nothing fetched overviews en masse; a card grid does. `loading="lazy"`
plus the browser's per-origin connection limit bounds it in practice, and
`RenderService`'s cache makes repeat loads free, but first paint on a
large catalog will be slow and can occupy tokio worker threads. This is
squarely M8's territory (bounded concurrency, `--n-workers`) and is the
concrete reason to do M8 rather than a theoretical one.
