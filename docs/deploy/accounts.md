# Accounts and permissions

Until a project has accounts, everyone who opens it is the same user, called
`default`, and there is no password. `default` can pick, edit layers and
change the project's settings. That is exactly right when you work on your
own computer. As soon as other people can reach the server, give the project
real accounts, so that everyone signs in as themselves and can only do what
their role allows.

:::{important}
Accounts use passwords, and passwords must only travel over HTTPS. Read
{doc}`reverse-proxy` before anyone signs in over a network.
:::

## The first administrator

The first account has to be made on the machine that holds the project,
since there is nobody yet who could approve it in the browser:

```console
$ ridal project user add jane --role admin
```

This prints a one-time invite link, as a path like `/invite/…`. Put the
address people use to reach the server in front of it, and send it to Jane
the way you would send a password. She opens it, chooses her own password,
and is signed in. The link works once and expires after 7 days.

From then on, the project has accounts. The `default` user no longer exists,
anyone who is not signed in can only read, and an administrator can add
everyone else from **Settings → Access** in the browser.

## Roles

Each account has one role, and each role can do everything the roles above it
can:

| Role | Can |
|---|---|
| `viewer` | Browse the catalog, open radargrams, and set their own display preferences. |
| `picker` | Pick and save their own interpretations, and make private derived items. |
| `operator` | Upload, replace and remove radargrams, correct their metadata, define layers and project-wide derived items, and change the project's settings. |
| `admin` | Manage accounts and the access settings, release derived results computed from everyone's picks, and set the project's size limit. |

Nobody can change someone else's picks, not even an administrator. An
interpretation belongs to the person who drew it.

## Downloads

Separately from their role, each account has a **download scope**, which says
what it may take away from the server. Each scope includes the ones before it:

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
someone should not have the data at all, do not give them an account.
:::

## People who are not signed in

Under **Settings → Access**, **People who are not signed in** has two
project-wide settings:

**Require a login to read the catalog**
: Off by default, so anyone can read the catalog and only signed-in people can
  change anything. When it is on, anyone who is not signed in sees nothing but
  the sign-in page.

**Anonymous downloads**
: What someone who is not signed in may download while reading is public. The
  default is `all`.

## Managing accounts

An administrator does all of this under **Settings → Access**. Changes to
roles and download scopes apply as soon as they are made, and reach the
person on their next request, even if they are already signed in.

To add someone, choose their name, role and download scope under **Add
someone**, and send them the invite link that appears. It is shown only once.
If it is lost, issue another.

To reset a password, issue a new invite link for that person. Their old
password keeps working until the new link is used, so a lost email never
locks anyone out.

Removing an account keeps that person's interpretations, still under their
name, since picks are part of the project's results rather than of the
account. Only their display preferences are deleted.

Ridal refuses to remove or demote the last administrator.

The same can be done from the command line on the server:

| Task | Command |
|---|---|
| Add someone | `ridal project user add jane --role picker` |
| List accounts | `ridal project user list` |
| Change a role or download scope | `ridal project user set jane --role operator --download results` |
| Issue a new invite link | `ridal project user reset jane` |
| Remove an account | `ridal project user remove jane` |

## A class or workshop

To give a group of people accounts at once, open **Add several people** under
**Settings → Access**. Choose how many accounts, their role and download
scope, and either a name prefix, which gives `student-01`, `student-02` and
so on, or random friendly usernames. **Create invite links** then gives every
account its own one-time link, so that each person sets their own password.

If handing out links is impractical, **Create with generated passwords**
makes the passwords for you instead, after you confirm that you understand
the risk. Shared passwords are weaker than invite links, since you know them
too, so keep them to short, supervised sessions.

On the command line, the same is:

```console
$ ridal project user add-bulk --count 20 --prefix student
```

`--random-names` uses friendly usernames instead of the prefix. `--passwords`
generates passwords, and must be given together with
`--i-know-what-i-am-doing`. The passwords are written to `passwords.txt`
rather than printed, since terminals are often logged. Hand them out, then
delete the file.

## Sessions and passwords

A sign-in lasts 14 days. Passwords are stored only as Argon2id hashes, and
invite links only as hashes, in `ridal_data/users.json`, which is readable by
its owner only. Deleting `ridal_data/session.key` signs everyone out.

## Publishing a finished project

`ridal server start --read-only` makes everyone a `viewer`, whatever their
account says, and accepts no changes at all. A read-only server needs no
accounts, so it is a simple way to publish a finished project for others to
browse. It still belongs behind HTTPS.
