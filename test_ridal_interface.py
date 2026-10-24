import datetime
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


FIXTURE = (
    Path(__file__).parent / "assets" / "mala" / "dronbreen-20250327-DAT_0066_A1.rd3"
)

needs_proj = pytest.mark.skipif(
    not (shutil.which("cs2cs") and shutil.which("projinfo")) or not FIXTURE.exists(),
    reason="processing the fixture's coordinates needs PROJ, and the fixture",
)


@needs_proj
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


@needs_proj
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

    body = ridal._ridal._preflight_body(output)
    assert body["radargram_id"] == record["radargram_id"]
    assert body["processing_datetime"] == record["processing_datetime"]
    assert len(body["time"]) == record["traces"]
    assert body["n_samples"] == record["samples"]

    with pytest.raises(RuntimeError, match="only apply to raw recordings"):
        ridal.info(output, crs="EPSG:32633")


@needs_proj
def test_batch_process_names_outputs_after_an_id_pattern(tmp_path):
    # #368: the same patterns as `batch-process --id-pattern`.
    outputs = ridal.batch_process(
        [str(FIXTURE.with_suffix(".rad"))],
        str(tmp_path),
        steps=["subset(0 50 0 100)"],
        quiet=True,
        id_pattern="line-{freq}mhz-{n:02, from=7}",
        display_name_pattern="Line {n} at {freq} MHz",
    )
    assert [Path(p).name for p in outputs] == ["line-25mhz-07.nc"]
    (record,) = ridal.info(outputs)
    assert record["radargram_id"] == "line-25mhz-07"
    assert record["display_name"] == "Line 7 at 25 MHz"

    with pytest.raises(RuntimeError, match="uppercase"):
        ridal.batch_process(
            [str(FIXTURE.with_suffix(".rad"))],
            str(tmp_path),
            quiet=True,
            id_pattern="line-{freq}MHz",
        )


@needs_proj
def test_render_figure_from_a_dict_or_a_spec_file(tmp_path):
    # #142: `render(figure=...)` is `ridal render --figure`, with the spec
    # given inline or as a file.
    import json

    output = tmp_path / "line.nc"
    ridal.process(str(FIXTURE), str(output), steps=["zero_corr"], quiet=True)
    spec = {
        "extent": {"traces": [10, 60]},
        "size": {"width": 400, "unit": "px", "dpi": 96},
        "x_axis": {"unit": "trace", "spacing": 10},
    }

    svg = ridal.render(output, tmp_path / "figure.svg", figure=spec)
    assert Path(svg["path"]).read_text().startswith("<svg")
    assert svg["width"] == 400

    spec_file = tmp_path / "spec.json"
    spec_file.write_text(json.dumps({**spec, "axes": False}))
    raw = ridal.render(output, tmp_path / "raw.png", figure=spec_file)
    sidecar = json.loads(Path(raw["sidecar"]).read_text())
    assert sidecar["traces"] == [10, 60]
    assert sidecar["x"]["unit"] == "trace"

    with pytest.raises(ValueError):
        ridal.render(output, tmp_path / "x.png", figure=spec, topo=True)
    with pytest.raises(RuntimeError, match="spaceing"):
        ridal.render(output, tmp_path / "x.png", figure={"x_axis": {"spaceing": 1}})

    # Picks come from a project: asked for without one, the figure is
    # refused rather than drawn without them, and a project needs a figure.
    with pytest.raises(RuntimeError, match="project"):
        ridal.render(output, tmp_path / "x.png", figure={"picks": {"items": [{}]}})
    with pytest.raises(ValueError, match="figure="):
        ridal.render(output, tmp_path / "x.png", project=tmp_path)
    assert svg["warnings"] == []


@needs_proj
def test_render_and_track_true_derive_paths_beside_the_output(tmp_path):
    # #93: `True` means what `ridal process -r` and `--track` without a value
    # mean, and `False` means leaving the argument out.
    output = tmp_path / "line.nc"
    ridal.process(
        str(FIXTURE), output, steps=["zero_corr"], render=True, track=True, quiet=True
    )
    assert (tmp_path / "line.jpg").is_file()
    assert (tmp_path / "line_track.csv").is_file()

    other = tmp_path / "other.nc"
    ridal.process(
        str(FIXTURE), other, steps=["zero_corr"], render=False, track=False, quiet=True
    )
    assert sorted(p.name for p in tmp_path.iterdir() if p.stem.startswith("other")) == [
        "other.nc"
    ]


def test_return_dataset_refuses_render_true_but_not_false():
    with pytest.raises(ValueError, match="render"):
        ridal.process(str(FIXTURE), return_dataset=True, render=True)
    with pytest.raises(ValueError, match="track"):
        ridal.process(str(FIXTURE), return_dataset=True, track=True)


def test_coords_writes_a_csv_that_process_reads(tmp_path):
    # #13: a recording without positions, its start and end given to
    # `coords`, then processed with nothing else.
    line = "dronbreen-20250327-DAT_0066_A1"
    for ext in ["rad", "rd3"]:
        shutil.copy(Path("assets/mala") / f"{line}.{ext}", tmp_path)
    rad = tmp_path / f"{line}.rad"
    start = datetime.datetime(
        2025, 3, 27, 14, 10, 20, tzinfo=datetime.timezone(datetime.timedelta(hours=2))
    )

    with pytest.warns(UserWarning, match="header's 0.2 s"):
        written = ridal.coords(
            rad,
            start=(16.648, 78.1019, 715),
            end=(16.661, 78.1034, 720),
            start_time=start,
        )
    assert written == str(tmp_path / f"{line}.coords.csv")
    rows = [
        row for row in Path(written).read_text().splitlines() if not row.startswith("#")
    ]
    assert rows[0] == "trace,time,lon,lat,alt,time_source,alt_source"
    assert rows[1].startswith(
        "0,2025-03-27T12:10:20Z,16.648000000,78.101900000,715.000,synthetic,"
    )

    with pytest.raises(RuntimeError, match="overwrite=True"):
        ridal.coords(rad, start=(16.648, 78.1019), end=(16.661, 78.1034))
    with pytest.raises(ValueError, match="both start and end"):
        ridal.coords(rad, start=(16.648, 78.1019), overwrite=True)

    output = tmp_path / "line.nc"
    ridal.process(str(rad), str(output), steps=["subset(0 100)"], quiet=True)
    (record,) = ridal.info(output)
    assert record["traces"] == 100


@needs_proj
def test_steps_may_be_a_string_a_list_or_a_tuple(tmp_path):
    # The recipe hash covers the steps that ran: the same hash from each
    # spelling means each ran the same steps.
    spellings = {
        "text": "subset(0 50 0 100), zero_corr",
        "list": ["subset(0 50 0 100)", "zero_corr"],
        "tuple": ("subset(0 50 0 100)", "zero_corr"),
    }
    revisions = {}
    for name, steps in spellings.items():
        output = tmp_path / f"{name}.nc"
        ridal.process(str(FIXTURE), str(output), steps=steps, quiet=True)
        (record,) = ridal.info(output)
        revisions[name] = record["recipe_hash"]
    assert len(set(revisions.values())) == 1, revisions

    other = tmp_path / "other.nc"
    ridal.process(str(FIXTURE), str(other), steps="subset(0 50 0 100)", quiet=True)
    (record,) = ridal.info(other)
    assert record["recipe_hash"] != revisions["text"]


@needs_proj
def test_batch_process_applies_the_shared_options(tmp_path):
    # Group, metadata and steps reach every output of a batch, as they do
    # the one output of `process`.
    (output,) = ridal.batch_process(
        [str(FIXTURE.with_suffix(".rad"))],
        str(tmp_path),
        steps=("subset(0 50 0 100)",),
        quiet=True,
        metadata={"campaign": "spring"},
        group_name="Drønbreen",
        group_id="dronbreen",
    )
    (record,) = ridal.info(output)
    assert record["group_name"] == "Drønbreen"
    assert record["group_id"] == "dronbreen"
    assert record["traces"] == 50

    dataset = ridal.process(
        str(FIXTURE),
        steps=["subset(0 50 0 100)"],
        metadata={"campaign": "spring"},
        quiet=True,
        return_dataset=True,
    )
    assert "campaign" in str(dataset["attrs"])


@pytest.mark.parametrize("function", ["process", "batch_process"])
def test_steps_are_chosen_one_way(function, tmp_path):
    run = getattr(ridal, function)
    args = (
        (str(FIXTURE), str(tmp_path))
        if function == "batch_process"
        else (str(FIXTURE),)
    )
    for choice in [
        {"steps": "zero_corr", "default": True},
        {"default": True, "default_with_topo": True},
        {"steps": ["zero_corr"], "default_with_topo": True},
    ]:
        with pytest.raises(ValueError, match="Only one of"):
            run(*args, quiet=True, **choice)
    with pytest.raises(ValueError, match="steps must be a string"):
        run(*args, steps=5, quiet=True)
    with pytest.raises(ValueError, match="no_such_step"):
        run(*args, steps=["no_such_step"], quiet=True)


def test_batch_process_side_outputs_must_be_directories(tmp_path):
    not_a_directory = tmp_path / "file.txt"
    not_a_directory.write_text("")
    for side_output in ["track", "render"]:
        with pytest.raises(
            ValueError, match=f"{side_output} must be an existing directory"
        ):
            ridal.batch_process(
                [str(FIXTURE)],
                str(tmp_path),
                quiet=True,
                **{side_output: str(not_a_directory)},
            )
    with pytest.raises(ValueError, match="output must be an existing directory"):
        ridal.batch_process([str(FIXTURE)], str(not_a_directory), quiet=True)
