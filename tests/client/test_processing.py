"""Processing on the server (#382), through the client, against ``ridal gui``.

The same steps the browser takes: raw data in, a processing profile, drafts,
a batch, and a DEM, on a project of the test's own.
"""

import shutil
import subprocess
from pathlib import Path

import pytest

pytest.importorskip("httpx")

from conftest import FIXTURE
from ridal import client as ridal_client
from ridal.client import errors
from servers import Server

STEPS = ["subset(0 100 0 200)"]


@pytest.fixture
def recording() -> list[Path]:
    """The fixture recording's three files."""
    if not (shutil.which("cs2cs") and shutil.which("projinfo")):
        pytest.skip("reading a recording needs PROJ")
    return sorted(FIXTURE.parent.glob(f"{FIXTURE.stem}.*"))


def test_a_radargram_is_made_from_raw_data_and_kept_up_to_date(
    empty_gui: Server, recording: list[Path], tmp_path: Path
) -> None:
    with ridal_client.Client(empty_gui.url) as client:
        assert client.sources() == ()
        stored = [client.upload_raw(path) for path in recording]
        assert {f.name for f in stored} == {p.name for p in recording}

        view = client.set_source("line-new", [stored])
        assert view.source.bundles[0].format == "ramac"
        (read,) = view.recordings
        assert read.metadata.antenna_mhz == 25
        assert view.etag is not None
        assert client.sources() == ("line-new",)

        profile = client.save_profile(
            "svalbard", name="Svalbard", steps=STEPS, medium_velocity=0.168
        )
        assert profile.current_version == 1 and profile.etag is not None
        client.assign("line-new", "svalbard")
        assert client.assignments()["line-new"].profile == "svalbard"
        (status,) = client.processing_status("line-new")
        assert status.state == "not_processed"

        # A draft, followed to the end, looked at, and applied.
        draft = client.create_draft("line-new", profile.current.steps)
        looks: list[ridal_client.Draft] = []
        done = client.wait_for_draft(draft.job, on_update=looks.append, interval=0.2)
        assert done.state == "done", done
        assert looks[-1] == done
        written = client.download_draft(draft.job, tmp_path)
        assert written.stat().st_size > 0
        added = client.commit_draft(draft.job)
        assert isinstance(added, ridal_client.Added)
        assert added.radargram_id == "line-new"
        (status,) = client.processing_status("line-new")
        assert status.state == "current"

        # A new version of the profile makes it stale; a batch brings it back.
        changed = client.save_profile(
            "svalbard",
            name="Svalbard",
            steps=[*STEPS, "dewow"],
            medium_velocity=0.168,
            etag=profile.etag,
        )
        assert changed.current_version == 2
        (status,) = client.processing_status("line-new")
        assert status.state == "stale" and status.custom is False
        batch = client.reprocess("svalbard")
        ended = client.wait_for_batch(batch.id, interval=0.2)
        (item,) = ended.items
        assert item.outcome == "committed", ended
        assert item.picks_not_carried == ()
        assert client.batches()[0].id == batch.id
        (status,) = client.processing_status("line-new")
        assert status.state == "current"

        # Nothing left to do is a refusal, not an empty batch.
        with pytest.raises(errors.Conflict) as refused:
            client.reprocess("svalbard")
        assert refused.value.code == "nothing_to_reprocess"


def test_process_does_it_all_in_one_call(
    empty_gui: Server, recording: list[Path]
) -> None:
    rd3 = next(path for path in recording if path.suffix == ".rd3")
    with ridal_client.Client(empty_gui.url) as client:
        # One file of a recording stands for all of them.
        added = client.process("line-one", [rd3], STEPS)
        assert isinstance(added, ridal_client.Added)
        before = client.dataset("line-one").revision_id

        # Again with other steps: a replacement, applied since nobody has
        # picked on it.
        replaced = client.process("line-one", [rd3], [*STEPS, "dewow"])
        assert isinstance(replaced, ridal_client.Replaced)
        assert replaced.from_revision == before
        assert client.dataset("line-one").revision_id == replaced.to_revision


def test_a_dem_is_added_linked_and_kept_while_linked(
    empty_gui: Server, recording: list[Path], tmp_path: Path
) -> None:
    if not shutil.which("gdal_translate"):
        pytest.skip("making a DEM over the fixture needs GDAL")
    # The fixtures have no DEM over a recording: one made from the Mettebreen
    # DEM's values, put over Drønbreen.
    dem = tmp_path / "dronbreen.tif"
    subprocess.run(
        [
            "gdal_translate",
            "-q",
            "-a_srs",
            "EPSG:4326",
            "-a_ullr",
            "16",
            "79",
            "17.5",
            "77.5",
            "-outsize",
            "10",
            "10",
            str(FIXTURE.parents[1] / "test_dem_dtm20_mettebreen.tif"),
            str(dem),
        ],
        check=True,
    )
    with ridal_client.Client(empty_gui.url) as client:
        stored = [client.upload_raw(path) for path in recording]
        client.set_source("line-new", [stored])
        added = client.add_dem("dronbreen", "Drønbreen", dem)
        assert added.file.name == "dronbreen.tif" and added.radargram_ids == ()
        with pytest.raises(errors.PreconditionFailed):
            client.add_dem("dronbreen", "Again", dem)

        linked = client.link_dem("line-new", "dronbreen")
        assert linked.source.dem is not None and linked.source.dem.id == "dronbreen"
        assert client.dem("dronbreen").radargram_ids == ("line-new",)
        assert client.rename_dem("dronbreen", "Drønbreen 2008").name == "Drønbreen 2008"
        with pytest.raises(errors.Conflict) as refused:
            client.delete_dem("dronbreen")
        assert refused.value.code == "dem_in_use"

        client.link_dem("line-new", None)
        client.delete_dem("dronbreen")
        assert client.dems() == ()
        client.delete_source("line-new")
        assert client.sources() == ()


def test_an_ingest_plan_names_what_it_would_make(
    empty_gui: Server, recording: list[Path]
) -> None:
    with ridal_client.Client(empty_gui.url) as client:
        stored = [client.upload_raw(path) for path in recording]
        plan = client.plan_ingest(
            [stored],
            merge_threshold_seconds=600,
            id_pattern="{group}-{freq}mhz-{n:02}",
            group_name="Drønbreen",
        )
        assert plan.ok, plan.problems
        (output,) = plan.outputs
        assert output.radargram_id == "dronbreen-25mhz-01"
        (planned,) = plan.recordings
        assert planned.output == 0 and planned.track is not None

        # Created as a source with its names, the id is taken.
        names = ridal_client.SourceNames(display_name="Line 1", group_name="Drønbreen")
        view = client.set_source(
            output.radargram_id, [stored], names=names, only_new=True
        )
        assert view.source.names == ridal_client.SourceNames(
            "Line 1", "Drønbreen", "dronbreen"
        )
        again = client.plan_ingest(
            [stored], id_pattern="{group}-{freq}mhz-{n:02}", group_name="Drønbreen"
        )
        with pytest.raises(errors.PreconditionFailed):
            client.set_source(output.radargram_id, [stored], only_new=True)
        (problem,) = again.problems
        assert problem.kind == "id_taken"
        assert (problem.served, problem.has_source) == (False, True)


def test_a_file_sent_in_chunks_is_stored_as_the_same_file(
    empty_gui: Server, recording: list[Path]
) -> None:
    (data,) = [path for path in recording if path.suffix == ".rd3"]
    with ridal_client.Client(empty_gui.url) as client:
        whole = client.upload_raw(data)
        client._http.chunked_above = 0
        client._http.chunk_bytes = 1 << 16
        chunked = client.upload_raw(data)
    assert data.stat().st_size > 2 * (1 << 16), "sent in more than one chunk"
    assert chunked == whole
