# Contributing

Bug reports, ideas and pull requests are welcome on
[GitHub](https://github.com/erikmannerfelt/ridal). For anything larger than a
small fix, please open an issue first, so that the approach can be agreed on
before the work is done.

## Building from source

Ridal is written in Rust, and building it needs a Rust toolchain
([rustup](https://rustup.rs)), the development files of HDF5, NetCDF and curl,
and CMake 3.26 or later (a pip-installed CMake does not work). On Debian and
Ubuntu, these are installed with:

```bash
sudo apt-get install cmake libhdf5-dev libnetcdf-dev libcurl4-openssl-dev
```

With Nix, `nix develop` gives a shell with all of them, as well as GDAL, PROJ
and maturin.

The command-line tool can then be built with or without the browser GUI and
server:

```bash
cargo build --no-default-features -F cli,server
cargo build --no-default-features -F cli
```

The default features also build the Python extension, which links against
Python, so it is simpler to leave it out with `--no-default-features` unless
it is needed. The Python package is built with
[maturin](https://www.maturin.rs):

```bash
pip install maturin
maturin build --release --features python --no-default-features
pip install target/wheels/ridal-*.whl
```

## Running the tests

```bash
cargo test --no-default-features -F cli,server --bin ridal
```

`--bin ridal` should always be given, since the library and the binary
declare the same modules, and every test would otherwise be built and run
twice. The full suite takes minutes, so while working on one part, it is
faster to filter the tests to that part, for example with
`cargo test --no-default-features -F cli --bin ridal steps::`.

Eight tests need GDAL and PROJ on the `PATH` (`gdal-bin` and `proj-bin`), and
they fail with "PROJ (projinfo) cannot be found" without them.

The Python tests (`test_ridal*.py`, and the HTTP client's tests in
`tests/client/`) are run with `pytest` against an installed wheel. The
client's tests against a real server are skipped unless `RIDAL_BIN` points
at a `ridal` built with `-F cli,server`.

Before pushing, run the same linters as CI:

```bash
cargo fmt --check
cargo clippy --no-default-features -F cli,server -- -D warnings
pre-commit run --all-files
```

and `ruff check python tests` for changes to the Python package.

## How the code is organized

| Path | What is there |
|---|---|
| `src/main.rs`, `src/cli.rs` | The command line. |
| `src/lib.rs` | The Python bindings. |
| `src/io/`, `src/formats.rs` | Reading raw formats, and writing the NetCDF output. |
| `src/gpr/` | The radargram in memory, with its positions and the operations that keep its per-trace state consistent. |
| `src/processing/` | Processing raw recordings: reading the inputs, running the steps, and planning and naming batches. |
| `src/processing/steps/` | The processing steps: the registry, with their names, arguments, help and versions, in `mod.rs`, and the code of each step or family of steps in a module of its own. |
| `src/inspect.rs` | `ridal info`. |
| `src/coords.rs`, `src/dem.rs` | CRS handling and DEM sampling. |
| `src/render/` | The renderer, shared by `ridal render` and the server. |
| `src/interp/` | Interpretations, i.e. checks, carrying, export and derived items. |
| `src/project/` | Projects and everything they store. |
| `src/server/` | The web server, with its routes, templates and browser code. |
| `python/ridal/` | The Python package around the extension, and the HTTP client. |
| `docs/` | This documentation. |

`ARCHITECTURE.md` in the repository explains the design of the server, the
renderer and the interpretation model, and why they are the way they are.

### Changing what processing writes

A processed file records the recipe that made it, and revisions are named
after their recipes. A change to what a step writes therefore has to bump
that step's version in `src/processing/steps/mod.rs`, and a change elsewhere that
reaches the output (reading, merging, coordinates, depth or export) has to
bump `recipe::CORE_PROCESSING_VERSION`. The `step_digests` test catches a
change that was not declared, and the digests are regenerated after a bump
with:

```bash
UPDATE_STEP_DIGESTS=1 cargo test --no-default-features -F cli --bin ridal step_digests
```

### Adding a processing step

A step is a variant of the `Step` enum in `src/processing/steps/mod.rs`,
and its doc comment is its help text, which `ridal steps`, the GUI and
{doc}`../reference/steps` all show, so it should be written for users. A new
step also needs a run in the digest test (`RUNS` in
`src/processing/steps/digests.rs`). Its code goes in the module of
`src/processing/steps/` for it or its family.

## Documentation

The documentation is a Sphinx site in `docs/`, written in MyST Markdown. Some
pages are generated from the code, and must be regenerated rather than
edited:

| Page | Regenerate with |
|---|---|
| `docs/reference/steps.md` | `UPDATE_STEPS_MD=1 cargo test --no-default-features -F cli --bin ridal steps_md` |
| `docs/reference/cli.md` | `UPDATE_CLI_MD=1 cargo test --no-default-features -F cli,server --bin ridal cli_md` |

To build the site, install `docs/requirements.txt` and a wheel of the current
tree into a virtual environment, and then run:

```bash
sphinx-build -W -b html docs docs/_build/html
```

`-W` turns warnings into errors, as on Read the Docs.
