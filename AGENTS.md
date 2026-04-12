# AGENTS.md

## Build & Test Commands

```bash
# Build CLI only (avoids Python linkage)
cargo build --no-default-features -F cli

# Run tests (CLI only; may hide problems with python features)
cargo test --no-default-features -F cli -- --nocapture

# Full lint check (runs pre-commit, fmt, clippy)
./lint.sh

# Individual lint steps
cargo fmt --check
cargo clippy -- -Dwarnings
pre-commit run --all-files

# Build Python wheel
maturin build --release --features python --no-default-features
```

## Pre-Push Checklist

**Always run before pushing:**

```bash
# Install lint tools if needed
rustup component add rustfmt clippy
pip install pre-commit

# Run all checks
./lint.sh
```

If any step fails, fix it and re-run before pushing.

## Key Patterns

- **Feature flags**: Default includes both `cli` and `python`. Use `--no-default-features -F cli` for CLI-only builds to avoid Python linkage overhead.
- **GDAL/PROJ**: Optional but required for DEM sampling and non-WGS84 UTM CRS. Install via `gdal-bin` and `proj-bin` (Debian) or OSGeo4W (Windows).
- **Processing steps**: All GPR filters are documented in `steps.md`. Run `ridal steps` to list them.

## Architecture

- Rust CLI/library in `src/`
- Python bindings via pyo3/maturin
- Test files: `test_ridal*.py` for Python, cargo tests in `src/`
- CI: `.github/workflows/rust.yml` (builds+tests), `.github/workflows/python.yml` (wheel building)

## Testing

- Rust: `cargo test --no-default-features -F cli`
- Python: `pytest` (after installing wheel via `pip install --force-reinstall --find-links target/wheels ridal`)
