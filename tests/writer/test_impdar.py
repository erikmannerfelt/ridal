"""`ridal.impdar`: ImpDAR's processing, checked against what Ridal writes.

Each radargram here is made and processed by ImpDAR itself, so what ImpDAR
says about it (its depth axis, its trigger) is the reference.
"""

from pathlib import Path
from typing import Any, Final

import numpy as np
import pytest
import ridal.impdar

# Reads the files back; installed in CI, skipped where it is not.
netCDF4 = pytest.importorskip("netCDF4")
RadarData = pytest.importorskip("impdar.lib.RadarData").RadarData

N_SAMPLES: Final = 400
N_TRACES: Final = 30
DT_S: Final = 1.6e-9
# ImpDAR's RAMAC loader sets this trigger for every file (`load_ramac.py`).
RAMAC_TRIGGER: Final = 36


def raw() -> Any:
    """A radargram as ImpDAR's RAMAC loader leaves it."""
    rd = RadarData(None)
    rng = np.random.default_rng(0)
    rd.data = rng.normal(size=(N_SAMPLES, N_TRACES))
    rd.snum, rd.tnum = N_SAMPLES, N_TRACES
    rd.dt = DT_S
    rd.travel_time = np.arange(N_SAMPLES) * DT_S * 1.0e6
    rd.trig = np.full(N_TRACES, float(RAMAC_TRIGGER))
    rd.trig_level = 0.0
    rd.trace_num = np.arange(N_TRACES) + 1
    rd.trace_int = np.full(N_TRACES, 0.5)
    # 2026-04-20T12:00:00 UTC, a trace every half second, as days since
    # 0001-01-01 the way the loader computes them.
    start = np.datetime64("2026-04-20T12:00:00") - np.datetime64("0001-01-01T00:00:00")
    rd.decday = (start / np.timedelta64(1, "D")) + np.arange(N_TRACES) * 0.5 / 86400.0
    rd.lat = np.full(N_TRACES, 61.582)
    rd.long = 7.94 + np.arange(N_TRACES) * 1e-5
    rd.elev = np.full(N_TRACES, 1700.0)
    rd.pressure = np.zeros(N_TRACES)
    rd.chan = 1
    rd.fn = "DAT_0001_A1.rd3"
    return rd


def processed() -> Any:
    """Pretrigger cropped, NMO corrected (6 m at 0.168 m/ns), cropped again."""
    rd = raw()
    rd.crop(0, dimension="pretrig")
    rd.nmo(6.0, uice=1.68e8)
    rd.crop(20, top_or_bottom="top", dimension="snum")
    return rd


def write(rd: Any, directory: Path, **arguments: Any) -> Any:
    path = ridal.impdar.from_radardata(
        rd, directory / "line.nc", antenna_mhz=25.0, radargram_id="line", **arguments
    )
    return netCDF4.Dataset(path)


def test_a_raw_file_has_no_time_zero_even_with_a_trigger(tmp_path: Path) -> None:
    with write(raw(), tmp_path, antenna_separation=6.2) as nc:
        assert float(nc["twtt_time_zero"][:]) == 0.0
        assert float(nc["twtt_crop"][:]) == 0.0
        assert nc.getncattr("antenna_separation_effective") == pytest.approx(6.2)
        assert "Time zero not located" in nc.getncattr("processing_log")


def test_a_cropped_file_puts_time_zero_where_impdar_did(tmp_path: Path) -> None:
    rd = processed()
    with write(rd, tmp_path) as nc:
        # The trigger crop and the later one, both from the top.
        expected = (RAMAC_TRIGGER + 20) * DT_S * 1.0e9
        assert float(nc["twtt_time_zero"][:]) == pytest.approx(expected, abs=1e-4)
        assert float(nc["twtt_crop"][:]) == pytest.approx(expected, abs=1e-4)
        assert float(nc["twtt"][0]) == 0.0
        assert nc["twtt"].getncattr("anchor_name") == "twtt_normal_incidence"


def test_depths_are_impdars_nmo_depths(tmp_path: Path) -> None:
    rd = processed()
    with write(rd, tmp_path) as nc:
        assert nc.getncattr("medium_velocity") == pytest.approx(0.168)
        assert nc.getncattr("antenna_separation") == pytest.approx(6.0)
        assert nc.getncattr("antenna_separation_effective") == 0.0
        depth = np.asarray(nc["depth"][:], dtype=np.float64)
        # ImpDAR crops its depth axis without re-zeroing it; Ridal's starts
        # at time zero, so they differ by the depth of the samples cropped.
        np.testing.assert_allclose(depth, rd.nmo_depth - rd.nmo_depth[0], atol=1e-3)


def test_trace_times_and_positions(tmp_path: Path) -> None:
    rd = raw()
    with write(rd, tmp_path, antenna_separation=6.2) as nc:
        assert nc.getncattr("start_datetime").startswith("2026-04-20T12:00:00")
        np.testing.assert_allclose(np.diff(nc["time"][:]), 0.5, atol=1e-3)
        np.testing.assert_allclose(nc["longitude"][:], rd.long, atol=1e-7)
        assert nc.getncattr("original_filepaths") == "DAT_0001_A1.rd3"


def test_impdars_processing_is_logged(tmp_path: Path) -> None:
    with write(processed(), tmp_path) as nc:
        log = nc.getncattr("processing_log")
        assert (
            "ImpDAR: nmo:\tNormal moveout correction for a 6 m antenna separation"
            in log
        )
        assert "ImpDAR: crop:\tCropped a cumulative 56 samples" in log
        # Ridal's own steps are only Ridal's.
        assert list(np.atleast_1d(nc.getncattr("processing_steps"))) in ([], [""])


def test_an_elevation_corrected_file_is_refused(tmp_path: Path) -> None:
    rd = processed()
    rd.flags.elev = 1
    with pytest.raises(ValueError, match="vertical axis is elevation"):
        write(rd, tmp_path)


def test_the_separation_is_required_without_nmo(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="antenna_separation is required"):
        write(raw(), tmp_path)
