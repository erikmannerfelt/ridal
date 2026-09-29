# HTTPS and reverse proxies

:::{danger}
**Anyone using a Ridal server over a network must reach it through
`https://`.** Ridal itself only speaks plain HTTP. Without HTTPS in front of
it, every password, every login cookie and every invite link crosses the
network readable by anyone on the way: the same Wi-Fi, the campus network,
or a provider in between. With any of them, someone else can sign in as that
person and change or delete their interpretations.
:::

## Why this is your job, not Ridal's

Ridal does not handle TLS, the encryption behind HTTPS. The supported
arrangement is to bind Ridal to the local machine only and put a **reverse
proxy** in front of it: a web server such as nginx, Caddy or Apache that
accepts HTTPS from the outside and passes each request on to Ridal. The proxy
holds the certificate, which [Let's Encrypt](https://letsencrypt.org) issues
for free.

```text
browser ──HTTPS──▶ reverse proxy ──HTTP──▶ Ridal on 127.0.0.1:8000
                   (same machine as Ridal)
```

Ridal guards against the obvious mistakes. Asked to listen on a network
address rather than `127.0.0.1`, a site refuses to start if it has accounts,
unless given `--allow-insecure-login`, and it refuses to start for a site
without accounts, unless given `--read-only`, since anyone who could reach it
could change the interpretations.

What Ridal **cannot** check is the proxy. Behind a proxy, every request
reaches Ridal over plain HTTP from the local machine, whether the browser
used HTTPS or not. A proxy that listens on plain HTTP passes every one of
Ridal's checks, and still sends passwords across the network unencrypted.
Configuring the proxy for HTTPS, and only HTTPS, is up to you.

## Checklist

- Serve Ridal on `https://` only, and give people only `https://` links.
- Redirect plain `http://` to `https://`, so that a typed or old link does
  not quietly send a password in the clear.
- Send a `Strict-Transport-Security` header, so that browsers refuse plain
  HTTP for the site from then on.
- Mark the login cookie `Secure` at the proxy, so that browsers never send
  it over plain HTTP. Ridal does not do this itself, because behind a proxy
  it cannot tell whether the browser used HTTPS.
- Keep Ridal on `127.0.0.1` (the default for `ridal server start`), so that
  it cannot be reached except through the proxy.
- Treat invite links like passwords. `ridal site account add` prints only the
  path, `/invite/...`; put your `https://` address in front of it, and send
  it the way you would send a password.
- Only use `--allow-insecure-login` when TLS really is handled in front of
  Ridal on another machine, such as by a load balancer. Traffic between that
  machine and Ridal is then unencrypted, so it must be a network you trust.

Even a read-only server without accounts is better behind HTTPS, since it
stops anyone in between from reading or altering what people see. With
accounts, it is essential.

## nginx

A complete site, assuming a certificate from Let's Encrypt's `certbot` for
`ridal.example.org`:

```nginx
# Send plain HTTP to HTTPS.
server {
    listen 80;
    server_name ridal.example.org;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    server_name ridal.example.org;

    ssl_certificate     /etc/letsencrypt/live/ridal.example.org/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/ridal.example.org/privkey.pem;

    # Browsers refuse plain HTTP for this site for a year after seeing this.
    add_header Strict-Transport-Security "max-age=31536000" always;

    location / {
        proxy_pass http://127.0.0.1:8000;

        # Never send the login cookie over plain HTTP (nginx 1.19.3 or later).
        proxy_cookie_flags ridal_session secure;

        # Ridal caps uploads itself and explains what it refused and why.
        client_max_body_size 0;

        # Let Ridal check an upload against that cap as it arrives.
        proxy_request_buffering off;

        # A radargram takes longer to upload than the default 60 s.
        client_body_timeout 300s;
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
    }
}
```

Two of these settings are about radargrams rather than security
([#248](https://github.com/erikmannerfelt/ridal/issues/248)), and nginx's
defaults get them wrong:

`client_max_body_size 0`
: nginx refuses request bodies over 1 MB by default, which every radargram
  exceeds. It does so before Ridal sees the upload, so the browser gets
  nginx's `413` rather than an explanation. Ridal already caps uploads
  against `max_bytes` under `[radargrams]` in `ridal.toml`, so removing
  nginx's limit leaves one cap, and it is the one that can describe itself.

`proxy_request_buffering off`
: With buffering on, nginx writes the whole file to its own disk before
  passing on a byte. An over-cap upload would then cost a full transfer and a
  second copy on the proxy before anything refused it.

:::{warning}
Ridal cannot detect a limit imposed in front of it. If an upload fails with a
message about the web server in front of Ridal, check these settings first.
:::

## Caddy

[Caddy](https://caddyserver.com) obtains and renews certificates by itself
and redirects HTTP to HTTPS by default, which makes it the shortest route to
a correct setup:

```text
ridal.example.org {
    reverse_proxy 127.0.0.1:8000
}
```

This configuration has not yet been tested with large radargram uploads. If
uploads fail, check Caddy's request body limits and timeouts, for the same
reasons as with nginx above.
