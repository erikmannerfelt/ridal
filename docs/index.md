# Ridal

**Fast, tested Ground Penetrating Radar processing, from the command line, from Python, or in the browser.**

Ridal reads raw GPR data, runs it through a pipeline of processing steps, and
writes the result as a self-describing NetCDF file. Radargrams can then be
browsed, interpreted and shared through a built-in web GUI.

```{figure} _static/kroppbreen_rgm.webp
:alt: A processed glacier radargram

Radargram (100 MHz Malå) of Kroppbreen in Svalbard, collected 28 Feb. 2023.
```

::::{grid} 1 2 2 2
:gutter: 3

:::{grid-item-card} Getting started
:link: getting-started/installation
:link-type: doc

Install the CLI or the Python package, and process your first file.
:::

:::{grid-item-card} User guide
:link: guide/index
:link-type: doc

Processing, the GUI, interpretation, rendering and coordinates.
:::

:::{grid-item-card} Reference
:link: reference/index
:link-type: doc

Every processing step, CLI command, Python function and HTTP route.
:::

:::{grid-item-card} Examples
:link: examples/index
:link-type: doc

Worked examples with figures, runnable as scripts or notebooks.
:::
::::

```{toctree}
:hidden:

getting-started/index
background/index
guide/index
deploy/index
reference/index
examples/index
project/index
```
