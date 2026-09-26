# Malå test recordings

Raw Malå `.rd3` data with their `.rad` headers and `.cor` positions, used by the tests and the web GUI's integration tests. They are left out of the published crate (`exclude` in `Cargo.toml`).

- `dronbreen-20220329-DAT_0237_A1`: 100 MHz, 2529 traces.
- `dronbreen-20250327-DAT_0066_A1`: 25 MHz, 3548 traces. The raw data behind the published `dronbreen-20250327-DAT_0066_A1_1`, used by `src/interp/derive_regression_tests.rs`.
- `scott_turnerbreen-20240207-DAT_0454_A1_2500-4000`: 100 MHz RTA, traces 2500-4000 of `DAT_0454_A1` in the raw data of `scott_turnerbreen-20240207-DAT_0453_A1_2` ([Zenodo 20734239](https://zenodo.org/records/20734239)). This is a salvaged recording: the fibre-optic cable was damaged and under tension, so time zero jumps from trace to trace and drifts. It is here to test `zero_corr` (`src/filters/zero_corr_asset_tests.rs`).
  - Cut from the original: the `.rd3` byte range of those traces; the `.cor` lines for them, renumbered from 1 and including the first point after the cut, so the last traces are interpolated as in the full file; and `LAST TRACE` set to 1500 in the `.rad`. Processed, the cut is identical to `subset(2500 4000)` of the full file, coordinates included.
