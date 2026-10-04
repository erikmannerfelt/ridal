import shutil
from pathlib import Path

import pytest
import ridal


def test_module_metadata_exists():
    assert isinstance(ridal.__version__, str)
    assert isinstance(ridal.all_steps, list)
    assert isinstance(ridal.all_step_descriptions, dict)
    assert isinstance(ridal.all_formats, list)
    assert isinstance(ridal.all_format_descriptions, dict)


def test_known_format_names_exist():
    assert "ramac" in ridal.all_formats
    assert "pulseekko" in ridal.all_formats
    assert "ramac" in ridal.all_format_descriptions
    assert "pulseekko" in ridal.all_format_descriptions


def test_run_cli_raises_migration_error():
    with pytest.raises(NotImplementedError):
        ridal.run_cli("--default", "line01.rad")


FIXTURE = Path(__file__).parent / "assets" / "mala" / "dronbreen-20250327-DAT_0066_A1.rd3"


@pytest.mark.skipif(
    not (shutil.which("cs2cs") and shutil.which("projinfo")) or not FIXTURE.exists(),
    reason="processing the fixture's coordinates needs PROJ, and the fixture",
)
def test_render_topo_matches_between_process_and_render(tmp_path):
    # #289: the corrected view from `process(render_topo=True)` (the array in
    # memory) and from `render(topo=True)` (the exported file) is one picture.
    output = tmp_path / "line.nc"
    from_process = tmp_path / "process.png"
    ridal.process(
        str(FIXTURE),
        str(output),
        steps=["zero_corr"],
        render=str(from_process),
        render_topo=True,
        quiet=True,
    )
    from_render = tmp_path / "render.png"
    topo = ridal.render(output, from_render, topo=True)
    standard = ridal.render(output, tmp_path / "standard.png")

    assert from_render.read_bytes() == from_process.read_bytes()
    assert topo["width"] == standard["width"]
    assert topo["height"] > standard["height"], "the corrected raster is taller"


@pytest.mark.skipif(
    not (shutil.which("cs2cs") and shutil.which("projinfo")) or not FIXTURE.exists(),
    reason="processing the fixture's coordinates needs PROJ, and the fixture",
)
def test_info_reads_a_processed_file_and_its_preflight_body(tmp_path):
    # #11: the HTTP client (#328) matches a local file to a served radargram
    # by its ids, and asks the server what replacing it would do (#331).
    output = tmp_path / "line.nc"
    ridal.process(str(FIXTURE), str(output), steps=["zero_corr"], quiet=True)

    (record,) = ridal.info(output)
    assert record["format"]["name"] == "ridal"
    assert record["reprocess_reason"] is None
    assert record["radargram_id"]
    assert record["revision_id"]
    assert record["samples"] > 0 and record["traces"] > 0

    body = ridal._preflight_body(output)
    assert body["radargram_id"] == record["radargram_id"]
    assert body["processing_datetime"] == record["processing_datetime"]
    assert len(body["time"]) == record["traces"]
    assert body["n_samples"] == record["samples"]

    with pytest.raises(RuntimeError, match="only apply to raw recordings"):
        ridal.info(output, crs="EPSG:32633")
