# Type stubs for the Rust extension. Exact for what the client calls (`info`,
# `_preflight_body`); the processing functions take many keyword options that
# their docstrings describe, and are typed loosely here rather than restated.
from collections.abc import Sequence
from os import PathLike
from typing import Any, TypeAlias

_Path: TypeAlias = str | PathLike[str]

version: str
__version__: str
all_steps: list[str]
all_step_descriptions: dict[str, str]
all_formats: list[str]
all_format_descriptions: dict[str, Any]

def info(
    inputs: _Path | Sequence[_Path],
    *,
    velocity: float = ...,
    cor: _Path | None = ...,
    dem: _Path | None = ...,
    crs: str | None = ...,
    override_antenna_mhz: float | None = ...,
    override_antenna_separation: float | None = ...,
) -> list[dict[str, Any]]: ...
def _preflight_body(path: _Path) -> dict[str, Any]: ...
def read(*args: Any, **kwargs: Any) -> Any: ...
def process(*args: Any, **kwargs: Any) -> Any: ...
def batch_process(*args: Any, **kwargs: Any) -> Any: ...
def render(*args: Any, **kwargs: Any) -> dict[str, Any]: ...
def run_cli(*args: Any, **kwargs: Any) -> None: ...
