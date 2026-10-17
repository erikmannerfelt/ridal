# HTTP API

The web server behind `ridal gui` and `ridal server start` is also an HTTP
API. The browser GUI uses nothing else, so anything the GUI can do, a script
can do too.

:::{note}
This page lists every endpoint, the permission it needs and what it does. A
test fails if a route is added to or removed from the server without this
page following. The machine-readable description is {download}`openapi.json`,
also served at `/api/v1/openapi.json`: it holds the schemas of the request
and response bodies the tables name, and of the picks document (`Document`,
from gprinterp). It is being extended section by section: the tables from
Server to Level 2 points are generated from it, with their bodies; the site
server's are written by hand, and their bodies are not described yet.
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

## Downloads

:::{http-endpoints} Downloads
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
