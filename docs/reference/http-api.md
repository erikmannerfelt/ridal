# HTTP API

The web server behind `ridal gui` and `ridal server start` is also an HTTP
API. The browser GUI uses nothing else, so anything the GUI can do, a script
can do too.

:::{note}
This page lists every endpoint, the permission it needs and what it does. A
test fails if a route is added to or removed from the server without this
page following. Request and response bodies are not described here yet. A
machine-readable OpenAPI description is in progress: {download}`openapi.json`,
also served at `/api/v1/openapi.json`, so far holds the schemas of the
responses the Python client reads and of the picks document (`Document`, from
gprinterp), and no routes.
:::

## Conventions

Base path
: Every endpoint is under `/api/v1`. A project's endpoints are served under
  its key, at `/api/v1/projects/{key}/…`; the tables below write them in
  the project's own shape, so `/api/v1/datasets` is
  `/api/v1/projects/{key}/datasets`. `ridal gui` serves its one project as
  `default`. The site's own endpoints, in {ref}`the last section
  <site-server>`, are at `/api/v1` itself.

Path parameters
: `{radargram_id}` is a radargram's id, `{group}` a group's id, `{user}` an
  account name and `{view}` either `standard` or `topo` (the
  topographically corrected view).

Bodies
: Requests and responses are JSON unless the endpoint is a download, which
  says what it returns.

Errors
: An error has the matching HTTP status and a JSON body of the form
  `{"error": {"code": "...", "message": "..."}}`. `code` is stable and meant
  for programs; `message` is meant for people.

Signing in
: `POST /api/v1/auth/login` sets a `ridal_session` cookie, which is then
  sent with every request. Accounts belong to a site; `ridal gui` has none,
  and every request acts as the single local user, an `operator`.

API tokens
: A script sends `Authorization: Bearer <token>` instead of a cookie; see
  {ref}`api-tokens`. A token acts as its account in the projects it is
  granted, with at most the role and download scope of its grant there, and
  the account's own membership still applies. It is refused with `403`
  `token_not_granted` in any other project, whether or not that project
  exists, and with `403` `token_not_allowed` on every site route except
  `GET /api/v1/auth/me`, `GET /api/v1/projects` and
  `GET /api/v1/projects/{key}`. A token that is malformed, revoked or
  expired gets `401` (`invalid_token`, `token_expired`), never anonymous
  access, and a token is used even when a session cookie is sent too. A
  refusal because of the grant rather than the account is `403`
  `token_limit`. Under `ridal gui` a token is ignored.

Permissions
: Each project membership has a **role**: `viewer` < `picker` < `operator`
  < `admin`, where each includes the ones before it. It also has a
  **download scope**: `none` < `results` < `picks` < `derived` < `all`. A
  server started with `--read-only` treats everyone as a `viewer`, and so
  does an archived project. The *Needs* column below gives the role or scope
  an endpoint checks; "anyone" means anyone who may read the catalog, which
  on a project that requires a login means its members.

Status codes for refusals
: `401` means signing in would help; `403` means it would not (the role or
  scope is too low, or the server is read-only); `409` with code
  `not_a_project` means the server is serving files that are not a project,
  so there is nowhere to write.

Concurrent edits
: Documents that can be edited (interpretations, layers, derived items,
  settings) are returned with an `ETag`. Send it back as `If-Match` when
  writing, and the write is refused with `412 Precondition Failed` if someone
  else changed the document in the meantime. `If-Match: *` requires the
  document to exist and `If-None-Match: *` requires that it does not. A write
  without either header overwrites unconditionally.

## Server

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/health` | nobody | `{"status": "ok", "version": …}` when the server is up, with the Ridal version it runs. Never requires a login. |
| `GET` | `/api/v1/openapi.json` | nobody | The OpenAPI description of this API, the same file as {download}`openapi.json`. Never requires a login. |
| `GET` | `/api/v1/profiles` | anyone | The names of the built-in render profiles. |

## Signing in

These are the site's, at `/api/v1/auth` rather than under a project, and
never require a login, since they are how one happens.

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/auth/me` | nobody | Who the site thinks is calling: their name, whether they are signed in, whether they are a server administrator, and whether the site has accounts at all. With an API token, also the token's id, name, grants and expiry. |
| `POST` | `/api/v1/auth/login` | nobody | Sign in with a name and password. Every failure gives the same answer, so that the endpoint cannot be used to find out which accounts exist. Refused on a network-bound server without `--allow-insecure-login`; see {doc}`../deploy/reverse-proxy`. |
| `POST` | `/api/v1/auth/logout` | nobody | Sign out. Succeeds whether or not anyone was signed in. |
| `POST` | `/api/v1/auth/invite` | nobody | Set a password with a one-time invite token, and sign in. Ends every other session of that account. |

## Preferences and project settings

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/preferences` | anyone | The caller's own display preferences. |
| `PUT` | `/api/v1/preferences` | signed in | Change the caller's own display preferences, such as render profile and horizontal scale. Any role may. |
| `GET` | `/api/v1/settings` | anyone | The project's defaults. On a server without a project, answers `project: false`. |
| `PUT` | `/api/v1/settings` | `operator` | Change the project's defaults. Changing the upload size limit or the project description needs `admin`. |

## Radargrams

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/datasets` | anyone | Every radargram being served, with its group and summary metadata. |
| `POST` | `/api/v1/datasets` | `operator` | Upload a processed `.nc` file as the request body. It is validated before anything is installed. Query: `filename`, used only in error messages. |
| `GET` | `/api/v1/datasets/{radargram_id}` | anyone | One radargram's summary metadata. |
| `DELETE` | `/api/v1/datasets/{radargram_id}` | `operator` | Remove a radargram from the project, or stop serving it. A removed one leaves its processing profile; one only stopped keeps it, shown as `hidden`. |
| `GET` | `/api/v1/datasets/{radargram_id}/attributes` | anyone | Human-readable metadata, the processing steps and log, and every raw attribute of the file. |
| `GET` | `/api/v1/datasets/{radargram_id}/axes/gprinterp` | anyone | The `coordinates.axes` block a gprinterp document drawn on the current revision should carry, as the viewer writes it when saving, with the revision it describes. `null` when the file cannot describe its axes. Picks saved without it cannot be carried onto a reprocessed revision. |
| `GET` | `/api/v1/datasets/{radargram_id}/axes` | anyone | The `twtt` and `depth` axes per sample and the `distance` and `elevation` per trace. An axis the file lacks is `null`. |
| `GET` | `/api/v1/datasets/{radargram_id}/traces/{trace}` | anyone | One trace's amplitudes. |
| `GET` | `/api/v1/datasets/{radargram_id}/track` | anyone | The radargram's track, for the map. |
| `GET` | `/api/v1/datasets/{radargram_id}/views/topo/geometry` | anyone | The geometry of the topographically corrected view, or why it is unavailable. |
| `GET` | `/api/v1/datasets/{radargram_id}/revisions` | `viewer` | Every revision this radargram id has had. |
| `GET` | `/api/v1/datasets/{radargram_id}/properties` | `operator` | The project's overrides of this radargram's metadata, and what each field would be without them. |
| `PUT` | `/api/v1/datasets/{radargram_id}/properties` | `operator` | Change those overrides. |
| `POST` | `/api/v1/datasets/{radargram_id}/replace/preflight` | `operator` | Report what replacing a radargram would do, as staging would, from the new file's axis declarations (the `RevisionDeclarations` schema in {download}`openapi.json`) instead of the file. Nothing is uploaded or staged. |
| `POST` | `/api/v1/datasets/{radargram_id}/replace` | `operator` | Upload a new revision of a radargram and report what replacing it would do to its interpretations. Nothing is installed yet. Query: `filename`. |
| `POST` | `/api/v1/datasets/{radargram_id}/replace/{token}` | `operator` | Install a staged replacement. |
| `DELETE` | `/api/v1/datasets/{radargram_id}/replace/{token}` | `operator` | Discard a staged replacement. |
| `POST` | `/api/v1/datasets/{radargram_id}/restore` | `operator` | Serve a radargram that was previously ignored. |
| `GET` | `/api/v1/catalog/ignored` | `operator` | Radargrams the project is not serving. |

## Groups

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/groups/{group}/tracks` | anyone | Every track in a group, for the maps. |
| `GET` | `/api/v1/groups/{group}/properties` | `operator` | The project's overrides of this group's metadata. |
| `PUT` | `/api/v1/groups/{group}/properties` | `operator` | Change those overrides. |

## Images

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/datasets/{radargram_id}/views/{view}/overview` | anyone | A low-resolution image of the whole radargram. Query: `profile`, `xscale`. |
| `GET` | `/api/v1/datasets/{radargram_id}/views/{view}/chunks/{profile}/{x}/{y}` | anyone | One tile of the radargram at full resolution, as the viewer requests them. |
| `GET` | `/api/v1/datasets/{radargram_id}/views/{view}/image` | download `derived` | The whole radargram as one image file. Query: `profile`, `width` (default: one pixel per trace), `format` (`png` or `jpeg`), `quality` (1–100, JPEG only). |
| `POST` | `/api/v1/datasets/{radargram_id}/figure` | download `derived` | An annotated figure, from a figure spec sent as the JSON body (see {doc}`../guide/rendering`). Query: `format` (`png`, the default, `svg`, or `json` for the sidecar of a figure without axes). Picks other than the caller's own need what the contributor overlay needs, and derived layers download `results`; a spec asking for more is refused. `409 revision_mismatch` when the spec names another revision. Picks that could not be drawn as stored are listed in the `X-Ridal-Figure-Warnings` header, as a JSON list. |

## Downloads

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/datasets/{radargram_id}/download` | download `all` | The processed NetCDF file. |
| `GET` | `/api/v1/datasets/{radargram_id}/track.geojson` | download `all` | The track as GeoJSON in WGS84, one feature per continuous segment. |
| `GET` | `/api/v1/groups/{group}/track.geojson` | download `all` | Every track in a group as one GeoJSON file. |
| `GET` | `/api/v1/catalog/track.geojson` | download `all` | Every track being served as one GeoJSON file. |

## Raw data

The raw recordings a radargram is made from, kept so that it can be
processed on the server. Each file is stored once, by its blake3, however
many radargrams use it, and counts toward the project's size limit. A
radargram's **source** names its recordings, in the order they are merged,
and optionally a CRS and one of the project's DEMs. A source is keyed by
radargram id, so it can be set before the radargram has been processed.
Removing a radargram from the project removes its source too; ignoring one
does not.

| Method | Path | Needs | Description |
|---|---|---|---|
| `POST` | `/api/v1/raw` | `operator` | Upload one raw file as the request body. Query: `filename`, required, since a file's name says what it is to its recording (`DAT_0001_A1.rd3`). Answers its `name`, `blake3` and size. Uploading a file the project already has stores nothing new. |
| `GET` | `/api/v1/raw/{blake3}` | download `all` | A stored raw file. |
| `GET` | `/api/v1/sources` | `viewer` | The ids of every radargram the project has raw data for, `radargram_ids`, whether or not it has been processed yet, and `names`, the names each source gives its radargram until it is served, by id, for those that give any. |
| `GET` | `/api/v1/sources/{radargram_id}` | `viewer` | What the radargram is made from, with an `ETag`. |
| `PUT` | `/api/v1/sources/{radargram_id}` | `operator` | Set or switch what the radargram is made from: `recordings`, a list of recordings in merge order, each a list of `{name, blake3}` as the upload answered, and optionally `crs`, an EPSG code such as `EPSG:32633` (without it, the positions go in the UTM zone that suits them best), `dem`, the id of one of the project's DEMs, `antenna_mhz` and `antenna_separation`, overriding the antenna frequency (MHz) and separation (m) the files give (each a positive number; `400 invalid_antenna_mhz` or `invalid_antenna_separation` otherwise; left out, read from the files), and `names`, `{display_name, group_name, group_id}`, what to call the radargram until it is served (the group id is derived from the name unless given; left out, the source keeps the names it had, and `null` removes them). Which file is a recording's header, data and positions is read from the names. Every recording is read before anything is saved, and what its header and positions say is in the answer. Recordings that cannot be merged into one radargram (different CRS, antenna frequency, time window or antenna separation) are refused with `400 recordings_not_mergeable`, and a DEM that does not cover them, or an antenna whose frequency the files do not give and no `antenna_mhz` does, with `400 source_unreadable`. Every draft and batch made from the source uses its CRS, DEM and antenna overrides, and changing them makes what was processed without them `stale`. Takes `If-Match`. |
| `PUT` | `/api/v1/sources/{radargram_id}/dem` | `operator` | Link the radargram to one of the project's DEMs, `{"dem": "<id>"}`, or unlink it, `{"dem": null}`, leaving its recordings as they are. The recordings are read with the DEM, as `PUT /api/v1/sources/{radargram_id}` reads them. A radargram processed before the change is stale afterwards. Takes `If-Match`. |
| `DELETE` | `/api/v1/sources/{radargram_id}` | `operator` | Forget what the radargram is made from, and take it out of its processing profile. Files no other source names are deleted after an hour. |
| `POST` | `/api/v1/ingest/plan` | `operator` | Plan many uploaded recordings as the radargrams they would make, creating nothing. Body: `recordings`, each `{files: [{name, blake3}]}`, in any order; `merge_threshold_seconds` (`null` never merges); `excluded`, `joins` and `splits`, recordings by key (a hash of their files' contents, which the answer gives) to leave out, to keep with the one before them whatever the gap, and to start a new radargram; `id_pattern` and `display_name_pattern` as `ridal batch-process` takes them; `ids` and `display_names`, typed over the patterns, by the key of an output's first recording; `group_name` and `group_id`; `crs` and `dem` as for a source; `antennas`, overrides by antenna as the headers name it, each `{mhz, separation}`, applied to every recording with that antenna before grouping and naming; and `include_tracks` (default `true`). Answers every recording (time, antenna and its frequency after any override, traces, a simplified WGS84 track, its output, or why it cannot be read), every output (its recordings in merge order, id, display name, the split that starts it, and the `antenna_mhz_override` and `antenna_separation_override` its source is to have), every antenna (what its name gives, the range of separations its files give, the overrides asked for, and how many recordings have it), and `problems`, each with a `kind`, a message and where it is: `invalid_pattern` (with the pattern and the character `span`), `invalid_group`, `invalid_id`, `id_collision`, `id_taken` (with `served` and `has_source`), `join_refused`, `unreadable` and `antenna_frequency_unknown` (an antenna whose name gives no frequency, until `antennas` gives one). A plan with problems is answered in full. Each call keeps its recordings' files from being deleted for another hour. |

## DEMs

A **DEM** gives processing the surface elevation along each radargram. It is
uploaded once for the whole project, as a raw file, and given a name and an
id; each radargram's source links to one by id. A DEM is never part of a
processing profile: which DEM covers a radargram depends on where it was
recorded, not on how it is processed. Its file is downloaded as raw data,
under download scope `all`.

A DEM's file cannot be replaced, nor the DEM deleted, while any radargram
links to it, so nobody's elevations change underneath them: to change the
DEM of linked radargrams, add another and link them to that.

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/dems` | `viewer` | Every DEM: `id`, `name`, `file` (`{name, blake3}`), its size in `bytes`, who uploaded it and when (`by`, `at`), and the `radargram_ids` linked to it. |
| `GET` | `/api/v1/dems/{dem}` | `viewer` | One DEM, as the list gives it, with an `ETag`. |
| `PUT` | `/api/v1/dems/{dem}` | `operator` | Add a DEM: `name`, and `file`, `{name, blake3}` of a file uploaded through `POST /api/v1/raw`, which GDAL must be able to read as a raster (`400 dem_unreadable` otherwise). For an existing DEM, `name` renames it and a different `file` replaces its file, refused with `409 dem_in_use` while any radargram links to it. Answers `201` when it is new. Takes `If-Match`, and `If-None-Match: *` to only create. |
| `DELETE` | `/api/v1/dems/{dem}` | `operator` | Delete a DEM. Refused with `409 dem_in_use`, naming the radargrams, while any links to it. Takes `If-Match`. |

## Processing profiles

A **processing profile** is processing that many radargrams share: a name,
a list of steps and a medium velocity. It is not a catalog group, and its
radargrams may be in any number of groups. Steps are stored written out in
full, with every default filled in. Changing the steps or the velocity makes
a new version, and every version is kept; changing only the name or
description does not. A radargram is **assigned** to one profile, and may
have steps of its own that run first, as long as they only choose traces and
samples (`subset`, `remove_traces`).

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/processing/profiles` | `viewer` | Every processing profile, each with its versions and the radargrams that use it. |
| `GET` | `/api/v1/processing/profiles/{profile}` | `viewer` | One processing profile, with an `ETag`. |
| `PUT` | `/api/v1/processing/profiles/{profile}` | `operator` | Create or change a processing profile: `name`, `steps`, `medium_velocity` and optionally `description`. Takes `If-Match`. |
| `DELETE` | `/api/v1/processing/profiles/{profile}` | `operator` | Delete a processing profile. Refused while any radargram uses it. |
| `GET` | `/api/v1/processing/assignments` | `viewer` | Which radargram uses which processing profile, and the steps of its own. |
| `PUT` | `/api/v1/processing/assignments/{radargram_id}` | `operator` | Put a radargram in a processing profile: `profile`, and optionally `pre_steps`, which may only be `subset` and `remove_traces`. Refused with `409 no_source` when the project has no raw data for it. |
| `DELETE` | `/api/v1/processing/assignments/{radargram_id}` | `operator` | Take a radargram out of its processing profile. |
| `GET` | `/api/v1/processing/activity` | `viewer` | Who is processing what now: each radargram with a draft queued or running, or in an unfinished batch item, as `radargram_id`, `kind` (`draft` or `batch`), `by` (whose draft, or who started the batch), `mine`, and `waiting` for a batch item not yet started. A draft says nothing more. `?radargram_id=` asks about one radargram. |
| `GET` | `/api/v1/processing/status` | `viewer` | Every radargram in a processing profile, and where it stands: `current`; `stale`, with the `reasons` (an older version of the profile, other steps, switched raw data, a changed DEM or CRS, or a file from before Ridal recorded what it was made from), and `custom`, true when its steps or velocity match no version of the profile; `older_processing`, meaning the same steps run by an older version of a step or of Ridal; `not_processed`; `no_raw_data`; or `hidden`, ignored in this project and kept in its profile until the ignore is lifted. Neither of the last two is ever counted as stale. `profile_version` is the version it matches, when it matches one. `?radargram_id=` asks about one radargram only. |
| `POST` | `/api/v1/processing/profiles/{profile}/reprocess` | `operator` | Start a batch: reprocess every `stale` and `not_processed` radargram in the profile (and `older_processing` ones too with `include_older: true`), at the profile's current version, and commit each with no review. `radargram_ids` limits it to those of the profile's radargrams. Answers `202` with the batch; `hidden` members are left out; members with no raw data, and members an unfinished batch is already processing, are listed as skipped. `409` when nothing needs it. |
| `GET` | `/api/v1/processing/batches` | `operator` | The 50 most recent batches, newest first, each item with its `outcome`: `waiting`, `processing`, `committed` (with `picks_not_carried`, whose interpretations did not come across whole: each `user`, its `severity`, `approximate`, `partial` or `refused`, and how many features were `dropped`), `already_current`, `skipped` (with a `reason`), `failed` (with a `message`) or `interrupted`, when the server stopped before it finished. |
| `GET` | `/api/v1/processing/batches/unseen` | `operator` | The caller's own batches that have ended (finished, or stopped by a restart) and whose report they have not dismissed, newest first. What the banner on every project page announces. |
| `GET` | `/api/v1/processing/batches/{batch}` | `operator` | One batch, item by item. |
| `POST` | `/api/v1/processing/batches/{batch}/seen` | `operator`, own | Dismiss the report of a batch the caller started, so it is no longer announced. `seen_at` says when. |

## Drafts

A **draft** is a radargram processed on the server from its raw data, to
look at before it becomes the radargram. Only an `operator` makes drafts, and
a draft is private to whoever made it: anyone else is told it does not
exist. Drafts are processed one or a few at a time, by the server's
processing queue, and kept for a day after they were last used
(`--draft-ttl`), or less when a project's finished drafts take more space
than `--draft-cache-mb` allows: the least recently used go first.

Committing a draft checks first that the radargram is still the revision it
was made against, and refuses with `radargram_moved` if someone has replaced
it, added it or removed it since. A draft of a radargram that is served is
staged as a replacement: the answer is the same consequence report and token
as `POST /api/v1/datasets/{radargram_id}/replace`, and the token commits it
in the same way. A draft of a new id is installed as an upload.

| Method | Path | Needs | Description |
|---|---|---|---|
| `POST` | `/api/v1/sources/{radargram_id}/drafts` | `operator` | Process the radargram's raw data: `steps`, and optionally `medium_velocity`. The antenna overrides are the source's (`PUT /api/v1/sources/{radargram_id}`). Answers `202` with the draft's `job` number and state. Asking again for the same draft answers the same one. `429` when the caller already has as many jobs waiting or running as the server allows one person (`too_many_jobs`), or the project as many as it may have (`project_too_many_jobs`). |
| `GET` | `/api/v1/drafts` | `operator` | The caller's drafts. |
| `GET` | `/api/v1/drafts/{job}` | `operator`, own | One draft: its `state` (`queued`, `running`, `done`, `failed`, `cancelled` or `timed_out`), the step it is on (`step`), whether every step has run and the file is being written (`writing`), and the revision it was made against. |
| `DELETE` | `/api/v1/drafts/{job}` | `operator`, own | Stop a draft, or throw a finished one away. |
| `GET` | `/api/v1/drafts/{job}/views/{view}/overview` | `operator`, own | A low-resolution image of a finished draft. Query: `profile`. |
| `GET` | `/api/v1/drafts/{job}/download` | `operator`, own; download `all` | A finished draft's NetCDF file. |
| `POST` | `/api/v1/drafts/{job}/commit` | `operator`, own | Make a finished draft the radargram, as described above. |

## Interpretations

A `picker` may only write their own interpretation. Reading someone else's
needs the `picks` download scope. Reading one's own needs no scope, since
the viewer draws it; downloading it as a file (`/raw`) still needs `picks`.

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/datasets/{radargram_id}/interpretations` | anyone | Who has interpreted this radargram. |
| `GET` | `/api/v1/interpretations` | anyone; everyone's needs `operator` | Every stored interpretation as statistics: lines, vertices, layers and the percentage of traces covered, with whether it is on the current revision, an earlier one, or archived by a removal. A caller below `operator` gets only their own. No picks are returned, so no download scope is needed. |
| `POST` | `/api/v1/datasets/{radargram_id}/interpretations/{user}/archive` | `admin` | Move someone's interpretation into the archive, where it stays attributed until purged. Conditional on `If-Match`. |
| `GET` | `/api/v1/interpretations/archived/{radargram_id}/{removal}/{user}` | download `picks` | One archived interpretation file, byte for byte. |
| `POST` | `/api/v1/interpretations/download` | download `picks` | Several interpretation files, live or archived, byte for byte in one zip, laid out as under `interpretations/`. Body: `items`, each a `radargram_id`, `user` and, for an archived one, `removal`. The zip's `manifest.json` lists what it holds and what was named but not found. |
| `POST` | `/api/v1/interpretations/archived/{radargram_id}/{removal}/{user}/restore` | `admin` | Move an archived interpretation back onto its radargram, as it was archived. Refused with `409` when the radargram is not in the catalog (`not_in_catalog`) or the user has picks on it now (`interpretation_exists`). |
| `DELETE` | `/api/v1/interpretations/archived/{radargram_id}/{removal}/{user}` | `admin` | Permanently delete one archived interpretation. |
| `DELETE` | `/api/v1/interpretations/archived/{radargram_id}/{removal}` | `admin` | Permanently delete every archived interpretation from one removal or archiving. |
| `GET` | `/api/v1/datasets/{radargram_id}/interpretations/{user}` | own, or download `picks` | One person's interpretation, with an `ETag`. |
| `PUT` | `/api/v1/datasets/{radargram_id}/interpretations/{user}` | `picker`, own | Save an interpretation. Refused if a line breaks its layer's rules, such as an overhang on a layer that does not allow them. |
| `DELETE` | `/api/v1/datasets/{radargram_id}/interpretations/{user}` | `picker`, own | Delete an interpretation. |
| `GET` | `/api/v1/datasets/{radargram_id}/interpretations/{user}/raw` | download `picks`, even for one's own | The stored interpretation file, byte for byte. |
| `GET` | `/api/v1/datasets/{radargram_id}/interpretations/{user}/carried` | own, or download `picks` | The interpretation as drawn on the radargram's current revision, when it was picked on an earlier one, with a report of what carrying it over changed. |
| `POST` | `/api/v1/datasets/{radargram_id}/interpretations/{user}/promote` | `picker`, own | Adopt the carried-over interpretation as the interpretation on the current revision. Query: `onto`, the revision the caller believes is current. |

## Layers

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/layers` | anyone | The project's layers, their rules and exclusivity groups, with an `ETag`. |
| `PUT` | `/api/v1/layers` | `operator` | Replace the layer definitions. |
| `GET` | `/api/v1/layers/usage` | anyone | How many picked features use each layer, and labels in use that no layer defines. |

## Derived items

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/derived` | anyone | The derived items the caller may see, with an `ETag`. |
| `PUT` | `/api/v1/derived` | signed in; `operator` for project-wide items | Replace the derived items. A private item can only be saved by its owner, and releasing a result computed from other people's picks to everyone needs `admin`. |
| `GET` | `/api/v1/datasets/{radargram_id}/contributors` | anyone | The interpretations the caller may see, for the layer panel. |
| `GET` | `/api/v1/datasets/{radargram_id}/derived/{item}` | download `results` | One derived item's values along the radargram. |
| `POST` | `/api/v1/datasets/{radargram_id}/derived/preview` | download `results` | Evaluate an unsaved expression over the caller's picks, without storing anything. |
| `GET` | `/api/v1/datasets/{radargram_id}/derived` | download `results` | Every derived item the caller may see, as a long-format CSV: one row per item per position. |

## Level 2 points

Exports of picked or derived layers as points with coordinates and depths.
They share these query parameters:

`spacing`
: `auto`, `per-trace`, `vertices`, or a distance in metres. A layer that
  allows overhangs is always exported as its vertices.

`format`
: `geojson` (default) or `csv`.

`crs`
: For GeoJSON geometry: omitted for WGS84, `native` for the radargram's own
  projected CRS, or any CRS that PROJ accepts. CSV always carries both.

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/datasets/{radargram_id}/interpretations/{user}/level2` | download `derived` | One person's picked layers as points, from what is saved. Query: `every_user` (`admin` only) exports every contributor instead. |
| `GET` | `/api/v1/datasets/{radargram_id}/derived/level2` | download `results` | Derived layers as points. Query: `include_unlisted`. |
| `GET` | `/api/v1/groups/{group}/level2` | download `derived` | Every interpreted radargram in a group, merged. Query: `user` (default: the caller), `every_user` (`admin` only), `derived`, `include_unlisted`. |
| `GET` | `/api/v1/catalog/level2` | download `derived` | Every interpreted radargram being served, merged. Same query as the group export. |

(site-server)=
## Site server

`ridal server start <site>` serves many projects from one root. A project's
pages live under `/p/{key}/…` and its API under `/api/v1/projects/{key}/…`;
the tables above describe that project-relative shape. This section lists the
endpoints that belong to the *site* rather than to any one project. Under
`ridal gui`, which has no site to manage, only `/api/v1/site`,
`/api/v1/site/preferences` and `/api/v1/auth/me` answer; the rest are `404`.

A project the caller may not see answers `404` exactly as one that does not
exist, to someone signed in. Someone not signed in, on a site with
accounts, is asked to sign in instead (`401`, or the login page), for a
project that exists and one that does not alike.

A **server administrator** is the `server_admin` flag on an account, which
is separate from a project role. A **project administrator** is a member
with the `admin` role in that project. A server administrator acts as a
project administrator in every project.

A server started `--read-only` refuses every change below with `403`
`read_only`, and an archived project refuses changes to its members, access
policy and invitations with `403` `archived`.

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/site` | anyone | The site's name, whether it has accounts, and who the caller is. |
| `GET` | `/api/v1/site/preferences` | signed in | The caller's own site-wide settings, currently the theme. |
| `PUT` | `/api/v1/site/preferences` | signed in | Change them. Any role may. |
| `GET` | `/api/v1/site/audit` | server administrator | The site's account, membership and project history, most recent first. |
| `GET` | `/api/v1/site/status` | server administrator | What the server's heartbeat would report now: render workers busy and waiting, the render cache, requests and processing since the last heartbeat, and the process's memory and CPU time. Reading it does not reset the heartbeat's counts. |
| `GET` | `/api/v1/site/processing` | server administrator | The processing queue: the configured limits, every job it still knows of with its project, owner, state and how long it waited and ran, and how many are queued and running and how much of the memory budget they use. |
| `DELETE` | `/api/v1/site/processing/{job}` | server administrator | Cancel anyone's processing job: a queued one never starts, a running one is stopped. Recorded in the site's history. `404` when the job has already ended or does not exist. |
| `GET` | `/api/v1/site/memberships` | server administrator | Every account and the projects it belongs to. |
| `GET` | `/api/v1/accounts` | server administrator | Every account, without hashes or invite tokens. |
| `POST` | `/api/v1/accounts` | server administrator | Create an account and mint its invite, optionally naming a project, role and download. It cannot create a server administrator. |
| `POST` | `/api/v1/accounts/bulk/invites` | server administrator | Create several invite-only accounts at once. |
| `POST` | `/api/v1/accounts/bulk/passwords` | server administrator | Create several accounts with generated passwords. |
| `PUT` | `/api/v1/accounts/{name}` | server administrator | Change the server-administrator flag. Refused if it would leave the site with none, or while the account has an invite outstanding. |
| `DELETE` | `/api/v1/accounts/{name}` | server administrator | Remove an account and its membership in every project. Picks are left in place. |
| `POST` | `/api/v1/accounts/{name}/invite` | server administrator | Issue a new invite link, for a reset or a lost one. |
| `GET` | `/api/v1/tokens` | signed in | The caller's API tokens, without their secrets. `?all=true` lists every account's, for a server administrator. |
| `POST` | `/api/v1/tokens` | signed in | Create an API token: a `name`, an `expires` (`30d`, `12w`, `2y` or `never`; default 90 days) and `grants`, each a `project`, `role` and `download` no higher than the caller's membership there. The token is in the response once and cannot be shown again. Needs a session, never a token. |
| `DELETE` | `/api/v1/tokens/{id}` | signed in | Revoke one of the caller's tokens, or any token for a server administrator. Someone else's token is `404`, as if it did not exist. |
| `GET` | `/api/v1/projects` | anyone | The projects the caller is a member of; every project for a server administrator. Public projects are not listed. With an API token, only its granted projects, with the role and download scope the token has there. |
| `POST` | `/api/v1/projects` | server administrator | Create a project at a key. |
| `GET` | `/api/v1/projects/{key}` | member, server administrator, or anyone for a public project | One project. |
| `PATCH` | `/api/v1/projects/{key}` | server administrator | Rename its display name; the key does not change. |
| `DELETE` | `/api/v1/projects/{key}` | server administrator | Delete an archived project and everything it owns. `409` `not_archived` otherwise. |
| `POST` | `/api/v1/projects/{key}/archive` | server administrator | Make the project read-only. |
| `POST` | `/api/v1/projects/{key}/unarchive` | server administrator | Reverse an archive. |
| `GET` | `/api/v1/projects/{key}/members` | project administrator | The membership list and access policy. |
| `POST` | `/api/v1/projects/{key}/members` | project administrator | Add or update an existing account's membership. |
| `POST` | `/api/v1/projects/{key}/members/invite` | project administrator | Create an account and invite it into this project only. `409` `name_in_use` for a name any project has a membership for. |
| `POST` | `/api/v1/projects/{key}/members/bulk/invites` | project administrator | Bulk-create accounts, each granted this project only. |
| `POST` | `/api/v1/projects/{key}/members/bulk/passwords` | project administrator | Bulk-create accounts with generated passwords, each granted this project only. |
| `PUT` | `/api/v1/projects/{key}/members/{name}` | project administrator | Change a member's role or download scope. |
| `DELETE` | `/api/v1/projects/{key}/members/{name}` | project administrator | Remove the membership. The account itself remains. |
| `PUT` | `/api/v1/projects/{key}/access` | project administrator | The project's read and anonymous-download policy. |
| `GET` | `/api/v1/projects/{key}/audit` | project administrator | This project's slice of the site history. |
