"""The examples in the client's docstrings, run against the installed package."""

import doctest

import pytest

pytest.importorskip("httpx")

from ridal.client import _http, errors


@pytest.mark.parametrize("module", [_http, errors], ids=lambda module: module.__name__)
def test_docstring_examples(module: object) -> None:
    failures, _ = doctest.testmod(module, verbose=False)
    assert failures == 0
