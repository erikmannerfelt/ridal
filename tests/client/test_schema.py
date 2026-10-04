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
}


def schemas() -> dict[str, Any]:
    return json.loads(SPEC.read_text())["components"]["schemas"]


def nullable(annotation: Any) -> bool:
    is_union = typing.get_origin(annotation) in (typing.Union, types.UnionType)
    return is_union and type(None) in typing.get_args(annotation)


@pytest.mark.parametrize("record", list(RECORDS), ids=lambda record: record.__name__)
def test_a_record_has_exactly_the_schemas_fields(record: type) -> None:
    name, renamed = RECORDS[record]
    schema = schemas()[name]
    hints = typing.get_type_hints(record)
    fields = {
        renamed.get(field.name, field.name) for field in dataclasses.fields(record)
    }
    assert fields == set(schema["properties"]), name

    # A field the server may send as null is optional here, and only then.
    for field in dataclasses.fields(record):
        server_name = renamed.get(field.name, field.name)
        assert nullable(hints[field.name]) == server_nullable(
            schema["properties"][server_name]
        ), f"{record.__name__}.{field.name}"


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
