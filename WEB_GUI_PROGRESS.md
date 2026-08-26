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
| M6: HTTP app + launch modes (#120) | **next** | - |
| M7: index, viewer, sync, x-scale (#121 + new) | pending | - |
| M8: concurrency hardening | stretch | - |

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
  a real Dronbreen line). The fix stores each retained vertex's real trace
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

## Where to resume

**Next: M6, the HTTP application and launch modes (#120).** This is the
largest remaining milestone: shared axum `Router` + `AppState`, `ridal
gui` and `ridal server start` (loopback default, ephemeral port
selection, browser opening that warns-not-fails, graceful shutdown),
MiniJinja base/index/viewer/error templates, versioned API routes, embedded
frontend assets (`include_bytes!` over M0's vendored Leaflet), structured
JSON error envelopes, and the `ridal server render overview|chunk`
diagnostic CLI commands deferred from M4.

**Two things M0 already found that apply directly here**:
1. Route paths must NOT put a parameter directly before a literal
   extension -- axum 0.8 rejects `{y}.jpg` at registration
   ("Only one parameter is allowed per path segment"). Use
   `.../chunks/{profile}/{x}/{y}` (no extension) and carry format via
   `Content-Type`, exactly as M0's throwaway smoke server proved works.
2. The `[[-(y*C+C), x*C], [-(y*C), x*C+C]]` Leaflet bounds formula is
   already verified against a real headless-Chromium screenshot -- no
   need to re-derive or re-verify that math, `grid.rs` (M4) already
   implements it and is tested.

**Also update**: `AGENTS.md` build lines and both CI workflow files need
`-F cli,server` added, per the plan. `AGENTS.md` is gitignored in this
repo, so that specific edit won't show up in `git status` -- don't let
that read as "forgotten."

**Before starting M6**, worth 5 minutes: decide whether to also fix the
`test_projinfo_to_wkt` pre-existing flake noted above (unrelated to this
work, already flagged in its own code comment) -- not blocking, just
noting it's now confirmed to still occur.

**Process reminder for whoever continues this**: gate every commit with
the three commands under "Gate applied before every commit" above, run the
test suite at least 3x before trusting a green result (this session found
two real bugs and one real flake this way that a single run would have
missed), and update this file's Status table before moving to the next
milestone.

## Open questions for the user

(none blocking; see plan artifact §11 for the pre-existing open items,
which remain unanswered but non-blocking as of M3)
