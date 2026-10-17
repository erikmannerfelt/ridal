"""Uploads sent in chunks (#396), against a fake server that keeps what
arrives and fails when told to."""

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

httpx = pytest.importorskip("httpx")

from ridal import client as ridal_client
from ridal.client import _http, errors

UPLOADS = "/api/v1/projects/glac/uploads"


@dataclass
class UploadServer:
    """The upload protocol, in memory.

    ``fail`` is called with each chunk's ``(start, body)`` before it is
    kept, and may answer what the server does instead: ``("break", n)``
    keeps the first ``n`` bytes and drops the connection, a response is sent
    as it is.
    """

    fail: Any = None
    size: int = 0
    received: bytearray = field(default_factory=bytearray)
    deleted: bool = False
    requests: list[str] = field(default_factory=list)

    def __call__(self, request: Any) -> Any:
        path = request.url.path
        self.requests.append(f"{request.method} {path}")
        if request.method == "POST" and path == UPLOADS:
            self.size = int(json.loads(request.content)["size"])
            return httpx.Response(201, json=self.status())
        if path == f"{UPLOADS}/u1":
            if request.method == "GET":
                return httpx.Response(200, json=self.status())
            if request.method == "DELETE":
                self.deleted = True
                return httpx.Response(204)
            match = re.fullmatch(
                r"bytes (\d+)-(\d+)/(\d+)", request.headers["Content-Range"]
            )
            assert match is not None
            start, end, total = map(int, match.groups())
            assert total == self.size
            body = request.read()
            assert len(body) == end - start + 1
            if start != len(self.received):
                return refusal(409, "offset_mismatch")
            outcome = self.fail(start, body) if self.fail else None
            if isinstance(outcome, tuple):
                self.received += body[: outcome[1]]
                raise httpx.ReadError("the connection dropped")
            if outcome is not None:
                return outcome
            self.received += body
            return httpx.Response(200, json=self.status())
        if request.method == "POST" and path == "/api/v1/projects/glac/raw":
            if request.url.params.get("upload") == "u1":
                assert len(self.received) == self.size
                assert request.content == b""
                size = len(self.received)
            else:
                size = len(request.read())
            return httpx.Response(
                201, json={"name": "big.tif", "blake3": "a" * 64, "bytes": size}
            )
        raise AssertionError(f"unexpected {request.method} {request.url}")

    def status(self) -> dict[str, Any]:
        return {
            "id": "u1",
            "name": "big.tif",
            "size": self.size,
            "offset": len(self.received),
            "expires_at": "2026-10-18T00:00:00Z",
        }


def refusal(status: int, code: str) -> Any:
    return httpx.Response(status, json={"error": {"code": code, "message": "No."}})


@pytest.fixture
def no_waiting(monkeypatch: pytest.MonkeyPatch) -> list[float]:
    waits: list[float] = []
    monkeypatch.setattr(_http.time, "sleep", waits.append)
    return waits


def client(server: UploadServer) -> ridal_client.Client:
    made = ridal_client.Client(
        "https://ridal.test/", project="glac", _transport=httpx.MockTransport(server)
    )
    made._http.chunked_above = 1000
    made._http.chunk_bytes = 1000
    return made


def big_file(tmp_path: Path, size: int = 3500) -> tuple[Path, bytes]:
    data = bytes((i * 31 + i // 256) % 256 for i in range(size))
    path = tmp_path / "big.tif"
    path.write_bytes(data)
    return path, data


def test_a_file_up_to_the_threshold_is_sent_whole(tmp_path: Path) -> None:
    path = tmp_path / "small.rd3"
    path.write_bytes(b"x" * 1000)
    server = UploadServer()
    with client(server) as made:
        stored = made.upload_raw(path)
    assert stored.bytes == 1000
    assert server.requests == ["POST /api/v1/projects/glac/raw"]


def test_a_large_file_goes_in_chunks_and_is_used_by_name(
    tmp_path: Path, no_waiting: list[float]
) -> None:
    path, data = big_file(tmp_path)
    server = UploadServer()
    seen: list[int] = []
    with client(server) as made:
        stored = made.upload_raw(path, progress=lambda event: seen.append(event.done))
    assert bytes(server.received) == data
    assert stored.bytes == len(data)
    assert server.requests == [
        f"POST {UPLOADS}",
        f"PUT {UPLOADS}/u1",
        f"PUT {UPLOADS}/u1",
        f"PUT {UPLOADS}/u1",
        f"PUT {UPLOADS}/u1",
        "POST /api/v1/projects/glac/raw",
    ]
    assert seen[-1] == len(data)
    assert no_waiting == []


def test_a_chunk_that_breaks_off_is_resumed_from_where_the_server_got_to(
    tmp_path: Path, no_waiting: list[float]
) -> None:
    path, data = big_file(tmp_path)
    broken: list[int] = []

    def fail(start: int, body: bytes) -> Any:
        if start == 1000 and not broken:
            broken.append(start)
            return ("break", 400)
        if start >= 2000 and len(broken) == 1:
            broken.append(start)
            return refusal(502, "http_502")
        return None

    server = UploadServer(fail=fail)
    with client(server) as made:
        stored = made.upload_raw(path)
    assert bytes(server.received) == data
    assert stored.bytes == len(data)
    # Each failure is followed by asking for the offset, then the rest.
    puts = [r for r in server.requests if r.startswith("PUT")]
    assert len(puts) == 6
    assert server.requests.count(f"GET {UPLOADS}/u1") == 2
    assert no_waiting == [1.0, 1.0]
    assert not server.deleted


def test_an_upload_that_keeps_failing_is_given_up_and_deleted(
    tmp_path: Path, no_waiting: list[float]
) -> None:
    path, _ = big_file(tmp_path)
    server = UploadServer(fail=lambda start, body: ("break", 0))
    with client(server) as made, pytest.raises(errors.TransportError):
        made.upload_raw(path)
    assert no_waiting == list(_http.RETRY_WAITS)
    assert server.deleted


def test_a_refusal_is_not_retried(tmp_path: Path, no_waiting: list[float]) -> None:
    path, _ = big_file(tmp_path)
    server = UploadServer(fail=lambda start, body: refusal(413, "project_full"))
    with client(server) as made, pytest.raises(errors.ServerError) as raised:
        made.upload_raw(path)
    assert raised.value.code == "project_full"
    assert no_waiting == []
    assert server.deleted
