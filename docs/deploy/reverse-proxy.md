# Behind a reverse proxy

Ridal does not encrypt anything itself. To serve a project beyond your own
machine, leave it bound to `127.0.0.1` and put a reverse proxy in front of it:

```bash
ridal server start my_survey/ --port 8000
```

The proxy needs two settings that its defaults get wrong for radargrams
([#248](https://github.com/erikmannerfelt/ridal/issues/248)).

## nginx

```nginx
location / {
    proxy_pass http://127.0.0.1:8000;

    # Ridal caps uploads itself and explains what it refused and why.
    client_max_body_size 0;

    # Let Ridal check the upload against that cap as it arrives.
    proxy_request_buffering off;

    # A radargram takes longer to upload than the default 60 s.
    client_body_timeout 300s;
    proxy_read_timeout 300s;
    proxy_send_timeout 300s;
}
```

`client_max_body_size 0`
: nginx refuses request bodies over 1 MB by default, which every radargram
  exceeds. It does so before Ridal sees the upload, so the browser gets nginx's
  `413` rather than an explanation. Ridal already caps uploads against
  `max_bytes` under `[radargrams]` in `ridal.toml`, so removing nginx's limit
  leaves one cap, and it is the one that can describe itself.

`proxy_request_buffering off`
: With buffering on, nginx writes the whole file to its own disk before passing
  on a byte. An over-cap upload would then cost a full transfer and a second
  copy on the proxy before anything refused it.

:::{warning}
Ridal cannot detect a limit imposed in front of it. If an upload fails with a
message about the web server in front of Ridal, check this section first.
:::
