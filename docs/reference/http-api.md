# HTTP API

The web server behind `ridal gui` and `ridal server start` is also an HTTP
API. The browser GUI uses nothing else, so anything the GUI can do, a script
can do too.

:::{note}
This page lists every endpoint, the permission it needs and what it does. A
test fails if a route is added to or removed from the server without this
page following. Request and response bodies are not described here yet; a
machine-readable OpenAPI description is planned.
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
| `GET` | `/api/v1/health` | nobody | `{"status": "ok"}` when the server is up. Never requires a login. |
| `GET` | `/api/v1/profiles` | anyone | The names of the built-in render profiles. |

## Signing in

These are the site's, at `/api/v1/auth` rather than under a project, and
never require a login, since they are how one happens.

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/auth/me` | nobody | Who the site thinks is calling: their name, whether they are signed in, whether they are a server administrator, and whether the site has accounts at all. |
| `POST` | `/api/v1/auth/login` | nobody | Sign in with a name and password. Every failure gives the same answer, so that the endpoint cannot be used to find out which accounts exist. Refused on a network-bound server without `--allow-insecure-login`; see {doc}`../deploy/reverse-proxy`. |
| `POST` | `/api/v1/auth/logout` | nobody | Sign out. Succeeds whether or not anyone was signed in. |
| `POST` | `/api/v1/auth/invite` | nobody | Set a password with a one-time invite token, and sign in. Ends every other session of that account. |

## Preferences and project settings

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/preferences` | anyone | The caller's own display preferences. |
| `PUT` | `/api/v1/preferences` | signed in | Change the caller's own display preferences, such as render profile and horizontal scale. Any role may. |
| `GET` | `/api/v1/project/settings` | anyone | The project's defaults. On a server without a project, answers `project: false`. |
| `PUT` | `/api/v1/project/settings` | `operator` | Change the project's defaults. Changing the upload size limit needs `admin`. |

## Radargrams

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/datasets` | anyone | Every radargram being served, with its group and summary metadata. |
| `POST` | `/api/v1/datasets` | `operator` | Upload a processed `.nc` file as the request body. It is validated before anything is installed. Query: `filename`, used only in error messages. |
| `GET` | `/api/v1/datasets/{radargram_id}` | anyone | One radargram's summary metadata. |
| `DELETE` | `/api/v1/datasets/{radargram_id}` | `operator` | Remove a radargram from the project, or stop serving it. |
| `GET` | `/api/v1/datasets/{radargram_id}/attributes` | anyone | Human-readable metadata, the processing steps and log, and every raw attribute of the file. |
| `GET` | `/api/v1/datasets/{radargram_id}/axes` | anyone | The `distance`, `twtt` and `depth` axes per sample and the `elevation` per trace. An axis the file lacks is `null`. |
| `GET` | `/api/v1/datasets/{radargram_id}/traces/{trace}` | anyone | One trace's amplitudes. |
| `GET` | `/api/v1/datasets/{radargram_id}/track` | anyone | The radargram's track, for the map. |
| `GET` | `/api/v1/datasets/{radargram_id}/views/topo/geometry` | anyone | The geometry of the topographically corrected view, or why it is unavailable. |
| `GET` | `/api/v1/datasets/{radargram_id}/revisions` | `viewer` | Every revision this radargram id has had. |
| `GET` | `/api/v1/datasets/{radargram_id}/properties` | `operator` | The project's overrides of this radargram's metadata, and what each field would be without them. |
| `PUT` | `/api/v1/datasets/{radargram_id}/properties` | `operator` | Change those overrides. |
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

## Downloads

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/datasets/{radargram_id}/download` | download `all` | The processed NetCDF file. |
| `GET` | `/api/v1/datasets/{radargram_id}/track.geojson` | download `all` | The track as GeoJSON in WGS84, one feature per continuous segment. |
| `GET` | `/api/v1/groups/{group}/track.geojson` | download `all` | Every track in a group as one GeoJSON file. |
| `GET` | `/api/v1/catalog/track.geojson` | download `all` | Every track being served as one GeoJSON file. |

## Interpretations

A `picker` may only write their own interpretation. Reading someone else's
needs the `picks` download scope. Reading one's own needs no scope, since
the viewer draws it; downloading it as a file (`/raw`) still needs `picks`.

| Method | Path | Needs | Description |
|---|---|---|---|
| `GET` | `/api/v1/datasets/{radargram_id}/interpretations` | anyone | Who has interpreted this radargram. |
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
| `GET` | `/api/v1/site/memberships` | server administrator | Every account and the projects it belongs to. |
| `GET` | `/api/v1/accounts` | server administrator | Every account, without hashes or invite tokens. |
| `POST` | `/api/v1/accounts` | server administrator | Create an account and mint its invite, optionally naming a project, role and download. It cannot create a server administrator. |
| `POST` | `/api/v1/accounts/bulk/invites` | server administrator | Create several invite-only accounts at once. |
| `POST` | `/api/v1/accounts/bulk/passwords` | server administrator | Create several accounts with generated passwords. |
| `PUT` | `/api/v1/accounts/{name}` | server administrator | Change the server-administrator flag. Refused if it would leave the site with none, or while the account has an invite outstanding. |
| `DELETE` | `/api/v1/accounts/{name}` | server administrator | Remove an account and its membership in every project. Picks are left in place. |
| `POST` | `/api/v1/accounts/{name}/invite` | server administrator | Issue a new invite link, for a reset or a lost one. |
| `GET` | `/api/v1/projects` | anyone | The projects the caller is a member of; every project for a server administrator. Public projects are not listed. |
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
