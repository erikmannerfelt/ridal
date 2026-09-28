"""Install the `ridal` wheel that the documentation describes.

Run by Read the Docs before Sphinx, since the Python reference is read from
an installed `ridal`. Building one from source there would compile HDF5,
which is too slow for a documentation build, so this installs a wheel that
was already built:

- a release tag installs that release from PyPI;
- anything else installs the newest wheel built from `main`, which CI keeps
  in the `docs-nightly` GitHub release;
- if there is no such wheel yet, the newest release on PyPI, with a warning.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Final, TypedDict

import tomllib

REPO_ROOT: Final = Path(__file__).resolve().parent.parent
NIGHTLY_RELEASE: Final = (
    "https://api.github.com/repos/erikmannerfelt/ridal/releases/tags/docs-nightly"
)
# Read the Docs builds with this Python; see `.readthedocs.yaml`.
WHEEL_TAG: Final = f"cp{sys.version_info.major}{sys.version_info.minor}"


class Asset(TypedDict):
    name: str
    browser_download_url: str
    updated_at: str


def is_release_build() -> bool:
    """Whether this build is of a `v*` tag, including Read the Docs' `stable`."""
    if os.environ.get("READTHEDOCS_VERSION_TYPE") == "tag":
        return True
    described = subprocess.run(
        ["git", "describe", "--tags", "--exact-match", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    return described.returncode == 0 and described.stdout.startswith("v")


def cargo_version() -> str:
    """The version in `Cargo.toml`, which a release tag matches."""
    with (REPO_ROOT / "Cargo.toml").open("rb") as file:
        return str(tomllib.load(file)["package"]["version"])


def nightly_wheel_url() -> str | None:
    """The newest wheel from `main` for this Python, or None if there is none."""
    try:
        with urllib.request.urlopen(NIGHTLY_RELEASE, timeout=30) as response:
            assets: list[Asset] = json.load(response)["assets"]
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise
    wheels = [
        asset
        for asset in assets
        if asset["name"].endswith(".whl")
        and f"-{WHEEL_TAG}-" in asset["name"]
        and "manylinux" in asset["name"]
    ]
    if not wheels:
        return None
    newest = max(wheels, key=lambda asset: asset["updated_at"])
    return newest["browser_download_url"]


def pip_install(requirement: str) -> None:
    print(f"Installing {requirement}", flush=True)
    subprocess.run([sys.executable, "-m", "pip", "install", requirement], check=True)


def main() -> None:
    if is_release_build():
        pip_install(f"ridal=={cargo_version()}")
        return
    url = nightly_wheel_url()
    if url is not None:
        pip_install(url)
        return
    print(
        "WARNING: no wheel from main in the docs-nightly release; the Python "
        "reference will describe the newest release on PyPI instead.",
        flush=True,
    )
    pip_install("ridal")


if __name__ == "__main__":
    main()
