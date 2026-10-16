# Upgrading

What to do when moving an existing project, server or script to a new
version. {doc}`changelog` lists everything that changed.

## From 0.9 to 0.10

Nothing needs migrating: projects, sites and processed files from 0.9 work
as they are, and no radargram's revision id changes.

- **`unphase` is retired** (#437). A saved processing profile, step file or
  script that names it fails when it next runs, with a message saying so.
  Remove the step; for a display of the magnitude, render with the
  `abslog` profile. Files already processed with it still open.
- **Derived layers may change** for picks drawn on an earlier revision
  (#440). They are now evaluated over the picks carried to the current
  revision, as the viewer draws them, where before they used the picks as
  stored. Picks on the current revision, and picks that cannot be carried
  (such as imports that name no revision), give the same result as before.
- **Carrying cuts lines at the edge** (#440). A line that runs past the
  edge of the target revision is now kept up to the edge and the carry is
  `partial`, where before the line was dropped. Code reading `CarryReport`
  can find the cut lines in its new `clipped` field.
- **HTTP API.** Each entry of `GET …/contributors` gains `view`, the
  document carried to the current revision (`null` when the carry is
  refused), and the `carry` report; `document` is still the one stored.
  The viewer draws `view`. `GET /groups/{g}/tracks` sends
  tracks simplified to map resolution (5 m); a radargram's own track and
  the GeoJSON downloads keep full resolution.
- **Disk.** A project gains `ridal_data/cache/tracks/`, which can be
  deleted at any time and is rebuilt as needed. Kept axes snapshots that
  nothing needs are now removed 30 days after their revision ended.
- **GUI names.** The project menu's entries are renamed (Radargram
  catalog, Layers, Processing, Project settings, All projects), and the
  viewer's **Hide radargram** button is a **Show radargram** checkbox in
  the Display menu.

## From 0.8 to 0.9

Nothing needs migrating: projects, sites and processed files from 0.8 work
as they are, and no radargram's revision id changes.

- **Processing.** A file processed by 0.9 is named by what it was made from
  (#383), so processing the same raw data with the same steps again gives
  the same revision rather than a new one. Reprocessing a radargram corrected
  with `correct_antenna_separation(slant)` gives a slightly different grid:
  depths come out up to 0.32 % deeper than before, which was the error
  (#380). Picks on such a radargram are carried onto the new revision
  correctly (#381).
- **Servers log.** `ridal gui` and `ridal server start` now write to stderr
  while they run: a line per request, a heartbeat every minute, and
  warnings. Under systemd it goes to the journal. Turn the heartbeat off
  with `--heartbeat-seconds 0`, and the request lines with
  `RUST_LOG=warn,ridal=info,ridal::access=warn`; see
  {doc}`../deploy/server`. The startup banner is unchanged.
- **Processing on a server is on by default** for operators of a writable
  project. A server limits it with the `--processing-*` and
  `--max-queued-*` flags (one job at a time and 4096 MB by default), and a
  project with `[processing] max_queued_jobs`.
- **GeoJSON** exported in a projected CRS now has a `crs` member (#376).
  WGS84 output is unchanged.
- **`batch-process`.** A bare `--render` or `--track` now writes into the
  output directory; before, it was ignored (#384).

## From 0.8.0 to 0.8.1

Nothing needs migrating. Two things in the Python client behave
differently (#362):

- **`apply()` stops on an outage.** At the first `TransportError` or
  `Unavailable` it stops, and the remaining files get the new action
  `not_attempted` rather than each failing in turn. Code that counts
  outcomes by action, or matches on every value of `Action`, should allow
  for it. Pass `stop_on_outage=False` for the 0.8.0 behaviour.
- **`502` and `504` are `Unavailable`.** It is still a `ServerError`, so
  code catching that is unaffected; code checking
  `type(error) is ServerError` for those statuses is not.

## From 0.7 to 0.8

Nothing needs migrating: projects, sites and processed files from 0.7 work
as they are.

- **Processing.** Reprocessing a profile whose traces run past its last (or
  before its first) `.cor` fix gives those traces new times (#350). Nothing
  else in the default profile changes.
- **Merging.** `batch-process --merge` no longer merges files that differ in
  antenna separation or medium velocity, so a merge of such files gives more
  groups than before (#151).
- **Python.** The extension module is now `ridal._ridal`. Code that imports
  from `ridal` is unaffected; only private names moved.
- **HTTP API.** The replace report (`CarryReport`, `Dropped` and
  `ConsequenceReport.shape`) now sends fields that do not apply as `null`
  rather than leaving them out (#339). The 503 `render_busy` is now also
  returned by group and catalog level 2 exports, which share the limit set
  by `--n-workers` (#336).

## From 0.6 to 0.7

### Processing gives different results

The default profile and several steps changed, so processing the same raw
data again gives a different radargram than 0.6 did: time zero, depth,
amplitudes and filtering all move. Reprocess on purpose, not by accident, and
do not mix radargrams from the two versions in one comparison.

To reproduce 0.6 output, for instance to compare against results already
published, spell out the steps instead of using the default profile. The two
steps that decide where samples fall in time and depth have a `legacy` method
that is kept unchanged:

| 0.6 | 0.7 equivalent |
|---|---|
| `zero_corr` | {step}`zero_corr(legacy) <zero_corr>` |
| `zero_corr_max_peak` | {step}`zero_corr(max_peak, trace, peak) <zero_corr>` |
| `correct_antenna_separation` | {step}`correct_antenna_separation(legacy) <correct_antenna_separation>` |
| `normalize_horizontal_magnitudes` | retired; the closest is {step}`dewow` |
| `dewow` | no exact equivalent; {step}`background_removal` is closer to what it did |
| `bandpass` | no exact equivalent; it is now zero-phase |

A retired step name is refused with a message that says what replaces it.

### Servers become sites

`ridal server start` takes a site, not a project, and project accounts are
gone. There is no automatic migration. To move a 0.6 server:

1. Make a site and its first account, as in {doc}`../deploy/sites`.
2. Copy the project directory into the site's `projects/<key>/`.
3. Move the project's `ridal_data/users.json` aside. It holds the old
   accounts, which a site refuses to read. Its `session.key` is no longer
   used either.
4. Create the accounts again with `ridal site account add` or
   `ridal site account add-bulk`, and add them to the project as members in
   the browser. Picks are stored under the account name, so an account
   recreated under the same name gets its picks back.
5. Set **Access** in the project's settings if it was private. The access
   policy was in the old `users.json`, and without one a project can be read
   by anyone who has its link.

`ridal gui` needs none of this: it opens a 0.6 project as before, ignores the
old accounts with a warning, and serves it at `/p/default/`.

Anything that calls the HTTP API directly needs the new URLs, which are under
`/api/v1/projects/{key}/`. {doc}`../reference/http-api` lists every route.

A project's history is now kept in `audit.jsonl`. The old `audit.json` is not
read; keep it if you want the history from before the upgrade.

### Python

Wheels are built for Python 3.11 to 3.14. On Python 3.10, stay on 0.6.1 or
upgrade Python.
