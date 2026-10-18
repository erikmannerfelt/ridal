"""The client against a fake server that answers from the OpenAPI description.

``test_schema.py`` holds each record to one schema. This holds each *call* to
an operation (#454): every request a client method sends must be an operation
in ``openapi.json``, send a body and query that operation takes, and parse
the answer the operation promises. The answers are made from the response
schemas, every required field filled, so a record that cannot read what the
server says it sends fails here rather than against a real server.

Every public method of :class:`~ridal.client.Client` is called once, from
:data:`CALLS`; a method missing from it fails. Operations no method calls
must be in :data:`NOT_WRAPPED`, with the reason, so a new endpoint is either
wrapped or deliberately left to the browser.
"""

import json
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Final

import pytest

httpx = pytest.importorskip("httpx")

from ridal import client as ridal_client
from ridal.client import models

SPEC: Final = Path(__file__).parents[2] / "docs" / "reference" / "openapi.json"
MALA: Final = Path(__file__).parents[2] / "assets" / "mala"
RECORDING: Final = MALA / "dronbreen-20250327-DAT_0066_A1.rd3"
DEM: Final = Path(__file__).parents[2] / "assets" / "test_dem_dtm20_mettebreen.tif"

#: What the browser and the server's own pages use, and the client leaves
#: out on purpose.
NOT_WRAPPED: Final[dict[str, str]] = {
    # Signing in by invite happens in the browser, from the link.
    "redeem_invite": "an invite is redeemed in the browser",
    "openapi_json": "this description itself",
    "list_profiles": "render profiles are the viewer's",
    "get_preferences": "a person's display preferences are the viewer's",
    "put_preferences": "a person's display preferences are the viewer's",
    "get_settings": "the settings page's",
    "put_settings": "the settings page's",
    "remove_dataset": "removing a radargram is done on the catalog page",
    "restore_dataset": "the catalog page's",
    "list_ignored": "the catalog page's",
    "dataset_attributes": "the viewer's metadata dialog",
    "dataset_trace": "the viewer's trace panel",
    "dataset_track": "the viewer's map; track() is the GeoJSON download",
    "dataset_topo_geometry": "the viewer's corrected view",
    "list_revisions": "the catalog page's history",
    "get_properties": "the catalog page's dialog",
    "put_properties": "the catalog page's dialog",
    "group_tracks": "the catalog's group maps",
    "get_group_properties": "the catalog page's dialog",
    "put_group_properties": "the catalog page's dialog",
    "group_track_geojson": "tracks() takes the whole catalog",
    "overview_image": "the viewer's image tiles",
    "chunk_image": "the viewer's image tiles",
    "dataset_image": "figure() is the client's image",
    "archive_interpretation": "the pick manager's",
    "download_archived": "the pick manager's",
    "download_bulk": "the pick manager's",
    "restore_archived": "the pick manager's",
    "purge_one": "the pick manager's",
    "purge_removal": "the pick manager's",
    "delete_interpretation": "the viewer's",
    "get_interpretation_raw": "interpretation() reads the same document",
    "layer_usage": "the layers page's",
    "get_contributors": "the viewer's layer panel",
    "get_derived_item": "the viewer's layer panel",
    "preview_derived": "the expression editor's",
    "group_level2": "level2() is per radargram",
    "catalog_level2": "level2() is per radargram",
    "draft_overview": "the processing page's preview",
    "processing_activity": "the processing page's",
    "get_upload": "only after a chunk fails (test_chunked.py)",
    "delete_upload": "only when a chunked upload is given up (test_chunked.py)",
    "unseen_batches": "the banner on every project page",
    "mark_batch_seen": "the banner on every project page",
    # The site server: administration is the settings pages' and the CLI's.
    "site_info": "the site's pages",
    "get_site_preferences": "the site's pages",
    "put_site_preferences": "the site's pages",
    "site_audit_log": "site administration",
    "site_status": "site administration",
    "site_processing": "site administration",
    "cancel_site_job": "site administration",
    "site_memberships": "site administration",
    "list_accounts": "site administration",
    "create_account": "site administration",
    "create_bulk_invites": "site administration",
    "create_bulk_passwords": "site administration",
    "update_account": "site administration",
    "delete_account": "site administration",
    "reissue_account_invite": "site administration",
    "list_tokens": "tokens are made in the browser, for the client to use",
    "create_token": "tokens are made in the browser, for the client to use",
    "revoke_token": "tokens are made in the browser, for the client to use",
    "create_project": "site administration",
    "project_info": "projects() lists them",
    "update_project": "site administration",
    "delete_project": "site administration",
    "archive_project": "site administration",
    "unarchive_project": "site administration",
    "list_members": "project administration",
    "add_member": "project administration",
    "invite_member": "project administration",
    "create_project_bulk_invites": "project administration",
    "create_project_bulk_passwords": "project administration",
    "update_member": "project administration",
    "remove_member": "project administration",
    "put_project_access": "project administration",
    "project_audit_log": "project administration",
}

#: Fields pinned in an operation's made-up answer, where the first value of
#: a schema would send a method round in circles or refuse to go on: a
#: draft that is still queued, a replacement whose report forbids it.
ANSWERS: Final[dict[str, dict[str, Any]]] = {
    "get_draft": {"state": "done", "revision_id": "a"},
    "stage_replacement": {"report": {"outgoing_axes_kept": True}},
    "commit_draft": {"report": {"outgoing_axes_kept": True}},
}

#: The media type a request's ``format`` asks for, where an operation
#: answers in several.
FORMATS: Final = {
    "json": "application/json",
    "geojson": "application/geo+json",
    "csv": "text/csv",
    "png": "image/png",
    "svg": "image/svg+xml",
}


def spec() -> dict[str, Any]:
    return json.loads(SPEC.read_text())


def resolve(schema: dict[str, Any]) -> dict[str, Any]:
    """``schema``, with a ``$ref`` followed."""
    while "$ref" in schema:
        schema = spec()["components"]["schemas"][schema["$ref"].rsplit("/", 1)[1]]
    return schema


def example(schema: dict[str, Any], depth: int = 0) -> Any:
    """A value of ``schema``: every property filled, one of each array,
    the first of each enum and of each ``oneOf``, never ``null``.

    >>> example({"type": ["integer", "null"]})
    1
    >>> example({"type": "object", "properties": {"a": {"type": "boolean"}}})
    {'a': False}
    """
    schema = resolve(schema)
    if depth > 12:
        return None
    if "allOf" in schema:
        merged: dict[str, Any] = {}
        for part in schema["allOf"]:
            merged.update(example(part, depth + 1) or {})
        return merged
    for key in ("oneOf", "anyOf"):
        if key in schema:
            branches = [b for b in schema[key] if resolve(b).get("type") != "null"]
            return example(branches[0], depth + 1)
    if "enum" in schema:
        return next(v for v in schema["enum"] if v is not None)
    kind = schema.get("type")
    if isinstance(kind, list):
        kind = next(k for k in kind if k != "null")
    if kind == "object" or "properties" in schema:
        value = {
            name: example(prop, depth + 1)
            for name, prop in schema.get("properties", {}).items()
        }
        extra = schema.get("additionalProperties")
        if isinstance(extra, dict) and not value:
            value = {"a": example(extra, depth + 1)}
        return value
    if kind == "array":
        items = schema.get("items", {})
        if "prefixItems" in schema:
            return [example(item, depth + 1) for item in schema["prefixItems"]]
        count = schema.get("minItems", 1)
        return [example(items, depth + 1) for _ in range(count)]
    scalars: dict[str, Any] = {
        "string": "a",
        "integer": 1,
        "number": 1.0,
        "boolean": False,
    }
    return scalars.get(str(kind), {})


def merge(value: Any, pinned: Any) -> Any:
    """``value`` with the fields ``pinned`` names replaced."""
    if isinstance(value, dict) and isinstance(pinned, dict):
        return {**value, **{k: merge(value.get(k), v) for k, v in pinned.items()}}
    return pinned


def problems(value: Any, schema: dict[str, Any], where: str) -> list[str]:
    """What in a request body ``schema`` does not take: a required field
    missing, or a field it does not have. Types are left to the server."""
    schema = resolve(schema)
    if "allOf" in schema:
        return [p for part in schema["allOf"] for p in problems(value, part, where)]
    for key in ("oneOf", "anyOf"):
        if key in schema:
            branches = [b for b in schema[key] if resolve(b).get("type") != "null"]
            found = [problems(value, b, where) for b in branches]
            return min(found, key=len) if found else []
    if isinstance(value, list):
        item = schema.get("items")
        return (
            [p for v in value for p in problems(v, item, f"{where}[]")] if item else []
        )
    if not isinstance(value, dict) or "properties" not in schema:
        return []
    found = [f"{where} lacks {k}" for k in schema.get("required", []) if k not in value]
    for key, item in value.items():
        if key in schema["properties"]:
            found += problems(item, schema["properties"][key], f"{where}.{key}")
        elif (
            schema.get("additionalProperties") is False
            or "additionalProperties" not in schema
        ):
            found.append(f"{where}.{key} is not in the schema")
    return found


@dataclass(frozen=True)
class Operation:
    id: str
    method: str
    path: str
    spec: dict[str, Any]

    def matches(self, method: str, segments: list[str]) -> int | None:
        """How many literal segments of the path ``segments`` match, or
        ``None`` if they do not match it."""
        template = self.path.strip("/").split("/")
        if method != self.method or len(template) != len(segments):
            return None
        literal = 0
        for want, got in zip(template, segments):
            if want.startswith("{"):
                continue
            if want != got:
                return None
            literal += 1
        return literal


def operations() -> list[Operation]:
    return [
        Operation(op["operationId"], method.upper(), path, op)
        for path, item in spec()["paths"].items()
        for method, op in item.items()
        if method != "parameters"
    ]


@dataclass
class SpecServer:
    """A transport answering every request from the description, and
    recording what was asked and what was wrong with it."""

    called: set[str] = field(default_factory=set)
    faults: list[str] = field(default_factory=list)

    def __call__(self, request: Any) -> Any:
        segments = request.url.raw_path.decode().split("?")[0].strip("/").split("/")
        candidates = [
            (score, op)
            for op in operations()
            if (score := op.matches(request.method, segments)) is not None
        ]
        where = f"{request.method} {request.url.path}"
        if not candidates:
            self.faults.append(f"{where} is no operation")
            return httpx.Response(404, json={"error": {"code": "x", "message": where}})
        _, op = max(candidates, key=lambda c: c[0])
        self.called.add(op.id)
        self.check(op, request, where)
        return self.answer(op, request)

    def check(self, op: Operation, request: Any, where: str) -> None:
        declared = {
            p["name"] for p in op.spec.get("parameters", []) if p.get("in") == "query"
        }
        for name in request.url.params:
            if name not in declared:
                self.faults.append(f"{where} ({op.id}) sends ?{name}, not a parameter")
        content = op.spec.get("requestBody", {}).get("content", {})
        if request.content and "application/json" in content:
            body = json.loads(request.content)
            schema = content["application/json"]["schema"]
            self.faults += [
                f"{where} ({op.id}): {p}" for p in problems(body, schema, "body")
            ]
        elif request.content and not content:
            self.faults.append(f"{where} ({op.id}) sends a body it does not take")

    def answer(self, op: Operation, request: Any) -> Any:
        status, response = min(
            (int(s), r) for s, r in op.spec["responses"].items() if s.startswith("2")
        )
        headers = {"ETag": '"v1"'}
        content = response.get("content", {})
        if not content:
            return httpx.Response(status, headers=headers)
        asked = FORMATS.get(request.url.params.get("format", ""))
        media = asked if asked in content else next(iter(content))
        schema = content[media].get("schema")
        if media == "application/json" and schema is not None:
            body = merge(example(schema), ANSWERS.get(op.id, {}))
            return httpx.Response(status, json=body, headers=headers)
        if media == "application/geo+json":
            body = {"type": "FeatureCollection", "features": []}
            return httpx.Response(status, json=body, headers=headers)
        # Named after the path, as a real server names a download after what
        # it is: one name for all of them sends parallel downloads to one
        # file, and on Windows the second rename onto it is refused.
        name = "-".join(request.url.path.strip("/").split("/")[-3:])
        headers["Content-Disposition"] = f'attachment; filename="{name}"'
        return httpx.Response(status, content=b"a", headers=headers)


@dataclass(frozen=True)
class Files:
    """Where a call may write, and the files it may send."""

    directory: Path
    radargram: Callable[[], Path]


def in_chunks(client: Any) -> Any:
    """``client``, sending every file in chunks of 64 kB."""
    client._http.chunked_above = 0
    client._http.chunk_bytes = 1 << 16
    return client


def stored(name: str) -> models.StoredFile:
    return models.StoredFile(name=name, blake3="a" * 64, bytes=1)


#: One call of each public method, with arguments a real server would take.
CALLS: Final[dict[str, Callable[[Any, Files], Any]]] = {
    "health": lambda c, f: c.health(),
    "me": lambda c, f: c.me(),
    "login": lambda c, f: c.login("anna", "a long passphrase"),
    "logout": lambda c, f: c.logout(),
    "projects": lambda c, f: c.projects(),
    "catalog": lambda c, f: c.catalog(),
    "dataset": lambda c, f: c.dataset("line-01"),
    "axes": lambda c, f: c.axes("line-01"),
    "interpretations": lambda c, f: c.interpretations("line-01"),
    "interpretation_summaries": lambda c, f: c.interpretation_summaries(),
    "interpretation": lambda c, f: c.interpretation("line-01", "anna"),
    "download_radargram": lambda c, f: c.download_radargram("line-01", f.directory),
    "download_radargrams": lambda c, f: c.download_radargrams(
        ["line-01", "line-02"], f.directory / "many"
    ),
    "track": lambda c, f: c.track("line-01"),
    "tracks": lambda c, f: c.tracks(),
    "level2": lambda c, f: c.level2("line-01", spacing=5, crs="native"),
    "download_level2": lambda c, f: c.download_level2(
        "line-01", f.directory, derived=True, format="csv"
    ),
    "download_derived": lambda c, f: c.download_derived("line-01", f.directory),
    "figure": lambda c, f: c.figure("line-01", f.directory / "f.png", {"axes": False}),
    "upload": lambda c, f: c.upload(f.radargram()),
    "preflight": lambda c, f: c.preflight(f.radargram()),
    "stage_replacement": lambda c, f: c.stage_replacement(f.radargram()),
    "commit_replacement": lambda c, f: c.commit_replacement("line-01", "token"),
    "discard_replacement": lambda c, f: c.discard_replacement("line-01", "token"),
    "replace": lambda c, f: c.replace(f.radargram()),
    "plan": lambda c, f: c.plan([f.radargram()]),
    "apply": lambda c, f: c.apply(c.plan([f.radargram()])),
    "interpretation_template": lambda c, f: c.interpretation_template("line-01"),
    "save_interpretation": lambda c, f: c.save_interpretation(
        "line-01", c.interpretation_template("line-01"), overwrite=True
    ),
    "carried": lambda c, f: c.carried("line-01"),
    "promote": lambda c, f: c.promote("line-01"),
    "layers": lambda c, f: c.layers(),
    "save_layers": lambda c, f: c.save_layers(
        {
            "schema": "ridal-layers",
            "schema_version": "1",
            "layers": [{"id": "bed", "name": "Bed"}],
            "groups": [],
        },
        etag=None,
    ),
    "derived": lambda c, f: c.derived(),
    "save_derived": lambda c, f: c.save_derived(
        {"schema": "ridal-derived", "schema_version": "1", "items": []}, etag='"v1"'
    ),
    "upload_raw": lambda c, f: c.upload_raw(RECORDING),
    "download_raw": lambda c, f: c.download_raw("a" * 64, f.directory),
    "sources": lambda c, f: c.sources(),
    "source": lambda c, f: c.source("line-01"),
    "set_source": lambda c, f: c.set_source(
        "line-01",
        [[stored("DAT_0001_A1.rd3"), stored("DAT_0001_A1.rad")]],
        crs="EPSG:32633",
        antenna_mhz=100.0,
        names=models.SourceNames(display_name="Line 1", group_name="Drønbreen"),
    ),
    "plan_ingest": lambda c, f: c.plan_ingest(
        [[stored("DAT_0001_A1.rd3")]],
        merge_threshold_seconds=60.0,
        id_pattern="line-{n}",
        antennas={"MALA 100": models.AntennaOverride(mhz=100.0)},
    ),
    "delete_source": lambda c, f: c.delete_source("line-01", etag='"v1"'),
    "link_dem": lambda c, f: c.link_dem("line-01", "mettebreen"),
    "dems": lambda c, f: c.dems(),
    "dem": lambda c, f: c.dem("mettebreen"),
    # In chunks, so that the upload operations are called too.
    "add_dem": lambda c, f: in_chunks(c).add_dem("mettebreen", "Mettebreen", DEM),
    "rename_dem": lambda c, f: c.rename_dem("mettebreen", "Mettebreen 2020"),
    "delete_dem": lambda c, f: c.delete_dem("mettebreen"),
    "create_draft": lambda c, f: c.create_draft(
        "line-01", ["zero_corr"], medium_velocity=0.168
    ),
    "drafts": lambda c, f: c.drafts(),
    "draft": lambda c, f: c.draft(1),
    "wait_for_draft": lambda c, f: c.wait_for_draft(1, interval=0),
    "download_draft": lambda c, f: c.download_draft(1, f.directory),
    "commit_draft": lambda c, f: c.commit_draft(1),
    "delete_draft": lambda c, f: c.delete_draft(1),
    "process": lambda c, f: c.process("line-01", [RECORDING], ["zero_corr"]),
    "profiles": lambda c, f: c.profiles(),
    "profile": lambda c, f: c.profile("standard"),
    "save_profile": lambda c, f: c.save_profile(
        "standard", name="Standard", steps=["zero_corr"], medium_velocity=0.168
    ),
    "delete_profile": lambda c, f: c.delete_profile("standard"),
    "assignments": lambda c, f: c.assignments(),
    "assign": lambda c, f: c.assign("line-01", "standard", pre_steps=["subset(0 10)"]),
    "unassign": lambda c, f: c.unassign("line-01"),
    "processing_status": lambda c, f: c.processing_status("line-01"),
    "reprocess": lambda c, f: c.reprocess("standard", radargram_ids=["line-01"]),
    "batches": lambda c, f: c.batches(),
    "batch": lambda c, f: c.batch("b1"),
    "wait_for_batch": lambda c, f: c.wait_for_batch("b1", interval=0),
}

#: The calls that send a processed radargram, which only the ``ridal``
#: command can make (:func:`conftest.process_fixture`).
WITH_A_RADARGRAM: Final = frozenset(
    {"upload", "preflight", "stage_replacement", "replace", "plan", "apply"}
)


def public_methods() -> set[str]:
    return {
        name
        for name, value in vars(ridal_client.Client).items()
        if not name.startswith("_") and callable(value) and name != "close"
    }


def files(tmp_path: Path, request: pytest.FixtureRequest) -> Files:
    return Files(
        directory=tmp_path,
        radargram=lambda: request.getfixturevalue("radargram"),
    )


def run(name: str, server: SpecServer, given: Files) -> None:
    transport = httpx.MockTransport(server)
    with ridal_client.Client(
        "https://ridal.test/", project="glac", _transport=transport
    ) as client:
        CALLS[name](client, given)


def test_every_public_method_is_called() -> None:
    assert public_methods() - set(CALLS) == set(), "add them to CALLS"
    assert set(CALLS) - public_methods() == set(), "methods that no longer exist"


@pytest.mark.parametrize("name", sorted(CALLS))
def test_a_method_calls_operations_as_they_are_described(
    name: str, tmp_path: Path, request: pytest.FixtureRequest
) -> None:
    server = SpecServer()
    run(name, server, files(tmp_path, request))
    assert server.faults == []
    assert server.called, f"{name} sent nothing"


def test_every_operation_is_wrapped_or_left_out_on_purpose(
    tmp_path: Path, request: pytest.FixtureRequest
) -> None:
    server = SpecServer()
    for name in sorted(CALLS):
        run(name, server, files(tmp_path / name, request))
    every = {op.id for op in operations()}
    assert every - server.called - set(NOT_WRAPPED) == set(), (
        "wrap them in the client, or say why not in NOT_WRAPPED"
    )
    assert set(NOT_WRAPPED) & server.called == set(), "wrapped now: remove them"
    assert set(NOT_WRAPPED) - every == set(), "no longer operations"


def test_a_request_that_is_no_operation_is_reported() -> None:
    server = SpecServer()
    transport = httpx.MockTransport(server)
    with (
        ridal_client.Client("https://ridal.test/", _transport=transport) as client,
        pytest.raises(ridal_client.errors.NotFound),
    ):
        client._http.json("GET", "/api/v1/projects/default/datasets/a/nothing")
    assert server.faults == [
        "GET /api/v1/projects/default/datasets/a/nothing is no operation"
    ]


def test_a_body_with_a_field_the_operation_does_not_take_is_reported() -> None:
    server = SpecServer()
    transport = httpx.MockTransport(server)
    with ridal_client.Client("https://ridal.test/", _transport=transport) as client:
        client._http.json("POST", "/api/v1/auth/login", json={"name": "a", "pass": "b"})
    assert server.faults == [
        "POST /api/v1/auth/login (login): body lacks password",
        "POST /api/v1/auth/login (login): body.pass is not in the schema",
    ]
