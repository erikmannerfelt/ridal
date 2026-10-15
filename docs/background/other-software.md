# Ridal and other software

Ridal is one of several open tools for GPR data, and much of it was inspired
by two of them, RGPR and ImpDAR, which are both more mature and support far
more formats and methods. This page describes what Ridal is built for and
where another tool may suit you better. The descriptions of the other tools
are brief and may be out of date, so check their own documentation before
choosing.

## What Ridal is built for

- **Many profiles processed the same way**, with batch processing, merging of
  recordings and processing profiles shared across a survey, and with every
  output recording exactly how it was made.
- **Interpretation by several people**, with picking in the browser, each
  person's picks kept apart, and consensus layers computed from them.
- **Interpretations that outlive the processing**, since picks are carried to
  the radargram's next processing instead of being redrawn.
- **Large radargrams**, since the server renders only the tiles being looked
  at and the browser never has to hold a whole radargram.
- **The same results from the command line, Python and the browser**, which
  all run the same code.

Ridal's output is NetCDF and its interpretations are in an open JSON format,
so it can also be one step in a workflow that uses other tools for the rest.

## Other open tools

[RGPR](https://github.com/emanuelhuber/RGPR)
: An R package for GPR processing, visualization and analysis. It reads many
  more formats than Ridal and covers common-midpoint (CMP) velocity analysis
  and 3D data, which Ridal does not, so it is a good choice when the
  analysis is done in R anyway.

[ImpDAR](https://github.com/dlilien/ImpDAR)
: A Python package for impulse radar on ice, with processing, migration and
  picking. It also reads many more formats than Ridal, and it processes
  phase-sensitive radar (ApRES), which Ridal does not.

[GPRPy](https://github.com/NSGeophysics/GPRPy)
: A Python package with a desktop GUI for profile processing and for CMP and
  wide-angle (WARR) velocity analysis.

[pyGIMLi](https://www.pygimli.org)
: A Python library for geophysical modeling and inversion, including
  traveltime tomography, for when the aim is a model of the subsurface rather
  than a processed radargram.

Commercial packages, such as ReflexW and the software that comes with each
radar, cover far more methods and formats, and are often what the radar's
manufacturer supports.

## When another tool may suit you better

Another tool is likely the better choice if 1) your data are in a format
that Ridal does not read (see {doc}`../guide/file-formats`), 2) you need CMP
or WARR velocity analysis or 3D grids of profiles, 3) you work with
phase-sensitive radar or need inversion or tomography (see {doc}`scope`), or
4) you need a processing method that is not among Ridal's steps (see
{doc}`../reference/steps`).

If what you need fits what Ridal is for, consider
[opening an issue](https://github.com/erikmannerfelt/ridal/issues), since
{doc}`scope` describes boundaries that can move.
