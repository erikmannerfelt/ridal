# HTTP API

The web server behind `ridal gui` and `ridal server start` is also an HTTP
API. The browser GUI uses nothing else, so anything the GUI can do, a script
can do too.

:::{note}
This page lists every endpoint, the permission it needs and what it does. A
test fails if a route is added to or removed from the server without this
page following. The tables are generated from the machine-readable
description, {download}`openapi.json`, also served at `/api/v1/openapi.json`.
It describes every endpoint here with its parameters, the schemas of the
request and response bodies the tables name, and the picks document
(`Document`, from gprinterp).
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

:::{http-endpoints} Server
:::

## Signing in

These are the site's, at `/api/v1/auth` rather than under a project, and
never require a login, since they are how one happens. A server bound to a
network address refuses passwords unless it is behind a TLS-terminating proxy
or started with `--allow-insecure-login`; see {doc}`../deploy/reverse-proxy`.

:::{http-endpoints} Signing in
:::

## Preferences and project settings

:::{http-endpoints} Preferences and project settings
:::

## Radargrams

:::{http-endpoints} Radargrams
:::

## Groups

:::{http-endpoints} Groups
:::

## Images

A figure spec, the body of `POST /api/v1/datasets/{radargram_id}/figure`, is
described in {doc}`../guide/rendering`.

:::{http-endpoints} Images
:::

## Render profiles

A **render profile** says how amplitude becomes an image: the limits, the
transforms, the colours. The built-in profiles are Ridal's; a project can add
its own, which an `operator` makes and changes, and which every member can
choose wherever a built-in is offered. A project profile cannot take a
built-in's name, nor start with `personal-`. The body of a `PUT` is the same
profile that `?format=toml` gives as a file and `ridal render
--render-profile FILE` reads, in JSON, with `name` matching the path.

Editing a profile changes how everything rendered with it looks from then on;
there are no versions. Deleting one is refused while it is the project's
default; a link or preference that names a deleted profile falls back to the
next profile it would have used. A built-in cannot be changed or deleted, but
an `operator` can hide it with `hidden_profiles` in the project settings,
which leaves it out of the lists while links that name it keep working.

A **personal** profile is one person's own, in one project, kept under
`/api/v1/preferences/render-profiles` like their other preferences: any
signed-in account may keep up to 20. Its name starts with `personal-`, and
two people may each have one of the same name, since a name is looked up
among the caller's own; a link to someone else's is a link to your own of
that name, or falls back. A personal profile draws the viewer's tiles,
figures and downloads, but never the catalog's thumbnails: those use its
`thumbnail_profile`, a built-in or project profile, or the project's default.
An `operator` can stop personal profiles with `personal_profiles` in the
project settings; the ones people have are then kept, but neither offered,
drawn nor added to, until it is turned on again.

:::{http-endpoints} Render profiles
:::

## Downloads

:::{http-endpoints} Downloads
:::

(chunked-uploads)=
## Uploads in chunks

A file can be sent in one request, as the body of the endpoint that takes
it, or in chunks, which survive a dropped connection: a DEM of several GB, a
long recording, or a processed radargram larger than a proxy in front of the
server lets through in one request (Cloudflare's limit is 100 MB).

1. Start an upload with `POST /api/v1/uploads` and `{"name": ..., "size":
   ...}`, the file's name and its whole size in bytes. The size is checked
   against the project's size limit here, and each chunk again as it comes.
2. Send each chunk with `PUT /api/v1/uploads/{upload}`, the bytes as the
   body and `Content-Range: bytes START-END/SIZE` saying where they go,
   `END` inclusive. A chunk may be at most 64 MiB; 16 to 32 MiB is a good
   size. A chunk must start where the upload has got to. One that does not
   is refused with `409 offset_mismatch`, and the offset the server has is
   in the `Upload-Offset` header.
3. Use the finished upload by naming it where the file would otherwise be
   the body: `POST /api/v1/raw?upload={upload}`, `POST
   /api/v1/datasets?upload={upload}` or `POST
   /api/v1/datasets/{radargram_id}/replace?upload={upload}`, with an empty
   body. The file is checked and stored exactly as one sent whole, and has
   the same blake3. The upload is gone afterwards, whatever the answer.

When a chunk fails, what arrived of it is kept. Ask `GET
/api/v1/uploads/{upload}` how far the upload has got (its `offset`, also in
`Upload-Offset`) and go on from there. An upload is only its sender's:
anyone else is told it does not exist. One nobody has sent to for a day is
deleted, and `expires_at` says when that will be; `DELETE
/api/v1/uploads/{upload}` deletes it straight away.

:::{http-endpoints} Uploads
:::

## Raw data

The raw recordings a radargram is made from, kept so that it can be
processed on the server. Each file is stored once, by its blake3, however
many radargrams use it, and counts toward the project's size limit. A
radargram's **source** names its recordings, in the order they are merged,
and optionally a CRS and one of the project's DEMs. A source is keyed by
radargram id, so it can be set before the radargram has been processed.
Removing a radargram from the project removes its source too; ignoring one
does not.

:::{http-endpoints} Raw data
:::

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

:::{http-endpoints} DEMs
:::

## Processing profiles

A **processing profile** is processing that many radargrams share: a name,
a list of steps and a medium velocity. It is not a catalog group, and its
radargrams may be in any number of groups. Steps are stored written out in
full, with every default filled in. Changing the steps or the velocity makes
a new version, and every version is kept; changing only the name or
description does not. A radargram is **assigned** to one profile, and may
have steps of its own that run first, as long as they only choose traces and
samples (`subset`, `remove_traces`).

:::{http-endpoints} Processing profiles
:::

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

:::{http-endpoints} Drafts
:::

## Interpretations

A `picker` may only write their own interpretation. Reading someone else's
needs the `picks` download scope. Reading one's own needs no scope, since
the viewer draws it; downloading it as a file (`/raw`) still needs `picks`.

:::{http-endpoints} Interpretations
:::

## Layers

:::{http-endpoints} Layers
:::

## Derived items

:::{http-endpoints} Derived items
:::

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

:::{http-endpoints} Level 2 points
:::

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
accounts, is asked to sign in instead (`401`, or the login page) by a
project's own endpoints and pages, for a project that exists and one that
does not alike; `GET /api/v1/projects/{key}` answers them `404` as well.

A **server administrator** is the `server_admin` flag on an account, which
is separate from a project role. A **project administrator** is a member
with the `admin` role in that project. A server administrator acts as a
project administrator in every project.

A server started `--read-only` refuses every change below with `403`
`read_only`, and an archived project refuses changes to its members, access
policy and invitations with `403` `archived`.

:::{http-endpoints} Site server
:full-paths:
:::
