"""Python interface for ridal.

The functions here are implemented in Rust (``ridal._ridal``) and re-exported
unchanged; see their own documentation. :mod:`ridal.client` is the HTTP client
for a Ridal server, and :mod:`ridal.impdar` writes ImpDAR radargrams with
:func:`write_netcdf`.
"""

from ridal import _ridal
from ridal._ridal import (
    __version__,
    all_format_descriptions,
    all_formats,
    all_step_descriptions,
    all_steps,
    batch_process,
    coords,
    info,
    override,
    process,
    read,
    render,
    run_cli,
    version,
    write_netcdf,
)

# The extension's own module documentation lists the entry points; it is the
# one place they are described.
__doc__ = _ridal.__doc__

__all__ = [
    "__version__",
    "all_format_descriptions",
    "all_formats",
    "all_step_descriptions",
    "all_steps",
    "batch_process",
    "coords",
    "info",
    "override",
    "process",
    "read",
    "render",
    "run_cli",
    "version",
    "write_netcdf",
]
