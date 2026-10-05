# Talking to a server from Python

`ridal.client` is an HTTP client for a Ridal server: `ridal gui` on your own
machine, or a site run with `ridal server start`. It reads what the server
holds, downloads it, uploads and replaces radargrams, and saves picks. It
needs the `client` extra:

```bash
pip install "ridal[client]"
```

{doc}`../reference/python` lists every function and record, and
{doc}`../reference/http-api` the HTTP API underneath.

## Connecting

A client acts in one project. `ridal gui` serves its one project as
`default` and prints its address when it starts; `--port` fixes that address,
so a script can name it in advance (`ridal gui --port 8765` is
`http://127.0.0.1:8765`). On a site, a project has a key, and an API token
says who you are:

```python
import ridal.client

client = ridal.client.Client("https://ridal.example.org", project="dronbreen")
print(client.health().version)  # The Ridal version the server runs.
print(client.me())  # Who it thinks you are, and with which token.
```

The token comes from `token=` or the `RIDAL_TOKEN` environment variable,
which keeps it out of scripts and notebooks. Make one on the site's Settings
page, or as a server administrator with `ridal site token add`; see
{ref}`api-tokens`. A token is limited to the
projects and roles it was made for, which is why a script should use one
rather than a password. `client.login(name, password)` also works, on a
server reachable over HTTPS.

Use the client as a context manager, or call `client.close()`, to close its
connection when done.

## Reading

```python
catalog = client.catalog()
for dataset in catalog.datasets:
    print(dataset.radargram_id, dataset.revision_id, dataset.shape)

table = catalog.to_pandas()  # Needs pandas: pip install "ridal[geo]".
axes = client.axes("line-07")  # NumPy arrays: distance, twtt, depth, elevation.
who = client.interpretations("line-07").users
picks = client.interpretation("line-07", "anna").document  # gprinterp
```

## Downloading

Each download is streamed to a temporary file next to its destination and
moved into place only when complete, so an interrupted download never leaves a
file that looks finished. A destination may be a file or a directory.

```python
client.download_radargram("line-07", "downloads/")
client.download_level2("line-07", "downloads/", format="csv", spacing=5.0)
client.download_level2("line-07", "downloads/", derived=True)
client.download_derived("line-07", "downloads/")
```

`download_radargrams` downloads several at once, four at a time by default
(`workers=`), and returns where each went in the order asked for:

```python
ids = [dataset.radargram_id for dataset in client.catalog().datasets]
paths = client.download_radargrams(ids, "downloads/")
```

If one fails, those not yet started are not started, those under way finish,
and the error is raised. A `Client` may also be shared between your own
threads, for example to fetch level 2 points for many radargrams at once.

Tracks and level 2 points also come back in memory, as GeoJSON that turns
into a `geopandas.GeoDataFrame` in the CRS it was asked for (it needs
geopandas: `pip install "ridal[geo]"`):

```python
tracks = client.tracks().to_geopandas()  # Every track, in WGS84.
bed = client.level2("line-07", crs="native").to_geopandas()
bed.plot(column="depth_m")
```

Pass `progress=ridal.client.tqdm_progress()` for a progress bar (it needs
tqdm), or any function that takes a {py:class}`ridal.client.ProgressEvent`.

## Uploading and replacing many files

A folder of reprocessed radargrams is sorted against the project before
anything is uploaded:

```python
from pathlib import Path

plan = client.plan(sorted(Path("processed").glob("*.nc")))
print(plan.summary())  # {'new': 3, 'unchanged': 40, 'safe': 2, 'risky': 1}
for record in plan.with_status("risky"):
    print(record.radargram_id, record.report.worst, record.report.headline)

outcomes = client.apply(plan)
```

`plan` reads each file's identity with {py:func}`ridal.info` and compares it
with the catalog. A file whose radargram nobody has interpreted is `safe`; one
that would replace a radargram with picks is `risky`, and the server says what
would happen to the picks without the file being uploaded: whether they would
be shown unmoved (`carried`), move (`approximate`), partly fall outside the
new revision (`partial`), or not be shown at all (`refused`). Nothing is
rewritten either way; picks stay as they were drawn.

`plan` looks at four files at a time (`workers=`). `apply` uploads the new
files and replaces the others one at a time, since the server takes uploads
one at a time, and by default only where every set of picks would still be
shown unmoved (`allow={"current", "carried"}`). Anything else is left alone and reported in
its outcome. Replacing needs the `operator` role.

A file the server refuses is reported as `failed` and the rest go on. If the
server stops answering instead (it cannot be reached, or answers `502`, `503`
or `504`), `apply` stops there: the files after it are `not_attempted`. Plan
again once the server is back, and what was already done shows up as
`unchanged`, so applying that plan carries on where the first one stopped.
Pass `stop_on_outage=False` to try every file regardless.

## Saving picks

A picks document is a gprinterp document. Start one from
`interpretation_template`, which names the current revision and carries its
axes, so the picks can be carried onto a later revision:

```python
document = client.interpretation_template("line-07")
document["features"].append({
    "type": "Feature",
    "geometry": {"type": "LineString", "coordinates": [[10, 200], [250, 210]]},
    "properties": {"label": "bed"},
})
saved = client.save_interpretation("line-07", document)
```

Saving without an `etag` only creates. To change picks that exist, read them
first and pass the version back, so someone else's change in the meantime is
refused ({py:class}`ridal.client.PreconditionFailed`) rather than overwritten:

```python
stored = client.interpretation("line-07", "anna")
client.save_interpretation("line-07", edited, etag=stored.etag)
```

After a radargram is replaced, picks drawn on the earlier revision are shown
carried onto the new one. `client.carried("line-07")` shows how, and
`client.promote("line-07")` adopts them as picks on the new revision.

## When something is refused

Every refusal is an exception with the server's stable `code` and a `message`
for people; its class says what kind of refusal it is:

```python
try:
    client.download_radargram("line-07", "downloads/")
except ridal.client.Forbidden as refusal:
    print(refusal.code, refusal.message)
```

{py:class}`~ridal.client.Unauthorized` means signing in or a valid token
would help, {py:class}`~ridal.client.Forbidden` that it would not, and
{py:class}`~ridal.client.NotFound` that there is no such thing or you may not
see it. All of them are {py:class}`~ridal.client.RidalError`.
