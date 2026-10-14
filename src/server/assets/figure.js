/* The figure dialog (#142): an annotated figure of part of this radargram.
 *
 * Loaded after viewer.js, panel.js and process.js on the viewer page, so the
 * map (`window.RIDAL_MAP`), the shared geometry (`window.RIDAL_GEOMETRY`) and
 * the page config (`window.RIDAL_VIEWER`) all exist. Everything is inside one
 * function, as classic scripts on one page share their top-level names.
 *
 * The dialog builds a figure spec, the same JSON `ridal render --figure`
 * takes, and asks `POST .../figure` for both the preview and the download.
 * The preview is the same spec at the preview's width, with the dpi scaled
 * by the same factor, so fonts, lines and margins keep their proportions:
 * what is previewed is the download, smaller.
 *
 * The extent is kept in the viewer's raster coordinates, trace and row, and
 * shown in the axis units. In the topographic view the spec gives it as
 * elevation, which outlives an edit of the radargram's elevation window that
 * moves every row.
 *
 * Settings persist: the style, axes, size and layer choices per project in
 * localStorage, so the next figure starts from the last; the extent per
 * radargram and view in sessionStorage, as it belongs to this radargram.
 */
(function setupFigureDialog() {
  const dialog = document.getElementById('figure-dialog');
  const opener = document.getElementById('dl-figure');
  if (!dialog || !opener) return;

  const config = window.RIDAL_VIEWER;
  const radargramId = config.radargramId;
  const $ = (id) => document.getElementById(id);
  const form = $('figure-form');

  /* Above this many samples in the extent, a change does not re-render the
   * preview by itself: reading the window is the cost, whatever the preview
   * width, and a whole long radargram takes a few seconds. */
  const AUTO_PREVIEW_SAMPLES = 4e6;
  const PREVIEW_DEBOUNCE_MS = 400;

  const state = {
    view: 'standard',
    // [first, last] trace and [top, bottom] row, in raster coordinates.
    traces: null,
    rows: null,
    axes: null,
    topo: null,
    documents: [],
    layers: [],
    derived: [],
    loaded: false,
    previewUrl: null,
    controller: null,
    timer: null,
  };

  // --- Storage -------------------------------------------------------------

  const prefsKey = `ridal-figure:${RIDAL.apiPath('')}`;
  const extentKey = () => `ridal-figure-extent:${RIDAL.apiPath('')}:${radargramId}:${state.view}`;

  function readStore(store, key) {
    try {
      const text = store.getItem(key);
      return text ? JSON.parse(text) : null;
    } catch {
      return null;
    }
  }

  function writeStore(store, key, value) {
    try {
      store.setItem(key, JSON.stringify(value));
    } catch {
      // Private browsing or a full quota: the dialog works without it.
    }
  }

  /* The controls that persist, by id: everything but the extent, which is
   * this radargram's, and the horizontal scale, which follows the viewer. */
  const PERSISTED = [
    'fig-width', 'fig-unit', 'fig-dpi', 'fig-x-unit', 'fig-y-unit',
    'fig-x-spacing', 'fig-y-spacing', 'fig-x-anchor', 'fig-y-anchor',
    'fig-x-label', 'fig-y-label', 'fig-axes', 'fig-font', 'fig-color',
    'fig-background', 'fig-transparent', 'fig-picks-width',
    'fig-picks-opacity', 'fig-derived-width', 'fig-derived-opacity',
    'fig-fills', 'fig-format', 'fig-layers',
  ];

  function savePrefs() {
    const values = {};
    PERSISTED.forEach((id) => {
      const input = $(id);
      values[id] = input.type === 'checkbox' ? input.checked : input.value;
    });
    values.picks = selectedPicks();
    values.derived = selectedDerived();
    writeStore(localStorage, prefsKey, values);
  }

  function restorePrefs() {
    const values = readStore(localStorage, prefsKey);
    if (!values) return null;
    PERSISTED.forEach((id) => {
      if (!(id in values)) return;
      const input = $(id);
      if (input.type === 'checkbox') input.checked = Boolean(values[id]);
      else input.value = values[id];
    });
    return values;
  }

  // --- Axes and units ------------------------------------------------------

  /* Linear interpolation at a fractional index, extrapolated from the end
   * pair: the same as `figure::axis::AxisValues::value_at`. */
  function interp(values, p) {
    const n = values.length;
    if (n === 0) return NaN;
    if (n === 1) return values[0];
    const i = Math.min(Math.max(Math.floor(p), 0), n - 2);
    const t = p - i;
    return values[i] + (values[i + 1] - values[i]) * t;
  }

  /* The fractional index where `values` reaches `v`, first along it. */
  function invert(values, v) {
    for (let i = 0; i + 1 < values.length; i++) {
      const a = values[i];
      const b = values[i + 1];
      if (!Number.isFinite(a) || !Number.isFinite(b)) continue;
      if ((a - v) * (b - v) <= 0 && a !== b) return i + (v - a) / (b - a);
      if (a === v) return i;
    }
    // Outside the axis: extrapolate from the nearer end.
    const n = values.length;
    if (n < 2) return 0;
    const first = (values[1] - values[0]) !== 0 ? (v - values[0]) / (values[1] - values[0]) : 0;
    const last = (values[n - 1] - values[n - 2]) !== 0
      ? n - 1 + (v - values[n - 1]) / (values[n - 1] - values[n - 2])
      : n - 1;
    return Math.abs(first) < Math.abs(last - (n - 1)) ? first : last;
  }

  function hasAxis(name) {
    return Boolean(state.axes && Array.isArray(state.axes[name]) && state.axes[name].length);
  }

  /* The units the extent fields are shown in: the axis's own when it can be
   * converted here, otherwise trace or row numbers. */
  function xFieldUnit() {
    const unit = $('fig-x-unit').value;
    if ((unit === 'distance' || unit === 'distance_km') && hasAxis('distance')) return unit;
    return 'trace';
  }

  function yFieldUnit() {
    if (state.view === 'topo') return state.topo ? 'elevation' : 'row';
    const unit = $('fig-y-unit').value;
    if ((unit === 'twtt' || unit === 'depth') && hasAxis(unit)) return unit;
    return 'row';
  }

  const UNIT_TEXT = {
    distance: 'm', distance_km: 'km', trace: 'trace', twtt: 'ns', depth: 'm', elevation: 'm a.s.l.', row: 'row',
  };

  function toXField(p) {
    const unit = xFieldUnit();
    if (unit === 'trace') return p;
    const metres = interp(state.axes.distance, p);
    return unit === 'distance_km' ? metres / 1000 : metres;
  }

  function fromXField(v) {
    const unit = xFieldUnit();
    if (unit === 'trace') return v;
    return invert(state.axes.distance, unit === 'distance_km' ? v * 1000 : v);
  }

  function toYField(row) {
    const unit = yFieldUnit();
    if (unit === 'elevation') return state.topo.elevation_top - row * state.topo.dz;
    if (unit === 'twtt' || unit === 'depth') return interp(state.axes[unit], row);
    return row;
  }

  function fromYField(v) {
    const unit = yFieldUnit();
    if (unit === 'elevation') return (state.topo.elevation_top - v) / state.topo.dz;
    if (unit === 'twtt' || unit === 'depth') return invert(state.axes[unit], v);
    return v;
  }

  const round = (v, digits) => Number(v.toFixed(digits));

  function showExtent() {
    if (!state.traces || !state.rows) return;
    // Kilometres to the metre, everything else to the hundredth.
    const xDigits = xFieldUnit() === 'distance_km' ? 3 : 2;
    $('fig-x0').value = round(toXField(state.traces[0]), xDigits);
    $('fig-x1').value = round(toXField(state.traces[1]), xDigits);
    $('fig-y0').value = round(toYField(state.rows[0]), 2);
    $('fig-y1').value = round(toYField(state.rows[1]), 2);
    const xu = UNIT_TEXT[xFieldUnit()];
    const yu = UNIT_TEXT[yFieldUnit()];
    $('fig-x-from-label').textContent = `From (${xu})`;
    $('fig-x-to-label').textContent = `To (${xu})`;
    $('fig-y-from-label').textContent = `Top (${yu})`;
    $('fig-y-to-label').textContent = `Bottom (${yu})`;
    const traces = state.traces[1] - state.traces[0];
    const rows = state.rows[1] - state.rows[0];
    $('fig-extent-note').textContent =
      `${Math.round(traces)} traces × ${Math.round(rows)} rows ` +
      `(traces ${round(state.traces[0], 1)}–${round(state.traces[1], 1)}).`;
    writeStore(sessionStorage, extentKey(), {
      traces: state.traces,
      rows: state.rows,
      revision: config.revisionId,
    });
  }

  function rasterHeight() {
    return window.RIDAL_GEOMETRY.rasterHeight;
  }

  /* Clip an extent to the raster and keep it ordered and non-empty.
   *
   * Rows typed into the topographic view are left alone (`clipRows`
   * false): there they may reach above the highest surface or below the
   * floor, to leave empty space around the radargram, as the server allows.
   * An extent taken from the map is always clipped, or zooming out would
   * frame the whole canvas. */
  function setExtent(traces, rows, clipRows = true) {
    const width = config.sourceWidth;
    const height = rasterHeight();
    const order = ([a, b]) => (a <= b ? [a, b] : [b, a]);
    const clip = ([a, b], n) => [Math.max(0, Math.min(a, n)), Math.max(0, Math.min(b, n))];
    let t = clip(order(traces), width);
    let r = clipRows ? clip(order(rows), height) : order(rows);
    if (t[1] - t[0] < 1) t = [Math.max(0, t[0] - 0.5), Math.min(width, t[0] + 0.5)];
    if (r[1] - r[0] < 1) r = [r[0] - 0.5, r[0] + 0.5];
    state.traces = t;
    state.rows = r;
    showExtent();
  }

  /* A map position as raster trace and row. Rows, not source samples: in
   * the topographic view the extent is a window of the corrected raster. */
  function toRaster(latlng) {
    const g = window.RIDAL_GEOMETRY;
    const scale = window.RIDAL_XSCALE || 1;
    return [latlng.lng / scale / g.rasterScale, -latlng.lat / g.verticalRasterScale];
  }

  function extentFromView() {
    const bounds = window.RIDAL_MAP.getBounds();
    const [t0, r0] = toRaster(bounds.getNorthWest());
    const [t1, r1] = toRaster(bounds.getSouthEast());
    setExtent([t0, t1], [r0, r1]);
  }

  function extentWhole() {
    setExtent([0, config.sourceWidth], [0, rasterHeight()]);
  }

  // --- Drawing a rectangle -------------------------------------------------

  /* Close the dialog and let a box be dragged on the radargram. A banner on
   * the map says what is expected, as nothing else on the page would. */
  function drawRectangle() {
    const map = window.RIDAL_MAP;
    const container = map.getContainer();
    dialog.close();
    map.dragging.disable();
    container.classList.add('figure-drawing');
    // A drag across the page otherwise selects its text as it goes.
    document.body.classList.add('figure-no-select');
    let start = null;
    let box = null;

    const banner = document.createElement('div');
    banner.className = 'figure-draw-banner';
    const text = document.createElement('span');
    text.textContent = 'Drag a rectangle over the part of the radargram to draw.';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.textContent = 'Cancel';
    banner.append(text, cancel);
    container.append(banner);
    // A press on the banner is not the start of a rectangle.
    L.DomEvent.disableClickPropagation(banner);
    cancel.addEventListener('click', () => {
      start = null;
      finish(null);
    });

    const finish = (latlng) => {
      map.off('mousedown', down);
      map.off('mousemove', move);
      map.off('mouseup', up);
      document.removeEventListener('keydown', key);
      map.dragging.enable();
      container.classList.remove('figure-drawing');
      document.body.classList.remove('figure-no-select');
      banner.remove();
      if (box) box.remove();
      if (start && latlng) {
        const [t0, r0] = toRaster(start);
        const [t1, r1] = toRaster(latlng);
        setExtent([t0, t1], [r0, r1]);
      }
      dialog.showModal();
      if (start && latlng) changed(true);
    };
    const down = (event) => {
      L.DomEvent.preventDefault(event.originalEvent);
      start = event.latlng;
      // Its own pane above the radargram's, or the box is drawn under the
      // tiles and shows only past their edge.
      if (!map.getPane('figure-draw')) map.createPane('figure-draw').style.zIndex = 450;
      box = L.rectangle([start, start], {
        pane: 'figure-draw',
        color: '#ff3b30',
        weight: 2,
        dashArray: '6 4',
        fill: true,
        fillOpacity: 0.12,
        interactive: false,
      }).addTo(map);
    };
    const move = (event) => {
      if (start && box) box.setBounds([start, event.latlng]);
    };
    const up = (event) => finish(event.latlng);
    const key = (event) => {
      if (event.key === 'Escape') {
        start = null;
        finish(null);
      }
    };
    map.on('mousedown', down);
    map.on('mousemove', move);
    map.on('mouseup', up);
    document.addEventListener('keydown', key);
  }

  // --- Layers --------------------------------------------------------------

  function layerFor(id) {
    return state.layers.find((layer) => layer.id === id) || null;
  }

  function swatch(colour) {
    const span = document.createElement('span');
    span.className = 'figure-swatch';
    span.style.background = colour || '#ffcc00';
    return span;
  }

  function checkbox(value, text, colour, checked) {
    const label = document.createElement('label');
    label.className = 'figure-check';
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.value = value;
    input.checked = checked;
    label.append(input, swatch(colour), document.createTextNode(` ${text}`));
    return label;
  }

  /* One group per contributor the server lets this caller see, with a box
   * per layer they picked. */
  function renderPicks(selected) {
    const list = $('fig-picks-list');
    list.replaceChildren();
    const chosen = new Set((selected || []).map((s) => `${s.user}\u0000${s.layer || ''}`));
    state.documents.forEach(({ user, document: doc }) => {
      // '' for lines with no layer, which a project without a layer
      // vocabulary has nothing but; the spec's `"layer": ""` asks for them.
      const labels = new Set();
      ((doc && doc.features) || []).forEach((feature) => {
        const label = (feature.properties && feature.properties.label) || '';
        if (feature.geometry && feature.geometry.type === 'LineString') labels.add(label);
      });
      if (!labels.size) return;
      const group = document.createElement('fieldset');
      group.className = 'figure-picker-group';
      const legend = document.createElement('legend');
      legend.textContent = user;
      group.append(legend);
      const all = chosen.has(`${user}\u0000`);
      [...labels].sort().forEach((label) => {
        const layer = layerFor(label);
        const box = checkbox(
          `${user}\u0000${label}`,
          label === '' ? 'Unlabelled' : (layer && layer.name) || label,
          layer && layer.color,
          all || chosen.has(`${user}\u0000${label}`),
        );
        group.append(box);
      });
      list.append(group);
    });
    if (!list.children.length) {
      list.textContent = 'No picks on this radargram that you can draw.';
    }
    summarise();
  }

  function renderDerived(selected) {
    const list = $('fig-derived-list');
    list.replaceChildren();
    const chosen = new Set(selected || []);
    state.derived
      .filter((item) => item.kind === 'layer' && item.listed !== false)
      .forEach((item) => {
        const target = item.fill_to && layerFor(item.fill_to.target);
        list.append(
          checkbox(item.id, item.name || item.id, item.color || (target && target.color), chosen.has(item.id)),
        );
      });
    if (!list.children.length) {
      list.textContent = 'No derived layers to draw.';
    }
    summarise();
  }

  /* Picks as spec selectors: a contributor with every layer ticked is one
   * selector for all of theirs, unlabelled lines included. */
  function selectedPicks() {
    const selectors = [];
    $('fig-picks-list').querySelectorAll('fieldset').forEach((group) => {
      const boxes = [...group.querySelectorAll('input[type=checkbox]')];
      const ticked = boxes.filter((b) => b.checked);
      if (!ticked.length) return;
      const user = ticked[0].value.split('\u0000')[0];
      if (ticked.length === boxes.length) {
        selectors.push({ user });
      } else {
        ticked.forEach((b) => selectors.push({ user, layer: b.value.split('\u0000')[1] }));
      }
    });
    return selectors;
  }

  function selectedDerived() {
    return [...$('fig-derived-list').querySelectorAll('input[type=checkbox]:checked')].map(
      (b) => b.value,
    );
  }

  function layersShown() {
    return $('fig-layers').checked;
  }

  /* The layer controls follow "Show layers": greyed out, as a whole, while
   * no layer is drawn. */
  function syncLayerControls() {
    $('fig-layer-controls').disabled = !layersShown();
  }

  function summarise() {
    const picks = $('fig-picks-list').querySelectorAll('input[type=checkbox]:checked').length;
    const derived = selectedDerived().length;
    $('fig-picks-summary').textContent = picks ? `${picks} selected` : 'none';
    $('fig-derived-summary').textContent = derived ? `${derived} selected` : 'none';
  }

  async function loadJson(url, fallback) {
    try {
      return await RIDAL.fetchJson(url);
    } catch (error) {
      console.warn(`Figure dialog: could not load ${url}: ${error.message}`);
      return fallback;
    }
  }

  /* What can be drawn, read on every open rather than once: picks saved, and
   * layers or derived items defined, since the dialog was last open belong
   * in its lists. What was ticked stays ticked. */
  async function loadLayers(prefs) {
    const first = !state.loaded;
    state.loaded = true;
    const picked = first ? prefs && prefs.picks : selectedPicks();
    const derivedPicked = first ? prefs && prefs.derived : selectedDerived();
    const [axes, layers, contributors, derived] = await Promise.all([
      state.axes ? state.axes : loadJson(RIDAL.apiPath('datasets', radargramId, 'axes'), null),
      loadJson(RIDAL.apiPath('layers'), { layers: [] }),
      loadJson(RIDAL.apiPath('datasets', radargramId, 'contributors'), { documents: [] }),
      loadJson(RIDAL.apiPath('derived'), { items: [] }),
    ]);
    state.axes = axes;
    state.layers = (layers && layers.layers) || [];
    state.documents = (contributors && contributors.documents) || [];
    state.derived = (derived && derived.items) || [];
    renderPicks(picked);
    renderDerived(derivedPicked);
  }

  async function loadTopo() {
    state.topo = null;
    if (state.view !== 'topo') return;
    try {
      state.topo = await RIDAL.fetchJson(
        RIDAL.apiPath('datasets', radargramId, 'views', 'topo', 'geometry'),
      );
    } catch (error) {
      message(`The topographic view's geometry could not be loaded: ${error.message}`, 'error');
    }
  }

  // --- The spec ------------------------------------------------------------

  const numberOr = (id, fallback) => {
    const value = $(id).value.trim();
    if (value === '') return fallback;
    const n = Number(value);
    return Number.isFinite(n) ? n : fallback;
  };
  const textOr = (id) => ($(id).value === '' ? null : $(id).value);

  function widthPx() {
    const width = numberOr('fig-width', 16);
    const dpi = numberOr('fig-dpi', 300);
    const unit = $('fig-unit').value;
    if (unit === 'px') return width;
    return unit === 'in' ? width * dpi : (width / 2.54) * dpi;
  }

  function spec() {
    const topo = state.view === 'topo';
    const extent = { traces: state.traces };
    if (topo && state.topo) {
      // Elevation rather than rows: rows move when the elevation window is
      // edited, and a spec saved from here should keep meaning this part.
      extent.elevation = state.rows.map((r) => state.topo.elevation_top - r * state.topo.dz);
    } else {
      extent.rows = state.rows;
    }
    const axis = (prefix, unit) => ({
      unit,
      spacing: numberOr(`fig-${prefix}-spacing`, null),
      anchor: numberOr(`fig-${prefix}-anchor`, 0),
      label: textOr(`fig-${prefix}-label`),
    });
    return {
      version: 1,
      view: state.view,
      revision_id: config.revisionId,
      profile: document.getElementById('profile-select').value,
      x_scale: numberOr('fig-xscale', 1),
      extent,
      size: {
        width: numberOr('fig-width', 16),
        unit: $('fig-unit').value,
        dpi: numberOr('fig-dpi', 300),
      },
      axes: $('fig-axes').checked,
      x_axis: axis('x', $('fig-x-unit').value),
      y_axis: axis('y', topo ? 'elevation' : $('fig-y-unit').value),
      style: {
        font_size: numberOr('fig-font', 10),
        color: $('fig-color').value,
        background: $('fig-transparent').checked ? null : $('fig-background').value,
      },
      radargram: $('fig-radargram').checked,
      // "Show layers" off draws none, whatever the lists say, and keeps
      // what is ticked there for when it is turned back on.
      picks: {
        items: layersShown() ? selectedPicks() : [],
        width: numberOr('fig-picks-width', 1),
        opacity: numberOr('fig-picks-opacity', 0.85),
      },
      derived: {
        items: layersShown() ? selectedDerived() : [],
        width: numberOr('fig-derived-width', 1.5),
        opacity: numberOr('fig-derived-opacity', 0.9),
        fills: $('fig-fills').checked,
      },
    };
  }

  /* The same figure at the preview's width: the dpi scaled by the same
   * factor keeps every point size, and so the whole layout, in proportion. */
  function previewSpec() {
    const full = spec();
    const pane = $('fig-preview');
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    const width = Math.round(Math.max(300, Math.min(1400, pane.clientWidth || 700)) * ratio);
    const scale = width / Math.max(1, widthPx());
    full.size = { width, unit: 'px', dpi: Math.min(2400, Math.max(10, full.size.dpi * scale)) };
    return full;
  }

  // --- Preview and download ------------------------------------------------

  function message(text, tone) {
    const box = $('fig-messages');
    const line = document.createElement('p');
    line.className = tone === 'error' ? 'figure-error' : 'hint';
    line.textContent = text;
    box.append(line);
  }

  function clearMessages() {
    $('fig-messages').replaceChildren();
  }

  async function errorText(response) {
    try {
      const body = await response.json();
      if (body && body.error && body.error.message) return body.error.message;
    } catch {
      // Not an envelope: something in front of the server answered.
    }
    return RIDAL.upstreamMessage(response.status);
  }

  /* Warnings about picks, one per contributor: a few are listed, more are
   * folded under one line, as ten identical sentences hide the figure's
   * own messages. */
  function showWarnings(warnings) {
    if (warnings.length <= 2) {
      warnings.forEach((w) => message(w));
      return;
    }
    const details = document.createElement('details');
    const summary = document.createElement('summary');
    summary.className = 'hint';
    summary.textContent = `${warnings.length} contributors' picks could not be drawn exactly as stored.`;
    const list = document.createElement('ul');
    warnings.forEach((w) => {
      const item = document.createElement('li');
      item.className = 'hint';
      item.textContent = w;
      list.append(item);
    });
    details.append(summary, list);
    $('fig-messages').append(details);
  }

  function warningsOf(response) {
    try {
      return JSON.parse(response.headers.get('x-ridal-figure-warnings') || '[]');
    } catch {
      return [];
    }
  }

  async function requestFigure(body, format, signal) {
    return fetch(`${RIDAL.apiPath('datasets', radargramId, 'figure')}?format=${format}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      signal,
    });
  }

  async function preview() {
    if (!state.traces) return;
    if (state.controller) state.controller.abort();
    const controller = new AbortController();
    state.controller = controller;
    const status = $('fig-preview-status');
    const pane = $('fig-preview');
    // Shown over the previous preview, which stays in place, dimmed, until
    // the next one arrives.
    pane.classList.add('is-busy');
    const done = () => {
      if (controller === state.controller) pane.classList.remove('is-busy');
    };
    status.hidden = false;
    status.textContent = 'Rendering…';
    $('fig-update').classList.remove('is-stale');
    let response;
    try {
      response = await requestFigure(previewSpec(), 'png', controller.signal);
    } catch (error) {
      if (error.name === 'AbortError') return;
      status.textContent = `Could not reach the server: ${error.message}`;
      done();
      return;
    }
    if (controller !== state.controller) return;
    clearMessages();
    if (!response.ok) {
      status.textContent = await errorText(response);
      done();
      return;
    }
    showWarnings(warningsOf(response));
    const blob = await response.blob();
    if (controller !== state.controller) return;
    if (state.previewUrl) URL.revokeObjectURL(state.previewUrl);
    state.previewUrl = URL.createObjectURL(blob);
    const image = $('fig-preview-image');
    image.onload = () => {
      // The download's size, from the preview's aspect: the same layout,
      // scaled.
      const width = Math.round(widthPx());
      const height = Math.round((width * image.naturalHeight) / Math.max(1, image.naturalWidth));
      $('fig-size-note').textContent = `About ${width} × ${height} px.`;
    };
    image.src = state.previewUrl;
    image.hidden = false;
    status.hidden = true;
    done();
  }

  function samples() {
    if (!state.traces || !state.rows) return 0;
    return (state.traces[1] - state.traces[0]) * (state.rows[1] - state.rows[0]);
  }

  /* After any change: save the settings, and re-render the preview if the
   * extent is small enough to do it as the controls move. */
  function changed(force) {
    syncLayerControls();
    savePrefs();
    summarise();
    clearTimeout(state.timer);
    if (force || samples() <= AUTO_PREVIEW_SAMPLES) {
      state.timer = setTimeout(preview, force ? 0 : PREVIEW_DEBOUNCE_MS);
    } else {
      // A preview already on its way is of the settings before this
      // change: it must not arrive and hide that the picture is stale.
      if (state.controller) state.controller.abort();
      state.controller = null;
      $('fig-preview').classList.remove('is-busy');
      $('fig-update').classList.add('is-stale');
      $('fig-preview-status').hidden = false;
      $('fig-preview-status').textContent = 'Update the preview to see the changes.';
    }
  }

  function saveBlob(blob, name) {
    const href = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = href;
    anchor.download = name;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    // Next tick: revoking synchronously cancels the download in Safari.
    setTimeout(() => URL.revokeObjectURL(href), 0);
  }

  async function download() {
    const format = $('fig-format').value;
    const button = $('fig-download');
    button.disabled = true;
    clearMessages();
    message('Rendering the figure…');
    try {
      const response = await requestFigure(spec(), format);
      clearMessages();
      if (!response.ok) {
        message(await errorText(response), 'error');
        return;
      }
      showWarnings(warningsOf(response));
      saveBlob(
        await response.blob(),
        RIDAL.filenameFrom(response.headers.get('content-disposition')) ||
          `${radargramId}-figure.${format}`,
      );
    } catch (error) {
      clearMessages();
      message(`Could not reach the server: ${error.message}`, 'error');
    } finally {
      button.disabled = false;
    }
  }

  function downloadSpec() {
    const text = `${JSON.stringify(spec(), null, 2)}\n`;
    saveBlob(new Blob([text], { type: 'application/json' }), `${radargramId}-figure.json`);
  }

  // --- Opening -------------------------------------------------------------

  /* The y units the view offers: elevation only in the topographic view,
   * never in the standard one. */
  function offerUnits() {
    const topo = state.view === 'topo';
    [...$('fig-y-unit').options].forEach((option) => {
      option.disabled = topo ? option.value !== 'elevation' : option.value === 'elevation';
    });
    const select = $('fig-y-unit');
    if (select.selectedOptions[0] && select.selectedOptions[0].disabled) {
      select.value = topo ? 'elevation' : 'twtt';
    }
  }

  async function open() {
    const prefs = state.loaded ? null : restorePrefs();
    state.view = window.RIDAL_GEOMETRY.view || 'standard';
    // The horizontal scale follows the viewer each time, as the figure
    // should look like what is on screen.
    $('fig-xscale').value = window.RIDAL_XSCALE || 1;
    if (typeof radargramVisible === 'boolean') $('fig-radargram').checked = radargramVisible;
    offerUnits();
    syncLayerControls();
    dialog.showModal();
    await Promise.all([loadLayers(prefs), loadTopo()]);

    const stored = readStore(sessionStorage, extentKey());
    if (stored && stored.revision === config.revisionId && stored.traces && stored.rows) {
      setExtent(stored.traces, stored.rows, state.view !== 'topo');
    } else {
      extentFromView();
    }
    changed(true);
  }

  opener.addEventListener('click', () => {
    const menu = opener.closest('details');
    if (menu) menu.open = false;
    open();
  });
  form.addEventListener('submit', (event) => event.preventDefault());
  form.addEventListener('change', (event) => {
    const id = event.target.id;
    if (['fig-x0', 'fig-x1'].includes(id)) {
      setExtent(
        [fromXField(Number($('fig-x0').value)), fromXField(Number($('fig-x1').value))],
        state.rows,
        state.view !== 'topo',
      );
    } else if (['fig-y0', 'fig-y1'].includes(id)) {
      setExtent(
        state.traces,
        [fromYField(Number($('fig-y0').value)), fromYField(Number($('fig-y1').value))],
        state.view !== 'topo',
      );
    } else if (id === 'fig-x-unit' || id === 'fig-y-unit') {
      showExtent();
    }
    changed(false);
  });
  $('fig-extent-view').addEventListener('click', () => {
    dialog.close();
    extentFromView();
    dialog.showModal();
    changed(true);
  });
  $('fig-extent-all').addEventListener('click', () => {
    extentWhole();
    changed(false);
  });
  $('fig-extent-draw').addEventListener('click', drawRectangle);
  $('fig-update').addEventListener('click', preview);
  $('fig-download').addEventListener('click', download);
  $('fig-download-spec').addEventListener('click', downloadSpec);
  $('fig-format').addEventListener('change', savePrefs);
  $('fig-close').addEventListener('click', () => dialog.close());
})();
