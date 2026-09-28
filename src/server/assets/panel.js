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
  //
  // Past either end the axis is continued from its outermost segment that is
  // not flat, exactly as `axis_invert` in `interp::derive` does (#270). A
  // derived layer may lie outside the radargram -- `median(bed) + 5.0` for a
  // known error in time zero -- and clamping it here drew it along the top or
  // bottom edge, where it looked like a real line on the data.

  /** The axis's change per sample at one end, or null if it is all flat. */
  function edgeSlope(axis, atStart) {
    const last = axis.length - 1;
    for (let step = 0; step < last; step++) {
      const i = atStart ? step : last - 1 - step;
      const slope = axis[i + 1] - axis[i];
      if (slope !== 0) return slope;
    }
    return null;
  }

  function invertAxis(axis, value) {
    if (!axis || axis.length < 2 || !Number.isFinite(value)) return null;
    // Non-decreasing axes only; depth and twtt both are.
    const last = axis.length - 1;
    if (value < axis[0]) {
      const slope = edgeSlope(axis, true);
      return slope === null ? null : (value - axis[0]) / slope;
    }
    if (value > axis[last]) {
      const slope = edgeSlope(axis, false);
      return slope === null ? null : last + (value - axis[last]) / slope;
    }
    let index = 0;
    while (index < axis.length && axis[index] < value) {
      index += 1;
    }
    if (index === 0) return 0;
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

  /** Draw one document's lines into `target`, honouring layer visibility.
   *
   * The parameter is `source`, not `document`: a parameter named `document`
   * shadows the global and made `document.createTextNode` below throw *after*
   * the line had been added to the map but *before* it was recorded in
   * `target`, so it could never be removed again. */
  function drawDocument(source, target, options) {
    const features = (source && source.features) || [];
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
        // A stable class so the harness can count these independently of the
        // picker's own editable lines.
        className: options.className || null,
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
      .forEach((entry) =>
        drawDocument(entry.document, ownLines, { className: "own-pick-line" }),
      );
  }

  /** How many people have picked at least one line here, the caller
   * included: the same number the catalog card gives (#272). */
  function contributorCount() {
    return state.documents.filter((entry) =>
      ((entry.document && entry.document.features) || []).some(
        (feature) => feature.geometry && feature.geometry.type === "LineString",
      ),
    ).length;
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
          className: "contributor-pick-line",
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
          // Stable per-item class, so the harness can follow one derived line
          // across redraws.
          className: `derived-line derived-line-${item.id}`,
        }).addTo(map),
      );
    });
  }

  async function refreshDerivedLines() {
    derivedLines.forEach((layers) => clearList(layers));
    derivedLines.clear();
    for (const item of state.items) {
      if (item.kind !== "layer") continue;
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
      if (item.kind !== "layer") continue;
      // A fill is drawn only while *both* its bounds are shown. Hiding either
      // one removes it: a band between a visible line and one the viewer has
      // switched off is a shape with no visible edges, and reading it as a
      // range is guesswork. This also covers a bound the caller may not see at
      // all -- it is absent from `state.items`, so no fill is drawn and its
      // position cannot leak.
      if (state.itemVisible.get(item.id) === false) continue;
      const target = state.items.find((i) => i.id === item.fill_to.target);
      const targetVisible = target
        ? state.itemVisible.get(target.id) !== false
        : state.layerVisible.get(item.fill_to.target) !== false;
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

  /* Redraw the derived lines and fills at the current geometry.
   *
   * Published for viewer.js: toggling the topographic correction changes
   * `window.RIDAL_GEOMETRY.shift`, and the existing lines were drawn with the
   * old one, so they sit in the wrong place until something redraws them.
   * picker.js is redrawn on the same event through `RIDAL_REDRAW_PICKS`.
   *
   * `force` clears the cached per-item values first. A geometry change keeps
   * the values and only moves them, but a *saved pick* changes the values
   * themselves, and without this the cached ones would be redrawn unchanged. */
  window.RIDAL_REDRAW_DERIVED = (force) => {
    if (force) state.values.clear();
    refreshDerivedLines().then(refreshFills).then(refreshAttributes);
  };

  // --- Attributes in the cursor readout (#280) -----------------------------
  //
  // An attribute is a number per trace with no line to draw, so the readout
  // is where it is seen: `count(bed)` as "Bed pickers: 4" under the cursor.
  // Every listed attribute is shown. Listing is the author's decision to show
  // it, and unticking "listed" takes it out again.

  /** Listed attributes, in the stored order. */
  function readoutAttributes() {
    return state.items.filter(
      (item) => item.kind === "attribute" && item.listed !== false,
    );
  }

  /** Fetch the values of every listed attribute into the shared cache, so
   * the readout can look them up synchronously on every mouse move. */
  async function refreshAttributes() {
    for (const item of readoutAttributes()) {
      try {
        await loadItemValues(item.id);
      } catch (error) {
        console.warn(`Could not load derived item '${item.id}': ${error.message}`);
      }
    }
  }

  const UNIT_SUFFIX = { meters: " m", nanoseconds: " ns", samples: " samples" };

  /** One attribute value as the readout shows it. A length or time is given
   * to one decimal like the readout's own depth; a count stays whole. */
  function formatAttribute(value, unit) {
    if (value === null || value === undefined || !Number.isFinite(value)) return "–";
    let text;
    if (unit === "meters" || unit === "nanoseconds") text = value.toFixed(1);
    else if (Number.isInteger(value)) text = String(value);
    else text = value.toFixed(2);
    return text + (UNIT_SUFFIX[unit] || "");
  }

  /* The readout terms at a (fractional) trace, e.g. ["Bed pickers: 4"].
   *
   * Published for viewer.js, which owns the readout. Reads only the cache:
   * an attribute whose values have not arrived yet, or failed, is left out
   * rather than stalling the readout. */
  window.RIDAL_ATTRIBUTE_READOUT = (traceIndex) => {
    const terms = [];
    for (const item of readoutAttributes()) {
      const body = state.values.get(item.id);
      if (!body || !Array.isArray(body.values) || !body.values.length) continue;
      const trace = Math.min(
        body.values.length - 1,
        Math.max(0, Math.round(traceIndex)),
      );
      terms.push(`${item.name || item.id}: ${formatAttribute(body.values[trace], body.unit)}`);
    }
    return terms;
  };

  // --- The panel control ---------------------------------------------------
  //
  // A `<details>` disclosure, closed by default. `<details>` opens and closes
  // with no JavaScript and is keyboard-accessible for free -- the same pattern
  // the header menu uses, including folding away on a click outside it (see
  // the listener below). Folding is the only way the panel gets out of the
  // way: there is deliberately no second control that hides it outright.

  function checkbox(container, { checked, label, title, onChange, swatch = true }) {
    const row = document.createElement("label");
    row.className = "layer-panel-row";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = checked;
    if (title) input.title = title;
    input.addEventListener("change", () => onChange(input.checked));
    row.appendChild(input);
    // A control with no colour of its own (the contributor toggle) gets no
    // swatch; an empty box beside it reads as a colour that failed to load.
    if (swatch) {
      const chip = document.createElement("span");
      chip.className = "layer-panel-swatch";
      if (label.color) chip.style.background = label.color;
      row.appendChild(chip);
    }
    const text = document.createElement("span");
    text.textContent = label.text;
    row.appendChild(text);
    container.appendChild(row);
    return input;
  }

  const LayerPanel = L.Control.extend({
    options: { position: "topright" },
    onAdd() {
      const details = L.DomUtil.create("details", "leaflet-bar layer-panel");
      details.id = "layer-panel";
      const summary = document.createElement("summary");
      summary.id = "layer-panel-summary";
      summary.textContent = "Layers";
      const body = document.createElement("div");
      body.className = "layer-panel-body";
      details.append(summary, body);
      L.DomEvent.disableClickPropagation(details);
      L.DomEvent.disableScrollPropagation(details);
      this._container = details;
      this._body = body;
      return details;
    },
  });
  const panelControl = new LayerPanel();
  map.addControl(panelControl);

  /* Cap the body to the map's height, not the viewport's.
   *
   * On a phone in portrait the layout stacks the overview map below, so the
   * map pane is shorter than `60vh`: a viewport-relative cap let the panel
   * run past the bottom of the map, where the map clipped it and the hidden
   * part could not be scrolled to. In landscape the map is shorter still but
   * `60vh` happened to fit, which is why only portrait looked broken. */
  function fitPanelToMap() {
    const body = panelControl._body;
    if (!body) return;
    const height = map.getSize().y;
    body.style.maxHeight = `${Math.max(120, height - 96)}px`;
  }
  map.on("resize", fitPanelToMap);
  fitPanelToMap();

  /* Close the disclosure on a click anywhere else.
   *
   * Phase 3 deliberately did the opposite, so that a map click would not fold
   * the panel mid-task; Erik has since asked for the menu to close on any
   * click in the viewer, which is the usual menu behaviour and keeps a long
   * panel from sitting over the radargram. A capture-phase listener is used
   * because `L.DomEvent.disableClickPropagation` stops clicks from inside the
   * panel reaching the document, and `contains` is still correct there. */
  document.addEventListener(
    "click",
    (event) => {
      const container = panelControl._container;
      if (container && container.open && !container.contains(event.target)) {
        container.open = false;
      }
    },
    true,
  );

  function derivedLabel(item) {
    // The label says when a project-wide definition is being read as a
    // personal number. Silently showing an OwnPicks evaluation under a name
    // like "Consensus" is the one failure nobody can see.
    const personal = item.audience === "own_picks" && !state.canSeeOthers;
    return item.name + (personal ? " (your picks)" : "");
  }

  /** Whether the server could not infer this item's kind: its expression
   * does not parse, or names a layer or item that no longer works (#271). */
  function derivedBroken(item) {
    return typeof item.kind === "string" && item.kind.startsWith("error");
  }

  /** One derived *layer*'s row. Only positions reach here: an attribute has
   * no line to draw, so it is not shown in the viewer's panel at all -- it
   * belongs on the /layers page. A broken item is shown too, marked and
   * unselectable, so it can be fixed or deleted from here. */
  function derivedRow(item, container) {
    const broken = derivedBroken(item);
    const row = document.createElement("div");
    row.className = "layer-panel-row layer-panel-derived-row";
    if (broken) row.classList.add("layer-panel-derived-broken");
    row.dataset.derivedId = item.id;

    const label = document.createElement("label");
    label.className = "layer-panel-toggle";
    // The server's message says what is wrong, which the expression alone
    // cannot.
    label.title = broken
      ? `${item.expression}\n${item.kind}`
      : item.expression;
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = !broken && state.itemVisible.get(item.id) === true;
    input.disabled = broken;
    input.addEventListener("change", () => {
      state.itemVisible.set(item.id, input.checked);
      refreshDerivedLines().then(refreshFills);
    });
    const swatch = document.createElement("span");
    swatch.className = "layer-panel-swatch";
    if (item.color) swatch.style.background = item.color;
    const text = document.createElement("span");
    text.textContent = derivedLabel(item) + (broken ? " (invalid)" : "");
    label.append(input, swatch, text);
    row.appendChild(label);

    if (state.canAuthor) {
      const actions = document.createElement("span");
      // `.row-actions` is the shared table-row button style, so Edit and
      // Delete read as one set here and on the /layers page.
      actions.className = "row-actions layer-panel-actions";
      const edit = document.createElement("button");
      edit.type = "button";
      edit.className = "layer-panel-edit";
      edit.textContent = "Edit";
      edit.title = `Edit '${item.name || item.id}'`;
      edit.addEventListener("click", () => openEditor(item));
      const remove = document.createElement("button");
      remove.type = "button";
      // `.danger` is the shared row-action style, so Edit and Delete match
      // the /layers page (and differ only in colour).
      remove.className = "danger";
      remove.textContent = "Delete";
      remove.title = `Delete '${item.name || item.id}'`;
      remove.addEventListener("click", () => confirmDelete(item, actions));
      actions.append(edit, remove);
      row.appendChild(actions);
    }
    container.appendChild(row);
  }

  /** Inline delete confirmation. Names the item, and does not use
   * `window.confirm`, which blocks the Chromium harness and reads as a
   * browser dialog rather than part of the panel. */
  function confirmDelete(item, actions) {
    actions.replaceChildren();
    const prompt = document.createElement("span");
    prompt.className = "layer-panel-confirm";
    prompt.textContent = `Delete '${item.name || item.id}'?`;
    const yes = document.createElement("button");
    yes.type = "button";
    yes.className = "danger";
    yes.textContent = "Delete";
    yes.addEventListener("click", () => deleteItem(item));
    const no = document.createElement("button");
    no.type = "button";
    no.textContent = "Cancel";
    no.addEventListener("click", renderPanel);
    prompt.append(yes, no);
    actions.appendChild(prompt);
  }

  function renderPanel() {
    const body = panelControl._body;
    if (!body) return;
    body.replaceChildren();

    const error = document.createElement("div");
    error.id = "layer-panel-error";
    error.className = "layer-panel-error";
    error.hidden = true;
    body.appendChild(error);

    state.layers.forEach((layer) => {
      checkbox(body, {
        checked: state.layerVisible.get(layer.id) !== false,
        // The drawn colour, which falls back to the viewer default, so the
        // swatch is never an empty box for a layer with no stored colour.
        label: { text: layer.name || layer.id, color: layerColor(layer.id) },
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

    // Only a position is a drawable derived layer, and only a *listed* one
    // belongs in this panel. An attribute (a number per position) and an
    // intermediate layer marked "not listed" are managed on the /layers page.
    const derivedLayers = state.items.filter(
      (item) =>
        (item.kind === "layer" || derivedBroken(item)) && item.listed !== false,
    );
    if (derivedLayers.length || state.canAuthor) {
      const derivedHeading = document.createElement("div");
      derivedHeading.className = "layer-panel-heading";
      derivedHeading.textContent = "Derived layers";
      body.appendChild(derivedHeading);
      derivedLayers.forEach((item) => derivedRow(item, body));
      // The create button belongs with the section it adds to, not after the
      // contributor toggle.
      if (state.canAuthor) {
        const add = document.createElement("button");
        add.type = "button";
        add.id = "derived-new";
        add.textContent = "New expression";
        add.addEventListener("click", () => openEditor(null));
        body.appendChild(add);
      }
    }

    if (state.canSeeOthers) {
      const contributors = document.createElement("div");
      contributors.className = "layer-panel-heading";
      contributors.textContent = "Contributors";
      body.appendChild(contributors);
      checkbox(body, {
        checked: state.showContributors,
        label: { text: `Show all contributors (n=${contributorCount()})` },
        // No colour of its own: no swatch.
        swatch: false,
        onChange: (visible) => {
          state.showContributors = visible;
          drawContributors();
        },
      });
    }

    const summary = document.getElementById("layer-panel-summary");
    if (summary) {
      // Exactly the number of layer rows the panel shows: picked layers plus
      // listed derived layers. The contributor toggle is not a layer.
      const total = state.layers.length + derivedLayers.length;
      summary.textContent = `Layers (${total})`;
    }
  }

  // --- The shared expression editor ----------------------------------------
  //
  // The dialog, its save path and the highlighting live in `RIDAL.derivedEditor`
  // (app.js) so the /layers page uses exactly the same one. The viewer adds the
  // one thing /layers cannot: a live preview line, which needs a radargram.

  function clearPreviewLine() {
    if (previewLine) {
      map.removeLayer(previewLine);
      previewLine = null;
    }
  }

  async function previewExpression(expression, unit) {
    clearPreviewLine();
    const body = await RIDAL.fetchJson(
      RIDAL.apiPath("datasets", RADARGRAM, "derived", "preview"),
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ expression, unit }),
      },
    );
    if (body.kind !== "layer") return body;
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
    if (points.length) {
      previewLine = L.layerGroup(
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
    }
    return body;
  }

  function openEditor(item) {
    RIDAL.derivedEditor.open({
      item,
      items: state.items,
      layerIds: state.layers.map((layer) => layer.id),
      unusable: state.unusable,
      canRelease: state.canRelease,
      preview: previewExpression,
      onSaved: loadDerived,
      onClose: clearPreviewLine,
    });
  }

  /** Delete one item, as a PUT that omits it.
   *
   * The server merges (it preserves items the caller cannot see) and refuses
   * a delete another item depends on, naming the dependent; its message is
   * shown as-is. Confirmation is inline rather than `window.confirm`, which
   * would block the Chromium harness and cannot name the item as legibly. */
  async function deleteItem(item) {
    const next = state.items
      .filter((existing) => existing.id !== item.id)
      .map((existing) => ({
        id: existing.id,
        name: existing.name,
        expression: existing.expression,
        unit: existing.unit,
        color: existing.color,
        show: existing.show,
        listed: existing.listed,
        fill_to: existing.fill_to,
        scope: existing.scope,
        audience: existing.audience,
      }));
    try {
      await RIDAL.fetchJson(RIDAL.apiPath("derived"), {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          schema: "ridal-derived",
          schema_version: "1",
          items: next,
        }),
      });
    } catch (error) {
      showPanelError(error.message);
      return;
    }
    await loadDerived();
  }

  function showPanelError(message) {
    const box = document.getElementById("layer-panel-error");
    if (!box) return;
    box.textContent = message;
    box.hidden = false;
  }

  // --- Load ----------------------------------------------------------------

  async function loadDerived() {
    try {
      const body = await RIDAL.fetchJson(RIDAL.apiPath("derived"));
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
    renderPanel();
    await refreshDerivedLines();
    await refreshFills();
    await refreshAttributes();
  }

  async function load() {
    try {
      const layersBody = await RIDAL.fetchJson(RIDAL.apiPath("layers"));
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
