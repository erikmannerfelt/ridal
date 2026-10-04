"""The client against real servers: ``ridal gui`` and a site with tokens."""

from pathlib import Path

import pytest

pytest.importorskip("httpx")

import ridal
from ridal import client as ridal_client
from ridal.client import errors
from servers import Server, Site


def test_the_server_says_what_it_runs(gui: Server) -> None:
    with ridal_client.Client(gui.url) as client:
        assert client.health().version == ridal.__version__
        me = client.me()
    assert me.user == "default"
    assert me.token is None


def test_a_local_file_is_recognised_on_the_server(gui: Server, radargram: Path) -> None:
    # The point of #11: matching a local file to a served radargram by ids,
    # without downloading anything.
    (local,) = ridal.info(radargram)
    with ridal_client.Client(gui.url) as client:
        catalog = client.catalog()
        served = catalog.by_id()[local["radargram_id"]]
        assert served.revision_id == local["revision_id"]
        assert served.shape == (local["samples"], local["traces"])
        # One radargram is the same record, except that it does not count
        # picks, which only the listing does.
        single = client.dataset(served.radargram_id)
        assert single.revision_id == served.revision_id
        assert single.line_count is None and served.line_count == 0

        axes = client.axes(served.radargram_id)
        samples, traces = served.shape
        assert axes.twtt is not None and axes.twtt.shape == (samples,)
        assert axes.distance is not None and axes.distance.shape == (traces,)

        listed = client.interpretations(served.radargram_id)
        assert listed.users == () and listed.writable


def test_downloads_are_the_files_the_server_holds(
    gui: Server, radargram: Path, tmp_path: Path
) -> None:
    (local,) = ridal.info(radargram)
    radargram_id = local["radargram_id"]
    events: list[ridal_client.ProgressEvent] = []
    with ridal_client.Client(gui.url) as client:
        written = client.download_radargram(
            radargram_id, tmp_path, progress=events.append
        )
        track = client.track(radargram_id)
        with pytest.raises(errors.NotFound):
            client.interpretation(radargram_id, "nobody")
        with pytest.raises(errors.NotFound):
            client.dataset("no-such-line")
    assert written.read_bytes() == radargram.read_bytes()
    assert events and events[-1].done == radargram.stat().st_size
    assert track.crs == "EPSG:4326"
    assert track.data["features"], "the fixture has a track"


def test_a_token_reaches_its_projects_on_a_site(site: Site) -> None:
    with ridal_client.Client(site.url, project="glac", token=site.token) as client:
        me = client.me()
        assert me.user == "anna"
        assert me.token is not None and me.token.name == "tests"
        assert me.token.grants == (ridal_client.Grant("glac", "viewer", "all"),)
        (project,) = client.projects()
        assert (project.key, project.role) == ("glac", "viewer")
        assert len(client.catalog().datasets) == 1

    with (
        ridal_client.Client(site.url, project="elsewhere", token=site.token) as client,
        pytest.raises(errors.Forbidden) as refused,
    ):
        client.catalog()
    assert refused.value.code == "token_not_granted"


def test_a_bad_token_is_refused_on_a_site(site: Site) -> None:
    with (
        ridal_client.Client(site.url, project="glac", token="ridal_wrong") as client,
        pytest.raises(errors.Unauthorized) as refused,
    ):
        client.catalog()
    assert refused.value.code == "invalid_token"
