/* The layer panel, contributor overlays and range fills (#209).
 *
 * First-party, embedded via assets.rs, loaded after picker.js. Wrapped in an
 * IIFE for the same reason picker.js is: classic scripts share one global
 * scope, and a top-level `const CFG` here would collide with viewer.js's own
 * and kill this whole file silently.
 *
 * Built as a small `L.Control` subclass rather than `L.control.layers`. The
 * built-in is a flat checkbox list with no way to (a) group derived items
 * apart from the picked layers, (b) label an item "(your picks)" when the
 * caller is seeing their own data under a project-wide definition, or (c)
 * add the single "show all contributors" toggle the design asks for without
 * doubling every layer's row. `L.control.layers` was tried first; the
 * subclass is less code than working around it.
 *
 * It owns:
 *   - the layer list, one checkbox per vocabulary layer (on by default);
 *   - the derived-item list, off by default, hidden when the caller cannot
 *     see the item (`scope` is applied server-side before we ever see it);
 *   - a single "show all contributors" toggle, shown only when the server
 *     says the caller may see others (`can_see_others`) -- never inferred
 *     from a role string here;
 *   - drawing of derived lines and range fills;
 *   - read-only drawing of the caller's own picks for a non-writable viewer,
 *     and of other contributors' picks when the toggle is on. A writable
 *     caller's own lines belong to picker.js, which is asked to hide a layer
 *     rather than have a second copy drawn underneath it.
 *
 * The expression editor's live preview also lives here (Q3).
 */
(function () {
  "use strict";

  const CFG = window.RIDAL_VIEWER;
  const map = window.RIDAL_MAP;
  const toLatLng = window.RIDAL_TO_LATLNG;
  if (!map || !toLatLng) return;

  const RADARGRAM = CFG.radargramId;

  // Published for the Chromium harness, which otherwise sees only a timeout
  // when a fetch fails before the panel renders. Harmless in production.
  window.RIDAL_PANEL_ERRORS = [];
  const noteError = (error) => {
    window.RIDAL_PANEL_ERRORS.push(String(error && error.message ? error.message : error));
  };

  /** The built-ins the server registers on the evaluator, for autocomplete
   * and for keeping a generated id from shadowing one (#205). */
  const BUILTINS = [
    "count",
    "median",
    "mean",
    "std",
    "nmad",
    "percentile",
    "percentile_lower",
    "min",
    "max",
    "concatenate",
    "shallowest",
    "deepest",
    "clamp",
    "where",
  ];
  const KEYWORDS = ["if", "else", "true", "false", "NaN"];
  const DEFAULT_COLOR = "#ffcc00";

  const state = {
    layers: [],
    items: [],
    unusable: [],
    documents: [],
    canSeeOthers: false,
    canAuthor: false,
    canRelease: false,
    axes: null,
    /** layer id -> false when switched off. Absent means visible. */
    layerVisible: new Map(),
    /** derived item id -> false when switched off. Absent means the item's
     * own `show`, which is false by default. */
    itemVisible: new Map(),
    showContributors: false,
    /** item id -> the `/derived/{id}` body, cached until the set changes. */
    values: new Map(),
  };

  // --- Leaflet layer bookkeeping -------------------------------------------

  const ownLines = [];
  const contributorLines = [];
  const derivedLines = new Map();
  const fillShapes = [];
  let previewLine = null;

  const clearList = (list) => {
    list.forEach((layer) => map.removeLayer(layer));
    list.length = 0;
  };

  function layerFor(label) {
    return state.layers.find((layer) => layer.id === label);
  }

  function layerColor(label) {
    const layer = layerFor(label);
    return (layer && layer.color) || DEFAULT_COLOR;
  }

  // --- Axis inversion ------------------------------------------------------
  //
  // A derived item's values are in its own unit; the map wants a sample
  // index. `/axes` gives the same depth/twtt arrays the cursor readout uses.

  function invertAxis(axis, value) {
    if (!axis || axis.length < 2 || !Number.isFinite(value)) return null;
    const ascending = axis[axis.length - 1] >= axis[0];
    let index = 0;
    while (index < axis.length && (ascending ? axis[index] < value : axis[index] > value)) {
      index += 1;
    }
    if (index === 0) return 0;
    if (index >= axis.length) return axis.length - 1;
    const lo = index - 1;
    const span = axis[index] - axis[lo];
    return span === 0 ? lo : lo + (value - axis[lo]) / span;
  }

  /** A position value in its unit, as a sample index, or null if it cannot
   * be one (a gap, or a dimensionless unit that may not be a position). */
  function valueToSample(value, unit) {
    if (!Number.isFinite(value)) return null;
    if (unit === "samples") return value;
    if (unit === "meters") return invertAxis(state.axes && state.axes.depth, value);
    if (unit === "nanoseconds") return invertAxis(state.axes && state.axes.twtt, value);
    return null;
  }

  // --- Drawing -------------------------------------------------------------

  /** Draw one document's lines into `target`, honouring layer visibility. */
  function drawDocument(document, target, options) {
    const features = (document && document.features) || [];
    features.forEach((feature) => {
      if (!feature.geometry || feature.geometry.type !== "LineString") return;
      const label = feature.properties && feature.properties.label;
      if (state.layerVisible.get(label) === false) return;
      const coordinates = feature.geometry.coordinates;
      if (!coordinates || coordinates.length < 2) return;
      const points = coordinates.map(([trace, sample]) => toLatLng(trace, sample));
      const line = L.polyline(points, {
        color: layerColor(label),
        weight: options.weight || 3,
        opacity: options.opacity == null ? 0.85 : options.opacity,
        dashArray: options.dashArray || null,
        interactive: false,
        pane: "radargram-lines",
      }).addTo(map);
      if (options.tooltip) {
        line.bindTooltip(document.createTextNode(options.tooltip(label)));
      }
      target.push(line);
    });
  }

  /** The caller's own picks, for a viewer picker.js does not run for. */
  function drawOwn() {
    clearList(ownLines);
    if (CFG.writable) return;
    state.documents
      .filter((entry) => entry.own)
      .forEach((entry) => drawDocument(entry.document, ownLines, {}));
  }

  /** Other contributors' picks, when the toggle is on. */
  function drawContributors() {
    clearList(contributorLines);
    if (!state.showContributors || !state.canSeeOthers) return;
    state.documents
      .filter((entry) => !entry.own)
      .forEach((entry) => {
        drawDocument(entry.document, contributorLines, {
          weight: 2,
          opacity: 0.55,
          tooltip: (label) => `${entry.user} · ${label || "unlabelled"}`,
        });
      });
  }

  async function loadItemValues(id) {
    if (state.values.has(id)) return state.values.get(id);
    const body = await RIDAL.fetchJson(
      RIDAL.apiPath("datasets", RADARGRAM, "derived", id),
    );
    state.values.set(id, body);
    return body;
  }

  /** Draw a position item as one polyline per contiguous run of values. */
  function drawDerivedLine(item, body, target) {
    const points = [];
    let run = [];
    body.values.forEach((value, trace) => {
      const sample = valueToSample(value, body.unit);
      if (sample === null) {
        if (run.length > 1) points.push(run);
        run = [];
        return;
      }
      run.push(toLatLng(trace, sample));
    });
    if (run.length > 1) points.push(run);
    points.forEach((runPoints) => {
      target.push(
        L.polyline(runPoints, {
          color: item.color || layerColor(item.fill_to && item.fill_to.target) || "#000000",
          weight: 2,
          opacity: 0.9,
          interactive: false,
          pane: "radargram-lines",
        }).addTo(map),
      );
    });
  }

  async function refreshDerivedLines() {
    derivedLines.forEach((layers) => clearList(layers));
    derivedLines.clear();
    for (const item of state.items) {
      if (item.kind !== "position") continue;
      if (state.itemVisible.get(item.id) === false) continue;
      let body;
      try {
        body = await loadItemValues(item.id);
      } catch (error) {
        console.warn(`Could not load derived item '${item.id}': ${error.message}`);
        continue;
      }
      const target = [];
      drawDerivedLine(item, body, target);
      derivedLines.set(item.id, target);
    }
  }

  // --- Range fills ---------------------------------------------------------

  /** Split a pair of sample arrays into non-self-intersecting rings.
   *
   * One ring per contiguous run where both bounds are finite, and a new ring
   * wherever the two cross, so a fill can never render as a bowtie. A gap on
   * either side ends the run, which is what makes the fill break rather than
   * bridge.
   */
  function fillRings(aSamples, bSamples) {
    const rings = [];
    let ring = null;
    for (let i = 0; i < aSamples.length; i++) {
      const a = aSamples[i];
      const b = bSamples[i];
      if (!Number.isFinite(a) || !Number.isFinite(b)) {
        if (ring) rings.push(ring);
        ring = null;
        continue;
      }
      if (!ring) {
        ring = { traces: [i], a: [a], b: [b] };
        continue;
      }
      const prevA = ring.a[ring.a.length - 1];
      const prevB = ring.b[ring.b.length - 1];
      const prevDiff = prevA - prevB;
      const diff = a - b;
      const crosses = prevDiff !== 0 && diff !== 0 && prevDiff > 0 !== diff > 0;
      if (!crosses) {
        ring.traces.push(i);
        ring.a.push(a);
        ring.b.push(b);
        continue;
      }
      // Crossing between trace i-1 and i: close this ring at the crossing and
      // start the next one there, so both rings share the exact crossing
      // point and neither folds back on itself.
      const t = prevDiff / (prevDiff - diff);
      const crossingTrace = i - 1 + t;
      const crossingSample = prevA + t * (a - prevA);
      ring.traces.push(crossingTrace);
      ring.a.push(crossingSample);
      ring.b.push(crossingSample);
      rings.push(ring);
      ring = {
        traces: [crossingTrace, i],
        a: [crossingSample, a],
        b: [crossingSample, b],
      };
    }
    if (ring) rings.push(ring);
    return rings.filter((r) => r.traces.length > 1);
  }

  function drawFill(item, itemValues, targetValues) {
    const targetItem = state.items.find((i) => i.id === item.fill_to.target);
    const targetUnit = targetItem ? targetItem.unit : item.unit;
    const aSamples = itemValues.values.map((v) => valueToSample(v, itemValues.unit));
    const bSamples = targetValues.values.map((v) => valueToSample(v, targetUnit));
    const color =
      item.fill_to.color || item.color || layerColor(item.fill_to.target) || "#888888";
    const opacity =
      item.fill_to.opacity == null ? 0.25 : item.fill_to.opacity;
    fillRings(aSamples, bSamples).forEach((ring) => {
      const latlngs = ring.traces.map((trace, index) =>
        toLatLng(trace, ring.a[index]),
      );
      for (let index = ring.traces.length - 1; index >= 0; index--) {
        latlngs.push(toLatLng(ring.traces[index], ring.b[index]));
      }
      fillShapes.push(
        L.polygon(latlngs, {
          stroke: false,
          fill: true,
          fillColor: color,
          fillOpacity: opacity,
          interactive: false,
          pane: "radargram-fills",
        }).addTo(map),
      );
    });
  }

  async function refreshFills() {
    clearList(fillShapes);
    for (const item of state.items) {
      if (!item.fill_to) continue;
      if (item.kind !== "position") continue;
      // A fill is independent of its bound *lines*: it is drawn with both
      // toggled off (the whole point of a percentile band). What it does
      // depend on is the caller being able to see both bounds at all, which
      // is what the server already filtered `state.items` by -- a derived
      // target missing from it is one the caller may not see, and a fill
      // against it would disclose its position exactly.
      const target = state.items.find((i) => i.id === item.fill_to.target);
      const targetVisible = target
        ? true
        : Boolean(layerFor(item.fill_to.target));
      if (!targetVisible) continue;
      let itemValues;
      let targetValues;
      try {
        itemValues = await loadItemValues(item.id);
        targetValues = target ? await loadItemValues(target.id) : null;
      } catch (error) {
        console.warn(`Could not load a fill bound for '${item.id}': ${error.message}`);
        continue;
      }
      if (!targetValues) continue;
      drawFill(item, itemValues, targetValues);
    }
  }

  // --- The panel control ---------------------------------------------------

  function checkbox(container, { checked, label, title, onChange }) {
    const row = document.createElement("label");
    row.className = "layer-panel-row";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = checked;
    if (title) input.title = title;
    input.addEventListener("change", () => onChange(input.checked));
    const swatch = document.createElement("span");
    swatch.className = "layer-panel-swatch";
    if (label.color) swatch.style.background = label.color;
    const text = document.createElement("span");
    text.textContent = label.text;
    row.append(input, swatch, text);
    container.appendChild(row);
    return input;
  }

  const LayerPanel = L.Control.extend({
    options: { position: "topright" },
    onAdd() {
      const container = L.DomUtil.create("div", "leaflet-bar layer-panel");
      container.id = "layer-panel";
      L.DomEvent.disableClickPropagation(container);
      L.DomEvent.disableScrollPropagation(container);
      this._container = container;
      return container;
    },
  });
  const panelControl = new LayerPanel();
  map.addControl(panelControl);

  function renderPanel() {
    const container = panelControl._container;
    if (!container) return;
    container.replaceChildren();

    const heading = document.createElement("div");
    heading.className = "layer-panel-heading";
    heading.textContent = "Layers";
    container.appendChild(heading);

    state.layers.forEach((layer) => {
      checkbox(container, {
        checked: state.layerVisible.get(layer.id) !== false,
        label: { text: layer.name || layer.id, color: layer.color },
        onChange: (visible) => {
          state.layerVisible.set(layer.id, visible);
          if (CFG.writable && window.RIDAL_SET_LAYER_VISIBLE) {
            window.RIDAL_SET_LAYER_VISIBLE(layer.id, visible);
          } else {
            drawOwn();
          }
          refreshFills();
        },
      });
    });

    if (state.items.length) {
      const derivedHeading = document.createElement("div");
      derivedHeading.className = "layer-panel-heading";
      derivedHeading.textContent = "Derived";
      container.appendChild(derivedHeading);
      state.items.forEach((item) => {
        // The label says when a project-wide definition is being read as a
        // personal number. Silently showing an OwnPicks evaluation under a
        // name like "Consensus" is the one failure nobody can see.
        const personal = item.audience === "own_picks" && !state.canSeeOthers;
        const text = item.name + (personal ? " (your picks)" : "");
        checkbox(container, {
          checked: state.itemVisible.get(item.id) === true,
          label: { text, color: item.color },
          title:
            item.kind === "position"
              ? item.expression
              : `${item.kind}: ${item.expression}`,
          onChange: (visible) => {
            state.itemVisible.set(item.id, visible);
            refreshDerivedLines().then(refreshFills);
          },
        });
      });
    }

    if (state.canSeeOthers) {
      const contributors = document.createElement("div");
      contributors.className = "layer-panel-heading";
      contributors.textContent = "Contributors";
      container.appendChild(contributors);
      checkbox(container, {
        checked: state.showContributors,
        label: { text: "Show all contributors" },
        onChange: (visible) => {
          state.showContributors = visible;
          drawContributors();
        },
      });
    }

    if (state.canAuthor) {
      const edit = document.createElement("button");
      edit.type = "button";
      edit.id = "derived-new";
      edit.textContent = "New expression";
      edit.addEventListener("click", () => openEditor(null));
      container.appendChild(edit);
    }
  }

  // --- The expression editor (Q3) ------------------------------------------

  const editor = document.getElementById("derived-editor");
  const editorTitle = document.getElementById("derived-editor-title");
  const nameInput = document.getElementById("derived-name");
  const idInput = document.getElementById("derived-id");
  const unitSelect = document.getElementById("derived-unit");
  const expressionInput = document.getElementById("derived-expression");
  const highlightBox = document.getElementById("derived-highlight");
  const statusBox = document.getElementById("derived-status");
  const unusableBox = document.getElementById("derived-unusable");
  const suggestions = document.getElementById("derived-suggestions");
  const saveButton = document.getElementById("derived-save");
  const cancelButton = document.getElementById("derived-cancel");
  let editingId = null;
  let previewTimer = null;

  function sanitizeId(name) {
    const translit = { ø: "o", å: "a", ä: "a", ö: "o", æ: "ae", é: "e" };
    let out = "";
    let lastSep = false;
    for (const character of name.toLowerCase()) {
      if (translit[character] !== undefined) {
        out += translit[character];
        lastSep = false;
      } else if (/[a-z0-9]/.test(character)) {
        out += character;
        lastSep = false;
      } else if (/[\x00-\x7f]/.test(character) && !lastSep && out) {
        out += "_";
        lastSep = true;
      }
    }
    out = out.replace(/_+$/, "");
    if (!out || /^[0-9]/.test(out)) out = `l_${out}`;
    if (BUILTINS.includes(out) || KEYWORDS.includes(out)) out += "_layer";
    return out;
  }

  function highlight(expression) {
    const token = /([A-Za-z_][A-Za-z0-9_]*)|(\d+(?:\.\d+)?)|([+\-*/<>=!]+)|([()[\]{},])|(\s+)|(.)/g;
    let out = "";
    let match;
    while ((match = token.exec(expression)) !== null) {
      const [text, identifier, number, operator] = match;
      if (identifier) {
        const kind = BUILTINS.includes(identifier)
          ? "tok-builtin"
          : KEYWORDS.includes(identifier)
            ? "tok-keyword"
            : "tok-layer";
        out += `<span class="${kind}">${RIDAL.escapeHtml(identifier)}</span>`;
      } else if (number) {
        out += `<span class="tok-number">${RIDAL.escapeHtml(number)}</span>`;
      } else if (operator) {
        out += `<span class="tok-op">${RIDAL.escapeHtml(text)}</span>`;
      } else {
        out += RIDAL.escapeHtml(text);
      }
    }
    return out;
  }

  function syncHighlight() {
    highlightBox.innerHTML = `${highlight(expressionInput.value)}\n`;
    highlightBox.scrollTop = expressionInput.scrollTop;
    highlightBox.scrollLeft = expressionInput.scrollLeft;
  }

  function clearPreview() {
    if (previewLine) {
      map.removeLayer(previewLine);
      previewLine = null;
    }
  }

  async function runPreview() {
    clearPreview();
    const expression = expressionInput.value.trim();
    if (!expression) {
      statusBox.textContent = "";
      statusBox.classList.remove("editor-error");
      return;
    }
    let body;
    try {
      body = await RIDAL.fetchJson(
        RIDAL.apiPath("datasets", RADARGRAM, "derived", "preview"),
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ expression, unit: unitSelect.value }),
        },
      );
    } catch (error) {
      // An invalid expression clears the previous preview rather than leaving
      // a stale line on screen pretending to be the current one.
      statusBox.textContent = error.message;
      statusBox.classList.add("editor-error");
      return;
    }
    statusBox.classList.remove("editor-error");
    const kindLabel = body.kind === "position" ? "position (a line)" : body.kind;
    statusBox.textContent = `${kindLabel} · ${body.unit || "no unit"}`;
    if (body.kind !== "position") return;
    const points = [];
    let run = [];
    body.values.forEach((value, trace) => {
      const sample = valueToSample(value, body.unit);
      if (sample === null) {
        if (run.length > 1) points.push(run);
        run = [];
        return;
      }
      run.push(toLatLng(trace, sample));
    });
    if (run.length > 1) points.push(run);
    if (!points.length) return;
    const group = L.layerGroup(
      points.map((runPoints) =>
        L.polyline(runPoints, {
          color: "#00e5ff",
          weight: 3,
          dashArray: "8 5",
          interactive: false,
          pane: "radargram-lines",
        }),
      ),
    ).addTo(map);
    previewLine = group;
  }

  function schedulePreview() {
    if (previewTimer) clearTimeout(previewTimer);
    previewTimer = setTimeout(runPreview, 300);
  }

  function openEditor(item) {
    editingId = item ? item.id : null;
    editorTitle.textContent = item ? `Edit '${item.name}'` : "New derived expression";
    nameInput.value = item ? item.name : "";
    idInput.value = item ? item.id : "";
    unitSelect.value = item ? item.unit : "meters";
    expressionInput.value = item ? item.expression : "";
    statusBox.textContent = "";
    statusBox.classList.remove("editor-error");
    unusableBox.hidden = state.unusable.length === 0;
    unusableBox.textContent = state.unusable.length
      ? `These layer ids cannot be used in an expression (a hyphen parses as a minus): ${state.unusable.join(", ")}.`
      : "";
    syncHighlight();
    if (typeof editor.showModal === "function") editor.showModal();
    else editor.setAttribute("open", "");
    schedulePreview();
  }

  function closeEditor() {
    clearPreview();
    if (typeof editor.close === "function") editor.close();
    else editor.removeAttribute("open");
  }

  function editorItem() {
    const name = nameInput.value.trim();
    const id = (idInput.value.trim() || sanitizeId(name)).trim();
    return {
      id,
      name: name || id,
      expression: expressionInput.value.trim(),
      unit: unitSelect.value,
      color: null,
      show: false,
      fill_to: null,
      scope: "project",
      audience: "own_picks",
    };
  }

  async function saveEditor() {
    const item = editorItem();
    if (!item.expression) {
      statusBox.textContent = "The expression is empty.";
      statusBox.classList.add("editor-error");
      return;
    }
    const index = state.items.findIndex((existing) => existing.id === editingId);
    const next = state.items.map((existing) => ({
      id: existing.id,
      name: existing.name,
      expression: existing.expression,
      unit: existing.unit,
      color: existing.color,
      show: existing.show,
      fill_to: existing.fill_to,
      scope: existing.scope,
      audience: existing.audience,
    }));
    if (index >= 0) next[index] = { ...next[index], ...item };
    else next.push(item);
    try {
      await RIDAL.fetchJson("/api/v1/derived", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ schema: "ridal-derived", schema_version: "1", items: next }),
      });
    } catch (error) {
      statusBox.textContent = error.message;
      statusBox.classList.add("editor-error");
      return;
    }
    closeEditor();
    await loadDerived();
  }

  function buildSuggestions() {
    const names = [
      ...state.layers.map((layer) => layer.id),
      ...state.items.map((item) => item.id),
      ...BUILTINS,
    ];
    suggestions.replaceChildren(
      ...names.map((name) => {
        const option = document.createElement("option");
        option.value = name;
        return option;
      }),
    );
  }

  nameInput.addEventListener("input", () => {
    if (!editingId) idInput.value = sanitizeId(nameInput.value);
  });
  expressionInput.addEventListener("input", () => {
    syncHighlight();
    schedulePreview();
  });
  expressionInput.addEventListener("scroll", syncHighlight);
  unitSelect.addEventListener("change", schedulePreview);
  saveButton.addEventListener("click", saveEditor);
  cancelButton.addEventListener("click", closeEditor);

  // --- Load ----------------------------------------------------------------

  async function loadDerived() {
    try {
      const body = await RIDAL.fetchJson("/api/v1/derived");
      state.items = body.items || [];
      state.unusable = body.layers_unusable_in_expressions || [];
      state.canAuthor = Boolean(body.can_author);
      state.canRelease = Boolean(body.can_release);
      state.values.clear();
      // A new expression is likelier to be wrong than the layers it is built
      // from, so an item starts off only if it says `show`.
      state.items.forEach((item) => {
        if (!state.itemVisible.has(item.id)) {
          state.itemVisible.set(item.id, Boolean(item.show));
        }
      });
    } catch (error) {
      console.warn(`Could not load derived items: ${error.message}`);
      noteError(error);
      state.items = [];
    }
    buildSuggestions();
    renderPanel();
    await refreshDerivedLines();
    await refreshFills();
  }

  async function load() {
    try {
      const layersBody = await RIDAL.fetchJson("/api/v1/layers");
      state.layers = layersBody.layers || [];
    } catch (error) {
      console.warn(`Could not load layers: ${error.message}`);
      noteError(error);
    }
    try {
      const contributors = await RIDAL.fetchJson(
        RIDAL.apiPath("datasets", RADARGRAM, "contributors"),
      );
      state.documents = contributors.documents || [];
      state.canSeeOthers = Boolean(contributors.can_see_others);
    } catch (error) {
      console.warn(`Could not load contributors: ${error.message}`);
      noteError(error);
    }
    try {
      state.axes = await RIDAL.fetchJson(
        RIDAL.apiPath("datasets", RADARGRAM, "axes"),
      );
    } catch (error) {
      console.warn(`Could not load axes: ${error.message}`);
      noteError(error);
    }
    drawOwn();
    await loadDerived();
  }

  load()
    .then(() => {
      window.RIDAL_PANEL_READY = true;
    })
    .catch((error) => {
      noteError(error);
      window.RIDAL_PANEL_READY = true;
    });
})();
