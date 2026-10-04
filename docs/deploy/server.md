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
  project a site serves. Building an overview reads a whole radargram, so at
  most a quarter of these (at least one) build overviews at a time; overviews
  already built are served without waiting. It also sets how many radargram
  files stay open between requests: twice this, at least 16.

`--cache-memory-mb`
: In-memory budget for encoded radargram images. Like `--n-workers`, it is
  **shared by the whole site**: one budget for every radargram of every
  project, not one each. Ridal evicts least-recently used images to stay
  inside it. Overviews, the whole-radargram thumbnails on the index and
  maps, are also kept in each project's cache directory (see
  [project files](../reference/project-files)), so after a restart they are
  read from disk instead of being rebuilt from every radargram.

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

Ridal is a foreground process; stopping it stops the server. On Ctrl+C or
SIGTERM it finishes the requests in flight before it exits. On a machine
that should always answer, run it under a service manager such as `systemd`,
with the site directory as its only argument, and let that manager restart
it. Sessions survive a restart: the signing key is the site's
`session.key`, so a restart does not sign everyone out.

### As a systemd service

Most Linux servers run services with systemd. The unit below runs Ridal as
its own unprivileged user, restarts it if it stops, and lets it write
nowhere but the site directory. It assumes the site is at `/srv/ridal` and
the `ridal` command at `/usr/local/bin/ridal`; change both to match. A
`ridal` installed with `cargo install` lives in that user's
`~/.cargo/bin`, which the unit cannot read: copy it somewhere such as
`/usr/local/bin` first.

Create a user that owns the site and cannot log in:

```console
$ sudo useradd --system --home-dir /srv/ridal --shell /usr/sbin/nologin ridal
$ sudo chown -R ridal:ridal /srv/ridal
```

Save this as `/etc/systemd/system/ridal.service`:

```ini
[Unit]
Description=Ridal server
After=network-online.target
Wants=network-online.target

[Service]
User=ridal
Group=ridal
ExecStart=/usr/local/bin/ridal server start /srv/ridal
Restart=on-failure
RestartSec=5

# Ridal writes only under the site directory.
ReadWritePaths=/srv/ridal
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

Then start it, and have it start again at boot:

```console
$ sudo systemctl daemon-reload
$ sudo systemctl enable --now ridal
$ systemctl status ridal
$ journalctl -u ridal -f
```

What Ridal prints when it starts, and any warnings after, go to the journal,
which the last command follows.

Options go on the `ExecStart` line, as on the command line: for example
`--n-workers 4 --cache-memory-mb 2048`. Leave `--host` out when a reverse
proxy on the same machine is in front, as {doc}`reverse-proxy` recommends.

Run the `ridal site` commands that change the site, such as adding accounts,
as the same user, so that the files they write stay readable to the
service:

```console
$ sudo -u ridal ridal site account add jane --path /srv/ridal
```

`ridal` needs PROJ (`projinfo` and `cs2cs`) and, for DEM corrections, GDAL
on the service's `PATH`. Installed from the system's packages, they are in
`/usr/bin`, which systemd includes.
