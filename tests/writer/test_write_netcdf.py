"""`ridal.write_netcdf`: a radargram from arrays, read back as written."""

from pathlib import Path
from typing import Any, Final

import numpy as np
import pytest
import ridal

# Reads the files back; installed in CI, skipped where it is not.
netCDF4 = pytest.importorskip("netCDF4")

N_SAMPLES: Final = 60
N_TRACES: Final = 12
DT_NS: Final = 0.8


def arguments(**changes: Any) -> dict[str, Any]:
    """Arguments for a small radargram near Drønbreen, time zero located."""
    base: dict[str, Any] = {
        "data": np.arange(N_SAMPLES * N_TRACES, dtype=np.float32).reshape(
            N_SAMPLES, N_TRACES
        ),
        "twtt_ns": np.arange(N_SAMPLES) * DT_NS,
        "time_zero_ns": 40.0,
        "trace_time": np.datetime64("2025-03-27T10:00:00", "ns")
        + np.arange(N_TRACES) * np.timedelta64(500, "ms"),
        "longitude": 16.9 + np.arange(N_TRACES) * 1e-5,
        "latitude": np.full(N_TRACES, 78.2),
        "elevation": np.full(N_TRACES, 500.0),
        "antenna_mhz": 100.0,
        "antenna_separation": 1.0,
        "radargram_id": "line-1",
    }
    base.update(changes)
    return base


def write(directory: Path, **changes: Any) -> Any:
    arguments_ = arguments(**changes)
    path = ridal.write_netcdf(
        directory / "line-1.nc", arguments_.pop("data"), **arguments_
    )
    return netCDF4.Dataset(path)


def test_the_arrays_are_written_as_given(tmp_path: Path) -> None:
    given = arguments()
    with write(tmp_path) as nc:
        np.testing.assert_array_equal(nc["data"][:], given["data"])
        np.testing.assert_allclose(nc["twtt"][:], given["twtt_ns"], atol=1e-4)
        assert float(nc["twtt_time_zero"][:]) == 40.0
        assert float(nc["twtt_crop"][:]) == 40.0
        np.testing.assert_allclose(nc["longitude"][:], given["longitude"], atol=1e-7)
        np.testing.assert_allclose(nc["elevation"][:], 500.0)
        assert nc.getncattr("ridal_radargram_id") == "line-1"
        assert nc.getncattr("crs") == "EPSG:32633"
        assert nc.getncattr("antenna_separation_effective") == 1.0


def test_datetime64_and_epoch_seconds_are_the_same_times(tmp_path: Path) -> None:
    given = arguments()["trace_time"]
    seconds = (given - np.datetime64(0, "s")) / np.timedelta64(1, "s")
    (tmp_path / "a").mkdir()
    (tmp_path / "b").mkdir()
    with write(tmp_path / "a") as a, write(tmp_path / "b", trace_time=seconds) as b:
        np.testing.assert_array_equal(a["time"][:], b["time"][:])
        assert a.getncattr("start_datetime").startswith("2025-03-27T10:00:00")


def test_an_unknown_time_zero_is_written_as_never_located(tmp_path: Path) -> None:
    with write(
        tmp_path, time_zero_ns=None, twtt_ns=12.0 + np.arange(N_SAMPLES) * DT_NS
    ) as nc:
        assert float(nc["twtt_time_zero"][:]) == 0.0
        assert float(nc["twtt_crop"][:]) == 12.0


def test_projected_coordinates_need_their_crs(tmp_path: Path) -> None:
    x = 500_000.0 + np.arange(N_TRACES)
    y = np.full(N_TRACES, 8_680_000.0)
    with write(
        tmp_path, longitude=None, latitude=None, x=x, y=y, crs="EPSG:32633"
    ) as nc:
        np.testing.assert_allclose(nc["easting"][:], x)
    with pytest.raises(ValueError, match="need crs"):
        write(tmp_path, longitude=None, latitude=None, x=x, y=y)


@pytest.mark.parametrize(
    ("changes", "message"),
    [
        ({"x": np.zeros(N_TRACES), "y": np.zeros(N_TRACES)}, "only one of those pairs"),
        ({"longitude": None, "latitude": None}, "Trace positions are required"),
        ({"elevation": None}, "Elevations are required"),
        ({"latitude": np.r_[78.2, np.nan, np.full(N_TRACES - 2, 78.2)]}, "at traces 1"),
        ({"data": np.zeros(N_SAMPLES)}, "two-dimensional"),
        ({"twtt_ns": np.arange(N_SAMPLES) ** 1.5}, "evenly spaced"),
        ({"time_zero_ns": 0.0}, "0 means never located"),
        ({"antenna_separation_effective": 0.5}, "antenna_separation_effective"),
    ],
)
def test_bad_arguments_are_refused(
    tmp_path: Path, changes: dict[str, Any], message: str
) -> None:
    with pytest.raises(ValueError, match=message):
        write(tmp_path, **changes)


def test_a_missing_directory_is_named(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="does not exist"):
        write(tmp_path / "missing")


def test_without_an_id_the_file_stem_is_used_with_a_warning(tmp_path: Path) -> None:
    with (
        pytest.warns(UserWarning, match="No radargram ID"),
        write(tmp_path, radargram_id=None) as nc,
    ):
        assert nc.getncattr("ridal_radargram_id") == "line-1"
