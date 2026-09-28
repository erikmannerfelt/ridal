# Installation

Ridal comes in two forms that install separately: a **command-line tool**,
which also contains the browser GUI and the web server, and a **Python
package**. You can install either or both.

::::{tab-set}

:::{tab-item} Python
```bash
pip install ridal
```

The wheels are self-contained, so no Rust toolchain or system libraries are
needed.

```python
import ridal

ridal.info("path/to/file.rad")
```
:::

:::{tab-item} Cargo
Install [Rust](https://rustup.rs), then:

```bash
cargo install ridal
```
:::

:::{tab-item} Nix
Run it once in an ephemeral shell:

```bash
nix shell github:erikmannerfelt/ridal
```

Or add it as a flake input:

```nix
inputs = {
  ridal.url = "github:erikmannerfelt/ridal";
};
```

Nix builds Ridal itself, so no Rust toolchain is needed.
:::

::::

## Optional system tools

Two features call external tools, which have to be on your `PATH`:

| Tool | Needed for | Debian/Ubuntu package |
|---|---|---|
| GDAL (`gdallocationinfo`) | Sampling heights from a DEM | `gdal-bin` |
| PROJ (`projinfo`, `cs2cs`) | Any CRS other than a WGS84 UTM zone | `proj-bin` |

Without them, everything else works. Ridal reports a clear error only when you
ask for a DEM or a CRS that needs them.

## Supported radar formats

Ridal currently reads Malå (`.rd3`), GSSI (`.dzt`) and pulseEKKO (`.dt1`)
data. See {doc}`../guide/file-formats` for details.

:::{note}
Before February 2026, Ridal was called `rsgpr`. The CLI and Python interfaces
changed completely in version 0.5; see
[issue #82](https://github.com/erikmannerfelt/ridal/issues/82).
:::
