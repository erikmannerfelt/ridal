# What Ridal does, and does not do

Knowing what a tool is not meant for is as useful as knowing what it is meant
for. This page says where Ridal's boundaries are, so that you can tell quickly
whether it fits your data. The boundaries can move: if something marked "not
yet" matters to you, [open an issue](https://github.com/erikmannerfelt/ridal/issues)
or get in touch.

## In scope

Impulse radar
: Common-offset profiles from impulse GPR systems. Ridal currently reads
  Malå (`.rd3`), GSSI (`.dzt`) and pulseEKKO (`.dt1`) data.

Processing 2D profiles
: Everything in {doc}`../reference/steps`: from time-zero correction and
  filtering to gain, migration and topographic correction, from the command
  line, from Python or in batches.

Interpretation
: Picking reflectors on 2D radargrams in the browser, alone or as a team,
  combining several people's picks, and exporting the result as points.

## Planned

Format conversion
: Converting between radar formats, such as `ridal convert input.rad
  output.dzt`. Ridal reads several formats but does not yet write any of
  them.

## Not yet in scope

These fit what Ridal is for, but nobody is working on them.

CMP and WARR surveys
: Common-midpoint and wide-angle surveys, and the velocity analysis done with
  them.

Multi-antenna and multi-frequency systems
: Processing data from several antennas or frequencies together.

## Not in scope

These are different enough from impulse GPR profiles to need different
tools.

Phase-sensitive radar
: Such as ApRES.

Inversion, tomography and holography
: Ridal processes and interprets radargrams; it does not invert them for
  subsurface properties.

Synthetic aperture radar
: Airborne or spaceborne SAR.

{doc}`other-software` points to tools for some of these.
