"""Exceptions raised by :mod:`ridal.client`.

A refusal from the server carries a stable, machine-readable ``code`` (such as
``dataset_not_found`` or ``token_limit``) and a ``message`` for people. The
class says what kind of refusal it is, from the HTTP status, so a script can
catch :class:`Unauthorized` without knowing every code, and branch on
``code`` when it needs to.
"""

from typing import Final


class RidalError(Exception):
    """Base class of every exception raised by the client."""


class TransportError(RidalError):
    """The server could not be reached, or the connection failed midway."""


class ProtocolError(RidalError):
    """The server answered with something the client does not understand.

    Usually a client and server from different Ridal versions; compare
    :attr:`ridal.client.Health.version` with ``ridal.__version__``.
    """


class ReplacementRefused(RidalError):
    """A replacement was not committed, because of what it would do to picks.

    Attributes
    ----------
    report : ConsequenceReport or None
        The server's report, which says what would have happened.
    """

    def __init__(self, reason: str, report: object | None) -> None:
        super().__init__(reason)
        self.report = report


class ServerError(RidalError):
    """The server refused or failed a request.

    Attributes
    ----------
    status : int
        The HTTP status.
    code : str
        The stable error code, such as ``dataset_not_found``. ``http_<status>``
        when the response carried no error envelope.
    message : str
        What went wrong, for a person. The wording may change between
        releases; branch on ``code`` instead.
    """

    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(f"{message} ({status} {code})")
        self.status = status
        self.code = code
        self.message = message


class BadRequest(ServerError):
    """``400``: something in the request needs fixing."""


class Unauthorized(ServerError):
    """``401``: sign in, or use a valid token, and try again."""


class Forbidden(ServerError):
    """``403``: the caller may not do this, and signing in again will not help."""


class NotFound(ServerError):
    """``404``: no such thing, or one the caller may not see."""


class Conflict(ServerError):
    """``409``: the server is not in a state that allows this."""


class NotAProject(Conflict):
    """``409 not_a_project``: the server is serving files that are not a project."""


class PreconditionFailed(ServerError):
    """``412``: the document changed since it was read. Read it again and retry."""


class PayloadTooLarge(ServerError):
    """``413``: the upload would take the project over its size limit."""


class Unavailable(ServerError):
    """``503``: the server is busy. Retrying later may work."""


_BY_STATUS: Final[dict[int, type[ServerError]]] = {
    400: BadRequest,
    401: Unauthorized,
    403: Forbidden,
    404: NotFound,
    409: Conflict,
    412: PreconditionFailed,
    413: PayloadTooLarge,
    503: Unavailable,
}


def from_response(status: int, code: str, message: str) -> ServerError:
    """The most specific exception for a refusal.

    >>> type(from_response(409, "not_a_project", "…")).__name__
    'NotAProject'
    >>> type(from_response(418, "teapot", "…")).__name__
    'ServerError'
    """
    if status == 409 and code == "not_a_project":
        return NotAProject(status, code, message)
    return _BY_STATUS.get(status, ServerError)(status, code, message)
