"""Download the data for render_kroppbreen.py from Zenodo.

The files are in two large zip archives in the dataset of Mannerfelt et al.,
https://doi.org/10.5281/zenodo.17882299. The archives are not compressed, so
instead of downloading them whole (580 MB), this asks the server for only the
bytes of the four files that are needed (85 MB).
"""

import sys
import urllib.request
import zipfile
from pathlib import Path

# The version of the dataset that the figure was made from.
RECORD = "https://zenodo.org/records/23101577/files"
RADARGRAM = "kroppbreen-20230228-DAT_0042_A1_1"


class RemoteFile:
    """A file on a web server, which zipfile can read like one on disk."""

    def __init__(self, url):
        self.url = url
        self.position = 0
        with self._get(0, 0) as response:
            self.size = int(response.headers["Content-Range"].split("/")[1])

    def _get(self, first, last):
        request = urllib.request.Request(
            self.url, headers={"Range": f"bytes={first}-{last}"}
        )
        response = urllib.request.urlopen(request)
        if response.status != 206:
            raise OSError(f"{self.url} cannot be downloaded in parts")
        return response

    def seekable(self):
        return True

    def tell(self):
        return self.position

    def seek(self, offset, whence=0):
        self.position = [0, self.position, self.size][whence] + offset
        return self.position

    def read(self, size=-1):
        end = self.size if size < 0 else min(self.position + size, self.size)
        if end <= self.position:
            return b""
        with self._get(self.position, end - 1) as response:
            data = response.read()
        self.position += len(data)
        return data


def extract(archive, name, directory):
    """Copy one file out of an archive, in large pieces to make few requests."""
    path = directory / name
    partial = directory / f"{name}.part"
    with archive.open(name) as source, open(partial, "wb") as target:
        while piece := source.read(8_000_000):
            target.write(piece)
            print(f"\r{name}: {target.tell() / 1e6:.0f} MB", end="", file=sys.stderr)
    print(file=sys.stderr)
    # Only a complete file gets its real name.
    partial.replace(path)


def download(directory: Path):
    directory.mkdir(parents=True, exist_ok=True)

    # The raw files are in a zip archive inside the Kroppbreen archive.
    with (
        zipfile.ZipFile(
            RemoteFile(f"{RECORD}/radargrams-kroppbreen.zip")
        ) as radargrams,
        radargrams.open(f"{RADARGRAM}/{RADARGRAM}_raw_data.zip") as raw_data_file,
        zipfile.ZipFile(raw_data_file) as raw_data,
    ):
        for name in ["DAT_0042_A1.rad", "DAT_0042_A1.rd3", "DAT_0042_A1.cor"]:
            extract(raw_data, name, directory)

    with zipfile.ZipFile(RemoteFile(f"{RECORD}/dems.zip")) as dems:
        extract(dems, "edvard_mette_ragna_kropp_dem_2024.tif", directory)
