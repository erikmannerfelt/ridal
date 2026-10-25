"""Process and render the Kroppbreen radargram shown in the README.

Run it from the repository root: python scripts/render_kroppbreen.py
It needs Pillow (pip install pillow) to save the figure as WebP.
The first run downloads the data (85 MB) from Zenodo into tmp/kroppbreen_data/.
"""

from pathlib import Path

import ridal
from PIL import Image

data = Path("tmp/kroppbreen_data")
radar_file = data / "DAT_0042_A1.rad"  # with .rd3 and .cor files beside it
dem_file = data / "edvard_mette_ragna_kropp_dem_2024.tif"

needed = [
    radar_file,
    radar_file.with_suffix(".rd3"),
    radar_file.with_suffix(".cor"),
    dem_file,
]
if not all(path.exists() for path in needed):
    import kroppbreen_data

    kroppbreen_data.download(data)

ridal.process(
    radar_file,
    data / "kroppbreen.nc",
    dem=dem_file,
    steps=[
        "remove_empty_traces",
        # The radar stopped now and then, and the GPS shows where.
        "remove_standstills(gps_uncertainty=1)",
        "zero_corr",
        "correct_antenna_separation",
        "dewow",
        "background_removal",
    ],
)

ridal.render(
    data / "kroppbreen.nc",
    data / "kroppbreen.png",
    figure={
        "view": "topo",
        "profile": "siglog-seismic",
        "extent": {"elevation": [600, 25]},
        "size": {"width": 20, "unit": "cm", "dpi": 200},
        "x_axis": {"unit": "distance_km", "label": "Distance (km)"},
        "y_axis": {"unit": "elevation", "label": "Elevation (m a.s.l.)"},
        # No background, and white edges around the text, to suit both
        # light and dark pages.
        "style": {"background": None, "halo": {}},
    },
)

# Ridal writes PNG or SVG; WebP is much smaller for the web.
Image.open(data / "kroppbreen.png").save("docs/_static/kroppbreen_rgm.webp", quality=85)
