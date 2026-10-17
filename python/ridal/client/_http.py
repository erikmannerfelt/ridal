"""The HTTP layer: one ``httpx.Client``, the error envelope, and transfers.

Everything here is about talking to a Ridal server, nothing about what the
answers mean; that is :mod:`ridal.client._client`. Nothing here holds state
between requests beyond the ``httpx.Client``, which is safe to share between
threads, so one :class:`Http` serves concurrent requests too.
"""

import contextlib
import email.message
import os
import tempfile
import time
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

#: Files larger than this are uploaded in chunks, which survive a dropped
#: connection (#396); smaller ones in one request.
CHUNKED_ABOVE: Final = 64 << 20
#: The size of each chunk of a chunked upload: under the 100 MB a proxy such
#: as Cloudflare lets through in one request, and the server's 64 MiB limit.
UPLOAD_CHUNK_BYTES: Final = 32 << 20
#: Seconds to wait before each retry of a chunk that failed. A chunk is
#: retried as many times in a row as there are waits; one that goes through
#: starts the count again.
RETRY_WAITS: Final = (1.0, 2.0, 5.0, 10.0, 30.0)
#: Refusals that mean "ask how far the upload got and go on from there".
_RESUMABLE_CODES: Final = frozenset(
    {"offset_mismatch", "range_mismatch", "upload_interrupted", "upload_busy"}
)


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
        #: Above how many bytes :meth:`upload` sends a file in chunks, and
        #: how large they are. Attributes rather than constants so that a
        #: test can send a small file in several.
        self.chunked_above = CHUNKED_ABOVE
        self.chunk_bytes = UPLOAD_CHUNK_BYTES
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
                with atomic_file(target) as file:
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
        uploads: str,
        label: str,
        progress: Progress | None,
    ) -> Any:
        """Send a file to ``path`` and return the JSON answer.

        A file over :attr:`chunked_above` is sent in chunks to ``uploads``,
        the project's ``/uploads``, retrying a chunk that fails from where
        the server got to, and then named to ``path`` as ``?upload=``. A
        smaller one is streamed as the request body.
        """
        total = source.stat().st_size
        if total > self.chunked_above:
            return self._upload_in_chunks(
                method,
                path,
                source,
                total,
                params=params,
                uploads=uploads,
                label=label,
                progress=progress,
            )

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

    def _upload_in_chunks(
        self,
        method: str,
        path: str,
        source: Path,
        total: int,
        *,
        params: Mapping[str, str] | None,
        uploads: str,
        label: str,
        progress: Progress | None,
    ) -> Any:
        started = self.json("POST", uploads, json={"name": source.name, "size": total})
        upload_id = str(started["id"])
        upload = f"{uploads}/{quote(upload_id)}"
        try:
            self._send_chunks(upload, source, total, label=label, progress=progress)
        except BaseException:
            # Given up, or interrupted: deleted now rather than holding its
            # share of the project's space for the day the server keeps it.
            with contextlib.suppress(errors.RidalError):
                self._send("DELETE", upload)
            raise
        return self.json(method, path, params={**(params or {}), "upload": upload_id})

    def _send_chunks(
        self,
        upload: str,
        source: Path,
        total: int,
        *,
        label: str,
        progress: Progress | None,
    ) -> None:
        """Send ``source`` to ``upload`` a chunk at a time, from the start.

        After a chunk fails, ask the server how far it got (it keeps what
        arrived of a chunk that broke off) and go on from there.
        """
        offset = 0
        failures = 0
        resync = False
        while resync or offset < total:
            try:
                if resync:
                    offset = int(self.json("GET", upload)["offset"])
                    resync = False
                    continue
                length = min(self.chunk_bytes, total - offset)
                self._send(
                    "PUT",
                    upload,
                    content=_read_range(
                        source,
                        offset,
                        length,
                        label=label,
                        total=total,
                        progress=progress,
                    ),
                    headers={
                        "Content-Type": "application/octet-stream",
                        "Content-Length": str(length),
                        "Content-Range": f"bytes {offset}-{offset + length - 1}/{total}",
                    },
                )
                offset += length
                failures = 0
            except errors.RidalError as error:
                if not _resumable(error) or failures == len(RETRY_WAITS):
                    raise
                time.sleep(RETRY_WAITS[failures])
                failures += 1
                resync = True


def _read_range(
    source: Path,
    offset: int,
    length: int,
    *,
    label: str,
    total: int,
    progress: Progress | None,
) -> Iterator[bytes]:
    """``length`` bytes of ``source`` from ``offset``, a piece at a time."""
    done = 0
    with source.open("rb") as file:
        file.seek(offset)
        while done < length and (piece := file.read(min(CHUNK_BYTES, length - done))):
            done += len(piece)
            yield piece
            if progress is not None:
                progress(ProgressEvent("upload", label, offset + done, total))


def _resumable(error: "errors.RidalError") -> bool:
    """Whether a failed chunk is worth resuming: the connection failed, a
    proxy or the server failed, or the server got a different part of the
    chunk than was sent."""
    if isinstance(error, errors.TransportError):
        return True
    if isinstance(error, errors.ServerError):
        return error.status >= 500 or error.code in _RESUMABLE_CODES
    return False


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
def atomic_file(target: Path) -> Iterator[IO[bytes]]:
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
