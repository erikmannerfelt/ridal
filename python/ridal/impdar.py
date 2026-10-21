"""Write an ImpDAR radargram as a Ridal NetCDF file.

:func:`from_radardata` maps an ImpDAR ``RadarData`` onto
:func:`ridal.write_netcdf`, so radargrams processed in ImpDAR can be picked in
Ridal. ImpDAR itself is not a dependency: load the data with it, then pass
the object here.

>>> from impdar.lib.RadarData import RadarData  # doctest: +SKIP
>>> rd = RadarData("line_proc.mat")  # doctest: +SKIP
>>> ridal.impdar.from_radardata(rd, "line.nc", antenna_mhz=100.0)  # doctest: +SKIP
"""

from os import PathLike
from pathlib import Path
from typing import Any, Final, Protocol

import numpy as np
import numpy.typing as npt

from ridal import _ridal

# ImpDAR keeps travel time in microseconds and Ridal in nanoseconds.
_NS_PER_US: Final = 1.0e3
_NS_PER_S: Final = 1.0e9
# ImpDAR's `decday` counts days from 0001-01-01 as day 0, so the Unix epoch is
# day 719162 (`datetime.date(1970, 1, 1).toordinal() - 1`).
_DECDAY_UNIX_EPOCH: Final = 719162.0
_SECONDS_PER_DAY: Final = 86400.0
# How far `nmo_depth` may stray from a straight line in travel time (as a
# fraction of a sample's depth) for one velocity to describe it.
_LINEAR_DEPTH_TOLERANCE: Final = 1.0e-3


class RadarFlags(Protocol):
    """The parts of ImpDAR's ``RadarFlags`` that are read here."""

    crop: Any
    nmo: Any
    elev: Any
    bpass: Any
    hfilt: Any
    rgain: Any
    agc: Any
    restack: Any
    reverse: Any
    interp: Any
    mig: Any


class RadarData(Protocol):
    """The parts of ImpDAR's ``RadarData`` that are read here."""

    data: npt.NDArray[Any]
    travel_time: npt.NDArray[Any]
    dt: float
    trig: Any
    decday: npt.NDArray[Any]
    lat: npt.NDArray[Any] | None
    long: npt.NDArray[Any] | None
    elev: npt.NDArray[Any] | None
    nmo_depth: npt.NDArray[Any] | None
    fn: str | None
    flags: RadarFlags


def from_radardata(
    rd: RadarData,
    path: str | PathLike[str],
    *,
    antenna_mhz: float,
    antenna_separation: float | None = None,
    medium_velocity: float | None = None,
    crs: str | None = None,
    elevation: npt.ArrayLike | None = None,
    dem: str | PathLike[str] | None = None,
    radargram_id: str | None = None,
    radargram_name: str | None = None,
    group_id: str | None = None,
    group_name: str | None = None,
    metadata: dict[str, Any] | None = None,
    antenna: str | None = None,
) -> str:
    """Write an ImpDAR ``RadarData`` as a Ridal NetCDF file.

    The data, travel times, trace times and positions come from `rd`; what
    ImpDAR does not record is an argument. ImpDAR's processing flags are
    written to the processing log.

    Time zero is taken as located only where ImpDAR asserted it: a crop from
    the top that left the trigger at sample 0 (``crop(..., zero_trig=True)``,
    the default, or a ``pretrig`` crop). Its position on the original
    recording is then ``flags.crop[1]`` samples in. Otherwise it is written
    as never located, even when ``rd.trig`` names a sample: ImpDAR's loaders
    can set the trigger without measuring it (the Malå RAMAC loader always
    sets 36).

    Parameters
    ----------
    rd : impdar.lib.RadarData.RadarData
        The radargram, loaded by ImpDAR.
    path : path-like
        The ``.nc`` file to write.
    antenna_mhz : float
        Antenna centre frequency in MHz, which ImpDAR does not record.
    antenna_separation : float, optional
        Transmitter-receiver separation at acquisition, in metres. Defaults to
        the separation ImpDAR's NMO correction used, if it ran; required
        otherwise.
    medium_velocity : float, optional
        Radar wave velocity in m/ns. Defaults to the one ImpDAR's NMO
        correction used (recovered from ``nmo_depth``), and otherwise to
        Ridal's default.
    crs : str, optional
        Coordinate system to project positions into. Defaults to the best WGS84
        UTM zone. ImpDAR's own projected coordinates are not used: it does not
        record which system they are in.
    elevation : array_like, optional
        Elevation of each trace in metres, instead of ``rd.elev``.
    dem : path-like, optional
        DEM to sample elevations from, instead of ``rd.elev``.
    radargram_id, radargram_name, group_id, group_name, metadata, antenna
        As in :func:`ridal.write_netcdf`. Give every version of one radargram
        the same `radargram_id`, so picks can be carried between them.

    Returns
    -------
    str
        The path written.

    Raises
    ------
    ValueError
        If `rd` has no positions, was elevation-corrected (its vertical axis
        is then elevation, not time), lacks something Ridal needs that no
        argument supplies, or fails :func:`ridal.write_netcdf`'s checks.
    """
    flags = rd.flags
    if _flag(flags.elev, 0):
        raise ValueError(
            "ImpDAR elevation-corrected this radargram, so its vertical axis is elevation, "
            "not travel time. Write it from before elev_correct."
        )
    if rd.lat is None or rd.long is None:
        raise ValueError(
            "The radargram has no latitude/longitude; Ridal needs a position per trace."
        )

    dt_ns = float(rd.dt) * _NS_PER_S
    snum = rd.data.shape[0]
    spacing_ns = (
        np.diff(np.asarray(rd.travel_time, dtype=np.float64).ravel()) * _NS_PER_US
    )
    if spacing_ns.size != snum - 1 or not np.allclose(spacing_ns, dt_ns, rtol=1.0e-3):
        raise ValueError(
            f"travel_time does not step by dt ({dt_ns:g} ns) for each of the {snum} samples."
        )
    nmo_applied = bool(_flag(flags.nmo, 0))

    if antenna_separation is None:
        if not nmo_applied:
            raise ValueError(
                "antenna_separation is required: ImpDAR records it only when NMO ran, and it "
                "did not."
            )
        antenna_separation = float(_flag(flags.nmo, 1))

    if medium_velocity is None and rd.nmo_depth is not None:
        medium_velocity = _nmo_velocity(rd.nmo_depth, rd.travel_time * _NS_PER_US)

    if elevation is None and dem is None:
        if rd.elev is None:
            raise ValueError("The radargram has no elevations; give elevation or dem.")
        elevation = rd.elev

    crop_samples = int(_flag(flags.crop, 1)) if _flag(flags.crop, 0) else 0
    trig = np.atleast_1d(np.asarray(rd.trig, dtype=np.float64))
    time_zero_located = crop_samples > 0 and bool(np.all(trig == 0))
    # Built from `dt` (checked against `travel_time` above) rather than taken from `travel_time`,
    # whose origin depends on whether the crop re-zeroed it (not recorded).
    # With time zero at sample 0 the samples are travel times from it;
    # without, they are positions on the recording clock, which starts
    # `crop_samples` before the first one.
    samples_ns = np.arange(snum, dtype=np.float64) * dt_ns
    time_zero_ns: float | None
    if time_zero_located:
        twtt_ns = samples_ns
        time_zero_ns = crop_samples * dt_ns
        zero_note = (
            f"ImpDAR: time_zero:\tTime zero at sample 0, which is {crop_samples} samples "
            f"x {dt_ns:.7f} ns = {time_zero_ns:.5f} ns into the original recording "
            "(flags.crop[1]), where ImpDAR's crop from the top put the trigger"
        )
    else:
        twtt_ns = samples_ns + crop_samples * dt_ns
        time_zero_ns = None
        reason = (
            "no crop from the top asserted it"
            if crop_samples == 0
            else f"the trigger is at sample {trig.max():g}, not at the crop"
        )
        zero_note = f"ImpDAR: time_zero:\tTime zero not located: {reason}"

    log = (*_describe_flags(flags), zero_note)

    return _ridal.write_netcdf(
        path,
        rd.data,
        twtt_ns=twtt_ns,
        time_zero_ns=time_zero_ns,
        trace_time=(np.asarray(rd.decday, dtype=np.float64) - _DECDAY_UNIX_EPOCH)
        * _SECONDS_PER_DAY,
        longitude=rd.long,
        latitude=rd.lat,
        crs=crs,
        elevation=elevation,
        dem=dem,
        antenna_mhz=antenna_mhz,
        antenna_separation=antenna_separation,
        # NMO corrects for the separation, so the data no longer needs it.
        antenna_separation_effective=0.0 if nmo_applied else None,
        medium_velocity=0.168 if medium_velocity is None else medium_velocity,
        radargram_id=radargram_id,
        radargram_name=radargram_name,
        group_id=group_id,
        group_name=group_name,
        metadata=metadata,
        log=list(log),
        source_file=None if rd.fn is None else str(Path(rd.fn)),
        antenna=antenna,
    )


def _flag(value: Any, index: int) -> Any:
    """Element `index` of an ImpDAR flag, or 0 when it has none.

    Flags are arrays such as ``crop = [applied, top, bottom]``, or scalars
    for the on/off ones; a missing element means the step did not run.

    >>> _flag(np.array([1.0, 80.0, 2046.0]), 1)
    np.float64(80.0)
    >>> _flag(0, 1)
    0
    """
    flat = np.atleast_1d(np.asarray(value)).ravel()
    return flat[index] if flat.size > index else 0


def _nmo_velocity(nmo_depth: npt.NDArray[Any], twtt_ns: npt.NDArray[Any]) -> float:
    """The velocity (m/ns) ImpDAR's NMO correction used, from its depth axis.

    From the slope rather than the last values, because a crop after NMO
    re-zeroes the travel times but not the depths.

    Raises
    ------
    ValueError
        If the depths are not linear in travel time: NMO with a velocity
        profile, which one medium velocity cannot describe.

    >>> _nmo_velocity(np.arange(5) * 0.084, np.arange(5) * 1.0)
    0.168
    """
    depth = np.asarray(nmo_depth, dtype=np.float64).ravel()
    time = np.asarray(twtt_ns, dtype=np.float64).ravel()
    slope = (depth[-1] - depth[0]) / (time[-1] - time[0])
    straight = depth[0] + (time - time[0]) * slope
    step = abs(slope * (time[1] - time[0]))
    if np.max(np.abs(depth - straight)) > _LINEAR_DEPTH_TOLERANCE * step:
        raise ValueError(
            "ImpDAR's nmo_depth is not linear in travel time (NMO with a velocity profile); "
            "give medium_velocity."
        )
    return round(float(2.0 * slope), 6)


def _describe_flags(flags: RadarFlags) -> tuple[str, ...]:
    """Processing log lines for the steps ImpDAR's flags say ran.

    Each is prefixed ``ImpDAR:`` so it cannot be mistaken for a Ridal step.
    """
    lines: list[str] = []
    if _flag(flags.crop, 0):
        lines.append(
            f"ImpDAR: crop:\tCropped a cumulative {int(_flag(flags.crop, 1))} samples off the "
            f"top of the original recording, keeping samples up to "
            f"{int(_flag(flags.crop, 2))}"
        )
    if _flag(flags.bpass, 0):
        lines.append(
            f"ImpDAR: vbp:\tVertical bandpass {_flag(flags.bpass, 1):g}-"
            f"{_flag(flags.bpass, 2):g} MHz"
        )
    if _flag(flags.hfilt, 0):
        lines.append(
            f"ImpDAR: hfilt:\tHorizontal filter type {int(_flag(flags.hfilt, 1))}"
        )
    if _flag(flags.rgain, 0):
        lines.append("ImpDAR: rgain:\tRange gain")
    if _flag(flags.agc, 0):
        lines.append("ImpDAR: agc:\tAutomatic gain control; amplitudes are relative")
    if _flag(flags.restack, 0):
        lines.append("ImpDAR: restack:\tRestacked traces")
    if _flag(flags.reverse, 0):
        lines.append("ImpDAR: reverse:\tReversed the profile direction")
    if _flag(flags.nmo, 0):
        lines.append(
            f"ImpDAR: nmo:\tNormal moveout correction for a {_flag(flags.nmo, 1):g} m "
            "antenna separation"
        )
    if _flag(flags.interp, 0):
        lines.append(
            f"ImpDAR: interp:\tInterpolated to a constant {_flag(flags.interp, 1):g} m trace "
            "spacing"
        )
    mig = str(_flag(flags.mig, 0))
    if mig not in ("none", "0"):
        lines.append(f"ImpDAR: migrate:\t{mig} migration")
    return tuple(lines)
