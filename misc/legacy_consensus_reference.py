"""Verified reproduction of the Mannerfelt et al. (2026) crowd-consensus algorithm.

This is the **oracle** for the derived-layer regression test (issue #210). It
reimplements `merge_all_interpretations` from the study's `svalbardradar`
package for a single radargram, reading the legacy pick JSONs directly, so that
Ridal's derived-layer evaluator can be checked against a known-good result
rather than against a reimplementation written at the same time as the code it
is meant to test.

Verification status (2026-09-20)
-------------------------------
Run against the complete Zenodo pick archive, this script reproduces **all 138
radargrams** with exactly matching contributor counts, and ``thickness`` agrees
with the published table to a **median absolute error of 0.0000 m** on every one
of them. (One profile, ``slakbreen-20230320-DAT_0303_A1_1``, has a 43.7 m max
deviation at a few positions; every median is 0.)

Driving the same picks through Ridal's own pipeline -- convert to gprinterp,
``ridal interp export --spacing per-trace``, reduce by ``shallowest``, then apply
the consensus below -- reproduces ``thickness`` and every ``thickness_user_*``
column to a median absolute error of **4e-6 m**, i.e. float32 precision.

The two subtleties that make it agree, both of which are easy to get wrong:

* ``thickness`` is the ``quantile(0.49, interpolation="lower")`` of pick depth,
  not the median. It selects the order statistic at index
  ``floor(0.49 * (n - 1))``. 0.49 rather than 0.50 keeps a 50/50 split from
  oscillating between two answers.
* Legacy pick ``y`` is **bottom-up**: ``depth(y) = depth_axis[n_samples - 1 - y]``.
  The published data README's "x and y from the upper left corner" is wrong.
  See PLAN.md §2.2.

Inputs live outside the repository, so this script is a local research tool
rather than part of the test suite; the committed fixture it produces is what CI
consumes. Take the pick JSONs from the **published Zenodo archive**, not from a
local copy -- one local copy was found to be missing 104 of the 1671 files,
including whole contributors, which silently shifts the consensus by one order
statistic:

    curl -L -o interpretation_lines.zip \
        "https://zenodo.org/records/21890778/files/interpretation_lines.zip?download=1"
    unzip -q interpretation_lines.zip -d interp_full

Usage
-----
    python3 misc/legacy_consensus_reference.py \
        --radar-key dronbreen-20250327-DAT_0066_A1_1 \
        --interpretations interp_full \
        --processed old/mannerfelt2026_processed_data \
        --consensus old/mannerfelt2026_consensus.arrow \
        --out assets/interp/dronbreen-20250327-DAT_0066_A1_1/expected_consensus.csv
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Final

import geopandas as gpd
import numpy as np
import pandas as pd

#: Legacy ``kind`` values, and the ``name`` values that stand in for them in
#: pre-May submissions where ``kind`` is absent. Note that the legacy name
#: fallback produces ``temperate_ice`` while the ``kind`` field says
#: ``temperate``, and the study's merge step only looked for ``temperate`` --
#: so old-style submissions were silently dropped from the published
#: consensus. Both spellings are normalised here.
NAME_TO_KIND: Final[dict[str, str]] = {
    "Glacier bed": "bed_unspecified",
    "Cold glacier bed": "bed_cold",
    "Glacier bed missing": "bed_missing",
    "Temperate ice": "temperate_ice",
}
KIND_ALIASES: Final[dict[str, str]] = {"temperate_ice": "temperate"}

#: Pick kinds pooled for the glacier-bed consensus. ``bed_cold`` means "bed
#: with no temperate ice above it", so it is a bed observation as well as a
#: statement about the CTS, and votes in both pools.
BED_KINDS: Final[tuple[str, ...]] = ("bed_cold", "bed_unspecified")
CTS_KINDS: Final[tuple[str, ...]] = ("bed_cold", "temperate")

#: Consensus percentile and its interpolation rule (see the module docstring).
CONSENSUS_Q: Final[float] = 0.49


def nmad(values: np.ndarray) -> float:
    """Normalised median absolute deviation, NaN-skipping.

    Parameters
    ----------
    values
        Sample values. NaN entries are ignored.

    Returns
    -------
    float
        ``1.4826 * median(abs(v - median(v)))``, or NaN for an empty sample.
        The scale factor makes this a consistent estimator of the standard
        deviation for normally distributed data.

    Examples
    --------
    >>> float(nmad(np.array([1.0, 2.0, 3.0, 4.0, 100.0])))
    1.4826
    """
    finite = np.asarray(values, dtype=float)
    finite = finite[~np.isnan(finite)]
    if finite.size == 0:
        return float("nan")
    return float(1.4826 * np.median(np.abs(finite - np.median(finite))))


def quantile_lower(values: np.ndarray, quantile: float) -> float:
    """Order statistic at ``floor(quantile * (n - 1))``, NaN-skipping.

    This is pandas' ``quantile(q, interpolation="lower")``. It never
    interpolates between two samples, so the result is always a value some
    contributor actually picked -- which is why the study used it.

    Examples
    --------
    >>> float(quantile_lower(np.array([10.0, 20.0, 30.0, 40.0, 50.0]), 0.49))
    20.0
    """
    finite = np.sort(np.asarray(values, dtype=float))
    finite = finite[~np.isnan(finite)]
    if finite.size == 0:
        return float("nan")
    return float(finite[int(np.floor(quantile * (finite.size - 1)))])


def latest_submission_per_user(interpretations: Path, radar_key: str) -> dict[str, Path]:
    """Map contributor nickname to their most recent submission.

    The radargram id inside the JSON files is known to be unreliable, so both
    the contributor and the radar key come from the directory structure
    (``.../<user>/<radar_key>/<file>.json``). Filenames carry an ISO timestamp,
    so the lexicographically greatest filename is the latest submission.
    """
    latest: dict[str, Path] = {}
    for path in interpretations.glob(f"*/{radar_key}/*.json"):
        user = path.parts[-3]
        current = latest.get(user)
        if current is None or path.name > current.name:
            latest[user] = path
    return latest


def read_picks(path: Path, n_samples: int) -> pd.DataFrame:
    """Read one legacy submission into per-trace pick depths in sample space.

    Applies the bottom-up ``y`` flip described in the module docstring and
    quantises to whole samples, which is what the legacy ``uint16`` storage
    did. Vertices are linearly interpolated onto every integer trace the
    feature spans, matching ``read_interpretation_xy``.

    Returns
    -------
    pandas.DataFrame
        Columns ``user``, ``kind``, ``trace``, ``sample``.
    """
    document = json.loads(path.read_text())
    if document.get("height") != n_samples:
        raise ValueError(
            f"{path} declares height {document.get('height')}, expected {n_samples}"
        )
    user = path.parts[-3]

    frames: list[pd.DataFrame] = []
    for feature in document["features"]["features"]:
        coordinates = feature["geometry"]["coordinates"]
        if not coordinates:
            continue
        vertices = np.asarray(coordinates, dtype=float)
        if vertices.ndim != 2 or vertices.shape[1] != 2:
            continue
        vertices = vertices[np.argsort(vertices[:, 0])]

        # Collapse contiguous runs of one rounded trace to their mean y, as
        # the legacy reader did, so a doubled-back vertex cannot create two
        # values at one trace.
        traces = np.rint(vertices[:, 0]).astype(np.int64)
        starts = np.r_[0, np.flatnonzero(traces[1:] != traces[:-1]) + 1]
        ends = np.r_[starts[1:], traces.size]
        unique_traces = traces[starts]
        mean_y = np.add.reduceat(vertices[:, 1], starts) / (ends - starts)
        if unique_traces.size == 0:
            continue

        span = np.arange(unique_traces[0], unique_traces[-1] + 1, dtype=np.int64)
        y_on_span = np.rint(np.interp(span, unique_traces, mean_y))

        properties = feature["properties"]
        kind = properties.get("kind") or NAME_TO_KIND[properties["name"]]
        kind = KIND_ALIASES.get(kind, kind)

        frames.append(
            pd.DataFrame(
                {
                    "user": user,
                    "kind": kind,
                    "trace": span,
                    # The flip. Positive down, 0 at the surface.
                    "sample": n_samples - 1 - y_on_span,
                }
            )
        )

    if not frames:
        return pd.DataFrame(columns=["user", "kind", "trace", "sample"])
    return pd.concat(frames, ignore_index=True)


def consensus(picks: pd.DataFrame, depth_axis: np.ndarray) -> pd.DataFrame:
    """Reproduce the published consensus columns, indexed by trace.

    Parameters
    ----------
    picks
        Output of :func:`read_picks`, concatenated over contributors.
    depth_axis
        Depth in metres per sample index, from the processed radargram.

    Returns
    -------
    pandas.DataFrame
        Indexed by ``trace``, with the ``thickness*`` and ``cts_depth``
        columns that PLAN.md §2.6 compares against.
    """
    picks = picks.copy()
    within = (picks["sample"] >= 0) & (picks["sample"] < depth_axis.size)
    if not within.all():
        # Out-of-grid picks are *dropped*, not clamped, because that is what
        # the legacy pipeline did (`bounds_error=False` yielded NaN, which a
        # later `dropna` removed). Clamping would turn "this contributor drew
        # above the surface" into a confident vote at the surface -- for the
        # CTS layer that reads as "fully temperate", which is a fabricated
        # opinion rather than a rounding choice.
        offenders = picks.loc[~within].groupby(["user", "kind"]).size()
        print(f"  dropping {int((~within).sum())} out-of-grid pick samples:")
        for (user, kind), count in offenders.items():
            print(f"    {user} / {kind}: {count}")
        picks = picks.loc[within]
    picks["depth"] = depth_axis[picks["sample"].astype(int)]

    # Reducer: shallowest. Stray picks on multiples and ringing lie *below*
    # the true reflector, so the minimum depth is the defensible choice.
    reduced = picks.groupby(["user", "kind", "trace"], as_index=False)["depth"].min()

    bed = reduced[reduced["kind"].isin(BED_KINDS)].groupby("trace")["depth"]
    cts = reduced[reduced["kind"].isin(CTS_KINDS)].groupby("trace")["depth"]

    out = pd.DataFrame(
        {
            "thickness": bed.apply(lambda v: quantile_lower(v.to_numpy(), CONSENSUS_Q)),
            "thickness_user_lower": bed.apply(lambda v: quantile_lower(v.to_numpy(), 0.25)),
            "thickness_user_upper": bed.apply(lambda v: quantile_lower(v.to_numpy(), 0.75)),
            "thickness_user_std": bed.std(),
            "thickness_user_nmad": bed.apply(lambda v: nmad(v.to_numpy())),
            "thickness_user_count": bed.count(),
            "cts_depth": cts.apply(lambda v: quantile_lower(v.to_numpy(), CONSENSUS_Q)),
        }
    )

    # A position is dropped only where *more* contributors say the bed is
    # missing than say it is there. A tie is kept -- see PLAN.md §2.1.
    missing = reduced[reduced["kind"] == "bed_missing"].groupby("trace")["depth"].count()
    if not missing.empty:
        aligned_missing, aligned_existing = missing.align(
            out["thickness_user_count"], join="inner"
        )
        drop = aligned_missing > aligned_existing
        out.loc[drop[drop].index, "thickness"] = np.nan

    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--radar-key", required=True)
    parser.add_argument("--interpretations", type=Path, required=True)
    parser.add_argument("--processed", type=Path, required=True)
    parser.add_argument("--consensus", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    glacier, date, stem = args.radar_key.split("-", 2)
    netcdf_path = args.processed / glacier / date / f"{stem}.nc"

    # Imported here so the rest of the script is usable without xarray.
    import xarray as xr

    with xr.open_dataset(netcdf_path) as dataset:
        depth_axis = dataset["depth"].values.astype(float)
        n_samples = int(dataset.sizes["y"])

    submissions = latest_submission_per_user(args.interpretations, args.radar_key)
    if not submissions:
        raise SystemExit(f"no submissions found for {args.radar_key}")
    print(f"{args.radar_key}: {len(submissions)} contributors, {n_samples} samples")

    picks = pd.concat(
        [read_picks(path, n_samples) for path in sorted(submissions.values())],
        ignore_index=True,
    )
    reproduced = consensus(picks, depth_axis)

    published = gpd.read_feather(args.consensus)
    published = published[published["radar_key"] == args.radar_key].set_index("x")

    joined = reproduced.join(published[["thickness"]], how="inner", rsuffix="_published")
    error = (joined["thickness"] - joined["thickness_published"]).abs()
    print(
        f"  joined {len(joined)} of {len(published)} published rows; "
        f"thickness median|err|={error.median():.4f} m max|err|={error.max():.4f} m"
    )

    # The fixture carries the **published** values. They are the thing Ridal
    # must reproduce; this script's own reproduction above exists only to
    # prove the algorithm is understood, and is deliberately not what CI
    # compares against -- an oracle that shares an implementation with the
    # code under test proves nothing.
    expected_columns: Final[tuple[str, ...]] = (
        "thickness",
        "thickness_user_lower",
        "thickness_user_upper",
        "thickness_user_std",
        "thickness_user_nmad",
        "thickness_user_count",
        "temperate",
        "temperate_user_lower",
        "temperate_user_upper",
        "temperate_user_std",
        "temperate_user_nmad",
        "temperate_user_count",
    )
    fixture = published.loc[:, list(expected_columns)].sort_index()

    args.out.parent.mkdir(parents=True, exist_ok=True)
    fixture.round(6).to_csv(args.out, index_label="trace")
    print(f"  wrote {len(fixture)} rows to {args.out}")


if __name__ == "__main__":
    main()
