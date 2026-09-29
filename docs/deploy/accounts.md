# Accounts and permissions

There are two separate ideas, and keeping them apart makes everything else
simpler:

- A **site account** is a person: a name and a password, at the site root.
- A **membership** is that person's role in one project. A project holds
  members; it never holds a password, so it stays portable.

Until a site has accounts, everyone who opens it is the same anonymous
reader. A lone project served with `ridal gui` behaves the same way, with an
implicit `default` user who can do everything. That is exactly right on your
own computer. As soon as other people can reach the server, make a
[site](sites) and give it accounts.

:::{important}
Accounts use passwords, and passwords must only travel over HTTPS. Read
{doc}`reverse-proxy` before anyone signs in over a network.
:::

## The first administrator

The first account has to be made on the machine that holds the site, since
there is nobody yet who could approve it in the browser:

```console
$ ridal site account add jane --server-admin --path /srv/ridal
```

This prints a one-time invite link, as a path like `/invite/…`. Put the
address people use to reach the server in front of it, and send it to Jane
the way you would send a password. She opens it, chooses her own password,
and is signed in. The link works once and expires after 7 days.

From then on, the site has a **server administrator**, who creates projects
and accounts and acts as an administrator in every project. New accounts are
made in the browser at **Settings → Accounts**; see below.

## Server administrators

The server-administrator flag on an account is not a project role. A server
administrator creates projects and accounts, renames, archives and deletes
projects, and has every project's `admin` rights without needing a
membership.

Granting it is deliberately deliberate. The account must already have set a
password — never an invite, whose link would otherwise hand the whole site
to whoever used it — and the change is confirmed in the browser. The last
server administrator cannot be demoted or removed, or nobody could manage
the site.

## Project roles

Each membership has one role, and each role can do everything the roles
above it can:

| Role | Can |
|---|---|
| `viewer` | Browse the catalog, open radargrams, and set their own display preferences. |
| `picker` | Pick and save their own interpretations, and make private derived items. |
| `operator` | Upload, replace and remove radargrams, correct their metadata, define layers and project-wide derived items, and change the project's settings. |
| `admin` | Manage this project's members and access settings, release derived results computed from everyone's picks, and set the project's size limit. |

Nobody can change someone else's picks, not even an administrator. An
interpretation belongs to the person who drew it.

## Downloads

Separately from their role, each membership has a **download scope**, which
says what that person may take away from the project. Each scope includes
the ones before it:

| Scope | Can download |
|---|---|
| `none` | Nothing. |
| `results` | Derived results, such as a consensus layer, but nobody's individual picks. |
| `picks` | Picks, as the stored files: their own, and other people's. |
| `derived` | Also level 2 points and rendered radargram images. |
| `all` | Also the radargram files themselves, and their tracks. The default. |

`results` comes before `picks` on purpose. A consensus over many people's
picks reveals less than any one person's picks, so "the results, but not the
individual interpretations" is a setting worth having.

### A person's own picks

To download their own picks as a file, a person needs the `picks` scope. To
download them as level 2 points, they need `derived`.

:::{note}
Even with `none`, a person can read their own picks through the
{doc}`HTTP API <../reference/http-api>`: the whole interpretation, as JSON.
This is how the viewer draws them, so it cannot be switched off without
breaking the viewer. A download scope decides what is offered as a file, not
whether someone can get at their own work.
:::

:::{warning}
A download scope controls downloading, not seeing. Anyone who can open a
radargram is already looking at its image and its track, and someone
determined could save those piece by piece. Use download scopes to keep bulk
downloads deliberate, and to say what the project expects of people. If
someone should not have the data at all, do not make them a member.
:::

## People who are not signed in

Under a project's **Settings → Access**, **People who are not signed in** has
two project-wide settings:

**Require a login to read the catalog**
: Off by default, so anyone can read the catalog and only signed-in people can
  change anything. When it is on, anyone who is not signed in sees nothing but
  the sign-in page.

**Anonymous downloads**
: What someone who is not signed in may download while reading is public. The
  default is `all`.

## Managing accounts

A server administrator does this under **Settings → Accounts**. Creating an
account can name a project and role, so it arrives already belonging
somewhere; the invite link that follows is shown only once. Changes to the
server-administrator flag take effect on that person's next request, even if
they are already signed in.

To reset a password, issue a new invite link for that account. The old
password keeps working until the new link is used, so a lost email never
locks anyone out.

Removing an account keeps that person's interpretations, still under their
name, since picks are part of the results rather than of the account. Their
memberships are left in place too, so recreating the same name reconnects
them.

The same can be done from the command line on the server:

| Task | Command |
|---|---|
| Add someone | `ridal site account add jane --path /srv/ridal` |
| List accounts | `ridal site account list /srv/ridal` |
| Grant or revoke server administration | `ridal site account set jane --server-admin` / `--no-server-admin` |
| Issue a new invite link | `ridal site account reset jane --path /srv/ridal` |
| Remove an account | `ridal site account remove jane --path /srv/ridal` |

## Managing a project's members

Under a project's **Settings → Members**, an administrator of that project
changes each member's role and download scope, which apply as soon as they
are made. **Add member** gives an existing site account a place in this
project. **Invite new member** creates a brand-new account and a link that
grants only this project, with the role and download scope chosen here.

A project administrator can do this without being a server administrator,
and can remove a membership — but not the account itself. Deleting,
resetting and promoting accounts stays with a server administrator on site
settings.

## A class or workshop

To give a group of people accounts at once, a server administrator opens
**Settings → Accounts → Add several people**, and a project administrator
opens **Settings → Members → Add several people** (which grants only that
project). Choose how many accounts, their role and download scope, and
either a name prefix, which gives `student-01`, `student-02` and so on, or
random friendly usernames. **Create invite links** then gives every account
its own one-time link, so that each person sets their own password.

If handing out links is impractical, **Create with generated passwords**
makes the passwords for you instead, after you confirm that you understand
the risk. Shared passwords are weaker than invite links, since you know them
too, so keep them to short, supervised sessions. Administrator accounts are
never made this way.

## Sessions and passwords

A sign-in lasts 14 days. Passwords are stored only as Argon2id hashes, and
invite links only as hashes, in the site's `accounts.json`, which is readable
by its owner only. A project's `ridal_data/users.json` holds only memberships
and the access policy — no passwords. Deleting the site's `session.key`
signs everyone out.

## Publishing a finished site

`ridal server start --read-only` makes everyone a `viewer`, whatever their
account says, and accepts no changes at all. A read-only site needs no
accounts, so it is a simple way to publish finished projects for others to
browse. It still belongs behind HTTPS.
