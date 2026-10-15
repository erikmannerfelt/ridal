# Ridal and other software

Ridal is one of several open tools for GPR data, and much of it was inspired
by two of them, RGPR and ImpDAR, which are older and cover more ground. This
page says what Ridal is built for, and where another tool may suit you
better. The descriptions of other tools are brief and may be out of date:
check their own documentation.

## What Ridal is built for

- **Many profiles, processed the same way**: batch processing, merging of
  recordings, and processing profiles shared across a survey, with every
  output recording exactly how it was made.
- **Interpretation by several people**: picking in the browser, each
  person's picks kept apart, and consensus layers computed from them.
- **Interpretations that outlive the processing**: picks are carried to the
  radargram's next processing rather than redrawn.
- **Large radargrams**: the server renders only the tiles being looked at,
  so the browser never has to hold a whole radargram.
- **One tool from three sides**: the command line, Python and the browser
  run the same code and give the same results.

Ridal's output is NetCDF and its interpretations are an open JSON format, so
it can be one step in a workflow that uses other tools for the rest.

## Other open tools

[RGPR](https://github.com/emanuelhuber/RGPR)
: An R package for GPR processing, visualisation and analysis. It reads many
  more formats than Ridal, and covers common-midpoint (CMP) velocity analysis
  and 3D data, which Ridal does not. A good choice when the analysis is in R.

[ImpDAR](https://github.com/dlilien/ImpDAR)
: A Python package for impulse radar on ice, with processing, migration and
  picking. It reads many more formats than Ridal, and also processes
  phase-sensitive radar (ApRES), which Ridal does not.

[GPRPy](https://github.com/NSGeophysics/GPRPy)
: A Python package with a desktop GUI for profile processing and for CMP and
  WARR velocity analysis.

[pyGIMLi](https://www.pygimli.org)
: A Python library for geophysical modelling and inversion, including
  traveltime tomography, for when the aim is a model of the subsurface
  rather than a processed radargram.

Commercial packages, such as ReflexW and the software that comes with each
radar, cover far more methods and formats, and are often what a radar's
manufacturer supports.

## When another tool may suit you better

- Your data are in a format Ridal does not read (see
  {doc}`../guide/file-formats`).
- You need CMP or WARR velocity analysis, or 3D grids of profiles.
- You work with phase-sensitive radar, or need inversion or tomography (see
  {doc}`scope`).
- You need a processing method that is not among Ridal's steps (see
  {doc}`../reference/steps`).

If what you need fits what Ridal is for, consider
[opening an issue](https://github.com/erikmannerfelt/ridal/issues): see
{doc}`scope` for where its boundaries are.
