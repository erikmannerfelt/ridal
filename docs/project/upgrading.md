# Upgrading

What to do when moving an existing project, server or script to a new
version. {doc}`changelog` lists everything that changed.

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
