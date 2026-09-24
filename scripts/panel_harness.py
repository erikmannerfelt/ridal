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
        "expression": "percentile(concatenate(bed, bed_no_temperate), 75.0)",
        "unit": "meters",
        "color": "#ff8800",
    },
    {
        "id": "band_bottom",
        "name": "Band bottom",
        "expression": "percentile(concatenate(bed, bed_no_temperate), 25.0)",
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
        "expression": "percentile(concatenate(bed, bed_no_temperate), 50.0)",
        "unit": "meters",
        "color": "#123456",
        "fill_to": {"target": "op_secret", "opacity": 0.3},
    },
    {
        "id": "bed_count",
        "name": "Bed count",
        "expression": "count(bed)",
        "unit": "dimensionless",
        "color": "#654321",
    },
    {
        "id": "intermediate",
        "name": "Intermediate layer",
        "expression": "median(bed)",
        "unit": "meters",
        "color": "#999999",
        "listed": False,
    },
    {
        "id": "dep_base",
        "name": "Dependency base",
        "expression": "median(bed)",
        "unit": "meters",
        "color": "#111111",
    },
    {
        "id": "dep_child",
        "name": "Dependency child",
        "expression": "dep_base + 1.0",
        "unit": "meters",
        "color": "#222222",
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
            ("bed", [[70.0, 20.0], [99.0, 20.0]]),
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
    (row) => {
      const input = row.querySelector("input");
      return { text: row.textContent.trim(), checked: input ? input.checked : null };
    },
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

function derivedRowById(doc, id) {
  return doc.querySelector(`.layer-panel-derived-row[data-derived-id="${id}"]`);
}

function openEditorFor(doc, id) {
  derivedRowById(doc, id).querySelector(".layer-panel-edit").click();
}

function findRow(doc, text) {
  return Array.from(doc.querySelectorAll("#layer-panel .layer-panel-row")).find(
    (row) => row.textContent.includes(text),
  );
}


/* The /layers page: derived items are managed there too (#209 S4). The live
 * preview needs a radargram, so it is absent here; everything else is the
 * same shared editor. */
async function layersMode(doc, frame, result) {
  // layers.js deletes a layer with `window.confirm`; stub it so the harness
  // is not blocked by a native dialog.
  try {
    frame.contentWindow.confirm = () => true;
  } catch (error) {
    // If the window is not reachable the layer-delete step is skipped below.
  }
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const derivedIds = async () =>
    fetch("/api/v1/derived")
      .then((r) => r.json())
      .then((body) => body.items.map((item) => item.id));

  await wait(600);
  result.derivedRows = Array.from(
    doc.querySelectorAll("#derived-table tbody tr"),
  ).map((row) => row.textContent.trim());
  result.canAdd = Boolean(doc.querySelector("#derived-new"));
  const depBaseRow = Array.from(
    doc.querySelectorAll("#derived-table tbody tr"),
  ).find((row) => row.textContent.includes("dep_base"));
  result.usedByDepBase = depBaseRow ? depBaseRow.textContent : null;
  const intermediateRow = Array.from(
    doc.querySelectorAll("#derived-table tbody tr"),
  ).find((row) => row.textContent.includes("Intermediate layer"));
  result.intermediateRow = intermediateRow ? intermediateRow.textContent : null;
  const typeRow = Array.from(doc.querySelectorAll("#derived-table tbody tr")).find(
    (row) => row.textContent.includes("Band top"),
  );
  result.typeLayer = typeRow ? typeRow.textContent : null;
  const attributeTypeRow = Array.from(
    doc.querySelectorAll("#derived-table tbody tr"),
  ).find((row) => row.textContent.includes("Bed count"));
  result.typeAttribute = attributeTypeRow ? attributeTypeRow.textContent : null;
  // The expression cell is syntax-highlighted, not plain text.
  result.expressionHighlighted = Boolean(
    typeRow && typeRow.querySelector("code .tok-builtin, code .tok-layer"),
  );

  // S4 #1: create on /layers, and it appears in the viewer panel.
  doc.querySelector("#derived-new").click();
  await wait(250);
  const dlg = doc.querySelector("#derived-editor");
  result.editorHasNoPreview = dlg.querySelector("#derived-editor-hint").textContent.includes(
    "no live preview",
  );
  /* #236: the range-target dropdown is populated for an item that has no
   * fill yet. The viewer's range test passes either way, because its item
   * already has a `fill_to` and `buildTargets` keeps an existing target as an
   * option so a save cannot drop it -- only a fresh item shows whether the
   * dropdown was built at all. This editor is already open on one. */
  result.fillTargetOptions = Array.from(
    dlg.querySelectorAll("#derived-range-target option"),
  ).map((option) => option.value);
  dlg.querySelector("#derived-name").value = "From the layers page";
  dlg.querySelector("#derived-name").dispatchEvent(new Event("input", { bubbles: true }));
  dlg.querySelector("#derived-id").value = "from_layers";
  dlg.querySelector("#derived-expression").value = "median(bed)";
  dlg.querySelector("#derived-save").click();
  await wait(900);
  result.createdInApi = (await derivedIds()).includes("from_layers");
  const createdRow = Array.from(
    doc.querySelectorAll("#derived-table tbody tr"),
  ).find((candidate) => candidate.textContent.includes("from_layers"));
  if (createdRow) {
    const editButton = createdRow.querySelector("button");
    const deleteButton = createdRow.querySelector("button.danger");
    const style = (element) => frame.contentWindow.getComputedStyle(element);
    result.buttonStyleMatches =
      editButton &&
      deleteButton &&
      style(editButton).paddingTop === style(deleteButton).paddingTop &&
      style(editButton).borderRadius === style(deleteButton).borderRadius &&
      style(editButton).fontSize === style(deleteButton).fontSize;
  } else {
    result.buttonStyleMatches = false;
  }

  // Load the viewer in a second iframe and check the panel lists it.
  const viewer = document.createElement("iframe");
  viewer.width = "1200";
  viewer.height = "800";
  viewer.src = "/view/%(radargram)s";
  document.body.appendChild(viewer);
  await wait(3500);
  const viewerDoc = viewer.contentDocument;
  result.createdInViewer = Boolean(
    viewerDoc &&
      Array.from(viewerDoc.querySelectorAll("#layer-panel .layer-panel-row")).some(
        (row) => row.textContent.includes("From the layers page"),
      ),
  );
  viewer.remove();

  // S4 #3: the vocabulary can still be edited with derived items present, and
  // the derived items survive it.
  const form = doc.querySelector("#add-layer");
  form.querySelector('input[name="id"]').value = "test_layer";
  form.querySelector('input[name="name"]').value = "Test layer";
  form.dispatchEvent(new Event("submit", { cancelable: true, bubbles: true }));
  await wait(900);
  result.layerAdded = await fetch("/api/v1/layers")
    .then((r) => r.json())
    .then((body) => body.layers.some((layer) => layer.id === "test_layer"));
  result.derivedAfterLayerSave = (await derivedIds()).includes("from_layers");

  // S4 #2: delete on /layers, and it is gone from the viewer.
  const row = Array.from(doc.querySelectorAll("#derived-table tbody tr")).find(
    (candidate) => candidate.textContent.includes("from_layers"),
  );
  row.querySelector("button.danger").click();
  await wait(150);
  row.querySelector("button").click();
  await wait(900);
  result.deletedFromApi = !(await derivedIds()).includes("from_layers");
  result.deletedRowGone = !Array.from(
    doc.querySelectorAll("#derived-table tbody tr"),
  ).some((candidate) => candidate.textContent.includes("from_layers"));

  // S4 #3 (the other direction): remove the layer again.
  const layerRow = Array.from(doc.querySelectorAll("#layers-table tbody tr")).find(
    (candidate) => candidate.textContent.includes("test_layer"),
  );
  if (layerRow) {
    layerRow.querySelector("button.danger").click();
    await wait(900);
    result.layerGone = await fetch("/api/v1/layers")
      .then((r) => r.json())
      .then((body) => !body.layers.some((layer) => layer.id === "test_layer"));
  } else {
    result.layerGone = false;
  }
}

/* Mutually exclusive groups (#247) on the /layers page. The seeded group
 * already holds bed + bed_no_temperate; this builds the non-transitive
 * Svalbard arrangement by adding a second group with bed_no_temperate +
 * temperate_ice. The conflict outcomes are invisible in the DOM, so they are
 * asserted in Python from the stored document, not here. */
async function groupsMode(doc, frame, result) {
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const cards = () => Array.from(doc.querySelectorAll("#groups-list .group-card"));
  // A group card's first field is its name; the second is its (read-only) id.
  const cardByName = (name) =>
    cards().find((card) => {
      const input = card.querySelector(".add-layer-fields input");
      return input && input.value.includes(name);
    });

  await wait(800);
  result.seedCards = cards().length;
  const seed = cardByName("Bed and cold bed");
  result.seedChips = seed
    ? Array.from(seed.querySelectorAll(".group-chip")).map((chip) =>
        chip.textContent.trim(),
      )
    : null;

  doc.querySelector("#group-new").click();
  await wait(250);
  const draft = doc.querySelector("#groups-list .group-draft");
  result.draftShown = Boolean(draft);
  const [nameInput, idInput] = draft.querySelectorAll(".add-layer-fields input");
  nameInput.value = "Temperate ice present";
  nameInput.dispatchEvent(new Event("input", { bubbles: true }));
  await wait(200);
  // The id autofills from the name and is hand-editable: keep the autofilled
  // value on the record, then override it and confirm the override is stored.
  result.draftId = idInput.value;
  result.draftIdEditable = !idInput.readOnly;
  idInput.value = "cold_and_temperate";
  idInput.dispatchEvent(new Event("input", { bubbles: true }));
  draft.querySelector("#group-draft-create").click();
  await wait(1000);

  result.secondGroupShown = Boolean(cardByName("Temperate ice present"));
  // Add both members through the new group's own dropdown. Re-query each
  // time: every save re-renders the cards, so the previous node is detached.
  for (const layerId of ["bed_no_temperate", "temperate_ice"]) {
    const select = cardByName("Temperate ice present").querySelector(".group-add");
    select.value = layerId;
    select.dispatchEvent(new Event("change", { bubbles: true }));
    await wait(1000);
  }
  const second = cardByName("Temperate ice present");
  result.secondChips = Array.from(second.querySelectorAll(".group-chip")).map(
    (chip) => chip.textContent.trim(),
  );

  // The layer table reports multiplicity, which is what hides the shared
  // member: bed_no_temperate belongs to two groups.
  const row = Array.from(doc.querySelectorAll("#layers-table tbody tr")).find(
    (candidate) => candidate.textContent.includes("bed_no_temperate"),
  );
  result.groupsCell = row ? row.children[5].textContent.trim() : null;
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
  frame.width = MODE === "narrow" ? "360" : "1200";
  frame.height = "800";
  frame.src =
    MODE === "layers" || MODE === "groups"
      ? "/layers"
      : "/view/%(radargram)s";
  document.body.appendChild(frame);

  await sleep(3500);
  const doc = frame.contentDocument;
  if (!doc) {
    finish({ who: WHO, mode: MODE, error: "no document" });
    return;
  }

  if (MODE === "layers" || MODE === "groups") {
    const result = { who: WHO, mode: MODE };
    if (MODE === "layers") {
      await layersMode(doc, frame, result);
    } else {
      await groupsMode(doc, frame, result);
    }
    finish(result);
    return;
  }

  if (!doc.querySelector("#layer-panel")) {
    finish({ who: WHO, mode: MODE, error: "no layer panel" });
    return;
  }

  if (MODE === "refresh") {
    // Editing a pick and saving must update the derived lines, which are
    // computed server-side from the saved picks. The panel caches them, so
    // the save has to force a refetch. This drives that path: change the
    // stored picks behind the viewer's back, then redraw with and without
    // the force flag.
    const result = { who: WHO, mode: MODE };
    const top = findRow(doc, "Band top");
    top.querySelector("input").click();
    await sleep(1400);
    const bandPaths = () =>
      Array.from(doc.querySelectorAll(".derived-line-band_top"))
        .map((path) => path.getAttribute("d"))
        .join("|");
    result.before = bandPaths();

    const documentUrl = "/api/v1/datasets/%(radargram)s/interpretations/" + WHO;
    const original = await fetch(documentUrl).then((r) => r.json());
    const modified = JSON.parse(JSON.stringify(original));
    const bed = modified.features.find(
      (feature) => feature.properties && feature.properties.label === "bed",
    );
    bed.geometry.coordinates = bed.geometry.coordinates.map(([trace]) => [trace, 55.0]);
    result.putStatus = (
      await fetch(documentUrl, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(modified),
      })
    ).status;

    // Without the force flag the cached values are redrawn unchanged.
    frame.contentWindow.RIDAL_REDRAW_DERIVED(false);
    await sleep(900);
    result.withoutForce = bandPaths();
    // With it, the values are refetched and the line moves.
    frame.contentWindow.RIDAL_REDRAW_DERIVED(true);
    await sleep(1400);
    result.withForce = bandPaths();
    finish(result);
    return;
  }

  if (MODE === "split") {
    /* #226: splitting a line asks first.
     *
     * The split is reached the way a person reaches it -- click a drawn line
     * to select it, then click an interior vertex handle -- because the whole
     * point of the change is what a *click* does. Picking mode is left off: a
     * line can only be selected while it is off.
     */
    const result = { who: WHO, mode: MODE };
    try {
      // Every fixture line has two vertices, so none of them has an interior
      // vertex to split at. Lay down one three-vertex line first. Safe to
      // rewrite the stored picks here because `split` is the last mode to
      // run, and each mode captured its own results in its own browser.
      const documentUrl = "/api/v1/datasets/%(radargram)s/interpretations/" + WHO;
      const original = await fetch(documentUrl).then((r) => r.json());
      const threeVertex = JSON.parse(JSON.stringify(original));
      threeVertex.features = [
        {
          type: "Feature",
          geometry: {
            type: "LineString",
            // Increasing in trace, so it is not an overhang.
            coordinates: [[10.0, 20.0], [50.0, 25.0], [90.0, 30.0]],
          },
          properties: { id: "f-split", label: "bed" },
        },
      ];
      result.putStatus = (
        await fetch(documentUrl, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(threeVertex),
        })
      ).status;
      frame.contentWindow.location.reload();
      await sleep(3500);
      const reloaded = frame.contentDocument;

      const click = (el) =>
        el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
      // The visible polyline is non-interactive, so the clickable element is
      // its wide invisible companion (`.hit-line`) in the picker's own pane
      // -- one per stored feature.
      const linePaths = () =>
        reloaded.querySelectorAll(".leaflet-radargram-lines-pane path.hit-line");
      result.lineCount = linePaths().length;

      click(linePaths()[0]);
      await sleep(300);
      result.selectionShown = !reloaded.querySelector("#pick-selection").hidden;

      const interior = () => reloaded.querySelectorAll(".pick-handle-interior");
      result.interiorHandles = interior().length;
      click(interior()[0]);
      await sleep(300);

      // The prompt is up and nothing has been split yet.
      result.promptShown = Boolean(reloaded.querySelector("#pick-split-confirm"));
      result.linesWhilePrompted = linePaths().length;

      // Cancel leaves the line alone.
      click(reloaded.querySelector("#pick-split-cancel"));
      await sleep(300);
      result.promptGoneAfterCancel = !reloaded.querySelector("#pick-split-confirm");
      result.linesAfterCancel = linePaths().length;

      // Confirming splits it: one line becomes two.
      click(interior()[0]);
      await sleep(300);
      result.promptShownAgain = Boolean(reloaded.querySelector("#pick-split-confirm"));
      click(reloaded.querySelector("#pick-split-confirm"));
      await sleep(400);
      result.linesAfterSplit = linePaths().length;

      /* Joining asks in exactly the same way, and the split just set the
       * case up: the two halves share the vertex they were split at, so one
       * half's end sits on top of the other's. Dragging it a few pixels and
       * dropping is well inside the snap radius.
       *
       * The drag is synthesised the way Leaflet listens for it -- mousedown
       * on the handle, mousemove on the document, mouseup -- and has to
       * exceed L.Draggable's 3px click tolerance to count as a drag at all.
       */
      const centre = (el) => {
        const box = el.getBoundingClientRect();
        return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
      };
      const mouse = (target, type, at) =>
        target.dispatchEvent(
          new MouseEvent(type, {
            bubbles: true,
            cancelable: true,
            clientX: at.x,
            clientY: at.y,
          }),
        );

      // Select one half so its end handles are drawn.
      click(linePaths()[0]);
      await sleep(300);
      const ends = Array.from(reloaded.querySelectorAll(".pick-handle-end"));
      result.endHandles = ends.length;
      // The shared vertex is the rightmost of this half's two ends: the
      // split line ran left to right.
      const shared = ends.sort((a, b) => centre(b).x - centre(a).x)[0];
      const from = centre(shared);
      const to = { x: from.x + 6, y: from.y + 6 };
      /* The move and release are dispatched at the map element rather than
       * at the document. Leaflet takes `event.target` as the drag target and
       * adds a class to it; a Document has no `className`, so the class call
       * throws inside `_onMove`, `finishDrag` throws again on the way out
       * before it can fire `dragend`, and `Draggable._dragging` is left set
       * so no later drag starts either. The events still bubble to the
       * document listener Leaflet actually registered.
       *
       * The steps are also spread over a few frames, the way a real drag
       * arrives, rather than fired in one tick. */
      const mapEl = reloaded.querySelector("#map");
      const drag = async (handle, at, target) => {
        mouse(handle, "mousedown", at);
        await sleep(60);
        mouse(mapEl, "mousemove", { x: at.x + 3, y: at.y + 3 });
        await sleep(60);
        mouse(mapEl, "mousemove", target);
        await sleep(60);
        mouse(mapEl, "mouseup", target);
        await sleep(300);
      };
      const geometry = () =>
        Array.from(linePaths()).map((n) => n.getAttribute("d")).join("|");
      await drag(shared, from, to);

      result.joinPromptShown = Boolean(reloaded.querySelector("#pick-join-confirm"));
      result.linesWhileJoinPrompted = linePaths().length;

      // Cancelling leaves two lines, and puts the dragged end back.
      click(reloaded.querySelector("#pick-join-cancel"));
      await sleep(300);
      result.linesAfterJoinCancel = linePaths().length;

      // Confirming merges them back into one.
      // `redraw` replaces every handle, so the element dragged the first
      // time is detached by now and a second drag of it would go nowhere.
      const endsAgain = Array.from(reloaded.querySelectorAll(".pick-handle-end"));
      const sharedAgain = endsAgain.sort((a, b) => centre(b).x - centre(a).x)[0];
      const fromAgain = centre(sharedAgain);
      await drag(sharedAgain, fromAgain, {
        x: fromAgain.x + 6,
        y: fromAgain.y + 6,
      });
      result.joinPromptShownAgain = Boolean(
        reloaded.querySelector("#pick-join-confirm"),
      );
      click(reloaded.querySelector("#pick-join-confirm"));
      await sleep(400);
      result.linesAfterJoin = linePaths().length;

      finish(result);
      return;
    } catch (error) {
      // Report how far it got: the top-level catch keeps only the
      // exception, which does not say which step broke.
      result.error = String(error);
      finish(result);
      return;
    }
  }

  if (MODE === "picking") {
    /* #240: the line-drawing gesture.
     *
     * One button starts a line and finishes it, and a double-click on the
     * last point is the second way to finish. The map is clicked through the
     * same geometry the picker uses, so a synthetic click lands where a real
     * one would. */
    const result = { who: WHO, mode: MODE };
    try {
      const win = frame.contentWindow;
      const map = win.RIDAL_MAP;
      const G = win.RIDAL_GEOMETRY;
      const xscale = win.RIDAL_XSCALE || 1;
      const button = doc.getElementById("pick-toggle");
      const mapEl = doc.getElementById("map");
      const mapRect = mapEl.getBoundingClientRect();
      const linePaths = () =>
        doc.querySelectorAll(".leaflet-radargram-lines-pane path.hit-line");
      result.lineCountBefore = linePaths().length;
      result.startLabel = button.textContent.trim();
      result.startPressed = button.getAttribute("aria-pressed");

      const latlng = (t, s) =>
        win.L.latLng(-s * G.verticalRasterScale, t * G.rasterScale * xscale);
      const clickMap = (t, s, detail) => {
        const p = map.latLngToContainerPoint(latlng(t, s));
        mapEl.dispatchEvent(
          new win.MouseEvent("click", {
            bubbles: true,
            cancelable: true,
            clientX: mapRect.left + p.x,
            clientY: mapRect.top + p.y,
            detail: detail || 1,
          }),
        );
      };

      button.click();
      await sleep(200);
      result.drawingLabel = button.textContent.trim();
      result.drawingPressed = button.getAttribute("aria-pressed");
      result.mapHasPicking = mapEl.classList.contains("picking");

      // In the 40..70 trace gap between the fixture's two bed lines, so the
      // new line does not overlap either and the span rule lets it finish.
      // Spaced well beyond the double-click interval: two clicks inside it
      // are seen as a double-click by the browser and finish the line.
      clickMap(45, 30, 1);
      await sleep(600);
      clickMap(55, 40, 1);
      await sleep(600);
      result.draftHandles = doc.querySelectorAll(".pick-handle-draft").length;
      result.drawingStatus = doc.getElementById("pick-status").textContent;

      // Double-click on a new point: the first click places the last vertex.
      clickMap(65, 50, 1);
      await sleep(80);
      // The second click lands on the handle the first click just created --
      // the same element a real double-click hits. Dispatching it at the map
      // instead would miss the bug where that handle's tap-to-remove eats
      // the vertex.
      const placed = doc.querySelectorAll(".pick-handle-draft");
      const target = placed[placed.length - 1];
      const targetRect = target.getBoundingClientRect();
      const targetAt = {
        x: targetRect.left + targetRect.width / 2,
        y: targetRect.top + targetRect.height / 2,
      };
      const atTarget = (type) =>
        target.dispatchEvent(
          new win.MouseEvent(type, {
            bubbles: true,
            cancelable: true,
            clientX: targetAt.x,
            clientY: targetAt.y,
            detail: 2,
          }),
        );
      atTarget("click");
      await sleep(80);
      atTarget("dblclick");
      await sleep(300);
      result.afterFinishLabel = button.textContent.trim();
      result.afterFinishPressed = button.getAttribute("aria-pressed");
      result.draftHandlesAfter = doc.querySelectorAll(".pick-handle-draft").length;
      result.lineCount = linePaths().length;

      // With no line in progress, the new line is selectable like any other.
      // The panel it opens must overlay the map, not push it down (#239).
      result.mapTopBeforeSelect = mapEl.getBoundingClientRect().top;
      const paths = linePaths();
      paths[paths.length - 1].dispatchEvent(
        new win.MouseEvent("click", { bubbles: true, cancelable: true }),
      );
      await sleep(200);
      const selection = doc.getElementById("pick-selection");
      result.selectionShown = !selection.hidden;
      // Wide frame: the editing help starts expanded.
      result.selectionHelpOpen = doc.getElementById("pick-selection-help").open;
      result.mapTopAfterSelect = mapEl.getBoundingClientRect().top;
      const selectionRect = selection.getBoundingClientRect();
      const mapRectAfter = mapEl.getBoundingClientRect();
      result.selectionWithinMap =
        selectionRect.top >= mapRectAfter.top - 1 &&
        selectionRect.bottom <= mapRectAfter.bottom + 1;
      // Three taps went in (two singles and the final double-click), so the
      // committed line must have three vertices, not two.
      result.selectedHandles = doc.querySelectorAll(
        ".pick-handle-interior, .pick-handle-end",
      ).length;

      finish(result);
    } catch (error) {
      result.error = String(error);
      finish(result);
    }
    return;
  }

  if (MODE === "duplicates") {
    /* #238: a duplicate value is marked like an overhang, but with its own
     * shape, and its reason is reachable by clicking rather than only on
     * hover. The duplicate document is written to disk before this browser
     * starts, because the save route refuses one. */
    const result = { who: WHO, mode: MODE };
    try {
      result.duplicateMarkers = doc.querySelectorAll(".pick-duplicate").length;
      result.overhangMarkers = doc.querySelectorAll(".pick-overhang").length;
      const marker = doc.querySelector(".pick-duplicate");
      const toast = doc.getElementById("pick-error");
      result.toastHiddenBefore = toast.hidden;
      marker.dispatchEvent(
        new frame.contentWindow.MouseEvent("click", {
          bubbles: true,
          cancelable: true,
        }),
      );
      await sleep(200);
      result.reasonShown = !toast.hidden;
      result.reason = toast.textContent;
      finish(result);
    } catch (error) {
      result.error = String(error);
      finish(result);
    }
    return;
  }

  if (MODE === "overlap") {
    /* #207: two lines covering the same traces are a violation, not only two
     * vertices on one trace. The overlap document is written to disk before
     * this browser starts, because the save route refuses it. */
    const result = { who: WHO, mode: MODE };
    try {
      const win = frame.contentWindow;
      const map = win.RIDAL_MAP;
      const G = win.RIDAL_GEOMETRY;
      const xscale = win.RIDAL_XSCALE || 1;
      const mapEl = doc.getElementById("map");
      const mapRect = mapEl.getBoundingClientRect();
      const button = doc.getElementById("pick-toggle");
      const toast = doc.getElementById("pick-error");
      const linePaths = () =>
        doc.querySelectorAll(".leaflet-radargram-lines-pane path.hit-line");
      result.bands = doc.querySelectorAll(".pick-overlap-band").length;
      result.markers = doc.querySelectorAll(".pick-overlap").length;
      result.duplicateMarkers = doc.querySelectorAll(".pick-duplicate").length;
      result.lineCountBefore = linePaths().length;

      const marker = doc.querySelector(".pick-overlap");
      marker.dispatchEvent(
        new win.MouseEvent("click", { bubbles: true, cancelable: true }),
      );
      await sleep(200);
      result.reasonShown = !toast.hidden;
      result.reason = toast.textContent;

      /* A line that already overlaps stays editable. Without the
       * grandfather, moving a vertex off an overlap is itself a move onto
       * an overlap, so the line can only be fixed by deleting it. */
      const click = (el) =>
        el.dispatchEvent(
          new win.MouseEvent("click", { bubbles: true, cancelable: true }),
        );
      const centre = (el) => {
        const box = el.getBoundingClientRect();
        return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
      };
      const mouse = (target, type, at) =>
        target.dispatchEvent(
          new win.MouseEvent(type, {
            bubbles: true,
            cancelable: true,
            clientX: at.x,
            clientY: at.y,
          }),
        );
      const drag = async (handle, at, target) => {
        mouse(handle, "mousedown", at);
        await sleep(60);
        mouse(mapEl, "mousemove", { x: at.x + 3, y: at.y + 3 });
        await sleep(60);
        mouse(mapEl, "mousemove", target);
        await sleep(60);
        mouse(mapEl, "mouseup", target);
        await sleep(300);
      };
      click(linePaths()[0]);
      await sleep(250);
      const ends = Array.from(doc.querySelectorAll(".pick-handle-end"));
      result.grandfatherEnds = ends.length;
      const from = centre(ends[0]);
      await drag(ends[0], from, { x: from.x + 25, y: from.y + 5 });
      const moved = centre(doc.querySelectorAll(".pick-handle-end")[0]);
      result.grandfathered =
        Math.hypot(moved.x - from.x, moved.y - from.y) > 5;

      // Drawing over an existing line is allowed; finishing it is not.
      const latlng = (t, s) =>
        win.L.latLng(-s * G.verticalRasterScale, t * G.rasterScale * xscale);
      const clickMap = (t, s) => {
        const p = map.latLngToContainerPoint(latlng(t, s));
        mapEl.dispatchEvent(
          new win.MouseEvent("click", {
            bubbles: true,
            cancelable: true,
            clientX: mapRect.left + p.x,
            clientY: mapRect.top + p.y,
            detail: 1,
          }),
        );
      };
      button.click();
      await sleep(150);
      clickMap(40, 100);
      await sleep(600);
      clickMap(60, 110);
      await sleep(600);
      result.draftLabelBeforeFinish = button.textContent.trim();
      button.click();
      await sleep(250);
      result.refusedLabel = button.textContent.trim();
      result.refusedStatus = doc.getElementById("pick-status").textContent;
      result.lineCountAfter = linePaths().length;
      result.refusedReason = toast.textContent;

      // The toast and the selection panel share the map's bottom row and
      // must not overlap. Both are forced visible so the rectangles can be
      // measured whatever the interaction left behind.
      const panelEl = doc.getElementById("pick-selection");
      panelEl.hidden = false;
      toast.textContent =
        "This line covers traces 853.2 to 1039.2, where Bed already has one.";
      toast.hidden = false;
      await sleep(100);
      const panelRect = panelEl.getBoundingClientRect();
      const toastRect = toast.getBoundingClientRect();
      result.panelToastOverlap = !(
        panelRect.right <= toastRect.left ||
        toastRect.right <= panelRect.left ||
        panelRect.bottom <= toastRect.top ||
        toastRect.bottom <= panelRect.top
      );
      finish(result);
    } catch (error) {
      result.error = String(error);
      finish(result);
    }
    return;
  }

  if (MODE === "narrow") {
    const result = { who: WHO, mode: MODE };
    // Narrow frame: the editing help must start collapsed, not cover the map.
    result.selectionHelpOpen = doc.getElementById("pick-selection-help").open;
    // The bottom-right toast and the bottom-left selection panel must still
    // not meet on a phone, where each is capped to just under half the
    // viewport. Forced visible with a long message so the cap is exercised.
    const selectionEl = doc.getElementById("pick-selection");
    const errorEl = doc.getElementById("pick-error");
    selectionEl.hidden = false;
    errorEl.textContent =
      "This line covers traces 853.2 to 1039.2, where Bed already has one.";
    errorEl.hidden = false;
    await sleep(150);
    const selectionRect = selectionEl.getBoundingClientRect();
    const errorRect = errorEl.getBoundingClientRect();
    result.bottomControlsOverlap = !(
      selectionRect.right <= errorRect.left ||
      errorRect.right <= selectionRect.left ||
      selectionRect.bottom <= errorRect.top ||
      errorRect.bottom <= selectionRect.top
    );
    selectionEl.hidden = true;
    errorEl.hidden = true;
    const panel = doc.querySelector("#layer-panel");
    panel.querySelector("summary").click();
    await sleep(300);
    const rect = panel.getBoundingClientRect();
    const viewport = frame.contentWindow.innerWidth;
    result.viewport = viewport;
    result.panelLeft = Math.round(rect.left);
    result.panelRight = Math.round(rect.right);
    result.panelFits = rect.left >= -1 && rect.right <= viewport + 1;
    const mapRect = doc.querySelector("#map").getBoundingClientRect();
    result.panelBottom = Math.round(rect.bottom);
    result.mapBottom = Math.round(mapRect.bottom);
    // The panel must stay inside the map, or its lower half is clipped and
    // unreachable.
    result.panelWithinMap = rect.bottom <= mapRect.bottom + 1;
    const body = panel.querySelector(".layer-panel-body");
    result.bodyMaxHeight = body.style.maxHeight;
    // A long row must scroll rather than be clipped: force a wide child.
    const wide = doc.createElement("div");
    wide.style.width = "600px";
    body.appendChild(wide);
    result.bodyScrollable = body.scrollWidth > body.clientWidth;
    body.scrollLeft = 40;
    result.bodyScrolled = body.scrollLeft > 0;
    // And tall content must scroll vertically inside the capped height.
    const tall = doc.createElement("div");
    tall.style.height = "900px";
    body.appendChild(tall);
    result.bodyScrollableV = body.scrollHeight > body.clientHeight;
    body.scrollTop = 40;
    result.bodyScrolledV = body.scrollTop > 0;
    finish(result);
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
    // S1: the panel is a disclosure, closed by default.
    const panel = doc.querySelector("#layer-panel");
    const firstRow = doc.querySelector("#layer-panel .layer-panel-row");
    result.panelOpenOnLoad = panel.open;
    const visible = (node) => Boolean(node && node.checkVisibility && node.checkVisibility());
    result.rowHiddenWhenClosed = firstRow ? !visible(firstRow) : null;
    panel.querySelector("summary").click();
    await sleep(200);
    result.rowShownWhenOpen = visible(firstRow);
    panel.querySelector("summary").click();
    await sleep(200);
    result.rowHiddenAfterClose = firstRow ? !visible(firstRow) : null;

    // Clicking elsewhere in the viewer closes the open disclosure. This is
    // the only way the panel gets out of the way -- the button that used to
    // hide the control outright was removed as a redundant second mechanism.
    panel.querySelector("summary").click();
    await sleep(200);
    doc.querySelector("#map").dispatchEvent(
      new MouseEvent("click", { bubbles: true }),
    );
    await sleep(200);
    result.panelClosesOnOutsideClick = panel.open === false;

    // #230: hiding the radargram makes every image chunk transparent, and
    // showing it again restores every one of them.
    //
    // The lazily-added case (a chunk placed while hidden must arrive
    // transparent) is deliberately not exercised here: panning far enough
    // to place new chunks starts image fetches that deadlock chromium's
    // virtual-time budget, which is the same limitation the window-size
    // comment in `chromium()` describes.
    const chunkOpacities = () =>
      Array.from(doc.querySelectorAll("#map img.leaflet-image-layer")).map(
        (img) => img.style.opacity,
      );
    const radargramToggle = doc.querySelector("#radargram-toggle");
    result.radargramOpacityShown = chunkOpacities();
    radargramToggle.click();
    await sleep(200);
    result.radargramOpacityHidden = chunkOpacities();
    result.radargramToggleLabel = radargramToggle.textContent.trim();
    radargramToggle.click();
    await sleep(200);
    result.radargramOpacityRestored = chunkOpacities();
    result.radargramToggleLabelBack = radargramToggle.textContent.trim();

    // The download menu renames picked points and adds derived points.
    const pickedButton = doc.querySelector("#dl-points");
    const derivedButton = doc.querySelector("#dl-derived-points");
    result.pickedPointsLabel = pickedButton ? pickedButton.textContent.trim() : null;
    result.hasDerivedPoints = Boolean(derivedButton);

    // The summary count is exactly the layer rows the panel shows (the
    // contributor toggle is not a layer).
    const summaryText = doc.querySelector("#layer-panel-summary").textContent;
    result.summaryText = summaryText;
    result.layerRows = doc.querySelectorAll("#layer-panel .layer-panel-row").length;
    result.contributorRows = Array.from(
      doc.querySelectorAll("#layer-panel .layer-panel-row"),
    ).filter((row) => row.textContent.includes("Show all contributors")).length;

    // A control with no colour gets no empty swatch.
    const contributorRow = findRow(doc, "Show all contributors");
    result.contributorSwatch = Boolean(
      contributorRow && contributorRow.querySelector(".layer-panel-swatch"),
    );

    // "New expression" belongs to the Derived layers section, before the
    // Contributors heading.
    const panelBody = doc.querySelector(".layer-panel-body");
    const order = Array.from(panelBody.children);
    const newButtonIndex = order.indexOf(doc.querySelector("#derived-new"));
    const contributorsIndex = order.findIndex((node) =>
      node.textContent.trim() === "Contributors",
    );
    result.newButtonBeforeContributors =
      newButtonIndex >= 0 && contributorsIndex >= 0 && newButtonIndex < contributorsIndex;

    // An unlisted intermediate layer is not in the panel at all.
    result.intermediateAbsent = !Array.from(
      doc.querySelectorAll("#layer-panel .layer-panel-row"),
    ).some((row) => row.textContent.includes("Intermediate layer"));

    // The contributor toggle shows and hides other users' lines, without
    // stacking them on repeated toggles.
    result.contributorsApi = await fetch(
      "/api/v1/datasets/%(radargram)s/contributors",
    )
      .then((r) => r.json())
      .then((body) => ({
        can_see_others: body.can_see_others,
        docs: (body.documents || []).map((entry) => ({
          user: entry.user,
          own: entry.own,
          features: (entry.document.features || []).length,
        })),
      }));
    const contributors = findRow(doc, "Show all contributors");
    if (contributors) {
      contributors.querySelector("input").click();
      await sleep(800);
      result.contributorOn = doc.querySelectorAll(".contributor-pick-line").length;
      contributors.querySelector("input").click();
      await sleep(800);
      result.contributorOff = doc.querySelectorAll(".contributor-pick-line").length;
      contributors.querySelector("input").click();
      await sleep(800);
      result.contributorOnAgain = doc.querySelectorAll(".contributor-pick-line").length;
    }

    // Q1 #2: toggling a layer off removes its polylines.
    const bedRow = findRow(doc, "Glacier bed");
    const before = lineCount(doc);
    bedRow.querySelector("input").click();
    await sleep(300);
    const afterOff = lineCount(doc);
    bedRow.querySelector("input").click();
    await sleep(300);
    result.layerToggle = { before, afterOff, afterOn: lineCount(doc) };

    // An attribute has no line, so it is not in the viewer panel at all --
    // it is managed on the /layers page.
    result.attributeAbsent = !Array.from(
      doc.querySelectorAll("#layer-panel .layer-panel-row"),
    ).some((row) => row.textContent.includes("Bed count"));

    // Q1 #3: derived items present, unchecked, private one only for op.
    result.derived = Array.from(doc.querySelectorAll("#layer-panel .layer-panel-row"))
      .filter((row) => /Band|Crossing|private/.test(row.textContent))
      .map((row) => ({ text: row.textContent.trim(), checked: row.querySelector("input").checked }));

    // Q2: a fill between the two bounds, broken at the NaN gap.
    const top = findRow(doc, "Band top");
    const bottom = findRow(doc, "Band bottom");
    const gapFill = findRow(doc, "Fill against a hidden bound");
    // One at a time: each toggle redraws asynchronously, and three at once
    // interleave their clears and redraws.
    // The hidden-bound item's own bound must be visible too, for the
    // operator (who can see it); the picker never sees the item at all.
    const hiddenBound = findRow(doc, "Operator's private line");
    top.querySelector("input").click();
    await sleep(500);
    bottom.querySelector("input").click();
    await sleep(500);
    if (hiddenBound) {
      hiddenBound.querySelector("input").click();
      await sleep(500);
    }
    gapFill.querySelector("input").click();
    await sleep(1400);
    result.bandFill = fillCount(doc);
    result.bandRings = fillColorCount(doc, "#0088ff");
    // A fill whose other bound is not visible to the caller must not draw.
    result.invisibleFill = fillColorCount(doc, "#123456");
    result.fillColours = Array.from(
      doc.querySelectorAll(".leaflet-radargram-fills-pane path"),
    ).map((path) => path.getAttribute("fill"));
    result.visibleDerived = Array.from(
      doc.querySelectorAll(".layer-panel-derived-row"),
    ).map((row) => ({ id: row.dataset.derivedId, checked: row.querySelector("input").checked }));

    // Q2: two crossing lines must not make a self-intersecting polygon.
    const crossTop = findRow(doc, "Crossing top");
    const crossBottom = findRow(doc, "Crossing bottom");
    crossTop.querySelector("input").click();
    await sleep(500);
    crossBottom.querySelector("input").click();
    await sleep(1400);
    result.crossingFill = fillCount(doc);
    // One crossing means two rings; a bowtie would be one.
    result.crossingRings = fillColorCount(doc, "#aa00aa");

    // A fill is visible only while both bounds are: hiding one removes it,
    // and hiding both keeps it gone.
    top.querySelector("input").click();
    await sleep(600);
    result.fillWithOneBoundOff = fillColorCount(doc, "#0088ff");
    bottom.querySelector("input").click();
    await sleep(600);
    result.fillWithLinesOff = fillColorCount(doc, "#0088ff");
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

  if (MODE === "manage" && result.canEdit) {
    // The dialog is created lazily on the first open, so it must be opened
    // before it can be queried.
    const editorDialog = () => doc.querySelector("#derived-editor");
    const saveAndWait = async (ms) => {
      editorDialog().querySelector("#derived-save").click();
      await sleep(ms || 900);
    };

    // S2 #1: editing pre-fills, the id is read-only, and a changed
    // expression is saved.
    openEditorFor(doc, "dep_child");
    await sleep(250);
    const dlg = editorDialog();
    result.editPrefill = {
      name: dlg.querySelector("#derived-name").value,
      id: dlg.querySelector("#derived-id").value,
      expression: dlg.querySelector("#derived-expression").value,
      idReadOnly: dlg.querySelector("#derived-id").readOnly,
    };
    dlg.querySelector("#derived-expression").value = "dep_base + 2.0";
    await saveAndWait(900);
    result.savedExpression = await fetch("/api/v1/derived")
      .then((r) => r.json())
      .then((body) => body.items.find((i) => i.id === "dep_child").expression);

    // S3 #4: a colour reaches the drawn line and the swatch.
    openEditorFor(doc, "band_top");
    await sleep(250);
    dlg.querySelector("#derived-no-color").checked = false;
    const colour = dlg.querySelector("#derived-color");
    colour.value = "#123456";
    colour.dispatchEvent(new Event("input", { bubbles: true }));
    await saveAndWait(900);
    const topRow = derivedRowById(doc, "band_top");
    if (!topRow.querySelector("input").checked) {
      topRow.querySelector("input").click();
    }
    await sleep(900);
    result.colourStroke = doc.querySelectorAll(
      '.leaflet-radargram-lines-pane path[stroke="#123456"]',
    ).length;
    result.colourSwatch = topRow.querySelector(".layer-panel-swatch").style.background;

    // S3 #5: a range set in the editor draws a fill.
    openEditorFor(doc, "band_bottom");
    await sleep(250);
    const range = dlg.querySelector("#derived-range");
    range.checked = true;
    range.dispatchEvent(new Event("change", { bubbles: true }));
    dlg.querySelector("#derived-range-target").value = "band_top";
    dlg.querySelector("#derived-range-color").value = "#abcdef";
    dlg.querySelector("#derived-range-opacity").value = "0.4";
    await saveAndWait(1100);
    const bottomRow = derivedRowById(doc, "band_bottom");
    if (!bottomRow.querySelector("input").checked) {
      bottomRow.querySelector("input").click();
    }
    await sleep(900);
    result.rangeFill = doc.querySelectorAll(
      '.leaflet-radargram-fills-pane path[fill="#abcdef"]',
    ).length;

    // S3 #6: a non-hex colour is refused with the server's message.
    openEditorFor(doc, "band_top");
    await sleep(250);
    dlg.querySelector("#derived-no-color").checked = false;
    dlg.querySelector("#derived-color").value = "red";
    await saveAndWait(900);
    result.nonHexStatus = dlg.querySelector("#derived-status").textContent;
    dlg.querySelector("#derived-cancel").click();
    await sleep(250);

    // #228: the colour round trip along the path a person actually takes.
    //
    // The colour check above passes without exercising the bug, because
    // assigning `.value` to a *disabled* input still works -- `disabled`
    // only stops user interaction -- and setting `.checked` directly fires
    // no `change` event. So it must click the checkbox, not assign to it.
    openEditorFor(doc, "crossing_top");
    await sleep(250);
    const noColour = () => dlg.querySelector("#derived-no-color");
    const colourText = () => dlg.querySelector("#derived-color");
    const colourPick = () => dlg.querySelector("#derived-color-picker");
    // The picker must carry this item's own colour, not whatever the last
    // item opened left in the single reused element. `crossing_top` is used
    // here because no check after this point reads its colour.
    result.pickerMatchesItem = colourPick().value === "#00aa00";
    noColour().click();
    await sleep(150);
    result.colourDisabledAfterCheck = colourText().disabled;
    await saveAndWait(900);
    result.colourClearedInStore = await fetch("/api/v1/derived")
      .then((r) => r.json())
      .then((body) => body.items.find((i) => i.id === "crossing_top").color ?? null);

    // Reopened with no colour: the box is checked and the inputs are off.
    // Unchecking it must turn them back on, and Save must then write the
    // colour the picker is showing -- `save` reads the text field, so it
    // has to have been seeded from the picker.
    openEditorFor(doc, "crossing_top");
    await sleep(250);
    result.colourDisabledOnReopen = colourText().disabled;
    result.pickerResetWhenColourless = colourPick().value;
    noColour().click();
    await sleep(150);
    result.colourDisabledAfterUncheck = colourText().disabled;
    result.colourSeededFromPicker = colourText().value;
    await saveAndWait(900);
    result.colourRestoredInStore = await fetch("/api/v1/derived")
      .then((r) => r.json())
      .then((body) => body.items.find((i) => i.id === "crossing_top").color ?? null);

    // S2 #3: deleting a depended-on item is refused, naming the dependent.
    const baseRow = derivedRowById(doc, "dep_base");
    baseRow.querySelector("button.danger").click();
    await sleep(150);
    baseRow.querySelector(".layer-panel-confirm button").click();
    await sleep(700);
    result.deleteRefused = doc.querySelector("#layer-panel-error").textContent;

    // S2 #2: deleting a standalone item removes it, and it stays gone.
    const childRow = derivedRowById(doc, "dep_child");
    childRow.querySelector("button.danger").click();
    await sleep(150);
    childRow.querySelector(".layer-panel-confirm button").click();
    await sleep(900);
    result.deletedGone = await fetch("/api/v1/derived")
      .then((r) => r.json())
      .then((body) => !body.items.some((i) => i.id === "dep_child"));
    result.deletedRowGone = !derivedRowById(doc, "dep_child");
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
        "--virtual-time-budget=" + ("25000" if mode in ("layers", "groups") else "15000"),
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

    blob = html.unescape(dom[start + len('<pre id="result">') : end])
    try:
        return json.loads(blob)
    except json.JSONDecodeError:
        raise SystemExit(f"harness left a non-JSON result: {blob[:500]!r}") from None


def assert_q1(operator: dict, picker: dict) -> None:
    assert operator.get("error") is None, operator
    assert picker.get("error") is None, picker
    labels = [row["text"] for row in operator["rows"]]
    for layer in LAYERS:
        assert any(layer["name"] in label for label in labels), (layer, labels)
    assert operator["attributeAbsent"] is True, "attributes belong on /layers, not the panel"
    assert operator["intermediateAbsent"] is True, "an unlisted layer must not be in the panel"
    count = int(operator["summaryText"].split("(")[1].split(")")[0])
    assert count == operator["layerRows"] - operator["contributorRows"], (
        f"summary says {operator['summaryText']} but there are "
        f"{operator['layerRows']} rows ({operator['contributorRows']} of them the contributor toggle)"
    )
    assert operator["contributorSwatch"] is False, "the contributor toggle needs no colour swatch"
    assert operator["newButtonBeforeContributors"] is True, (
        "'New expression' must sit in the Derived layers section"
    )
    assert all(
        o in ("", "1") for o in operator["radargramOpacityShown"]
    ), f"chunks should start opaque, saw {operator['radargramOpacityShown']}"
    assert operator["radargramOpacityHidden"], "no image chunks were found to hide"
    assert all(
        o == "0" for o in operator["radargramOpacityHidden"]
    ), f"hiding must make every chunk transparent, saw {operator['radargramOpacityHidden']}"
    assert (
        operator["radargramToggleLabel"] == "Show radargram"
    ), "the button must offer the way back"
    assert all(
        o == "1" for o in operator["radargramOpacityRestored"]
    ), f"showing again must restore every chunk, saw {operator['radargramOpacityRestored']}"
    assert (
        operator["radargramToggleLabelBack"] == "Hide radargram"
    ), "the button must return to offering the hide"
    assert operator["panelClosesOnOutsideClick"] is True, (
        "the panel must close when the viewer is clicked"
    )
    assert operator["pickedPointsLabel"] == "Picked layer points", operator[
        "pickedPointsLabel"
    ]
    assert operator["hasDerivedPoints"] is True, "the derived-points entry must be offered"
    assert operator["contributorOn"] > 0, "the contributor toggle must draw lines"
    assert operator["contributorOff"] == 0, "disabling it must remove them"
    assert (
        operator["contributorOnAgain"] == operator["contributorOn"]
    ), "re-enabling must not stack lines"
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
    # Hiding either bound removes the fill; hiding both keeps it gone.
    assert operator["fillWithOneBoundOff"] == 0, operator["fillWithOneBoundOff"]
    assert operator["fillWithLinesOff"] == 0, operator["fillWithLinesOff"]
    # A fill whose other bound the caller cannot see is not drawn.
    assert operator["invisibleFill"] > 0, operator["invisibleFill"]
    assert picker["invisibleFill"] == 0, picker["invisibleFill"]


def assert_q3(operator: dict) -> None:
    assert operator["canEdit"], "an operator must be offered the editor"
    assert "layer" in operator["previewStatus"], operator["previewStatus"]
    assert operator["previewLines"] > operator["layerToggle"]["afterOn"], operator
    assert operator["invalidStatus"], "an invalid expression must show a message"
    assert operator["previewAfterInvalid"] <= operator["layerToggle"]["afterOn"], operator
    suggestions = operator["suggestions"]
    assert "bed" in suggestions, suggestions
    assert "median" in suggestions, suggestions


def assert_layers(layers: dict) -> None:
    offered = layers["fillTargetOptions"]
    assert "band_top" in offered and "crossing_top" in offered, (
        "a new item must be offered every layer item as a range target -- this "
        f"is #236, where the dropdown was empty. Offered: {offered}"
    )
    assert "bed_count" not in offered, (
        "an attribute has no position to fill toward, so it must not be "
        f"offered: {offered}"
    )

    assert layers.get("error") is None, layers
    assert layers["canAdd"] is True, "an operator must be able to add on /layers"
    assert layers["buttonStyleMatches"] is True, "Edit and Delete must share one style"
    assert layers["usedByDepBase"] is not None and "dep_child" not in (
        layers["usedByDepBase"] or ""
    ), layers["usedByDepBase"]
    assert "1" in (layers["usedByDepBase"] or ""), (
        "the used-by counter must show dep_base is used once"
    )
    assert layers["intermediateRow"] is not None and "no" in layers["intermediateRow"], (
        layers["intermediateRow"]
    )
    assert layers["typeLayer"] is not None and "layer" in layers["typeLayer"], layers[
        "typeLayer"
    ]
    assert (
        layers["typeAttribute"] is not None and "attribute" in layers["typeAttribute"]
    ), layers["typeAttribute"]
    assert layers["expressionHighlighted"] is True, "the expression must be highlighted"
    assert layers["editorHasNoPreview"] is True, layers
    assert layers["createdInApi"] is True, layers
    assert layers["createdInViewer"] is True, "an item created on /layers must appear in the viewer"
    assert layers["deletedFromApi"] is True, layers
    assert layers["deletedRowGone"] is True, layers
    assert layers["layerAdded"] is True, layers
    assert layers["derivedAfterLayerSave"] is True, "a layer save must not lose derived items"
    assert layers["layerGone"] is True, layers


def assert_groups(groups: dict) -> None:
    assert groups.get("error") is None, groups
    assert groups["seedCards"] == 1, groups
    assert groups["draftShown"] is True, groups
    assert groups["draftId"] == "temperate_ice_present", groups["draftId"]
    assert groups["draftIdEditable"] is True, (
        "the id must be hand-editable, not only generated"
    )
    assert groups["secondGroupShown"] is True, groups
    assert any(
        "Glacier bed (no temperate ice above)" in chip for chip in groups["secondChips"]
    ), groups["secondChips"]
    # bed_no_temperate is now in the seeded group and the new one.
    assert groups["groupsCell"] == "2 groups", groups["groupsCell"]

    stored = groups["stored"]
    by_id = {group["id"]: set(group.get("members", [])) for group in stored["groups"]}
    assert "cold_and_temperate" in by_id, (
        "the hand-typed id must be what is stored, not the generated one: "
        f"{sorted(by_id)}"
    )

    def conflicts(a: str, b: str) -> bool:
        return any(a in members and b in members for members in by_id.values())

    assert conflicts("bed", "bed_no_temperate"), by_id
    assert conflicts("bed_no_temperate", "temperate_ice"), by_id
    assert not conflicts("bed", "temperate_ice"), by_id


def assert_refresh(refresh: dict) -> None:
    assert refresh.get("error") is None, refresh
    assert refresh["before"], "the derived line must be drawn"
    assert refresh["putStatus"] == 200, refresh["putStatus"]
    assert refresh["withoutForce"] == refresh["before"], (
        "a plain redraw must reuse the cached values"
    )
    assert refresh["withForce"] != refresh["before"], (
        "a forced redraw must refetch and move the line"
    )


def assert_split(split: dict) -> None:
    assert split.get("error") is None, split
    assert split["putStatus"] == 200, f"laying down the test line failed: {split}"
    assert split["lineCount"] == 1, f"expected the one line just written, got {split}"
    assert split["selectionShown"] is True, "clicking a line must select it"
    assert split["interiorHandles"] > 0, "a selected line must show interior handles"
    assert split["promptShown"] is True, (
        "tapping an interior vertex must ask before splitting -- this is #226, "
        "and without it the line is split on the spot"
    )
    assert (
        split["linesWhilePrompted"] == split["lineCount"]
    ), "nothing may be split while the prompt is still up"
    assert split["promptGoneAfterCancel"] is True, "Cancel must dismiss the prompt"
    assert (
        split["linesAfterCancel"] == split["lineCount"]
    ), "Cancel must leave the line whole"
    assert split["promptShownAgain"] is True, "the prompt must come back on a second tap"
    assert split["linesAfterSplit"] == split["lineCount"] + 1, (
        "confirming must split one line into two, got "
        f"{split['linesAfterSplit']} from {split['lineCount']}"
    )
    assert split["endHandles"] == 2, f"a split half must show two ends, got {split}"
    assert split["joinPromptShown"] is True, (
        "dropping an end onto another line's end must ask before joining -- "
        "this is the other half of #226"
    )
    assert (
        split["linesWhileJoinPrompted"] == 2
    ), "nothing may be joined while the prompt is still up"
    assert (
        split["linesAfterJoinCancel"] == 2
    ), "cancelling must leave both lines alone"
    assert split["joinPromptShownAgain"] is True, (
        "the join prompt must come back on a second drop"
    )
    assert (
        split["linesAfterJoin"] == 1
    ), f"confirming must join the two halves back into one, got {split['linesAfterJoin']}"


def assert_picking(picking: dict) -> None:
    assert picking.get("error") is None, picking
    assert picking["startLabel"] == "Add line", picking["startLabel"]
    assert picking["startPressed"] == "false", picking["startPressed"]
    assert picking["drawingLabel"].startswith("Finish line"), picking["drawingLabel"]
    assert picking["drawingPressed"] == "true", picking["drawingPressed"]
    assert picking["mapHasPicking"] is True, "the crosshair must follow the line"
    assert picking["draftHandles"] == 2, (
        f"two taps must leave two draft handles, got {picking['draftHandles']}"
    )
    assert picking["afterFinishLabel"] == "Add line", picking["afterFinishLabel"]
    assert picking["afterFinishPressed"] == "false", picking["afterFinishPressed"]
    assert picking["draftHandlesAfter"] == 0, picking["draftHandlesAfter"]
    assert (
        picking["lineCount"] == picking["lineCountBefore"] + 1
    ), f"the double-click must commit one line, got {picking}"
    assert picking["selectionShown"] is True, (
        "finishing must return to selection, so the new line can be picked"
    )
    assert picking["selectedHandles"] == 3, (
        "the double-click must keep the vertex it placed, not remove it: "
        f"got {picking['selectedHandles']} handles"
    )
    assert (
        picking["mapTopAfterSelect"] == picking["mapTopBeforeSelect"]
    ), "opening the selection panel must not move the radargram (#239)"
    assert picking["selectionWithinMap"] is True, (
        "the selection panel must overlay the map, not sit past its edge"
    )
    assert picking["selectionHelpOpen"] is True, (
        "the editing help must start expanded on a wide screen"
    )


def assert_duplicates(duplicates: dict) -> None:
    assert duplicates.get("error") is None, duplicates
    assert duplicates["duplicateMarkers"] == 2, (
        "both vertices sharing a trace must be marked, got "
        f"{duplicates['duplicateMarkers']}"
    )
    assert duplicates["overhangMarkers"] == 0, (
        "neither line doubles back, so no overhang may be marked: "
        f"{duplicates['overhangMarkers']}"
    )
    assert duplicates["toastHiddenBefore"] is True, duplicates
    assert duplicates["reasonShown"] is True, (
        "clicking a duplicate marker must show its reason -- hover is "
        "unreachable on a touch screen (#238)"
    )
    assert "Glacier bed" in duplicates["reason"], duplicates["reason"]
    assert "2 values" in duplicates["reason"], duplicates["reason"]


def assert_overlap(overlap: dict) -> None:
    assert overlap.get("error") is None, overlap
    assert overlap["bands"] == 2, (
        "a pair of overlapping lines needs a band on each, got "
        f"{overlap['bands']}"
    )
    assert overlap["markers"] == 1, (
        f"one pair means one reason marker, got {overlap['markers']}"
    )
    assert overlap["duplicateMarkers"] == 0, (
        "no vertex is shared, so this must not be marked as a duplicate: "
        f"{overlap['duplicateMarkers']}"
    )
    assert overlap["reasonShown"] is True, (
        "clicking an overlap marker must show its reason"
    )
    assert "same traces" in overlap["reason"], overlap["reason"]
    assert overlap["draftLabelBeforeFinish"].startswith("Finish line"), overlap[
        "draftLabelBeforeFinish"
    ]
    assert overlap["refusedLabel"].startswith("Finish line"), (
        "a refused finish must leave the draft in progress, got "
        f"{overlap['refusedLabel']!r}"
    )
    assert (
        overlap["lineCountAfter"] == overlap["lineCountBefore"]
    ), "an overlapping line must not be committed"
    assert "already has one" in overlap["refusedReason"], overlap["refusedReason"]
    assert overlap["grandfatherEnds"] == 2, overlap["grandfatherEnds"]
    assert overlap["grandfathered"] is True, (
        "an already-overlapping line must stay editable, or it can only be "
        "fixed by deleting it"
    )
    assert overlap["panelToastOverlap"] is False, (
        "the error toast and the selection panel must not share space"
    )


def assert_narrow(narrow: dict) -> None:
    assert narrow.get("error") is None, narrow
    assert narrow["selectionHelpOpen"] is False, (
        "the editing help must start collapsed on a narrow screen"
    )
    assert narrow["bottomControlsOverlap"] is False, (
        "the error toast and the selection panel must not share space on a "
        "narrow screen either"
    )
    assert narrow["panelFits"] is True, narrow
    assert narrow["panelWithinMap"] is True, "the panel must not run past the map"
    assert narrow["bodyScrollable"] is True, "the panel body must scroll, not clip"
    assert narrow["bodyScrolled"] is True, "horizontal touch scrolling must work"
    assert narrow["bodyScrollableV"] is True, "tall content must scroll vertically"
    assert narrow["bodyScrolledV"] is True, "vertical touch scrolling must work"


def assert_s1(operator: dict) -> None:
    assert operator["panelOpenOnLoad"] is False, "the panel must start closed"
    assert operator["rowHiddenWhenClosed"] is True, "closed rows must not be visible"
    assert operator["rowShownWhenOpen"] is True, "opening must reveal the rows"
    assert operator["rowHiddenAfterClose"] is True, "closing must hide them again"


def assert_manage(operator: dict) -> None:
    prefill = operator["editPrefill"]
    assert prefill["id"] == "dep_child", prefill
    assert prefill["idReadOnly"] is True, "an existing id must be read-only"
    assert "dep_base" in prefill["expression"], prefill
    assert "+ 2.0" in operator["savedExpression"], operator["savedExpression"]
    assert operator["colourStroke"] > 0, "the colour must reach the drawn line"
    assert "18, 52, 86" in operator["colourSwatch"], operator["colourSwatch"]
    assert operator["rangeFill"] > 0, "a range set in the editor must draw a fill"
    assert "hex" in operator["nonHexStatus"], operator["nonHexStatus"]
    assert operator["pickerMatchesItem"] is True, (
        "the colour picker must open on the item's own colour, not the one "
        "left by the previously edited item"
    )
    assert (
        operator["colourDisabledAfterCheck"] is True
    ), "ticking 'No colour' must disable the colour inputs"
    assert (
        operator["colourClearedInStore"] is None
    ), f"saving with 'No colour' must clear it, got {operator['colourClearedInStore']!r}"
    assert (
        operator["colourDisabledOnReopen"] is True
    ), "an item with no colour must reopen with the inputs disabled"
    assert (
        operator["pickerResetWhenColourless"] == "#ffcc00"
    ), f"a colourless item must reset the picker, got {operator['pickerResetWhenColourless']!r}"
    assert operator["colourDisabledAfterUncheck"] is False, (
        "unticking 'No colour' must re-enable the inputs -- this is #228, and "
        "it fails without a change listener on the checkbox"
    )
    assert (
        operator["colourSeededFromPicker"] == "#ffcc00"
    ), f"unticking must seed the text field from the picker, got {operator['colourSeededFromPicker']!r}"
    assert operator["colourRestoredInStore"] == "#ffcc00", (
        "a colourless item must be givable a colour again, got "
        f"{operator['colourRestoredInStore']!r}"
    )
    assert "dep_child" in operator["deleteRefused"], operator["deleteRefused"]
    assert operator["deletedGone"] is True, "a deleted item must stay gone"
    assert operator["deletedRowGone"] is True, "its row must be gone too"


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
        # Before `manage`, which deletes a dependent item; the /layers used-by
        # counter is checked while the dependency still exists.
        layers = run("op", "layers")
        groups = run("op", "groups")
        # #247: read the stored document the UI just wrote, so the conflict
        # outcomes are asserted against what is persisted, not what the DOM
        # happens to show.
        groups["stored"] = json.loads(
            (workdir / "ridal_data" / "layers" / "layers.json").read_text()
        )
        operator.update(run("op", "manage"))
        picker = run("picker", "panel")
        narrow = run("op", "narrow")
        refresh = run("op", "refresh")
        picking = run("op", "picking")
        split = run("op", "split")
        # #238: a duplicate document, written straight to disk because the
        # save route refuses one. Written just before the mode that reads it,
        # since the refresh and split modes rewrite op's picks.
        duplicate_path = (
            workdir / "ridal_data" / "interpretations" / RADARGRAM / "op.gprinterp.json"
        )
        duplicate_path.write_text(
            json.dumps(
                {
                    "key": RADARGRAM,
                    "source": {"revision_id": revision},
                    "features": [
                        {
                            "type": "Feature",
                            "geometry": {
                                "type": "LineString",
                                "coordinates": [[10.0, 20.0], [50.0, 30.0]],
                            },
                            "properties": {"id": "f-dup-a", "label": "bed"},
                        },
                        {
                            "type": "Feature",
                            "geometry": {
                                "type": "LineString",
                                "coordinates": [[50.0, 60.0], [90.0, 40.0]],
                            },
                            "properties": {"id": "f-dup-b", "label": "bed"},
                        },
                    ],
                },
                indent=2,
            )
        )
        duplicates = run("op", "duplicates")
        # #207: two lines covering the same traces, also written straight to
        # disk because the save route refuses it.
        duplicate_path.write_text(
            json.dumps(
                {
                    "key": RADARGRAM,
                    "source": {"revision_id": revision},
                    "features": [
                        {
                            "type": "Feature",
                            "geometry": {
                                "type": "LineString",
                                "coordinates": [[0.0, 50.0], [99.0, 60.0]],
                            },
                            "properties": {"id": "f-ov-a", "label": "bed"},
                        },
                        {
                            "type": "Feature",
                            "geometry": {
                                "type": "LineString",
                                "coordinates": [[30.0, 150.0], [90.0, 160.0]],
                            },
                            "properties": {"id": "f-ov-b", "label": "bed"},
                        },
                    ],
                },
                indent=2,
            )
        )
        overlap = run("op", "overlap")

        external = [path for path in all_requests if "://" in path]
        print(
            json.dumps(
                {
                    "operator": operator,
                    "picker": picker,
                    "layers": layers,
                    "groups": groups,
                    "narrow": narrow,
                    "refresh": refresh,
                    "picking": picking,
                    "split": split,
                    "duplicates": duplicates,
                    "overlap": overlap,
                },
                indent=2,
            )
        )
        print(f"proxy saw {len(all_requests)} requests; external: {external}")

        assert_q1(operator, picker)
        assert_q2(operator, picker)
        assert_q3(operator)
        assert_s1(operator)
        assert_manage(operator)
        assert_layers(layers)
        assert_groups(groups)
        assert_narrow(narrow)
        assert_refresh(refresh)
        assert_picking(picking)
        assert_split(split)
        assert_duplicates(duplicates)
        assert_overlap(overlap)
        assert not external, external
        print("PANEL HARNESS: all assertions passed")
    finally:
        server.terminate()
        server.wait(timeout=10)
        if not args.keep:
            shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    main()
