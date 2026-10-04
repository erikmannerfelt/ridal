"""The client against a fake server: requests, refusals and transfers."""

import json
import os
from pathlib import Path
from typing import Any

import pytest

httpx = pytest.importorskip("httpx")

from ridal import client as ridal_client
from ridal.client import errors

DATASET: dict[str, Any] = {
    "radargram_id": "line-01",
    "effective_label": "line-01",
    "display_name": None,
    "group_name": "Drønbreen 2025",
    "group_id": "dronbreen-2025",
    "relative_path": "line-01.nc",
    "processing_datetime": "2025-03-27T10:00:00Z",
    "processing_datetime_display": "2025-03-27 10:00",
    "revision_id": "c92181a29a7b28b99605e463f6890a27",
    "shape": [977, 551],
    "track_length_m": 1234.5,
    "in_project": True,
    "unlisted": False,
    "line_count": 2,
    "contributor_count": 1,
}


def fake(handler: Any, **kwargs: Any) -> ridal_client.Client:
    """A client whose requests go to ``handler`` instead of a server."""
    return ridal_client.Client(
        "https://ridal.test/", _transport=httpx.MockTransport(handler), **kwargs
    )


def refusal(status: int, code: str, message: str = "No.") -> Any:
    return httpx.Response(status, json={"error": {"code": code, "message": message}})


def test_requests_name_the_project_and_escape_their_parts() -> None:
    seen: list[str] = []

    def handler(request: Any) -> Any:
        seen.append(request.url.raw_path.decode())
        return httpx.Response(200, json=DATASET)

    with fake(handler, project="glac") as client:
        dataset = client.dataset("line 01")
    assert seen == ["/api/v1/projects/glac/datasets/line%2001"]
    assert dataset.shape == (977, 551)
    assert dataset.group_name == "Drønbreen 2025"


def test_a_token_is_sent_as_a_bearer_header(monkeypatch: pytest.MonkeyPatch) -> None:
    headers: list[str | None] = []

    def handler(request: Any) -> Any:
        headers.append(request.headers.get("Authorization"))
        return httpx.Response(200, json={"status": "ok", "version": "0.8.0"})

    with fake(handler, token="ridal_abc_def") as client:
        client.health()
    monkeypatch.setenv("RIDAL_TOKEN", "ridal_from_env")
    with fake(handler) as client:
        client.health()
    monkeypatch.delenv("RIDAL_TOKEN")
    with fake(handler) as client:
        client.health()
    assert headers == ["Bearer ridal_abc_def", "Bearer ridal_from_env", None]


@pytest.mark.parametrize(
    ("status", "code", "kind"),
    [
        (400, "invalid_radargram_id", errors.BadRequest),
        (401, "invalid_token", errors.Unauthorized),
        (403, "token_limit", errors.Forbidden),
        (404, "dataset_not_found", errors.NotFound),
        (409, "not_a_project", errors.NotAProject),
        (409, "wrong_radargram", errors.Conflict),
        (412, "version_conflict", errors.PreconditionFailed),
        (413, "project_full", errors.PayloadTooLarge),
        (503, "busy", errors.Unavailable),
        (500, "store_failed", errors.ServerError),
    ],
)
def test_a_refusal_is_the_matching_exception(
    status: int, code: str, kind: type
) -> None:
    with (
        fake(lambda request: refusal(status, code, "Because.")) as client,
        pytest.raises(kind) as raised,
    ):
        client.catalog()
    assert type(raised.value) is kind
    assert (raised.value.status, raised.value.code) == (status, code)
    assert raised.value.message == "Because."


def test_a_refusal_without_an_envelope_still_says_what_it_was() -> None:
    with (
        fake(
            lambda request: httpx.Response(502, text="<html>Bad gateway</html>")
        ) as client,
        pytest.raises(errors.ServerError) as raised,
    ):
        client.catalog()
    assert raised.value.code == "http_502"


def test_an_answer_that_is_not_understood_is_a_protocol_error() -> None:
    with (
        fake(lambda request: httpx.Response(200, json={"entries": "nope"})) as client,
        pytest.raises(errors.ProtocolError),
    ):
        client.catalog()
    with (
        fake(lambda request: httpx.Response(200, text="not json")) as client,
        pytest.raises(errors.ProtocolError),
    ):
        client.health()


def test_an_unreachable_server_is_a_transport_error() -> None:
    def handler(request: Any) -> Any:
        raise httpx.ConnectError("refused", request=request)

    with fake(handler) as client, pytest.raises(errors.TransportError):
        client.health()
        with pytest.raises(errors.TransportError):
            client.download_radargram("line-01", Path(os.devnull).parent)


def test_a_download_reports_progress_and_lands_whole(tmp_path: Path) -> None:
    body = bytes(range(256)) * 9000  # A little over two chunks.
    events: list[ridal_client.ProgressEvent] = []

    def handler(request: Any) -> Any:
        return httpx.Response(
            200,
            content=body,
            headers={"Content-Disposition": 'attachment; filename="line-01.nc"'},
        )

    with fake(handler) as client:
        written = client.download_radargram("line-01", tmp_path, progress=events.append)
    assert written == tmp_path / "line-01.nc"
    assert written.read_bytes() == body
    assert [event.done for event in events][-1] == len(body)
    assert all(event.total == len(body) for event in events)
    assert list(tmp_path.iterdir()) == [written], "no temporary file left behind"

    # A file path is used as it is.
    with fake(handler) as client:
        named = client.download_radargram("line-01", tmp_path / "copy.nc")
    assert named.read_bytes() == body


def test_a_refused_download_leaves_nothing(tmp_path: Path) -> None:
    with (
        fake(lambda request: refusal(403, "download_not_permitted")) as client,
        pytest.raises(errors.Forbidden),
    ):
        client.download_radargram("line-01", tmp_path)
    assert list(tmp_path.iterdir()) == []


def test_an_interrupted_download_leaves_nothing(tmp_path: Path) -> None:
    class Broken(httpx.SyncByteStream):
        def __iter__(self) -> Any:
            yield b"partial"
            raise httpx.ReadError("connection reset")

    with (
        fake(lambda request: httpx.Response(200, stream=Broken())) as client,
        pytest.raises(errors.TransportError),
    ):
        client.download_radargram("line-01", tmp_path / "line-01.nc")
    assert list(tmp_path.iterdir()) == []


def test_level2_asks_for_the_callers_own_picks_by_default(tmp_path: Path) -> None:
    seen: list[str] = []

    def handler(request: Any) -> Any:
        seen.append(str(request.url))
        if request.url.path == "/api/v1/auth/me":
            return httpx.Response(
                200,
                json={
                    "user": "anna",
                    "authenticated": True,
                    "server_admin": False,
                    "authentication_configured": True,
                    "token": None,
                },
            )
        return httpx.Response(200, json={"type": "FeatureCollection", "features": []})

    with fake(handler) as client:
        client.download_level2("line-01", tmp_path, spacing=5.0, crs="native")
        client.download_level2("line-01", tmp_path, derived=True, format="csv")
    assert seen[1] == (
        "https://ridal.test/api/v1/projects/default/datasets/line-01/interpretations/"
        "anna/level2?format=geojson&spacing=5.0&crs=native"
    )
    assert seen[2].endswith("/datasets/line-01/derived/level2?format=csv")


def test_an_interpretation_keeps_its_version() -> None:
    document = {"key": "line-01", "features": []}

    def handler(request: Any) -> Any:
        return httpx.Response(200, json=document, headers={"ETag": '"v1"'})

    with fake(handler) as client:
        interpretation = client.interpretation("line-01", "anna")
    assert interpretation.document == document
    assert interpretation.etag == '"v1"'


def test_the_catalog_becomes_a_table() -> None:
    pd = pytest.importorskip("pandas")
    payload = {"entries": [DATASET], "warnings": ["one file could not be read"]}
    with fake(lambda request: httpx.Response(200, json=payload)) as client:
        catalog = client.catalog()
    assert catalog.warnings == ("one file could not be read",)
    frame = catalog.to_pandas()
    assert isinstance(frame, pd.DataFrame)
    assert frame.loc["line-01", "traces"] == 551
    assert list(catalog.by_id()) == ["line-01"]
    json.dumps(DATASET)  # The fixture itself stays plain JSON.
