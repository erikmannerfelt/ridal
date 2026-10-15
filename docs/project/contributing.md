# Contributing

Bug reports, ideas and pull requests are welcome on
[GitHub](https://github.com/erikmannerfelt/ridal). For anything larger than
a small fix, open an issue first, so that the approach can be agreed before
the work is done.

## Building from source

Ridal is written in Rust. Building it needs a Rust toolchain
([rustup](https://rustup.rs)) and the development files of HDF5, NetCDF and
curl, and CMake 3.26 or later (a pip-installed CMake does not work). On
Debian and Ubuntu:

```bash
sudo apt-get install cmake libhdf5-dev libnetcdf-dev libcurl4-openssl-dev
```

With Nix, `nix develop` gives a shell with all of them, and GDAL, PROJ and
maturin too.

Then build the command-line tool, with or without the browser GUI and server:

```bash
cargo build --no-default-features -F cli,server
cargo build --no-default-features -F cli
```

The default features also build the Python extension, which links against
Python; leave it out with `--no-default-features` unless you need it.

The Python package is built with [maturin](https://www.maturin.rs):

```bash
pip install maturin
maturin build --release --features python --no-default-features
pip install target/wheels/ridal-*.whl
```

## Running the tests

```bash
cargo test --no-default-features -F cli,server --bin ridal
```

Always pass `--bin ridal`: the library and the binary declare the same
modules, so without it every test is built and run twice. The full suite
takes minutes; while working on one part, filter to it, such as
`cargo test --no-default-features -F cli --bin ridal steps::`.

Eight tests need GDAL and PROJ on the `PATH` (`gdal-bin` and `proj-bin`), and
fail with "PROJ (projinfo) cannot be found" without them.

The Python tests (`test_ridal*.py`, and the HTTP client's in `tests/client/`)
run with `pytest` against an installed wheel. The client's tests against a
real server skip unless `RIDAL_BIN` points at a `ridal` built with
`-F cli,server`.

Before pushing, run the linters, as CI does:

```bash
cargo fmt --check
cargo clippy --no-default-features -F cli,server -- -D warnings
pre-commit run --all-files
```

and, for changes to the Python package, `ruff check python tests`.

## How the code is organised

| Path | What is there |
|---|---|
| `src/main.rs`, `src/cli.rs` | The command line. |
| `src/lib.rs` | The Python bindings. |
| `src/io/`, `src/formats.rs` | Reading raw formats, and writing the NetCDF output. |
| `src/gpr.rs` | The radargram in memory, and most processing. |
| `src/steps/` | The registry of processing steps: names, arguments, help and versions. |
| `src/filters/` | The larger filters, one per module. |
| `src/coords.rs`, `src/dem.rs` | CRS handling and DEM sampling. |
| `src/render/` | The renderer, shared by `ridal render` and the server. |
| `src/interp/` | Interpretations: checks, carrying, export, and derived items. |
| `src/project/` | Projects and everything they store. |
| `src/server/` | The web server: routes, templates and the browser code. |
| `python/ridal/` | The Python package around the extension, and the HTTP client. |
| `docs/` | This documentation. |

`ARCHITECTURE.md` in the repository explains the design of the server, the
renderer and the interpretation model, and why it is the way it is.

### Changing what processing writes

A processed file records the recipe that made it, and revisions are named
after their recipes. A change to what a step writes therefore has to bump
that step's version in `src/steps/mod.rs`, and a change elsewhere that
reaches the output (reading, merging, coordinates, depth, export) bumps
`recipe::CORE_PROCESSING_VERSION`. The `step_digests` test catches a change
that was not declared. After bumping, regenerate the digests:

```bash
UPDATE_STEP_DIGESTS=1 cargo test --no-default-features -F cli --bin ridal step_digests
```

### Adding a processing step

A step is a variant of the `Step` enum in `src/steps/mod.rs`. Its doc comment
is its help text, which `ridal steps`, the GUI and
{doc}`../reference/steps` all show, so write it for users. A new step also
needs a run in the digest test (`RUNS` in `src/steps/digests.rs`).

## Documentation

The documentation is a Sphinx site in `docs/`, in MyST Markdown. Some pages
are generated from the code and must be regenerated, not edited:

| Page | Regenerate with |
|---|---|
| `docs/reference/steps.md` | `UPDATE_STEPS_MD=1 cargo test --no-default-features -F cli --bin ridal steps_md` |
| `docs/reference/cli.md` | `UPDATE_CLI_MD=1 cargo test --no-default-features -F cli,server --bin ridal cli_md` |

To build the site, install `docs/requirements.txt` and a wheel of the current
tree into a virtual environment, then:

```bash
sphinx-build -W -b html docs docs/_build/html
```

`-W` turns warnings into errors, as on Read the Docs.
