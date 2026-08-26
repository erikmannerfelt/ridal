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
| M3: catalog + track (#122 + new) | pending | - |
| M4: streaming renderer (#118) | pending | - |
| M5: render service + cache (#119) | pending | - |
| M6: HTTP app + launch modes (#120) | pending | - |
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

## Deviations from the plan so far

- Route paths drop file extensions (axum constraint, M0). Plan will be
  updated to match before M6.
- `RevisionId`/`FastRevisionFingerprintV1` moved from M2 to M3 (see above).

## Open questions for the morning

(none yet)
