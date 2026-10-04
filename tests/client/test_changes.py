"""The client changing things on a real ``ridal gui``: uploads, replaces and picks."""

import time
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("httpx")

import ridal
from conftest import process_fixture
from ridal import client as ridal_client
from ridal.client import errors
from servers import Server


def revision(directory: Path, name: str) -> Path:
    """A new revision of the fixture radargram. A second apart, so the
    processing dates, and with them the revision ids, differ."""
    time.sleep(1.1)
    return process_fixture(directory / name)


def a_line(client: ridal_client.Client, radargram_id: str) -> dict[str, Any]:
    """A picked line on the current revision, as the browser would save it."""
    document = client.interpretation_template(radargram_id)
    document["features"].append(
        {
            "type": "Feature",
            "geometry": {
                "type": "LineString",
                "coordinates": [[5.0, 20.0], [40.0, 25.0]],
            },
            "properties": {"id": "f-0001", "label": "bed"},
        }
    )
    return document


def test_a_plan_uploads_new_files_and_replaces_unpicked_ones(
    empty_gui: Server, tmp_path: Path
) -> None:
    first = revision(tmp_path, "first.nc")
    with ridal_client.Client(empty_gui.url) as client:
        planned = client.plan([first])
        assert [record.status for record in planned.records] == ["new"]
        (outcome,) = client.apply(planned)
        assert outcome.action == "uploaded", outcome.detail
        assert outcome.added is not None
        radargram_id = outcome.added.radargram_id

        # Nothing to do the second time.
        assert client.plan([first]).summary() == {"unchanged": 1}

        # A new revision of a radargram nobody has picked is safe.
        second = revision(tmp_path, "second.nc")
        planned = client.plan([second])
        assert planned.summary() == {"safe": 1}
        (outcome,) = client.apply(planned)
        assert outcome.action == "replaced", outcome.detail
        (local,) = ridal.info(second)
        assert client.dataset(radargram_id).revision_id == local["revision_id"]

        # And uploading it again as if it were new is refused.
        with pytest.raises(errors.Conflict):
            client.upload(second)


def test_picks_are_saved_carried_across_a_replace_and_promoted(
    empty_gui: Server, tmp_path: Path
) -> None:
    first = revision(tmp_path, "first.nc")
    with ridal_client.Client(empty_gui.url) as client:
        added = client.upload(first)
        radargram_id = added.radargram_id
        document = a_line(client, radargram_id)
        assert "coordinates" in document, "the template carries the revision's axes"

        saved = client.save_interpretation(radargram_id, document)
        # A second "create" is refused.
        with pytest.raises(errors.PreconditionFailed):
            client.save_interpretation(radargram_id, document)
        # An edit from the version it was read at is saved; one from a
        # version that has moved on is refused.
        stored = client.interpretation(radargram_id, saved.user)
        edited = {**stored.document, "meta": {"note": "checked"}}
        resaved = client.save_interpretation(radargram_id, edited, etag=stored.etag)
        assert resaved.version != saved.version
        with pytest.raises(errors.PreconditionFailed):
            client.save_interpretation(radargram_id, stored.document, etag=stored.etag)

        # The picks come back as level 2 points with coordinates.
        points = client.level2(radargram_id)
        assert points.data["features"], "a picked line has points"
        if _has_geopandas():
            frame = points.to_geopandas()
            assert frame.crs.to_epsg() == 4326
            assert set(frame["layer"]) == {"bed"}

        # Replacing a picked radargram is risky, and the server says why.
        second = revision(tmp_path, "second.nc")
        planned = client.plan([second])
        (record,) = planned.records
        assert record.status == "risky"
        assert record.report is not None
        (consequence,) = record.report.documents
        assert consequence.user == saved.user
        # The preflight is the report staging would give.
        staged = client.stage_replacement(second)
        assert staged.report == record.report
        client.discard_replacement(radargram_id, staged.token)

        (outcome,) = client.apply(
            planned, allow={"current", "carried", "approximate", "partial"}
        )
        assert outcome.action == "replaced", outcome.detail

        # The picks are still drawn on the first revision, shown carried, and
        # can be adopted onto the new one.
        view = client.carried(radargram_id)
        assert view.document is not None
        assert view.report.to_revision == client.dataset(radargram_id).revision_id
        promoted = client.promote(radargram_id)
        assert promoted.to_revision == view.report.to_revision
        assert promoted.from_revision == added.revision_id


def test_a_replacement_beyond_what_is_allowed_is_given_back(
    empty_gui: Server, tmp_path: Path
) -> None:
    first = revision(tmp_path, "first.nc")
    with ridal_client.Client(empty_gui.url) as client:
        added = client.upload(first)
        client.save_interpretation(
            added.radargram_id, a_line(client, added.radargram_id)
        )
        second = revision(tmp_path, "second.nc")
        with pytest.raises(errors.ReplacementRefused) as refused:
            client.replace(second, allow=set())
        assert refused.value.report is not None
        # Nothing changed, and nothing was left staged.
        assert client.dataset(added.radargram_id).revision_id == added.revision_id
    staging = empty_gui.root / "ridal_data" / ".staging"
    assert not any(staging.glob("*.nc"))


def test_project_documents_round_trip_with_their_version(empty_gui: Server) -> None:
    with ridal_client.Client(empty_gui.url) as client:
        layers = client.layers()
        assert layers.etag is None, "a new project has no layers yet"
        bed = {**layers.data, "layers": [{"id": "bed", "name": "Bed"}]}
        saved = client.save_layers(bed, etag=layers.etag)
        assert saved.etag is not None
        # Created meanwhile: a second create is refused, and so is an edit
        # from before the latest change.
        with pytest.raises(errors.PreconditionFailed):
            client.save_layers(bed, etag=None)
        surface = {
            **layers.data,
            "layers": [
                {"id": "bed", "name": "Bed"},
                {"id": "surface", "name": "Surface"},
            ],
        }
        client.save_layers(surface, etag=saved.etag)
        with pytest.raises(errors.PreconditionFailed):
            client.save_layers(bed, etag=saved.etag)
        ids = [layer["id"] for layer in client.layers().data["layers"]]
        assert ids == ["bed", "surface"]

        derived = client.derived()
        client.save_derived(derived.data, etag=derived.etag)


def _has_geopandas() -> bool:
    try:
        import geopandas  # noqa: F401
    except ImportError:
        return False
    return True
