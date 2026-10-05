"""The client against a fake server: requests, refusals and transfers."""

import json
import os
import threading
from pathlib import Path
from typing import Any

import pytest

httpx = pytest.importorskip("httpx")

import ridal
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
        (502, "http_502", errors.Unavailable),
        (503, "busy", errors.Unavailable),
        (504, "http_504", errors.Unavailable),
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


def test_downloads_run_side_by_side_and_keep_their_order(tmp_path: Path) -> None:
    # Each answer waits until both requests are in, which only happens if
    # they are in flight at once; one at a time, the barrier times out.
    both = threading.Barrier(2, timeout=10)

    def handler(request: Any) -> Any:
        both.wait()
        radargram_id = request.url.path.split("/")[-2]
        return httpx.Response(200, content=radargram_id.encode())

    events: list[ridal_client.ProgressEvent] = []
    with fake(handler) as client:
        written = client.download_radargrams(
            ["line-02", "line-01"], tmp_path / "new", workers=2, progress=events.append
        )
    assert written == (tmp_path / "new" / "line-02.nc", tmp_path / "new" / "line-01.nc")
    assert [path.read_bytes() for path in written] == [b"line-02", b"line-01"]
    assert {event.label for event in events} == {"line-01.nc", "line-02.nc"}


def test_a_failed_download_stops_those_not_yet_started(tmp_path: Path) -> None:
    asked: list[str] = []

    def handler(request: Any) -> Any:
        radargram_id = request.url.path.split("/")[-2]
        asked.append(radargram_id)
        if radargram_id == "line-02":
            return refusal(403, "download_not_permitted")
        return httpx.Response(200, content=b"ok")

    with fake(handler) as client, pytest.raises(errors.Forbidden):
        client.download_radargrams(
            ["line-01", "line-02", "line-03"], tmp_path, workers=1
        )
    assert asked == ["line-01", "line-02"]
    assert [path.name for path in tmp_path.iterdir()] == ["line-01.nc"]


def test_workers_must_be_at_least_one(tmp_path: Path) -> None:
    with fake(lambda request: httpx.Response(200)) as client, pytest.raises(ValueError):
        client.download_radargrams(["line-01"], tmp_path, workers=0)


def test_a_plan_looks_at_files_side_by_side_and_keeps_their_order(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    served = [
        {**DATASET, "radargram_id": radargram_id, "revision_id": "old"}
        for radargram_id in ("line-01", "line-02")
    ]
    local = {
        "a.nc": ("line-02", "new"),
        "b.nc": ("line-01", "new"),
        "c.nc": ("line-09", "new"),
        "d.nc": ("line-01", "old"),
    }
    monkeypatch.setattr(
        ridal,
        "info",
        lambda path: [
            {
                "radargram_id": local[Path(path).name][0],
                "revision_id": local[Path(path).name][1],
                "reprocess_reason": None,
            }
        ],
    )
    # Both changed files are asked about at once, or the barrier times out.
    both = threading.Barrier(2, timeout=10)

    def handler(request: Any) -> Any:
        if request.url.path.endswith("/datasets"):
            return httpx.Response(200, json={"entries": served, "warnings": []})
        radargram_id = request.url.path.split("/")[-2]
        both.wait()
        return httpx.Response(
            200,
            json={"radargram_id": radargram_id, "users": [], "writable": True},
        )

    with fake(handler) as client:
        planned = client.plan(local, workers=4)
    assert [(record.path.name, record.status) for record in planned.records] == [
        ("a.nc", "safe"),
        ("b.nc", "safe"),
        ("c.nc", "new"),
        ("d.nc", "unchanged"),
    ]


def new_files(directory: Path, *names: str) -> ridal_client.Plan:
    """A plan that uploads an empty ``<name>.nc`` for each of ``names``."""
    records = []
    for name in names:
        path = directory / f"{name}.nc"
        path.touch()
        records.append(ridal_client.Record(path, "new", name, "a", None))
    return ridal_client.Plan(tuple(records))


def uploaded_name(request: Any) -> str:
    return Path(request.url.params["filename"]).stem


def added(request: Any) -> Any:
    name = uploaded_name(request)
    return httpx.Response(
        200,
        json={
            "radargram_id": name,
            "revision_id": "a",
            "display_name": None,
            "group_name": None,
            "bytes": 0,
            "archived_interpretations": 0,
        },
    )


def unreachable(request: Any) -> Any:
    raise httpx.ConnectError("refused", request=request)


@pytest.mark.parametrize(
    "outage",
    [
        lambda request: httpx.Response(502, text="<html>Bad gateway</html>"),
        lambda request: refusal(503, "busy"),
        unreachable,
    ],
    ids=["502", "503", "unreachable"],
)
def test_apply_stops_when_the_server_stops_answering(
    outage: Any, tmp_path: Path
) -> None:
    asked: list[str] = []

    def handler(request: Any) -> Any:
        asked.append(uploaded_name(request))
        return outage(request) if len(asked) == 2 else added(request)

    planned = new_files(tmp_path, "line-01", "line-02", "line-03", "line-04")
    planned = ridal_client.Plan(
        (
            *planned.records[:3],
            ridal_client.Record(Path("old.nc"), "unchanged", "line-05", "a", "a"),
            planned.records[3],
        )
    )
    with fake(handler) as client:
        outcomes = client.apply(planned)
    assert asked == ["line-01", "line-02"]
    assert [outcome.action for outcome in outcomes] == [
        "uploaded",
        "failed",
        "not_attempted",
        "skipped",
        "not_attempted",
    ]


def test_apply_goes_on_past_a_refusal_or_when_asked_to(tmp_path: Path) -> None:
    def handler(request: Any) -> Any:
        if uploaded_name(request) == "line-02":
            return refusal(409, "dataset_exists")
        if uploaded_name(request) == "line-03":
            return refusal(503, "busy")
        return added(request)

    with fake(handler) as client:
        outcomes = client.apply(
            new_files(tmp_path, "line-01", "line-02", "line-03", "line-04"),
            stop_on_outage=False,
        )
    assert [outcome.action for outcome in outcomes] == [
        "uploaded",
        "failed",
        "failed",
        "uploaded",
    ]


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
        "anna/level2?spacing=5.0&crs=native&format=geojson"
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


def test_geojson_becomes_a_geodataframe_in_its_own_crs() -> None:
    pytest.importorskip("geopandas")
    point = {
        "type": "Feature",
        "geometry": {"type": "Point", "coordinates": [400030.0, 8700000.0]},
        "properties": {"layer": "bed", "depth_m": 120.5},
    }

    def handler(request: Any) -> Any:
        if request.url.path.endswith("track.geojson"):
            return httpx.Response(
                200, json={"type": "FeatureCollection", "features": [point]}
            )
        return httpx.Response(
            200,
            json={
                "type": "FeatureCollection",
                "ridal": {"product_level": 2, "output_crs": "EPSG:32633"},
                "features": [point],
            },
        )

    with fake(handler) as client:
        track = client.track("line-01")
        points = client.level2("line-01", user="anna", crs="native")
    assert track.crs == "EPSG:4326"
    frame = points.to_geopandas()
    assert frame.crs.to_epsg() == 32633, "the CRS the points were asked in"
    assert frame.loc[0, "depth_m"] == 120.5
    assert frame.geometry[0].x == 400030.0


def test_anything_but_a_feature_collection_is_not_understood() -> None:
    with (
        fake(lambda request: httpx.Response(200, json={"type": "Feature"})) as client,
        pytest.raises(errors.ProtocolError),
    ):
        client.track("line-01")
