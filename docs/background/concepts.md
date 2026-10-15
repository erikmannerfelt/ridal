# Ridal's data model

Ridal's data pass through a few stages, and each stage is kept in its own
form. Knowing the stages makes it clear what can be redone at any time, what
has to be kept, and what a pick is actually tied to.

```text
raw recording ──process──▶ radargram ──pick──▶ interpretation ──export──▶ points
 .rad/.rd3/.cor            NetCDF (.nc)        gprinterp JSON              GeoJSON, CSV
                           one revision        (level 1)                   (level 2)
```

## Raw data

The raw data are the recording as the radar wrote it: a header, the
amplitudes and the positions (see {doc}`../guide/file-formats`). Ridal never
changes raw data, and since everything after this stage can be made again
from it, it is the one stage that must always be kept.

## Radargrams and revisions

Processing a recording (or several merged recordings) with a list of steps
gives a **radargram**, which is one NetCDF file with the amplitudes, their
axes (travel time, depth, distance, time, position and elevation) and a
record of how the file was made.

A radargram has an **id**, such as `dronbreen-2025-066`, that stays the same
however many times it is processed. Each processing of it is a **revision**,
named by the hash of its **recipe**, which lists the raw files, the steps,
the velocity, the coordinate reference system (CRS) and every other choice
that went into it. Processing the same raw files the same way therefore
gives the same revision, while changing any part of the recipe gives a new
one.

A radargram can also have a **display name** and belong to a **group**
(a survey or a site) so that people can find it. Unlike the id, both can be
changed freely.

## Interpretations (level 1)

An **interpretation** is one person's picks on one radargram, stored as
lines that each belong to a **layer** and have their vertices in the
radargram's own trace and sample numbers. This is the level 1 product: the
interpretation itself, as close as possible to what the interpreter saw, with
no velocity, CRS or point spacing applied yet.

Picks are stored as traces and samples rather than as depths and coordinates
on purpose. A depth depends on the velocity and a position depends on the CRS
and the GNSS processing, and both may change after the picking is done
without the reflector having moved at all. A pick that is kept as traces and
samples stays what it was, and any number of products can be derived from it
later.

Interpretations are stored in the [gprinterp](#gprinterp) format, with one
file per person per radargram.

## Points (level 2)

**Exporting** an interpretation gives **level 2 points**: positions along
the ground track at a chosen spacing, each with its easting and northing,
longitude and latitude, travel time and depth. These are what go into a GIS
or an analysis, and they can be made again at any time with another spacing,
another CRS or, after reprocessing, another velocity (see
{doc}`../guide/interpretation`).

**Derived items** sit beside the picks as expressions that combine everyone's
picks at each position, for example into a consensus line, a measure of
spread or a count. They are evaluated when they are used and are exported as
points in the same way.

## Projects

A **project** is a directory with a `ridal.toml` file, which holds the
radargrams and everything people add to them: the layers, the
interpretations, the derived items and, on a server, the raw data and the
processing profiles. All of this lives in `ridal_data/` beside `ridal.toml`,
so the user's own files are never touched (see
{doc}`../reference/project-files`).

A **site** is a server with several projects and with accounts that can be
members of them (see {doc}`../deploy/sites`), while `ridal gui` serves one
project on one computer without any accounts.

## gprinterp

[gprinterp](https://docs.rs/gprinterp) is a small JSON format for GPR
interpretations, and also a Rust crate that reads, validates and converts
it. It is developed alongside Ridal, in its own repository. An interpretation
is a list of GeoJSON features whose coordinates are trace and sample numbers,
with optional metadata about the radargram they were drawn on.

That metadata is what lets picks outlive a single processing. Each document
can record the **anchor axes** of its revision, which are the time each trace
was recorded and, for each sample, its travel time or its time on the
recording's own clock. A new revision of the same recording has the same
anchors at different indices, so the picks can be placed on it exactly, for
example after a new zero correction, a `subset`, removed standstills or an
antenna separation correction. This is called **carrying**, and gprinterp
specifies how it is done.

gprinterp is a separate format so that interpretations are not locked into
Ridal: the format is open and permissive, it is simple enough to read or
write from any language, and Ridal's own picks are ordinary gprinterp
documents. The format is still a draft, however, so details may change
before a first stable version.
