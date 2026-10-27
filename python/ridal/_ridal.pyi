# Type stubs for the Rust extension. Exact for what the client calls (`info`,
# `_preflight_body`); the processing functions take many keyword options that
# their docstrings describe, and are typed loosely here rather than restated.
import datetime
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

def coords(
    input: _Path,
    output: _Path | None = ...,
    *,
    positions: _Path | None = ...,
    start: tuple[float, float] | tuple[float, float, float] | None = ...,
    end: tuple[float, float] | tuple[float, float, float] | None = ...,
    track: _Path | None = ...,
    time_offset: float | None = ...,
    src_crs: str | None = ...,
    start_time: str | datetime.datetime | None = ...,
    time_interval: float | None = ...,
    dem: _Path | None = ...,
    overwrite: bool = ...,
) -> str: ...
def info(
    inputs: _Path | Sequence[_Path],
    *,
    velocity: float = ...,
    cor: _Path | None = ...,
    dem: _Path | None = ...,
    crs: str | None = ...,
    override_antenna_mhz: float | None = ...,
    override_antenna_separation: float | None = ...,
    override_date: datetime.date | str | None = ...,
    sidecar: _Path | bool | None = ...,
) -> list[dict[str, Any]]: ...
def override(
    inputs: _Path | Sequence[_Path],
    *,
    antenna_mhz: float | None = ...,
    antenna_separation_m: float | None = ...,
    unset: str | Sequence[str] | None = ...,
    lines: int | Sequence[int] | str | None = ...,
    template: bool = ...,
) -> list[str]: ...
def _preflight_body(path: _Path) -> dict[str, Any]: ...
def read(*args: Any, **kwargs: Any) -> Any: ...
def process(*args: Any, **kwargs: Any) -> Any: ...
def batch_process(*args: Any, **kwargs: Any) -> Any: ...
def render(*args: Any, **kwargs: Any) -> dict[str, Any]: ...
def write_netcdf(
    path: _Path,
    data: Any,
    *,
    twtt_ns: Any,
    time_zero_ns: float | Any | None,
    trace_time: Any,
    antenna_mhz: float,
    antenna_separation: float,
    longitude: Any | None = ...,
    latitude: Any | None = ...,
    x: Any | None = ...,
    y: Any | None = ...,
    crs: str | None = ...,
    elevation: Any | None = ...,
    dem: _Path | None = ...,
    antenna_separation_effective: float | None = ...,
    medium_velocity: float = ...,
    radargram_id: str | None = ...,
    radargram_name: str | None = ...,
    group_id: str | None = ...,
    group_name: str | None = ...,
    metadata: Any | None = ...,
    log: Sequence[str] | None = ...,
    source_file: str | None = ...,
    antenna: str | None = ...,
) -> str: ...
def run_cli(*args: Any, **kwargs: Any) -> None: ...
