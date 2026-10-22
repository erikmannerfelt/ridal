"""The client against real servers: ``ridal gui`` and a site with tokens."""

import dataclasses
import json
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


def test_a_figure_is_drawn_by_the_server(
    gui: Server, radargram: Path, tmp_path: Path
) -> None:
    # #142: the same figure spec the command line takes, rendered by the
    # server with what this account may see.
    (local,) = ridal.info(radargram)
    radargram_id = local["radargram_id"]
    spec = {
        "extent": {"traces": [100, 400]},
        "size": {"width": 400, "unit": "px", "dpi": 96},
        "x_axis": {"spacing": 50},
    }
    with ridal_client.Client(gui.url) as client:
        svg = client.figure(radargram_id, tmp_path / "figure.svg", spec)
        png = client.figure(radargram_id, tmp_path / "figure.png", spec)
        raw = client.figure(radargram_id, tmp_path / "raw.png", {**spec, "axes": False})
        with pytest.raises(errors.Conflict):
            client.figure(radargram_id, tmp_path / "x.png", {"revision_id": "another"})
        with pytest.raises(errors.BadRequest):
            client.figure(radargram_id, tmp_path / "x.png", {"profile": "/etc/passwd"})
        with pytest.raises(ValueError):
            client.figure(radargram_id, tmp_path / "x.jpg", spec)
    assert svg.read_text().startswith("<svg")
    assert png.read_bytes().startswith(b"\x89PNG")
    sidecar = json.loads(raw.with_suffix(".json").read_text())
    assert sidecar["traces"] == [100, 400]
    assert sidecar["width"] == 400
    # The server's figure is the one the command line makes of the file.
    local_svg = tmp_path / "local.svg"
    ridal.render(radargram, local_svg, figure=spec)
    assert local_svg.read_text() == svg.read_text()


def test_a_project_render_profile_draws_as_its_file_does(
    gui: Server, radargram: Path, tmp_path: Path
) -> None:
    # #183: a project's own profile, started from a built-in, renders on the
    # server exactly as the file it downloads as renders on the command line.
    (local,) = ridal.info(radargram)
    radargram_id = local["radargram_id"]
    spec = {
        "extent": {"traces": [100, 400]},
        "size": {"width": 400, "unit": "px", "dpi": 96},
        "profile": "survey",
    }
    with ridal_client.Client(gui.url) as client:
        seismic = client.render_profile("seismic")
        assert seismic.scope == "built_in"
        saved = client.save_render_profile(
            dataclasses.replace(seismic, name="survey", contrast=1.5)
        )
        try:
            assert saved.scope == "project"
            assert saved.etag is not None
            listed = client.render_profiles()
            assert listed.profiles[-1].name == "survey"
            # Only a new profile, unless asked to replace one.
            with pytest.raises(errors.PreconditionFailed):
                client.save_render_profile(saved)
            client.save_render_profile(saved, etag=saved.etag)
            with pytest.raises(errors.Conflict):
                client.save_render_profile(seismic, overwrite=True)

            profile_file = tmp_path / "survey.toml"
            profile_file.write_text(client.render_profile_file("survey"))
            served = client.figure(radargram_id, tmp_path / "served.svg", spec)
        finally:
            client.delete_render_profile("survey")
        assert "survey" not in {p.name for p in client.render_profiles().profiles}
    drawn = tmp_path / "local.svg"
    ridal.render(radargram, drawn, figure={**spec, "profile": str(profile_file)})
    assert drawn.read_text() == served.read_text()


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
