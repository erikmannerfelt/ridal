# Running a site

A **site** is one server hosting many projects from one address, with one
list of people. It is what `ridal server start` serves.

Use a site when the same group of people should be able to reach more than
one project without a separate account on each. If you only have one project
and work on your own machine, you do not need one: `ridal gui` serves a
single project with no accounts. The two are deliberate alternatives, and
`ridal server start` refuses a bare project rather than guessing which you
meant.

## Making a site

```console
$ ridal site init /srv/ridal --name "Glaciology 2026"
$ ridal site account add jane --server-admin --path /srv/ridal
$ ridal site project add dronbreen --name "Drønbreen 2025" --path /srv/ridal
$ ridal server start /srv/ridal
```

`site init` writes the marker and an empty `projects/` directory. The first
account must be a server administrator, since there is nobody yet who could
approve it in the browser; `site account add` prints a one-time invite link,
the same way a project's first account used to. `site project add` creates an
empty project directory, and you can copy an existing project into
`projects/<key>/` instead if you already have one.

`ridal server start` takes the **site** directory, not a project. Its
[server options](server) are on the next page.

## What is where

```text
/srv/ridal/
  ridal-site.toml      the site: its name and which projects are archived
  accounts.json        everyone who can sign in (readable by its owner only)
  session.key          signs login cookies (readable by its owner only)
  audit.json           who changed accounts, memberships and projects
  preferences/
    <name>.json        one person's site-wide settings, such as the theme
  projects/
    dronbreen/         an ordinary project directory
    share-anna/
```

The site owns **identity**: accounts, the session key and the site ledger.
Each project owns its **data** and its own memberships, exactly as it would
standing alone. A project directory never contains a password, so it stays
portable: copy it to another site, or open it with `ridal gui`.

## The browser

The **landing page** (`/`) lists the projects you are a member of; a server
administrator sees every project. A public project is **unlisted**: anyone
with its link can open it, but it is never shown to someone who is not a
member. A server administrator's cards carry the member and radargram counts
and an **Edit** menu to rename, archive or delete the project, plus a **New
project** form. Everyone else sees the list alone.

**Site settings** (`/settings`) holds what belongs to you and, for a server
administrator, to the site:

- **My site settings** — your theme, the same on every project.
- **Accounts** (server administrators) — create an account, with a project
  and role if you like; bulk-create several; reset a password; remove an
  account; and a read-only **Memberships** overview of who belongs where.
- **History** (server administrators) — the site ledger, see below.

The top-right menu offers **Projects** and **Settings** on site pages, and
**Radargram catalog**, **Layers**, **Settings** and **Projects** on a
project's pages. The Ridal wordmark always opens the project you are in.

## Who may do what

Two separate things decide what someone can do:

- A **server administrator** flag on the account. A server administrator
  creates projects and accounts, and acts as an administrator in every
  project. Granting it is a deliberate step: the account must already have a
  password, and the change is confirmed in the browser.
- A **project role** — `viewer`, `picker`, `operator` or `admin` — and a
  **download scope**, held per project. These are the same roles and scopes
  a lone project has; see {doc}`accounts`.

A project administrator (a member with the `admin` role) manages their own
project's people, and can create an account for it — but only that one
membership. Making someone a server administrator, removing their account,
or resetting a password stays with a server administrator on site settings.

## History

Every change to accounts, memberships and projects is appended to
`audit.json` at the site root, with who did it and when. Server
administrators can read the whole log under **History** on site settings;
project administrators see their own project's slice under **History** on
the project's settings page.

It is a record to work out what happened, **not a security control**: anyone
who can edit the site directory can edit the file, and Ridal has no
tamper-evidence to offer. The same is true of a project's `audit.json`,
which records changes to its radargrams.

## Archiving and deleting

Archiving a project from its card's **Edit** menu makes it read-only
everywhere, its members and access policy included, while keeping its data
and its memberships. Only an archived project can be deleted, which removes
the whole project directory for good; unarchive it instead if there is any
doubt.

## Publishing a finished site

`ridal server start --read-only` makes everyone a `viewer`, whatever their
account says, and accepts no changes at all. A read-only site needs no
accounts, which makes it a simple way to publish finished projects. Its
landing lists nothing, so share each project's link. It still
belongs behind HTTPS; see {doc}`reverse-proxy`.
