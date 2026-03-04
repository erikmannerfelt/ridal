import numpy as np
import xarray as xr
import matplotlib.pyplot as plt
import matplotlib.patheffects
import os
from pathlib import Path

def normalize(data: np.ndarray, mask, contrast: float = 1.):
    """Normalize the data and convert to an unsigned 8 bit integer array."""
    data_abs = np.abs(data)
    minval_abs, maxval_abs = np.percentile(data_abs[mask], [5, 99])
    data_abs = np.clip(contrast * (data_abs - minval_abs) / (maxval_abs - minval_abs + 1e-12), 0, 1)

    return (data_abs * 255).astype("uint8")

def process_kroppbreen():
    processed_path = Path("./kropp.nc").absolute()
    if processed_path.is_file():
        return processed_path

    # For development
    # import sys
    # sys.path.insert(1, "venv/lib/python3.13/site-packages/")

    import ridal
    filepath = Path(os.environ.get("DATA_DIR", "")) / "DAT_0042_A1.rad"
    dem_filepath = Path(os.environ.get("DEM_DIR", "")) / "edvard_mette_ragna_kropp_dem_2024.tif"

    ridal.run_cli(
        filepath=str(filepath),
        dem=str(dem_filepath),
        output=str(processed_path),
        steps=[
            "remove_empty_traces",
            "equidistant_traces",
            "zero_corr",
            "correct_antenna_separation",
            "bandpass",
            "dewow(15)",
            "gain(0.003556)",
            "siglog(1)",
            "correct_topography",
        ],
    )
    return processed_path

def apply_stroke_to_axis(ax, stroke):
    # Labels
    for label in ax.get_xticklabels() + ax.get_yticklabels():
        label.set_path_effects(stroke)
    ax.xaxis.label.set_path_effects(stroke)
    ax.yaxis.label.set_path_effects(stroke)
    ax.title.set_path_effects(stroke)

    # Tick lines (major + minor)
    for tl in ax.xaxis.get_ticklines(minor=False) + ax.yaxis.get_ticklines(minor=False):
        tl.set_path_effects(stroke)
    for tl in ax.xaxis.get_ticklines(minor=True) + ax.yaxis.get_ticklines(minor=True):
        tl.set_path_effects(stroke)

def main():

    processed_path = process_kroppbreen()
    with xr.open_dataset(processed_path) as data:
        # Make a mask to exclude all areas above and below the radargram
        data["elev"] = ("y2",), np.linspace(data["elevation"].min().item() - data["depth"].max().item(), data["elevation"].max().item(), data["data_topographically_corrected"].shape[0])[::-1]
        data["elev"] = data["elev"].broadcast_like(data["data_topographically_corrected"])
        mask = (data["elev"] >= data["elevation"]) | ((data["elev"] + data["depth"].max().item()) <= data["elevation"])

        arr = normalize(data.data_topographically_corrected.values, ~mask)

        extent=(0, data["distance"].max().item() / 1e3, data["elevation"].min().item() - data["depth"].max().item(), data["elevation"].max().item())

    stroke = [matplotlib.patheffects.withStroke(foreground="white", linewidth=1.)]
    fig = plt.figure(figsize=(8, 4))
    fig.patch.set_alpha(0.)
    plt.imshow(np.ma.masked_array(arr, mask=mask), extent=extent, aspect="auto", cmap="Greys", interpolation="lanczos", vmin=0, vmax=255)
    plt.gca().patch.set_alpha(0.)
    plt.ylim(25, 600)
    plt.xlabel("Distance (km)")
    plt.ylabel("Elevation (m a.s.l.)")
    apply_stroke_to_axis(plt.gca(), stroke)
    plt.tight_layout()
    plt.savefig("images/kroppbreen_rgm.webp", dpi=300)
    plt.close()


if __name__ == "__main__":
    main()
