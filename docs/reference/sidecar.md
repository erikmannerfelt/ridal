# Sidecar files

A sidecar is a TOML file beside a raw recording, named after its stem
(`DAT_0066_A1.ridal.toml` for `DAT_0066_A1.rad`), holding values that take
precedence over what the recording's header says. It is how a correction
belongs to one recording rather than to a whole run: a survey whose antenna
separation changed between recordings gets one sidecar per recording, and
`batch-process` then needs no `--override-*` options.

The sidecar is an overlay, not a replacement. The header is still read, and
the sidecar sets only the values it names; the raw files are never changed.

```toml
schema = 1

[overrides]
antenna_separation_m = 4.0
```

## Finding and choosing a sidecar

`ridal process`, `batch-process` and `info` (and `process()`,
`batch_process()`, `read()` and `info()` in Python) read the
`<stem>.ridal.toml` beside each recording's header when there is one,
whatever the case of its name. Each recording has its own; the two channels
of a dual-channel RAMAC recording (`_A1` and `_A2`) are two recordings, with
two sidecars.

`--sidecar PATH` (`sidecar=PATH` in Python) reads another file instead, for
one recording or the lines of one IceRadar file. `--no-sidecar`
(`sidecar=False`) reads none, so the header alone can be compared with the
sidecar's result.

## Precedence

For each value, the first of these that gives it is used:

1. the option given for the run (`--override-antenna-separation`,
   `override_antenna_separation=` in Python), which then applies to every
   recording, with a warning where it differs from a sidecar's value;
2. the line's table of the sidecar (`[lines.<N>]`, IceRadar only);
3. the sidecar's `[overrides]`;
4. the header.

`ridal info` lists what the sidecar overrode next to what the header says.
While processing, Ridal prints what each sidecar decided.

## Layout

`schema`
: The layout of the file. Ridal reads `schema = 1`, and a file without it
  as that. A file with any other value is refused.

`[overrides]`
: Values that take precedence over the header's. The keys are listed
  below. An unknown key is an error, so a misspelled key is never silently
  ignored.

`[native.<format>]`
: Fields that only one format records, by the format's name as
  `ridal formats` lists it (`ramac`, `pulseekko`, `gssi`, `iceradar`). They
  are kept for writing that format, which Ridal cannot do yet, and do not
  change processing. Their contents are not checked, but the format's name
  is.

`[lines.<N>]`
: For an IceRadar file of several lines: tables for line `N` alone,
  numbered as `--lines` takes them, with the same layout as the top level
  (`[lines.3.overrides]`, `[lines.3.native.iceradar]`). A line's value takes
  precedence over the file's. A line the file does not have, and any
  `[lines]` table for a format without lines, is an error.

```toml
schema = 1

# Every line.
[overrides]
antenna_mhz = 25
antenna_separation_m = 4.0

# Line 3 only, which was recorded with the antennas further apart.
[lines.3.overrides]
antenna_separation_m = 6.0
```

## Override keys

<!-- Generated from OVERRIDE_KEYS in src/sidecar.rs. Regenerate with
UPDATE_SIDECAR_MD=1 cargo test --no-default-features -F cli sidecar_md -->

| Key | Unit | Option that takes precedence | Description |
|---|---|---|---|
| `antenna_mhz` | MHz | `--override-antenna-mhz` | The antenna's center frequency. Needed for an IceRadar file, which does not record it. |
| `antenna_separation_m` | m | `--override-antenna-separation` | The horizontal distance between the transmitter and the receiver. GSSI and IceRadar files do not record it, and it is taken as 0 m without one. |

<!-- end of generated keys -->

## In processed files

What a sidecar decided is part of the processed file's recipe
(`ridal_recipe`), per recording, so editing a sidecar gives a new revision.
Only the values count: a comment, or a file written differently with the
same values, gives the same revision. A value that an option took
precedence over did not decide anything, and is not recorded.

Recordings whose effective values differ cannot be merged into one
radargram, as with any other difference in their settings: lines of one
IceRadar file with different `[lines]` separations are refused together, as
are two files whose sidecars differ.
