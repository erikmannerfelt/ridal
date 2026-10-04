"""An HTTP client for a Ridal server.

Needs the ``client`` extra: ``pip install ridal[client]``.

>>> import ridal.client  # doctest: +SKIP
>>> with ridal.client.Client("https://ridal.example.org", project="glac") as client:  # doctest: +SKIP
...     print(client.health().version)
"""

from ridal.client._client import DEFAULT_PROJECT, TOKEN_VARIABLE, Client
from ridal.client.errors import (
    BadRequest,
    Conflict,
    Forbidden,
    NotAProject,
    NotFound,
    PayloadTooLarge,
    PreconditionFailed,
    ProtocolError,
    RidalError,
    ServerError,
    TransportError,
    Unauthorized,
    Unavailable,
)
from ridal.client.models import (
    Axes,
    Catalog,
    Dataset,
    Grant,
    Health,
    Interpretation,
    InterpretationList,
    Me,
    Project,
    SignedIn,
    TokenSummary,
)
from ridal.client.progress import Progress, ProgressEvent, tqdm_progress

__all__ = [
    "DEFAULT_PROJECT",
    "TOKEN_VARIABLE",
    "Axes",
    "BadRequest",
    "Catalog",
    "Client",
    "Conflict",
    "Dataset",
    "Forbidden",
    "Grant",
    "Health",
    "Interpretation",
    "InterpretationList",
    "Me",
    "NotAProject",
    "NotFound",
    "PayloadTooLarge",
    "PreconditionFailed",
    "Progress",
    "ProgressEvent",
    "Project",
    "ProtocolError",
    "RidalError",
    "ServerError",
    "SignedIn",
    "TokenSummary",
    "TransportError",
    "Unauthorized",
    "Unavailable",
    "tqdm_progress",
]
