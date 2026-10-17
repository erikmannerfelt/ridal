"""Install the `ridal` wheel that the documentation describes.

Run by Read the Docs before Sphinx, since the Python reference is read from
an installed `ridal`. Building one from source there would compile HDF5,
which is too slow for a documentation build, so this installs a wheel that
CI already built (#345, #359).

CI uploads a wheel for every commit it builds, pull requests included, to
the `docs-wheels` GitHub release as `<commit>.whl`. Read the Docs starts
building on the push, usually before that wheel exists, so this waits for
it rather than relying on a rebuild being triggered later. If it never
comes:

- a release tag installs that release from PyPI;
- anything else installs the newest wheel from `main` (`main.whl`), with a
  warning, since the Python reference may then not match the commit.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
import zipfile
from pathlib import Path
from typing import Final

REPO_ROOT: Final = Path(__file__).resolve().parent.parent
# Plain download URLs rather than the GitHub API, which allows 60
# unauthenticated requests an hour per IP, shared by every Read the Docs
# build on the same machine.
WHEELS_URL: Final = (
    "https://github.com/erikmannerfelt/ridal/releases/download/docs-wheels"
)
# CI's wheel is ready a few minutes after a push; a Read the Docs build is
# cancelled after 15.
WAIT_SECONDS: Final = 480
POLL_SECONDS: Final = 15
PYPI_ATTEMPTS: Final = 6


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


def commit() -> str:
    """The commit being documented."""
    from_rtd = os.environ.get("READTHEDOCS_GIT_COMMIT_HASH")
    if from_rtd:
        return from_rtd
    return subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def cargo_version() -> str:
    """The version in `Cargo.toml`, which a release tag matches."""
    with (REPO_ROOT / "Cargo.toml").open("rb") as file:
        return str(tomllib.load(file)["package"]["version"])


def download(asset: str, directory: Path) -> Path | None:
    """Download a `docs-wheels` asset under its wheel file name.

    The assets are named by commit, which pip cannot install, so the file is
    renamed after the name and tag recorded inside the wheel.

    Returns
    -------
    The wheel, or None if the release has no such asset.
    """
    raw = directory / asset
    try:
        with urllib.request.urlopen(f"{WHEELS_URL}/{asset}", timeout=60) as response:
            raw.write_bytes(response.read())
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise
    with zipfile.ZipFile(raw) as wheel:
        metadata = next(
            name
            for name in wheel.namelist()
            if name.count("/") == 1 and name.endswith(".dist-info/WHEEL")
        )
        tag = next(
            line.removeprefix("Tag:").strip()
            for line in wheel.read(metadata).decode().splitlines()
            if line.startswith("Tag:")
        )
    name_version = metadata.removesuffix(".dist-info/WHEEL")
    return raw.rename(directory / f"{name_version}-{tag}.whl")


def wait_for(asset: str, directory: Path) -> Path | None:
    """Download `asset`, waiting up to `WAIT_SECONDS` for CI to upload it."""
    deadline = time.monotonic() + WAIT_SECONDS
    while True:
        wheel = download(asset, directory)
        if wheel is not None:
            return wheel
        if time.monotonic() >= deadline:
            return None
        print(f"Waiting for {WHEELS_URL}/{asset}", flush=True)
        time.sleep(POLL_SECONDS)


def pip_install(requirement: str) -> None:
    print(f"Installing {requirement}", flush=True)
    subprocess.run(
        [sys.executable, "-m", "pip", "install", "--no-cache-dir", requirement],
        check=True,
    )


def install_release(version: str) -> None:
    """Install a release from PyPI, retrying while its index catches up."""
    for attempt in range(1, PYPI_ATTEMPTS + 1):
        try:
            pip_install(f"ridal=={version}")
            return
        except subprocess.CalledProcessError:
            if attempt == PYPI_ATTEMPTS:
                raise
            time.sleep(30)


def main() -> None:
    sha = commit()
    with tempfile.TemporaryDirectory() as tmp:
        directory = Path(tmp)
        wheel = wait_for(f"{sha}.whl", directory)
        if wheel is not None:
            pip_install(str(wheel))
            return
        if is_release_build():
            print(f"No wheel for {sha}; installing the release from PyPI.", flush=True)
            install_release(cargo_version())
            return
        print(
            f"WARNING: CI uploaded no wheel for {sha}; the Python reference will "
            "describe main instead.",
            flush=True,
        )
        wheel = download("main.whl", directory)
        if wheel is None:
            sys.exit("No wheel from main in the docs-wheels release either.")
        pip_install(str(wheel))


if __name__ == "__main__":
    main()
