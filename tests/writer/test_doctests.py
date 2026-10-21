"""The examples in `ridal.impdar`'s docstrings, run against the installed package."""

import doctest

import ridal.impdar


def test_docstring_examples() -> None:
    failures, tried = doctest.testmod(ridal.impdar, verbose=False)
    assert tried > 0
    assert failures == 0
