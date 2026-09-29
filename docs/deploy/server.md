# Running a server

`ridal server start <site>` serves a [site](sites): many projects from one
address, behind one list of accounts.

```console
$ ridal server start /srv/ridal
Serving on http://127.0.0.1:8000
```

For one project on your own machine, use `ridal gui <project>` instead. It
serves the project alone, at `/p/default/`, with no accounts, and needs no
setup; see {doc}`../guide/gui`. `server start` refuses a directory that is a
project rather than a site, and says so, so that "many people can reach
this" is never a mode you arrive at by accident.

## Options

`--host`, `--port`
: Where to listen. The default is `127.0.0.1:8000`, which only this machine
  can reach. Binding to another address means Ridal is reachable over the
  network, so read {doc}`reverse-proxy` first.

`--read-only`
: Serve everything, change nothing. Every caller is a `viewer`, whatever
  their account says, and no account, membership or project can change. A
  read-only site needs no accounts, which makes this a simple way to publish
  finished projects by their links.

`--allow-insecure-login`
: Accept password sign-ins while bound to a network address. Ridal does not
  terminate TLS, so without this it refuses, to keep passwords off a network
  you may not control. Only use it when something in front is doing TLS.

`--n-workers`
: How many CPU-heavy renders run at once. The budget is **shared by the
  whole site**, not per project, so it caps concurrent rendering across every
  project a site serves.

`--cache-memory-mb`
: In-memory budget for encoded radargram images. Ridal evicts least-recently
  used images to stay inside it.

`--open-browser`
: Open the landing page after starting. Off by default here, because a
  server normally runs where there is no browser.

## What happens when it starts

A site lists its projects without opening them. Each project's catalog is
read the first time someone opens that project, and kept until it changes or
is evicted, so a site with many projects does not pay for all of them at
startup. The first time a project is opened, Ridal also records its catalog
size in `ridal_data/catalog-summary.json`, which is what the landing page's
project card shows.

A site refuses to start unless it either has accounts or is `--read-only`:
with no accounts there is nobody to sign in and manage it. Bound to a
network address with accounts, it also refuses password sign-ins unless
`--allow-insecure-login` is given (see {doc}`reverse-proxy`).

## Keeping it running

Ridal is a foreground process; stopping it stops the server. On a machine
that should always answer, run it under a service manager such as `systemd`,
with the site directory as its only argument, and let that manager restart
it. Sessions survive a restart: the signing key is the site's
`session.key`, so a restart does not sign everyone out.
