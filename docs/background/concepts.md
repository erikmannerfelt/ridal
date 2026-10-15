# Ridal's data model

Ridal's data passes through a few stages, each kept in its own form. Knowing
them makes clear what can be redone, what has to be kept, and what a pick is
tied to.

```text
raw recording ──process──▶ radargram ──pick──▶ interpretation ──export──▶ points
 .rad/.rd3/.cor            NetCDF (.nc)        gprinterp JSON              GeoJSON, CSV
                           one revision        (level 1)                   (level 2)
```

## Raw data

The recording as the radar wrote it: a header, the amplitudes and the
positions (see {doc}`../guide/file-formats`). Ridal never changes raw data.
Everything after it can be made again from it, so it is the one stage that
must be kept.

## Radargrams and revisions

Processing a recording (or several merged recordings) with a list of steps
gives a **radargram**: one NetCDF file with the amplitudes, their axes
(travel time, depth, distance, time, position, elevation) and a record of
how it was made.

A radargram has an **id**, such as `dronbreen-2025-066`, which stays the same
whenever it is processed again. Each processing of it is a **revision**,
named by the hash of its **recipe**: the raw files, the steps, the velocity,
the CRS and every other choice. Processing the same raw files the same way
gives the same revision, and changing anything gives a new one.

A radargram can also have a **display name** and belong to a **group**, a
survey or site, for people to find it by. Unlike the id, both can be changed
freely.

## Interpretations (level 1)

An **interpretation** is one person's picks on one radargram: lines, each in
a **layer**, with their vertices in the radargram's own trace and sample
numbers. This is the level 1 product: the interpretation itself, as close to
what the interpreter saw as possible, with no velocity, CRS or spacing
applied yet.

Storing picks as traces and samples rather than as depths and coordinates is
deliberate. A depth depends on the velocity, a position on the CRS and the
GNSS processing, and both can change after picking without the reflector
moving. Kept as traces and samples, a pick stays what it was, and any number
of products can be derived from it.

Interpretations are stored in the [gprinterp](#gprinterp) format, one file
per person per radargram.

## Points (level 2)

**Exporting** an interpretation gives **level 2 points**: positions along the
ground track at a chosen spacing, each with its easting and northing,
longitude and latitude, travel time and depth. This is what goes into a GIS
or an analysis. It can be made again at any time with another spacing, CRS
or, after reprocessing, velocity (see {doc}`../guide/interpretation`).

**Derived items** sit beside the picks: expressions that combine everyone's
picks at each position into a consensus line, a spread or a count. They are
evaluated when they are used and exported as points the same way.

## Projects

A **project** is a directory with a `ridal.toml`, holding radargrams and
everything people add to them: the layers, the interpretations, the derived
items and, on a server, the raw data and processing profiles. All of it lives
in `ridal_data/` beside `ridal.toml`, and the user's own files are never
touched (see {doc}`../reference/project-files`).

A **site** is a server with several projects and accounts that can be
members of them (see {doc}`../deploy/sites`). `ridal gui` serves one project
on one computer, without accounts.

## gprinterp

[gprinterp](https://docs.rs/gprinterp) is a small JSON format for GPR
interpretations, and a Rust crate that reads, validates and converts it,
developed alongside Ridal in its own repository. An interpretation is a list
of GeoJSON features whose coordinates are trace and sample numbers, with
optional metadata about the radargram it was drawn on.

That metadata is what lets picks outlive one processing. Each document can
record the **anchor axes** of its revision: for each trace, the time it was
recorded, and for each sample, its travel time or its time on the recording's
own clock. A new revision of the same recording has the same anchors at
different indices, so the picks can be placed on it exactly: across a new
zero correction, a `subset`, removed standstills or an antenna separation
correction. This is **carrying**, and gprinterp specifies how it is done.

gprinterp is a separate format so that interpretations are not locked into
Ridal. The format is open, permissive, and simple enough to read or write
from any language, and Ridal's own picks are ordinary gprinterp documents.
The format is still a draft.
