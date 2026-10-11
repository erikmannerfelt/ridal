"""The client's records must match the server's schemas, field for field.

The schemas are derived from the types the server serializes (``openapi.json``
is regenerated from them, and a Rust test fails when it is stale), so this is
what keeps the client from drifting from the server.
"""

import dataclasses
import json
import types
import typing
from pathlib import Path
from typing import Any, Final

import pytest

httpx = pytest.importorskip("httpx")

from ridal import client as ridal_client
from ridal.client import models

SPEC: Final = Path(__file__).parents[2] / "docs" / "reference" / "openapi.json"

#: Each record, the schema it mirrors, and any field it names differently.
RECORDS: Final[dict[type, tuple[str, dict[str, str]]]] = {
    models.Health: ("Health", {}),
    models.Me: ("Me", {}),
    models.SignedIn: ("SignedIn", {}),
    models.Project: ("ProjectEntry", {}),
    models.Dataset: ("DatasetSummary", {}),
    models.Catalog: ("DatasetList", {"datasets": "entries"}),
    models.Axes: ("DatasetAxes", {}),
    models.InterpretationList: ("InterpretationList", {}),
    models.Grant: ("Grant", {}),
    models.TokenSummary: ("TokenSummary", {}),
    # Processing on the server (#382).
    models.HashedFile: ("HashedFile", {}),
    models.StoredFile: ("StoredFile", {}),
    models.BundleFile: ("BundleFile", {}),
    models.Bundle: ("Bundle", {}),
    models.SourceDem: ("SourceDem", {}),
    models.Source: ("RawSource", {}),
    models.NamedFormat: ("NamedFormat", {}),
    models.MetadataSummary: ("MetadataSummary", {}),
    models.CentroidSummary: ("CentroidSummary", {}),
    models.CorrectionSummary: ("CorrectionSummary", {}),
    models.LocationSummary: ("LocationSummary", {}),
    models.RelatedFiles: ("RelatedFiles", {}),
    models.Recording: ("InfoRecord", {}),
    models.SourceView: ("SourceView", {}),
    models.Dem: ("DemView", {}),
    models.StepProgress: ("StepProgress", {}),
    models.Draft: ("DraftView", {}),
    models.ProfileVersion: ("ProfileVersion", {}),
    models.Profile: ("ProfileView", {}),
    models.Assignment: ("Assignment", {}),
    models.RadargramStatus: ("RadargramStatus", {}),
    models.PicksNotCarried: ("PicksNotCarried", {}),
    models.BatchItem: ("Item", {}),
    models.Batch: ("Batch", {}),
}

#: Fields a record has that the server does not send in the body: the
#: version from the ``ETag`` header, for a conditional change.
CLIENT_ONLY: Final = frozenset({"etag"})


def schemas() -> dict[str, Any]:
    return json.loads(SPEC.read_text())["components"]["schemas"]


def nullable(annotation: Any) -> bool:
    is_union = typing.get_origin(annotation) in (typing.Union, types.UnionType)
    return is_union and type(None) in typing.get_args(annotation)


def properties(schema: dict[str, Any]) -> dict[str, bool]:
    """Every property an object of ``schema`` may have, and whether it may
    be null or missing.

    A flattened field makes an ``allOf``, whose parts are all there; a
    tagged enum makes a ``oneOf``, of which one is, so a property only some
    of its variants have may be missing, which the client reads as ``None``.
    """
    if "$ref" in schema:
        return properties(schemas()[schema["$ref"].rsplit("/", 1)[1]])
    if "allOf" in schema:
        merged: dict[str, bool] = {}
        for part in schema["allOf"]:
            merged.update(properties(part))
        return merged
    if "oneOf" in schema:
        variants = [properties(v) for v in schema["oneOf"]]
        names = set().union(*variants)
        return {name: any(name not in v or v[name] for v in variants) for name in names}
    required = set(schema.get("required", []))
    return {
        name: server_nullable(prop) or name not in required
        for name, prop in schema.get("properties", {}).items()
    }


@pytest.mark.parametrize("record", list(RECORDS), ids=lambda record: record.__name__)
def test_a_record_has_exactly_the_schemas_fields(record: type) -> None:
    name, renamed = RECORDS[record]
    server = properties(schemas()[name])
    hints = typing.get_type_hints(record)
    fields = [f for f in dataclasses.fields(record) if f.name not in CLIENT_ONLY]
    assert {renamed.get(f.name, f.name) for f in fields} == set(server), name

    # A field the server may send as null, or leave out, is optional here,
    # and only then.
    for field in fields:
        server_name = renamed.get(field.name, field.name)
        assert nullable(hints[field.name]) == server[server_name], (
            f"{record.__name__}.{field.name}"
        )


def server_nullable(property: dict[str, Any]) -> bool:
    """Whether a schema property allows ``null``: a type list that includes
    it, or (for a reference) a ``oneOf`` with a null branch."""
    if "null" in property.get("type", []):
        return True
    return any(branch.get("type") == "null" for branch in property.get("oneOf", []))


def undeclared(value: Any, schema: dict[str, Any], where: str) -> list[str]:
    """Keys of ``value`` its schema does not name, and required keys it lacks.

    The gprinterp schemas allow further properties (its SPEC §3.3), so a
    misspelt key would validate; this is stricter, because the client should
    only write what the format defines.
    """
    if "$ref" in schema:
        return undeclared(value, schemas()[schema["$ref"].rsplit("/", 1)[1]], where)
    if "oneOf" in schema:
        if value is None:
            return []
        (branch,) = [b for b in schema["oneOf"] if b.get("type") != "null"]
        return undeclared(value, branch, where)
    if not isinstance(value, dict) or "properties" not in schema:
        return []
    problems = [
        f"{where} lacks {key}" for key in schema.get("required", []) if key not in value
    ]
    for key, item in value.items():
        if key not in schema["properties"]:
            problems.append(f"{where}.{key}")
        else:
            problems += undeclared(item, schema["properties"][key], f"{where}.{key}")
    return problems


def test_an_interpretation_template_is_a_gprinterp_document() -> None:
    answer = {
        "radargram_id": "line-01",
        "revision_id": "c92181a29a7b28b99605e463f6890a27",
        "axes": {"x": {"primary": {"name": "trace", "unit": "index"}}},
    }
    # The fake answer is what the server sends, so the template is built
    # from the real shape.
    assert set(answer) == set(schemas()["GprinterpAxes"]["properties"])

    def handler(request: Any) -> Any:
        return httpx.Response(200, json=answer)

    transport = httpx.MockTransport(handler)
    with ridal_client.Client("https://ridal.test/", _transport=transport) as client:
        document = client.interpretation_template("line-01")
    assert undeclared(document, schemas()["Document"], "document") == []
    assert document["source"]["id"] == "line-01"
