#!/usr/bin/env python3
"""Convert the Mannerfelt et al. (2026) legacy pick JSONs to gprinterp.

The legacy format is a one-off: a GeoJSON FeatureCollection in a *bottom-up*
``(x, y)`` canvas space, with the layer in ``properties.kind`` (or
``properties.name`` for early submissions where ``kind`` is absent). It is not
part of the shipping binary, so the converter lives here rather than in
``ridal``; the committed fixtures it produces are what the regression test in
``src/interp/derive_regression_tests.rs`` consumes.

Two details are easy to get wrong and are the reason this script exists:

* The legacy pick ``y`` axis is bottom-up. The processed NetCDF and the
  published thumbnail are top-down, so the converter flips it:

      sample = n_samples - 1 - y_legacy

  Ridal stores ``sample`` 0-at-surface, increasing downward. The published
  data README's "x and y from the upper left corner" is wrong; see PLAN.md
  §2.2.

* The user and radar key come from the **directory structure**
  (``.../<user>/<radar_key>/<file>.json``), never from the JSON's own fields,
  which are known to be mangled for some radargrams.

Out-of-grid vertices are **not** dropped here. The legacy pipeline dropped
out-of-grid *interpolated* samples (after resampling each line onto integer
traces), and doing the same thing at the vertex level changes the interpolated
line: an endpoint at ``x = -2`` still contributes to traces 0..n. The drop
therefore happens in the evaluator (``reduce_picks``), where it is applied to
the same interpolated positions the legacy code dropped. This converter
*reports* out-of-grid vertices so the drop is visible, but preserves the
geometry.

Usage
-----
    python3 misc/convert_legacy_interpretations.py \
        --interpretations /tmp/interp_full \
        --radar-key dronbreen-20250327-DAT_0066_A1_1 \
        --n-samples 700 --n-traces 3548 \
        --out assets/interp/dronbreen-20250327-DAT_0066_A1_1
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Final

#: Bumped when the emitted document shape changes, so a fixture says which
#: converter made it.
CONVERTER_VERSION: Final[str] = "1"

#: Legacy ``kind`` -> ``(new layer id, display name)``. ``temperate_ice`` is
#: normalised in as well: the legacy name-fallback produced that spelling while
#: the ``kind`` field said ``temperate``, and the study's merge step only ever
#: looked for ``temperate`` -- so old-style submissions were silently dropped
#: from the published consensus.
KIND_TO_LAYER: Final[dict[str, tuple[str, str]]] = {
    "bed_unspecified": ("bed", "Glacier bed"),
    "bed_cold": ("bed_no_temperate", "Glacier bed (no temperate ice above)"),
    "temperate": ("temperate_ice", "Temperate ice (CTS)"),
    "temperate_ice": ("temperate_ice", "Temperate ice (CTS)"),
    "bed_missing": ("bed_not_visible", "Glacier bed not visible"),
}

#: Legacy ``name`` -> the same pairs, for pre-May submissions where ``kind`` is
#: absent. The README warns both fields are unreliable for early data.
NAME_TO_LAYER: Final[dict[str, tuple[str, str]]] = {
    "Glacier bed": ("bed", "Glacier bed"),
    "Cold glacier bed": ("bed_no_temperate", "Glacier bed (no temperate ice above)"),
    "Temperate ice": ("temperate_ice", "Temperate ice (CTS)"),
    "Glacier bed missing": ("bed_not_visible", "Glacier bed not visible"),
}

#: Axis metadata for the one radargram this converter ships a fixture for.
#: ``twtt`` is regular to float32 precision; t0/dt come from the processed
#: NetCDF's ``twtt`` coordinate.
TWTT_T0_NS: Final[float] = 0.0
TWTT_DT_NS: Final[float] = 2129.150390625 / 699.0


class ConversionError(SystemExit):
    """Raised for anything that must fail the run loudly."""


def latest_submission_per_user(interpretations: Path, radar_key: str) -> dict[str, Path]:
    """Map contributor nickname to their most recent submission.

    Filenames carry an ISO timestamp, so the lexicographically greatest is the
    latest. The user is the directory above the radar key.
    """
    latest: dict[str, Path] = {}
    for path in interpretations.glob(f"*/{radar_key}/*.json"):
        user = path.parts[-3]
        current = latest.get(user)
        if current is None or path.name > current.name:
            latest[user] = path
    return latest


def map_layer(properties: dict, path: Path, feature_index: int) -> tuple[str, str, bool]:
    """Resolve a feature's layer to ``(id, display_name, used_name_fallback)``."""
    kind = properties.get("kind")
    if kind in KIND_TO_LAYER:
        layer_id, name = KIND_TO_LAYER[kind]
        return layer_id, name, False
    name = properties.get("name")
    if name in NAME_TO_LAYER:
        layer_id, display = NAME_TO_LAYER[name]
        return layer_id, display, True
    raise ConversionError(
        f"{path}: feature {feature_index} has unknown kind {kind!r} and name "
        f"{name!r}; refusing to guess a layer"
    )


def convert_feature(
    feature: dict,
    path: Path,
    feature_index: int,
    user: str,
    n_samples: int,
    n_traces: int,
    out_of_grid: dict[tuple[str, str], int],
) -> tuple[dict, bool]:
    """Convert one legacy feature. Returns ``(feature, used_fallback)``."""
    geometry = feature.get("geometry") or {}
    if geometry.get("type") != "LineString":
        raise ConversionError(
            f"{path}: feature {feature_index} is a {geometry.get('type')!r}, not a "
            "LineString; refusing to drop it silently"
        )
    coordinates = geometry.get("coordinates") or []
    if len(coordinates) < 2:
        raise ConversionError(
            f"{path}: feature {feature_index} has {len(coordinates)} vertices; a "
            "LineString needs at least 2"
        )

    properties = dict(feature.get("properties") or {})
    layer_id, display_name, used_fallback = map_layer(properties, path, feature_index)

    flipped = []
    for x, y in coordinates:
        sample = n_samples - 1 - y
        if not (0.0 <= x < n_traces) or not (0.0 <= sample < n_samples):
            out_of_grid[(user, layer_id)] = out_of_grid.get((user, layer_id), 0) + 1
        flipped.append([x, sample])

    converted = {
        "type": "Feature",
        "geometry": {"type": "LineString", "coordinates": flipped},
        "properties": {
            **properties,
            "id": f"{user.lower()}-{feature_index}",
            "label": layer_id,
            "name": display_name,
        },
    }
    return converted, used_fallback


def user_id(contributor: str) -> str:
    """Ridal's user id for a legacy contributor nickname.

    Ridal's `UserId` accepts lowercase ASCII letters, digits, `-` and `_`, and
    **rejects** anything else rather than sanitising it. The study's nicknames
    are capitalised ("AvalancheAmigo"), and the interpretation filename *is*
    the user id, so writing them verbatim produces files the server cannot
    read. Worse, it cannot read them loudly: `get_contributors` turns the
    rejection into a 500 for the whole request, so one capitalised name hides
    every valid one behind it and the viewer shows no contributors at all.

    The nickname is kept in `meta.contributor`, so nothing is lost.

    Examples
    --------
    >>> user_id("AvalancheAmigo")
    'avalancheamigo'
    >>> user_id("Radar Force 2")
    'radar_force_2'
    """
    out = []
    for character in contributor.lower():
        if character.isascii() and (character.isalnum() or character in "-_"):
            out.append(character)
        else:
            out.append("_")
    cleaned = "".join(out).strip("_")
    if not cleaned:
        raise ValueError(f"no usable user id in contributor name {contributor!r}")
    return cleaned


def convert_submission(
    path: Path,
    radar_key: str,
    n_samples: int,
    n_traces: int,
    out_of_grid: dict[tuple[str, str], int],
) -> tuple[dict, int, int]:
    """Convert one submission. Returns ``(document, n_features, n_fallbacks)``."""
    document = json.loads(path.read_text())
    if document.get("height") != n_samples:
        raise ConversionError(
            f"{path}: declares height {document.get('height')}, expected {n_samples}"
        )
    if document.get("width") != n_traces:
        raise ConversionError(
            f"{path}: declares width {document.get('width')}, expected {n_traces}"
        )
    if not isinstance(document.get("features"), dict):
        raise ConversionError(f"{path}: 'features' is not a FeatureCollection")

    user = path.parts[-3]
    converted_features = []
    fallbacks = 0
    for index, feature in enumerate(document["features"]["features"]):
        converted, used_fallback = convert_feature(
            feature, path, index, user, n_samples, n_traces, out_of_grid
        )
        converted_features.append(converted)
        fallbacks += int(used_fallback)

    out = {
        "key": radar_key.lower(),
        "schema": "gprinterp",
        "schema_version": "0.3",
        "date_modified": document.get("date_modified"),
        "source": {
            "id": radar_key.lower(),
            "n_traces": n_traces,
            "n_samples": n_samples,
        },
        "coordinates": {
            "space": "index2d",
            "convention": {
                "origin": "upper-left",
                "indexing": "zero-based",
                "pixel_reference": "center",
                "axis_directions": {"x": "right", "y": "down"},
            },
            "axes": {
                "x": {"primary": {"name": "trace", "unit": "index"}},
                "y": {
                    "primary": {"name": "sample", "unit": "index"},
                    "anchor": [
                        {
                            "name": "twtt",
                            "unit": "ns",
                            "type": "regular",
                            "t0": TWTT_T0_NS,
                            "dt": TWTT_DT_NS,
                        }
                    ],
                },
            },
        },
        "features": converted_features,
        "meta": {
            "legacy_source": str(path),
            "legacy_date_modified": document.get("date_modified"),
            "contributor": user,
            "difficulty": document.get("difficulty"),
            "comment": document.get("comment"),
            "converter_version": CONVERTER_VERSION,
        },
    }
    return out, len(converted_features), fallbacks


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interpretations", type=Path, required=True)
    parser.add_argument("--radar-key", required=True)
    parser.add_argument("--n-samples", type=int, required=True)
    parser.add_argument("--n-traces", type=int, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    submissions = latest_submission_per_user(args.interpretations, args.radar_key)
    if not submissions:
        raise ConversionError(f"no submissions found for {args.radar_key}")

    args.out.mkdir(parents=True, exist_ok=True)
    out_of_grid: dict[tuple[str, str], int] = {}
    total_features = 0
    total_fallbacks = 0

    for user, path in sorted(submissions.items()):
        document, n_features, fallbacks = convert_submission(
            path, args.radar_key, args.n_samples, args.n_traces, out_of_grid
        )
        destination = args.out / f"{user_id(user)}.gprinterp.json"
        destination.write_text(json.dumps(document, indent=2) + "\n")
        total_features += n_features
        total_fallbacks += fallbacks

    print(
        f"{args.radar_key}: {len(submissions)} contributors, "
        f"{total_features} features, {total_fallbacks} name-fallback features",
        file=sys.stderr,
    )
    if total_fallbacks:
        print(
            "  NOTE: some features had no 'kind' and took the 'name' fallback; "
            "see the report above. Old-style 'Temperate ice' is normalised to "
            "temperate_ice.",
            file=sys.stderr,
        )
    if out_of_grid:
        total = sum(out_of_grid.values())
        print(
            f"  {total} out-of-grid vertices kept (dropped at interpolation time, "
            "not here):",
            file=sys.stderr,
        )
        for (user, layer_id), count in sorted(out_of_grid.items()):
            print(f"    {user} / {layer_id}: {count}", file=sys.stderr)


if __name__ == "__main__":
    main()
