"""The servers the integration fixtures in ``conftest.py`` start."""

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Server:
    url: str
    root: Path


@dataclass(frozen=True)
class Site(Server):
    token: str
    """A token for ``anna``, a server administrator, as a viewer in ``glac``."""
