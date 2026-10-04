"""Real servers for the client's integration tests.

They need the ``ridal`` command with the server built in (``RIDAL_BIN``, or
``ridal`` on ``PATH``), and PROJ to process the fixture radargram; without
either, the tests that use them are skipped.
"""

import os
import re
import shutil
import socket
import subprocess
import time
from collections.abc import Iterator
from pathlib import Path
from typing import Final

import pytest
from servers import Server, Site

FIXTURE: Final = (
    Path(__file__).parents[2] / "assets" / "mala" / "dronbreen-20250327-DAT_0066_A1.rd3"
)
SERVING: Final = re.compile(r"Serving on (http://[^/\s]+)")


def ridal_binary() -> str:
    binary = os.environ.get("RIDAL_BIN") or shutil.which("ridal")
    if not binary:
        pytest.skip("needs the ridal command (set RIDAL_BIN)")
    return binary


@pytest.fixture(scope="session")
def radargram(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """A processed radargram, made once for the session."""
    if not (shutil.which("cs2cs") and shutil.which("projinfo")) or not FIXTURE.exists():
        pytest.skip("processing the fixture needs PROJ, and the fixture")
    import ridal

    output = tmp_path_factory.mktemp("processed") / "line.nc"
    ridal.process(str(FIXTURE), str(output), steps=["zero_corr"], quiet=True)
    return output


def start(argv: list[str]) -> tuple[subprocess.Popen[str], str]:
    """Start a server and return it with the URL it printed."""
    process = subprocess.Popen(
        argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )
    assert process.stdout is not None
    deadline = time.monotonic() + 60
    seen = []
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if not line:
            break
        seen.append(line)
        if match := SERVING.search(line):
            return process, match.group(1)
    process.kill()
    raise RuntimeError("the server did not start:\n" + "".join(seen))


def stop(process: subprocess.Popen[str]) -> None:
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()


@pytest.fixture(scope="session")
def gui(tmp_path_factory: pytest.TempPathFactory, radargram: Path) -> Iterator[Server]:
    """``ridal gui`` over a project holding the radargram."""
    binary = ridal_binary()
    root = tmp_path_factory.mktemp("project")
    subprocess.run(
        [binary, "project", "init", str(root)], check=True, capture_output=True
    )
    shutil.copy(radargram, root / radargram.name)
    process, url = start([binary, "gui", str(root)])
    try:
        yield Server(url, root)
    finally:
        stop(process)


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


@pytest.fixture(scope="session")
def site(tmp_path_factory: pytest.TempPathFactory, radargram: Path) -> Iterator[Site]:
    """``ridal server start`` over a site with one project and an API token."""
    binary = ridal_binary()
    root = tmp_path_factory.mktemp("site")

    def ridal_cli(*argv: str) -> str:
        done = subprocess.run(
            [binary, *argv], check=True, capture_output=True, text=True
        )
        return done.stdout

    ridal_cli("site", "init", str(root))
    ridal_cli("site", "account", "add", "anna", "--server-admin", "--path", str(root))
    ridal_cli("site", "project", "add", "glac", "--path", str(root))
    shutil.copy(radargram, root / "projects" / "glac" / radargram.name)
    printed = ridal_cli(
        "site",
        "token",
        "add",
        "anna",
        "--name",
        "tests",
        "--grant",
        "glac:viewer",
        "--path",
        str(root),
    )
    token = next(line for line in printed.splitlines() if line.startswith("ridal_"))
    process, url = start(
        [binary, "server", "start", str(root), "--port", str(free_port())]
    )
    try:
        yield Site(url, root, token)
    finally:
        stop(process)
