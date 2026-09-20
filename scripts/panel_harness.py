#!/usr/bin/env python3
"""Chromium harness for the layer panel, range fills and expression editor (#209).

Builds a throwaway project, starts `ridal server`, and drives the viewer page
from a same-origin iframe harness through a small forwarding proxy. The proxy
serves `/harness.html` itself and forwards everything else to the real server,
which is also how request counts are observed: `performance.getEntriesByType`
caps at 250 entries and silently under-reports (see AGENTS.md).

Run it with a built binary:

    cargo build --no-default-features -F cli,server
    python3 scripts/panel_harness.py

The harness writes a JSON blob into `<pre id="result">` in the iframe's parent
page, which `--dump-dom` then exposes. Every assertion is evaluated here rather
than in the browser so a failure prints what was actually seen.
"""

from __future__ import annotations

import argparse
import http.cookiejar
import http.server
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Final

import netCDF4
import numpy as np

ROOT: Final[Path] = Path(__file__).resolve().parent.parent
RADARGRAM: Final[str] = "line-01"
PASSWORD: Final[str] = "harness-passphrase"

# The pick layers and derived items the harness asserts on. Kept here rather
# than as committed fixtures because they are test scaffolding, not a product.
LAYERS: Final[list[dict]] = [
    {"id": "bed", "name": "Glacier bed", "color": "#ce00ff"},
    {
        "id": "bed_no_temperate",
        "name": "Glacier bed (no temperate ice above)",
        "color": "#002ebd",
    },
    {"id": "temperate_ice", "name": "Temperate ice (CTS)", "color": "#e6194b"},
    {"id": "upper_bound", "name": "Upper bound", "color": "#ff8800"},
    {"id": "lower_bound", "name": "Lower bound", "color": "#0088ff"},
]

DERIVED: Final[list[dict]] = [
    {
        "id": "band_top",
        "name": "Band top",
        "expression": "percentile_lower(concatenate(bed, bed_no_temperate), 75.0)",
        "unit": "meters",
        "color": "#ff8800",
    },
    {
        "id": "band_bottom",
        "name": "Band bottom",
        "expression": "percentile_lower(concatenate(bed, bed_no_temperate), 25.0)",
        "unit": "meters",
        "color": "#0088ff",
        "fill_to": {"target": "band_top", "opacity": 0.3},
    },
    {
        "id": "crossing_top",
        "name": "Crossing top",
        "expression": "median(upper_bound)",
        "unit": "meters",
        "color": "#00aa00",
    },
    {
        "id": "crossing_bottom",
        "name": "Crossing bottom",
        "expression": "median(lower_bound)",
        "unit": "meters",
        "color": "#aa00aa",
        "fill_to": {"target": "crossing_top", "opacity": 0.3},
    },
    {
        "id": "band_gap_fill",
        "name": "Fill against a hidden bound",
        "expression": "percentile_lower(concatenate(bed, bed_no_temperate), 50.0)",
        "unit": "meters",
        "color": "#123456",
        "fill_to": {"target": "op_secret", "opacity": 0.3},
    },
    {
        "id": "op_secret",
        "name": "Operator's private line",
        "expression": "median(bed)",
        "unit": "meters",
        "color": "#444444",
        "scope": {"private": {"user": "op"}},
    },
]


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def write_radargram(path: Path) -> None:
    """A small processed radargram Ridal recognises."""
    n_samples, n_traces = 200, 100
    with netCDF4.Dataset(path, "w") as dataset:
        dataset.createDimension("y", n_samples)
        dataset.createDimension("x", n_traces)
        data = dataset.createVariable("data", "f4", ("y", "x"))
        data[:] = (np.arange(n_samples * n_traces, dtype="f4") % 97).reshape(
            n_samples, n_traces
        )
        twtt = dataset.createVariable("twtt", "f4", ("y",))
        twtt[:] = np.arange(n_samples, dtype="f4") * 0.4
        twtt.anchor_name = "twtt_normal_incidence"
        depth = dataset.createVariable("depth", "f4", ("y",))
        depth[:] = np.arange(n_samples, dtype="f4") * 0.02
        for name in ["distance", "easting", "northing", "longitude", "latitude"]:
            dataset.createVariable(name, "f8", ("x",))
        dataset.variables["distance"][:] = np.arange(n_traces, dtype="f8")
        dataset.variables["easting"][:] = 400_000.0 + np.arange(n_traces, dtype="f8")
        dataset.variables["northing"][:] = 8_700_000.0
        dataset.variables["longitude"][:] = 15.0 + np.arange(n_traces, dtype="f8") * 1e-5
        dataset.variables["latitude"][:] = 78.0
        dataset.ridal_radargram_id = RADARGRAM
        dataset.ridal_version = "ridal version 0.0.0 by harness"
        dataset.ridal_processing_datetime = "2020-01-01T00:00:00Z"
        dataset.crs = "EPSG:32633"


def pick_document(features: list[list[list[float]]]) -> dict:
    return {
        "key": RADARGRAM,
        "features": [
            {
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": coordinates},
                "properties": {"id": f"f-{index}", "label": label},
            }
            for index, (label, coordinates) in enumerate(features)
        ],
    }


def build_project(root: Path, binary: Path) -> None:
    data = root / "ridal_data"
    (data / "radargrams").mkdir(parents=True)
    write_radargram(data / "radargrams" / "line-01.nc")
    subprocess.run(
        [str(binary), "project", "init", str(root)],
        check=True,
        capture_output=True,
        text=True,
    )
    # The first account must be an admin; the operator and picker follow.
    for name, role in [("admin", "admin"), ("op", "operator"), ("picker", "picker")]:
        result = subprocess.run(
            [str(binary), "project", "user", "add", name, "--role", role, "--path", str(root)],
            check=True,
            capture_output=True,
            text=True,
        )
        # `print_invite` prints the path `/invite/<token>`; the token is the
        # last segment, and parsing it here is what lets the harness redeem it.
        token = None
        for line in result.stdout.splitlines():
            stripped = line.strip()
            if stripped.startswith("/invite/"):
                token = stripped.rsplit("/", 1)[1]
        if token is None:
            raise SystemExit(f"could not find an invite token in:\n{result.stdout}")
        (root / f"{name}.token").write_text(token)

    (data / "layers" / "layers.json").write_text(
        json.dumps(
            {
                "schema": "ridal-layers",
                "schema_version": "1",
                "layers": LAYERS,
                "groups": [
                    {
                        "id": "bed_and_cold_bed",
                        "name": "Bed and cold bed",
                        "members": ["bed", "bed_no_temperate"],
                    }
                ],
            },
            indent=2,
        )
    )
    (data / "derived" / "derived.json").write_text(
        json.dumps(
            {"schema": "ridal-derived", "schema_version": "1", "items": DERIVED},
            indent=2,
        )
    )

    picks = {
        "op": [
            ("bed", [[0.0, 20.0], [40.0, 20.0]]),
            ("bed", [[60.0, 20.0], [99.0, 20.0]]),
            ("bed_no_temperate", [[60.0, 25.0], [99.0, 25.0]]),
            ("upper_bound", [[0.0, 10.0], [99.0, 40.0]]),
            ("lower_bound", [[0.0, 40.0], [99.0, 10.0]]),
        ],
        "picker": [("bed", [[0.0, 30.0], [40.0, 30.0]])],
    }
    for user, features in picks.items():
        directory = data / "interpretations" / RADARGRAM
        directory.mkdir(parents=True, exist_ok=True)
        (directory / f"{user}.gprinterp.json").write_text(
            json.dumps(pick_document(features), indent=2)
        )


class Proxy(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, upstream: int, harness: str):
        super().__init__(address, ProxyHandler)
        self.upstream = upstream
        self.harness = harness
        self.requests: list[str] = []


class ProxyHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # silence the default stderr line
        pass

    def _upstream(self, method: str) -> None:
        length = int(self.headers.get("Content-Length", 0) or 0)
        body = self.rfile.read(length) if length else None
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.server.upstream}{self.path}",
            data=body,
            method=method,
        )
        for name, value in self.headers.items():
            if name.lower() in ("host", "content-length", "connection"):
                continue
            request.add_header(name, value)
        try:
            with urllib.request.urlopen(request) as response:
                payload = response.read()
                status = response.status
                headers = response.headers
        except urllib.error.HTTPError as error:
            payload = error.read()
            status = error.code
            headers = error.headers
        self.send_response(status)
        for name, value in headers.items():
            if name.lower() in ("content-length", "transfer-encoding", "connection"):
                continue
            # The app ships no CSP; the harness injects a same-origin one so
            # "no request leaves the origin" is a real constraint rather than
            # an observation that there happened to be none.
            if name.lower() == "content-type" and "text/html" in value:
                self.send_header(
                    "Content-Security-Policy",
                    "default-src 'self' data: blob: 'unsafe-inline'",
                )
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self.server.requests.append(self.path)
        if self.path.startswith("/harness.html"):
            payload = self.server.harness.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        self._upstream("GET")

    def do_POST(self):
        self.server.requests.append(self.path)
        self._upstream("POST")

    def do_PUT(self):
        self.server.requests.append(self.path)
        self._upstream("PUT")

    def do_DELETE(self):
        self.server.requests.append(self.path)
        self._upstream("DELETE")


HARNESS_TEMPLATE: Final[str] = """<!doctype html>
<html>
<head><meta charset="utf-8"><title>panel harness</title></head>
<body>
<pre id="result">running</pre>
<script>
const WHO = new URLSearchParams(location.search).get("who");
const PASSWORD = %(password)s;
const FRAME_ID = "viewer-frame";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function finish(value) {
  document.getElementById("result").textContent = JSON.stringify(value);
}

function panelRows(doc) {
  return Array.from(doc.querySelectorAll("#layer-panel .layer-panel-row")).map(
    (row) => ({ text: row.textContent.trim(), checked: row.querySelector("input").checked }),
  );
}

function lineCount(doc) {
  return doc.querySelectorAll(".leaflet-radargram-lines-pane path").length;
}

function fillCount(doc) {
  return doc.querySelectorAll(".leaflet-radargram-fills-pane path").length;
}

function fillColorCount(doc, color) {
  return doc.querySelectorAll(
    `.leaflet-radargram-fills-pane path[fill="${color}"]`,
  ).length;
}

function findRow(doc, text) {
  return Array.from(doc.querySelectorAll("#layer-panel .layer-panel-row")).find(
    (row) => row.textContent.includes(text),
  );
}

/* Fixed sleeps only. Under Chromium's virtual time a poll loop keeps a timer
 * pending forever and hangs the run (AGENTS.md). Panel interactions and the
 * editor's fetch are run in separate chromium invocations: combining them
 * deadlocks virtual time (a fetch issued while earlier fetches are pending
 * never resolves), and a mode is simpler than working around that. */
async function main() {
  const MODE = new URLSearchParams(location.search).get("mode") || "panel";
  const login = await fetch("/api/v1/auth/login", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: WHO, password: PASSWORD }),
  });
  if (!login.ok) throw new Error("login failed: " + login.status);

  const frame = document.createElement("iframe");
  frame.id = FRAME_ID;
  frame.width = "1200";
  frame.height = "800";
  frame.src = "/view/%(radargram)s";
  document.body.appendChild(frame);

  await sleep(3500);
  const doc = frame.contentDocument;
  if (!doc || !doc.querySelector("#layer-panel")) {
    finish({ who: WHO, mode: MODE, error: "no layer panel" });
    return;
  }

  const result = {
    who: WHO,
    mode: MODE,
    rows: panelRows(doc),
    canEdit: Boolean(doc.querySelector("#derived-new")),
    errors: frame.contentWindow ? frame.contentWindow.RIDAL_PANEL_ERRORS : null,
    me: await fetch("/api/v1/auth/me").then((r) => r.json()).catch(() => null),
  };

  if (MODE === "panel") {
    // Q1 #2: toggling a layer off removes its polylines.
    const bedRow = findRow(doc, "Glacier bed");
    const before = lineCount(doc);
    bedRow.querySelector("input").click();
    await sleep(300);
    const afterOff = lineCount(doc);
    bedRow.querySelector("input").click();
    await sleep(300);
    result.layerToggle = { before, afterOff, afterOn: lineCount(doc) };

    // Q1 #3: derived items present, unchecked, private one only for op.
    result.derived = Array.from(doc.querySelectorAll("#layer-panel .layer-panel-row"))
      .filter((row) => /Band|Crossing|private/.test(row.textContent))
      .map((row) => ({ text: row.textContent.trim(), checked: row.querySelector("input").checked }));

    // Q2: a fill between the two bounds, broken at the NaN gap.
    const top = findRow(doc, "Band top");
    const bottom = findRow(doc, "Band bottom");
    top.querySelector("input").click();
    bottom.querySelector("input").click();
    await sleep(1200);
    result.bandFill = fillCount(doc);
    result.bandRings = fillColorCount(doc, "#0088ff");
    // A fill whose other bound is not visible to the caller must not draw.
    result.invisibleFill = fillColorCount(doc, "#123456");

    // Q2: two crossing lines must not make a self-intersecting polygon.
    const crossTop = findRow(doc, "Crossing top");
    const crossBottom = findRow(doc, "Crossing bottom");
    crossTop.querySelector("input").click();
    crossBottom.querySelector("input").click();
    await sleep(1200);
    result.crossingFill = fillCount(doc);
    // One crossing means two rings; a bowtie would be one.
    result.crossingRings = fillColorCount(doc, "#aa00aa");

    // Q2 #4: the fill survives with both bound lines off.
    top.querySelector("input").click();
    bottom.querySelector("input").click();
    await sleep(500);
    result.fillWithLinesOff = fillCount(doc);
  }

  if (MODE === "editor" && result.canEdit) {
    doc.querySelector("#derived-new").click();
    const expression = doc.querySelector("#derived-expression");
    const status = doc.querySelector("#derived-status");
    expression.value = "median(bed)";
    expression.dispatchEvent(new Event("input", { bubbles: true }));
    await sleep(1500);
    result.previewStatus = status.textContent;
    result.previewLines = lineCount(doc);
    expression.value = "median(no_such_layer)";
    expression.dispatchEvent(new Event("input", { bubbles: true }));
    await sleep(1500);
    result.invalidStatus = status.textContent;
    result.previewAfterInvalid = lineCount(doc);
    result.suggestions = Array.from(doc.querySelectorAll("#derived-suggestions option")).map(
      (option) => option.value,
    );
  }

  finish(result);
}

main().catch((error) => finish({ who: WHO, error: String(error) }));
</script>
</body>
</html>
"""


def api(port: int, method: str, path: str, body=None, cookie=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}", data=data, method=method
    )
    if data:
        request.add_header("Content-Type", "application/json")
    if cookie:
        request.add_header("Cookie", cookie)
    try:
        with urllib.request.urlopen(request) as response:
            return response.status, response.read(), response.headers
    except urllib.error.HTTPError as error:
        return error.code, error.read(), error.headers


def redeem(port: int, name: str, token: str) -> None:
    status, body, _ = api(
        port,
        "POST",
        "/api/v1/auth/invite",
        {"token": token, "password": PASSWORD},
    )
    if status != 200:
        raise SystemExit(f"redeeming {name}'s invite failed ({status}): {body!r}")


def chromium(proxy_port: int, who: str, mode: str = "panel") -> str:
    profile = tempfile.mkdtemp(prefix="ridal-chromium-")
    command = [
        "chromium",
        "--headless=old",
        "--no-sandbox",
        "--disable-gpu",
        # Deliberately no --window-size: a large viewport loads enough
        # radargram chunks that virtual time deadlocks on the editor's preview
        # fetch. The panel tests do not need a specific size.
        f"--user-data-dir={profile}",
        "--virtual-time-budget=15000",
        "--dump-dom",
        f"http://127.0.0.1:{proxy_port}/harness.html?who={who}&mode={mode}",
    ]
    try:
        return subprocess.run(
            command, check=True, capture_output=True, text=True, timeout=30
        ).stdout
    finally:
        shutil.rmtree(profile, ignore_errors=True)


def result_from_dom(dom: str) -> dict:
    start = dom.find('<pre id="result">')
    end = dom.find("</pre>", start)
    if start < 0 or end < 0:
        raise SystemExit("the harness left no <pre id=\"result\">")
    import html

    return json.loads(html.unescape(dom[start + len('<pre id="result">') : end]))


def assert_q1(operator: dict, picker: dict) -> None:
    assert operator.get("error") is None, operator
    assert picker.get("error") is None, picker
    labels = [row["text"] for row in operator["rows"]]
    for layer in LAYERS:
        assert any(layer["name"] in label for label in labels), (layer, labels)
    assert operator["layerToggle"]["before"] > operator["layerToggle"]["afterOff"], operator[
        "layerToggle"
    ]
    assert operator["layerToggle"]["afterOn"] == operator["layerToggle"]["before"], operator[
        "layerToggle"
    ]

    # The operator sees the private item; the picker does not.
    op_derived = [row["text"] for row in operator["derived"]]
    picker_derived = [row["text"] for row in picker["derived"]]
    assert any("private" in text for text in op_derived), op_derived
    assert not any("private" in text for text in picker_derived), picker_derived
    assert all(not row["checked"] for row in operator["derived"]), operator["derived"]

    # The contributor toggle is the server's answer, not a role string.
    assert any("Show all contributors" in row["text"] for row in operator["rows"]), labels
    assert not any(
        "Show all contributors" in row["text"] for row in picker["rows"]
    ), [row["text"] for row in picker["rows"]]


def assert_q2(operator: dict, picker: dict) -> None:
    # A fill exists, and breaks at the NaN gap: more than one ring.
    assert operator["bandRings"] >= 2, operator["bandRings"]
    # A crossing splits the fill rather than leaving one self-intersecting
    # ring; Leaflet renders each ring as two <path>s, so the count is even and
    # at least two.
    assert operator["crossingRings"] >= 2, operator["crossingRings"]
    # It survives with both bound lines off.
    assert operator["fillWithLinesOff"] > 0, operator["fillWithLinesOff"]
    # A fill whose other bound the caller cannot see is not drawn.
    assert operator["invisibleFill"] > 0, operator["invisibleFill"]
    assert picker["invisibleFill"] == 0, picker["invisibleFill"]


def assert_q3(operator: dict) -> None:
    assert operator["canEdit"], "an operator must be offered the editor"
    assert "position" in operator["previewStatus"], operator["previewStatus"]
    assert operator["previewLines"] > operator["layerToggle"]["afterOn"], operator
    assert operator["invalidStatus"], "an invalid expression must show a message"
    assert operator["previewAfterInvalid"] <= operator["layerToggle"]["afterOn"], operator
    suggestions = operator["suggestions"]
    assert "bed" in suggestions, suggestions
    assert "median" in suggestions, suggestions


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/ridal")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()

    if not args.binary.exists():
        raise SystemExit(f"build first: {args.binary} does not exist")

    workdir = Path(tempfile.mkdtemp(prefix="ridal-harness-"))
    print(f"project: {workdir}")
    build_project(workdir, args.binary)

    server_port = free_port()
    server = subprocess.Popen(
        [str(args.binary), "server", "start", str(workdir), "--port", str(server_port)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(100):
            try:
                api(server_port, "GET", "/api/v1/health")
                break
            except Exception:
                time.sleep(0.1)
        for name in ["admin", "op", "picker"]:
            redeem(server_port, name, (workdir / f"{name}.token").read_text().strip())

        # Stamp the picks with the revision the server computed, so the viewer
        # draws them directly instead of routing them through the carry path
        # (which needs axes the harness does not write).
        _, detail, _ = api(server_port, "GET", f"/api/v1/datasets/{RADARGRAM}")
        revision = json.loads(detail)["revision_id"]
        for user in ["op", "picker"]:
            path = (
                workdir / "ridal_data" / "interpretations" / RADARGRAM / f"{user}.gprinterp.json"
            )
            document = json.loads(path.read_text())
            document["source"] = {"revision_id": revision}
            path.write_text(json.dumps(document, indent=2))

        harness = HARNESS_TEMPLATE % {
            "password": json.dumps(PASSWORD),
            "radargram": RADARGRAM,
        }

        # A fresh proxy per run. Reusing one across chromium invocations left
        # an in-flight connection from the previous run that stalled the next.
        all_requests: list[str] = []

        def run(who: str, mode: str) -> dict:
            # Virtual time occasionally deadlocks on a fetch issued while the
            # page is otherwise busy (a known Chromium limitation); retry
            # rather than treat a flaky hang as a product failure.
            for attempt in range(3):
                proxy_port = free_port()
                proxy = Proxy(("127.0.0.1", proxy_port), server_port, harness)
                proxy.requests = all_requests
                threading.Thread(target=proxy.serve_forever, daemon=True).start()
                try:
                    return result_from_dom(chromium(proxy_port, who, mode))
                except subprocess.TimeoutExpired:
                    print(f"  ({who}/{mode} attempt {attempt + 1} hung; retrying)")
                finally:
                    proxy.shutdown()
                    proxy.server_close()
            raise SystemExit(f"{who}/{mode} hung three times")

        operator = run("op", "editor")
        operator.update(run("op", "panel"))
        picker = run("picker", "panel")

        external = [path for path in all_requests if "://" in path]
        print(json.dumps({"operator": operator, "picker": picker}, indent=2))
        print(f"proxy saw {len(all_requests)} requests; external: {external}")

        assert_q1(operator, picker)
        assert_q2(operator, picker)
        assert_q3(operator)
        assert not external, external
        print("PANEL HARNESS: all assertions passed")
    finally:
        server.terminate()
        server.wait(timeout=10)
        if not args.keep:
            shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    main()
