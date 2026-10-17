"""The ``http-endpoints`` directive, for the HTTP API reference.

``:::{http-endpoints} Radargrams`` renders the table of every operation in
``reference/openapi.json`` tagged ``Radargrams``: its method, its path, what it
needs of the caller (``x-ridal-needs``) and its description (the handler's doc
comment), with the request and response bodies named. The spec is committed
and regenerated with ``UPDATE_OPENAPI=1``, so Read the Docs never has to build
Ridal to render it (#330).

A project's routes are written in the spec as a site serves them, under
``/api/v1/projects/{key}``; the table shows them in the project's own shape,
``/api/v1/…``, as the page's conventions say.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from docutils import nodes
from docutils.statemachine import StringList
from sphinx.application import Sphinx
from sphinx.util.docutils import SphinxDirective

SPEC = Path("reference") / "openapi.json"
PROJECT_PREFIX = "/api/v1/projects/{key}"
METHODS = ("get", "post", "put", "patch", "delete")

# What `x-ridal-needs.who` reads as in the Needs column. A role is its name.
WHO = {
    "nobody": "nobody",
    "anyone": "anyone",
    "signed_in": "signed in",
}
ROLES = ("viewer", "picker", "operator", "admin")


def needs_text(needs: dict[str, Any]) -> str:
    """The Needs column for one operation's ``x-ridal-needs``."""
    who = needs["who"]
    if who in WHO:
        return WHO[who]
    if who in ROLES:
        return f"`{who}`"
    raise ValueError(f"unknown x-ridal-needs.who {who!r}")


def shown_path(path: str) -> str:
    """A project's path in the project's own shape."""
    if path.startswith(PROJECT_PREFIX + "/"):
        return "/api/v1" + path[len(PROJECT_PREFIX) :]
    return path


def schema_name(content: dict[str, Any] | None) -> str | None:
    """The schema a JSON body refers to, if it names one."""
    schema = (content or {}).get("application/json", {}).get("schema", {})
    ref = schema.get("$ref") or schema.get("items", {}).get("$ref")
    return ref.rsplit("/", 1)[-1] if ref else None


def description(operation: dict[str, Any]) -> str:
    """The doc comment on one line, with the bodies it takes and returns."""
    text = " ".join(
        part
        for part in (operation.get("summary"), operation.get("description"))
        if part
    )
    text = " ".join(text.split())
    request = schema_name(operation.get("requestBody", {}).get("content"))
    response = schema_name(operation["responses"].get("200", {}).get("content"))
    if request:
        text += f" Body: `{request}`."
    if response:
        text += f" Returns `{response}`."
    return text.replace("|", "\\|")


class HttpEndpoints(SphinxDirective):
    """One table of endpoints: every operation with the given tag."""

    required_arguments = 1
    final_argument_whitespace = True

    def run(self) -> list[nodes.Node]:
        tag = self.arguments[0]
        spec_path = Path(self.env.srcdir) / SPEC
        self.env.note_dependency(str(spec_path))
        spec = json.loads(spec_path.read_text(encoding="utf-8"))
        rows = []
        for path, item in spec["paths"].items():
            for method in METHODS:
                operation = item.get(method)
                if operation is None or tag not in operation.get("tags", []):
                    continue
                try:
                    needs = needs_text(operation["x-ridal-needs"])
                except (KeyError, ValueError) as error:
                    raise self.error(f"{method.upper()} {path}: {error}") from error
                rows.append(
                    f"| `{method.upper()}` | `{shown_path(path)}` | {needs} "
                    f"| {description(operation)} |"
                )
        if not rows:
            raise self.error(f"no operation in {SPEC} is tagged {tag!r}")
        table = [
            "| Method | Path | Needs | Description |",
            "|---|---|---|---|",
            *rows,
        ]
        container = nodes.container()
        self.state.nested_parse(StringList(table, source=str(spec_path)), 0, container)
        return container.children


def setup(app: Sphinx) -> dict[str, bool]:
    app.add_directive("http-endpoints", HttpEndpoints)
    return {"parallel_read_safe": True}
