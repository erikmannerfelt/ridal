"""The HTTP layer: one ``httpx.Client``, the error envelope, and transfers.

Everything here is about talking to a Ridal server, nothing about what the
answers mean; that is :mod:`ridal.client._client`. Keeping it in one place is
what lets the transfers become concurrent later without the public API
changing.
"""

import contextlib
import email.message
import os
import tempfile
import urllib.parse
from collections.abc import Iterator, Mapping
from pathlib import Path
from typing import IO, Any, Final

from ridal.client import errors
from ridal.client.progress import Progress, ProgressEvent

try:
    import httpx
except ImportError as error:  # pragma: no cover - depends on the environment
    raise ImportError(
        "ridal.client needs httpx, which is not installed: pip install ridal[client]"
    ) from error

# Small enough that progress moves visibly on a slow link, large enough that
# the per-chunk overhead is noise next to a 100 MB radargram.
CHUNK_BYTES: Final = 1 << 20


def quote(segment: str) -> str:
    """One path segment, escaped.

    >>> quote("line 01/a")
    'line%2001%2Fa'
    """
    return urllib.parse.quote(segment, safe="")


class Http:
    """A connection to one server, with its authentication.

    Parameters
    ----------
    url : str
        The server's root, such as ``https://ridal.example.org``.
    token : str or None
        An API token, sent as ``Authorization: Bearer``. Without one, a
        session from :meth:`ridal.client.Client.login` is used, if any.
    timeout : float
        Seconds to wait for a connection, and between bytes once connected.
        A transfer may take longer in total.
    transport : httpx.BaseTransport, optional
        For tests.
    """

    def __init__(
        self,
        url: str,
        *,
        token: str | None,
        timeout: float,
        transport: "httpx.BaseTransport | None" = None,
    ) -> None:
        headers = {"Accept": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        self.base = url.rstrip("/")
        self._client = httpx.Client(
            base_url=self.base,
            headers=headers,
            timeout=httpx.Timeout(timeout),
            transport=transport,
            follow_redirects=False,
        )

    def close(self) -> None:
        self._client.close()

    def _send(self, method: str, path: str, **kwargs: Any) -> "httpx.Response":
        try:
            response = self._client.request(method, path, **kwargs)
        except httpx.TransportError as error:
            raise errors.TransportError(
                f"{method} {self.base}{path}: {error}"
            ) from error
        raise_for_status(response)
        return response

    def json(self, method: str, path: str, **kwargs: Any) -> Any:
        """Send a request and return its JSON body."""
        response = self._send(method, path, **kwargs)
        try:
            return response.json()
        except ValueError as error:
            raise errors.ProtocolError(
                f"{method} {path} did not answer with JSON"
            ) from error

    def request(self, method: str, path: str, **kwargs: Any) -> "httpx.Response":
        """Send a request and return the response, for its headers."""
        return self._send(method, path, **kwargs)

    def download(
        self,
        path: str,
        destination: Path,
        *,
        params: Mapping[str, str] | None = None,
        label: str,
        progress: Progress | None,
    ) -> Path:
        """Stream a response body to a file and return where it went.

        ``destination`` may be a directory, in which case the server's
        suggested file name is used, or ``label`` without one. The body goes
        to a temporary file beside the destination first and is moved into
        place only when complete, so an interrupted download never leaves a
        file that looks finished.
        """
        try:
            with self._client.stream("GET", path, params=params) as response:
                if response.is_error:
                    response.read()
                    raise_for_status(response)
                target = _resolve_destination(destination, response, label)
                total = _content_length(response)
                with _atomic_file(target) as file:
                    done = 0
                    for chunk in response.iter_bytes(CHUNK_BYTES):
                        file.write(chunk)
                        done += len(chunk)
                        if progress is not None:
                            progress(ProgressEvent("download", label, done, total))
        except httpx.TransportError as error:
            raise errors.TransportError(f"GET {self.base}{path}: {error}") from error
        return target

    def upload(
        self,
        method: str,
        path: str,
        source: Path,
        *,
        params: Mapping[str, str] | None = None,
        label: str,
        progress: Progress | None,
    ) -> Any:
        """Stream a file as a request body and return the JSON answer."""
        total = source.stat().st_size

        def chunks() -> Iterator[bytes]:
            done = 0
            with source.open("rb") as file:
                while chunk := file.read(CHUNK_BYTES):
                    done += len(chunk)
                    yield chunk
                    if progress is not None:
                        progress(ProgressEvent("upload", label, done, total))

        return self.json(
            method,
            path,
            params=params,
            content=chunks(),
            headers={
                "Content-Type": "application/octet-stream",
                "Content-Length": str(total),
            },
        )


def raise_for_status(response: "httpx.Response") -> None:
    """Raise the exception matching a refusal, from its error envelope."""
    if not response.is_error:
        return
    code = f"http_{response.status_code}"
    message = response.reason_phrase or "The server refused the request."
    try:
        envelope = response.json()["error"]
        code, message = str(envelope["code"]), str(envelope["message"])
    except (ValueError, KeyError, TypeError):
        pass
    raise errors.from_response(response.status_code, code, message)


def _content_length(response: "httpx.Response") -> int | None:
    try:
        return int(response.headers["Content-Length"])
    except (KeyError, ValueError):
        return None


def _resolve_destination(
    destination: Path, response: "httpx.Response", label: str
) -> Path:
    if not destination.is_dir():
        return destination
    suggested = _attachment_name(response.headers.get("Content-Disposition", ""))
    return destination / (suggested or label)


def _attachment_name(disposition: str) -> str | None:
    """The file name a ``Content-Disposition`` header suggests, made safe.

    >>> _attachment_name('attachment; filename="line-01.nc"')
    'line-01.nc'
    >>> _attachment_name('attachment; filename="../../etc/passwd"')
    'passwd'
    >>> _attachment_name("inline") is None
    True
    """
    if not disposition:
        return None
    message = email.message.Message()
    message["Content-Disposition"] = disposition
    name = message.get_filename()
    if not name:
        return None
    # Only ever a name inside the directory the caller chose.
    name = Path(name.replace("\\", "/")).name
    return name if name not in {"", ".", ".."} else None


@contextlib.contextmanager
def _atomic_file(target: Path) -> Iterator[IO[bytes]]:
    """A file that appears at ``target`` only if the block completes.

    The temporary file is a sibling, so the final rename stays on one
    filesystem, and keeps the suffix, so nothing reading it by extension is
    confused if it is ever left behind.
    """
    target.parent.mkdir(parents=True, exist_ok=True)
    handle, temporary = tempfile.mkstemp(
        dir=target.parent, prefix=f".{target.stem}.", suffix=f".tmp{target.suffix}"
    )
    try:
        with os.fdopen(handle, "wb") as file:
            yield file
        os.replace(temporary, target)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temporary)
