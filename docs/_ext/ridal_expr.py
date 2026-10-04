"""Highlighting for derived item expressions, matching the GUI's editor.

The function names are read from the GUI's own lists in
``src/server/assets/app.js``, so the documentation colours a name exactly
when the editor does. Registers the ``ridal-expr`` language for code blocks
and an ``expr`` role for inline expressions: {expr}`median(bed)`.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, ClassVar

from docutils import nodes
from pygments.lexer import RegexLexer, words
from pygments.token import Number, Operator, Punctuation, String, Text, Token
from sphinx.application import Sphinx

_APP_JS = Path(__file__).resolve().parents[2] / "src" / "server" / "assets" / "app.js"

# Custom token types, so that only this lexer's output picks up the colours
# in ridal.css (Pygments names their classes `n-Ridal-Reduce` and so on).
Reduce = Token.Name.Ridal.Reduce
Builtin = Token.Name.Ridal.Builtin
Keyword = Token.Name.Ridal.Keyword
Layer = Token.Name.Ridal.Layer


def _js_list(source: str, name: str) -> list[str]:
    """The string literals of ``const NAME = [...]`` in the GUI's source."""
    match = re.search(rf"const {name} = \[(.*?)\];", source, re.DOTALL)
    if match is None:
        raise RuntimeError(f"{name} not found in {_APP_JS}; did the editor move?")
    return re.findall(r'"([^"]+)"', match.group(1))


_source = _APP_JS.read_text(encoding="utf-8")
REDUCERS = _js_list(_source, "IDENTIFIER_REDUCERS")
# The builtins list starts by spreading in the reducers.
BUILTINS = [n for n in _js_list(_source, "IDENTIFIER_BUILTINS") if n not in REDUCERS]
KEYWORDS = _js_list(_source, "IDENTIFIER_KEYWORDS")


class RidalExpressionLexer(RegexLexer):
    name = "Ridal derived item expression"
    aliases: ClassVar[list[str]] = ["ridal-expr"]

    tokens: ClassVar[dict[str, list[Any]]] = {
        "root": [
            (r"\s+", Text),
            # A username for only() and without(), before anything inside it
            # can match as a name.
            (r'"[^"]*"', String.Ridal),
            (words(REDUCERS, suffix=r"\b"), Reduce),
            (words(BUILTINS, suffix=r"\b"), Builtin),
            (words(KEYWORDS, suffix=r"\b"), Keyword),
            (r"[A-Za-z_][A-Za-z0-9_]*", Layer),
            (r"\d+(\.\d+)?", Number.Ridal),
            (r"[+\-*/<>=!]+", Operator.Ridal),
            (r"[()\[\]{},;]", Punctuation),
            (r".", Text),
        ]
    }


def expr_role(
    name: str,
    rawtext: str,
    text: str,
    lineno: int,
    inliner: Any,
    options: dict[str, Any] | None = None,
    content: list[str] | None = None,
) -> tuple[list[nodes.Node], list[nodes.system_message]]:
    """Inline code that Sphinx's HTML writer highlights as ``ridal-expr``.

    Not docutils' own ``code`` role: that looks lexers up in Pygments
    directly, and cannot see one registered with Sphinx.
    """
    node = nodes.literal(rawtext, text, classes=["code", "highlight"])
    node["language"] = "ridal-expr"
    return [node], []


def setup(app: Sphinx) -> dict[str, bool]:
    app.add_lexer("ridal-expr", RidalExpressionLexer)
    app.add_role("expr", expr_role)
    return {"parallel_read_safe": True}
