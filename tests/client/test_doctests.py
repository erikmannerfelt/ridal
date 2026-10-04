"""The examples in the client's docstrings, run against the installed package."""

import doctest
from types import ModuleType

import pytest

pytest.importorskip("httpx")

from ridal.client import _client, _http, errors, plan


@pytest.mark.parametrize(
    "module", [_client, _http, errors, plan], ids=lambda module: module.__name__
)
def test_docstring_examples(module: ModuleType) -> None:
    failures, _ = doctest.testmod(module, verbose=False)
    assert failures == 0
