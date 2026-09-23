/* Viewer page behaviour (#115, #121).
 *
 * Loaded by viewer.html.jinja after leaflet.js and app.js, so `L` and
 * `RIDAL` are both defined. Server-side values arrive through the
 * `window.RIDAL_VIEWER` object the template emits immediately before this
 * file -- that inline block is the only JS the template still contains, so
 * everything here stays a plain static asset with no templating in it.
 *
 * Deliberately NOT under assets/vendor/ -- scripts/vendor_leaflet.sh does
 * `rm -rf` on that directory.
 */

const CFG = window.RIDAL_VIEWER;

const RADARGRAM_ID = CFG.radargramId;
const GROUP = CFG.groupId;
const CHUNK_SIZE = CFG.chunkSize;
const SOURCE_WIDTH = CFG.sourceWidth;
const SOURCE_HEIGHT = CFG.sourceHeight;

/* Shared, mutable geometry state (#168), read live by both this file and
 * picker.js on every call rather than captured once as `const`s at load.
 * Toggling the topographically corrected view changes the display height
 * and the raster<->source-sample mapping; capturing either at
 * initialization would leave existing markers drawing through one mapping
 * while newly placed picks are stored through another, corrupting picks
 * silently -- precisely the failure "always store picks in source
 * coordinates" exists to prevent. `window.RIDAL_GEOMETRY` (not a local
 * variable) because picker.js is a separate classic script with no module
 * boundary to this one (#120: no build step).
 *
 * `rasterScale`/`verticalRasterScale` stay `1` in both views: the viewer
 * never resamples (`ARCHITECTURE.md`), in the corrected view exactly as
 * in the standard one -- a corrected raster is *taller*, from the shear,
 * never resampled to a different scale. They are kept as named fields
 * rather than assumed to be `1` inline, so a future genuinely-downsampled
 * raster (if one is ever reintroduced) has one place to change this.
 * `shift` is the per-trace downward shift in samples the corrected view
 * needs to convert between a raster row and a source sample; `null`
 * outside that view.
 */
window.RIDAL_GEOMETRY = {
  view: "standard",
  sourceWidth: SOURCE_WIDTH,
  sourceHeight: SOURCE_HEIGHT,
  rasterWidth: CFG.viewerWidth,
  rasterHeight: CFG.viewerHeight,
  nCols: CFG.nCols,
  nRows: CFG.nRows,
  rasterScale: CFG.viewerWidth / SOURCE_WIDTH,
  verticalRasterScale: CFG.viewerHeight / SOURCE_HEIGHT,
  shift: null,
  fingerprint: null,
};

/* Per-trace shift, linearly interpolated at a fractional trace index, or
 * `0` outside the corrected view. The inverse of `TopoSource`'s own
 * per-trace shift lookup (`src/render/topo.rs`) -- the same number, read
 * back rather than recomputed, which is what keeps a placed pick and the
 * render it was placed on agreeing about where "here" is. */
function shiftAt(trace) {
  const g = window.RIDAL_GEOMETRY;
  if (g.view !== "topo" || !g.shift || g.shift.length === 0) return 0;
  const clamped = Math.min(Math.max(trace, 0), g.shift.length - 1);
  const i0 = Math.floor(clamped);
  const i1 = Math.min(i0 + 1, g.shift.length - 1);
  const f = clamped - i0;
  return g.shift[i0] * (1 - f) + g.shift[i1] * f;
}

/* Index-space <-> viewer-raster conversion, published for picker.js and the
 * layer panel (#209) so there is one definition of where a stored
 * (trace, sample) lands on screen. Reads `window.RIDAL_GEOMETRY` live for the
 * same reason picker.js does: toggling the topographic view changes the
 * mapping and a captured copy would draw through the wrong one. */
window.RIDAL_TO_LATLNG = function (trace, sample) {
  const g = window.RIDAL_GEOMETRY;
  const scale = window.RIDAL_XSCALE || 1;
  const rasterRow = sample + shiftAt(trace);
  return L.latLng(-rasterRow * g.verticalRasterScale, trace * g.rasterScale * scale);
};

window.RIDAL_TO_INDEX = function (latlng) {
  const g = window.RIDAL_GEOMETRY;
  const scale = window.RIDAL_XSCALE || 1;
  const trace = latlng.lng / scale / g.rasterScale;
  const rasterRow = -latlng.lat / g.verticalRasterScale;
  return [trace, rasterRow - shiftAt(trace)];
};

/* Redraw every index-space overlay after the geometry changed.
 *
 * The picker's editable lines and the layer panel's derived lines and fills
 * all convert (trace, sample) through `window.RIDAL_GEOMETRY`, which the
 * topographic toggle and the horizontal-scale change rewrite. The picker was
 * already redrawn on those events; the derived overlays were not, so they
 * stayed where the previous geometry put them until they were toggled. */
function redrawOverlays() {
  if (window.RIDAL_REDRAW_PICKS) window.RIDAL_REDRAW_PICKS();
  if (window.RIDAL_REDRAW_DERIVED) window.RIDAL_REDRAW_DERIVED();
  if (window.RIDAL_REDRAW_TRACE) window.RIDAL_REDRAW_TRACE();
}

/* A short alias onto the one geometry object, not a copy: `G.nCols` etc.
 * always reads whatever the most recent toggle wrote, since object
 * property lookups go through the live reference. Everything below reads
 * through `G` rather than caching a field in a local, for the same reason
 * the object exists at all. */
const G = window.RIDAL_GEOMETRY;

// The project's default stretch, already validated against the offered
// factors server-side, so this is the value the dropdown is showing.
let xScale = Number(document.getElementById('xscale-select').value) || 1;

function currentProfile() {
  return document.getElementById('profile-select').value;
}

function chunkUrl(x, y, profile) {
  const base = RIDAL.apiPath("datasets", RADARGRAM_ID, "views", G.view, "chunks", profile, x, y);
  // The corrected view's geometry fingerprint, as a cache-busting query
  // parameter (#168): an elevation-range edit keeps the same
  // view/x/y/profile, so the URL would otherwise be unchanged and a
  // browser could serve a stale cached chunk for it. The server does not
  // read this back -- the current override is always the source of truth
  // for what a chunk renders.
  return G.view === "topo" && G.fingerprint ? `${base}?fp=${G.fingerprint}` : base;
}

function chunkBounds(x, y, scale) {
  // Bounds formula verified in M0 against a real headless-Chromium
  // screenshot: places chunk (0,0) upper-left with no transposition.
  // The x-scale factor stretches/squeezes only the horizontal extent --
  // a pure client-side view transform, no new render, no server
  // round-trip (the plan's explicit design for this control).
  //
  // The rightmost/bottommost chunks cover fewer than CHUNK_SIZE pixels
  // (the raster rarely divides evenly), and the server renders them at
  // exactly that size. Placing them in a full CHUNK_SIZE box instead
  // would stretch them over their neighbours' edges.
  const validWidth = Math.min(CHUNK_SIZE, G.rasterWidth - x * CHUNK_SIZE);
  const validHeight = Math.min(CHUNK_SIZE, G.rasterHeight - y * CHUNK_SIZE);
  return [
    [-(y * CHUNK_SIZE + validHeight), x * CHUNK_SIZE * scale],
    [-(y * CHUNK_SIZE), (x * CHUNK_SIZE + validWidth) * scale],
  ];
}

/* Chunk (x, y)'s raster-row data band in the corrected view: the union,
 * over its own columns, of `[shift[c], shift[c] + sourceHeight)` --
 * widened to `[min shift, max shift + sourceHeight)` across the chunk
 * rather than tracked per column, which is a superset (safe: it can keep
 * a chunk with no *own* data next to one that has some, never drop one
 * that does) computed from the shift array already in hand. `null`
 * outside the corrected view, where every chunk is a candidate exactly as
 * before. */
function chunkDataBand(x) {
  if (G.view !== "topo" || !G.shift) return null;
  const c0 = x * CHUNK_SIZE;
  const c1 = Math.min(c0 + CHUNK_SIZE, G.shift.length);
  let lo = Infinity;
  let hi = -Infinity;
  for (let c = c0; c < c1; c++) {
    const s = G.shift[c];
    if (s < lo) lo = s;
    if (s > hi) hi = s;
  }
  if (!isFinite(lo)) return null;
  return [lo, hi + G.sourceHeight];
}

/* One chunk overlay, fetched only once the browser decides it is near the
 * viewport.
 *
 * L.imageOverlay(url, ...) sets `img.src` the moment the layer is added
 * (`_initImage`), with no viewport test -- so every chunk in the grid used
 * to be requested on page load whatever the map was looking at, and zooming
 * in could not prevent it. Leaflet also accepts an existing <img> in place
 * of a URL, and in that branch it does *not* touch `src`. So the element is
 * built here instead, with `loading` set before `src` (after, the attribute
 * has no effect), and handed over ready-made.
 *
 * This is what lets the viewer render 1:1 with no resolution cap: cost now
 * tracks what is on screen rather than how long the radargram is.
 */
function chunkImage(x, y, profile) {
  const img = document.createElement('img');
  img.loading = 'lazy';
  img.decoding = 'async';
  img.alt = '';
  img.src = chunkUrl(x, y, profile);
  return img;
}

/* Which chunks the current view touches, padded by one chunk so a small
 * pan reveals an already-loaded tile rather than a blank one. In the
 * corrected view, a chunk whose row range cannot contain any data (per
 * `chunkDataBand`) is skipped regardless of viewport overlap -- panning
 * into the wedge above a sheared trace must not fetch a chunk that can
 * only ever be NaN. */
function chunksInView(scale) {
  const bounds = map.getBounds();
  const west = bounds.getWest() - CHUNK_SIZE;
  const east = bounds.getEast() + CHUNK_SIZE;
  const south = bounds.getSouth() - CHUNK_SIZE;
  const north = bounds.getNorth() + CHUNK_SIZE;
  const found = [];
  for (let x = 0; x < G.nCols; x++) {
    const band = chunkDataBand(x);
    for (let y = 0; y < G.nRows; y++) {
      if (band) {
        const [rowLo, rowHi] = [y * CHUNK_SIZE, (y + 1) * CHUNK_SIZE];
        if (rowHi <= band[0] || rowLo >= band[1]) continue;
      }
      const [[lat0, lng0], [lat1, lng1]] = chunkBounds(x, y, scale);
      if (lng1 < west || lng0 > east || lat1 < south || lat0 > north) continue;
      found.push([x, y]);
    }
  }
  return found;
}

/* Chunks are added as the view reaches them, and then kept.
 *
 * Adding the whole grid up front meant opening a radargram rendered every
 * chunk of it -- the cost of opening a file scaled with its length rather
 * than with what was being looked at, which is what the old 8192 px cap
 * was really paying for. `loading="lazy"` alone is not enough to rely on:
 * it is a browser heuristic (and headless Chromium ignores it outright), so
 * the decision is made here instead and the attribute is left on as a
 * second line of defence.
 *
 * Kept rather than evicted once loaded: panning back over ground already
 * visited should not re-fetch it, and what has been looked at is a far
 * smaller bound than the whole file.
 */
let chunkLayers = [];
const chunksAdded = new Set();

/* Whether the radargram image itself is drawn (#230).
 *
 * Opacity rather than removing the layers: a hidden chunk stays loaded, so
 * toggling back is instant and re-fetches nothing, which is the same reason
 * chunks are kept rather than evicted above. Read by `addChunksInView`, not
 * only applied to the chunks already placed -- chunks arrive lazily as the
 * view reaches them, so without this, panning while hidden would bring the
 * radargram back one chunk at a time.
 */
let radargramVisible = true;

function addChunksInView(profile, scale) {
  for (const [x, y] of chunksInView(scale)) {
    const key = `${x},${y}`;
    if (chunksAdded.has(key)) continue;
    chunksAdded.add(key);
    const layer = L.imageOverlay(chunkImage(x, y, profile), chunkBounds(x, y, scale), {
      opacity: radargramVisible ? 1 : 0,
    }).addTo(map);
    chunkLayers.push(layer);
  }
}

function setRadargramVisible(visible) {
  radargramVisible = visible;
  chunkLayers.forEach((layer) => layer.setOpacity(visible ? 1 : 0));
}

/* Full rebuild: the profile or the horizontal scale changed, so every
 * placed chunk is either the wrong image or in the wrong place. */
function loadChunks(map, profile, scale) {
  chunkLayers.forEach((l) => map.removeLayer(l));
  chunkLayers = [];
  chunksAdded.clear();
  addChunksInView(profile, scale);
}

const map = L.map('map', { crs: L.CRS.Simple, minZoom: -6, attributionControl: false });
// Published for picker.js, which draws onto this same map and needs the
// current horizontal stretch to convert clicks to trace indices. Plain
// globals rather than exports: these are classic scripts with no module
// boundary between them (#120: no build step).
window.RIDAL_MAP = map;
window.RIDAL_XSCALE = xScale;

/* Two panes so a range fill can sit behind every line (#209). Leaflet's
 * `overlayPane` holds both the radargram images and the SVG lines, so
 * "behind the lines but in front of the radargram" needs panes of its own:
 * fills at 401, all panel and pick lines at 402. Markers (handles, overhang
 * markers) stay in `markerPane` at 600, above both. */
map.createPane("radargram-fills").style.zIndex = 401;
map.createPane("radargram-lines").style.zIndex = 402;
/* Open on the start of the radargram at full depth, not on the whole thing.
 *
 * Fitting the entire length put every chunk in the viewport at once, which
 * defeats lazy loading and lands the user on a squashed overview they have
 * to zoom into anyway. Fitting the *height* gives readable detail
 * immediately and leaves the rest of the length to load as they pan into
 * it. A radargram short enough to fit whole still does -- the min() below
 * means nothing changes for those.
 */
function fitToScale(scale) {
  const size = map.getSize();
  // Viewer px that span the container once the full sample range fits it.
  const widthAtFullHeight = size.y > 0 ? (G.rasterHeight * size.x) / size.y : G.rasterWidth * scale;
  const width = Math.min(G.rasterWidth * scale, widthAtFullHeight);
  map.fitBounds([[-G.rasterHeight, 0], [0, width]]);
}
fitToScale(xScale);
loadChunks(map, currentProfile(), xScale);
map.on('moveend zoomend', () => addChunksInView(currentProfile(), xScale));

document.getElementById('profile-select').addEventListener('change', () => {
  loadChunks(map, currentProfile(), xScale);
});

document.getElementById('xscale-select').addEventListener('change', (event) => {
  // Rescale from the centre trace, not the left edge: re-lay the
  // overlays at the new scale, then re-derive the map centre's
  // longitude by the same scale ratio the overlays themselves just
  // moved by. Latitude and zoom are untouched -- chunkBounds only ever
  // stretches the horizontal extent, and keeping zoom fixed is what
  // makes this a horizontal-only zoom rather than a no-op.
  const oldScale = xScale;
  const newScale = parseFloat(event.target.value);
  const center = map.getCenter();
  xScale = newScale;
  window.RIDAL_XSCALE = newScale;
  loadChunks(map, currentProfile(), xScale);
  redrawOverlays();
  map.setView(
    [center.lat, center.lng * (newScale / oldScale)],
    map.getZoom(),
    { animate: false },
  );
});

// --- Overview map: this radargram's track, plus sibling tracks in the
// same group (clickable, navigating to that radargram), plus a marker
// that follows the viewer cursor (#121's cursor-sync feature). ---
const overviewMap = RIDAL.basemap(L.map('overview-map'), 'overview-map');

// --- The viewer's two drag handles ----------------------------------------
//
// `#split-resizer` sizes the radargram region against the overview map, and
// `#trace-resizer` (inside the region) sizes the radargram against the trace
// panel. Both are plain pointer-drag handles with keyboard and double-click
// support.
//
// `.layout` is `flex-wrap: nowrap` and stacks only through the narrow-screen
// media query, so a handle can never be hidden by the state its own drag
// created -- the feedback loop that made #199 unrecoverable, including after
// a browser zoom out.

// Leaflet does not re-lay its tiles when its container is resized by
// something other than a window resize event it listens for itself -- it
// has to be told. Throttled to one call per frame since pointermove fires
// far more often than the browser can usefully repaint.
let invalidateQueued = false;
function scheduleInvalidate() {
  if (invalidateQueued) return;
  invalidateQueued = true;
  requestAnimationFrame(() => {
    invalidateQueued = false;
    // `pan: false` is load-bearing on a phone. The default re-centres the
    // map to keep the previous centre visible, which reads as the viewer
    // jumping -- and it fires exactly when a first tap collapses the
    // browser's address bar and changes the 70vh map height. The picks
    // stay put either way; only the view was moving.
    map.invalidateSize({ pan: false });
    overviewMap.invalidateSize({ pan: false });
  });
}

const splitResizer = (function setupSplitResizer() {
  const layout = document.getElementById('viewer-layout');
  const resizer = document.getElementById('split-resizer');
  // The resizer sizes the whole radargram region (radargram + optional
  // trace panel, #181) against the overview map, not `#map` alone -- so
  // toggling the trace panel never changes what the drag means.
  const radarEl = document.getElementById('radar-region');
  const overviewEl = document.getElementById('overview-map');
  // The two panes' `min-width`s in app.css.
  const MAP_MIN_PX = 200;
  const OVERVIEW_MIN_PX = 80;
  const KEYBOARD_STEP_PX = 24;
  // The last width a drag set, so a layout resize (a browser zoom, a
  // window resize) can re-clamp it instead of leaving it overflowing.
  let draggedPx = null;

  // Read the real gutter rather than assuming `--space-4`: the clamp has
  // to leave room for both gaps on the side-by-side line.
  function layoutGapPx() {
    const gap = parseFloat(getComputedStyle(layout).columnGap);
    return Number.isFinite(gap) ? gap : 16;
  }

  // The region's own minimum, from CSS: 200px of radargram, or the sum of
  // radargram, trace handle, trace panel and gutters when the trace is
  // open. Reading it back is what keeps the drag from shrinking the region
  // past the point where the trace would be pushed onto its own row.
  function radarMinPx() {
    const min = parseFloat(getComputedStyle(radarEl).minWidth);
    return Number.isFinite(min) ? min : MAP_MIN_PX;
  }

  function setMapBasisPx(px) {
    const layoutWidth = layout.getBoundingClientRect().width;
    const resizerWidth = resizer.getBoundingClientRect().width;
    const minPx = radarMinPx();
    // What the drag may leave for the radargram region: everything except
    // the resizer, the overview pane's minimum, and the two gutters that
    // separate the three items. The overview's *outer* minimum is used,
    // not its `min-width`: with the default `box-sizing: content-box` its
    // 1px border sits outside the flex-basis, and that unaccounted 2px was
    // exactly enough to push a full-right drag across the old wrap
    // threshold (#199's bug). The trailing `- 1` is a sub-pixel guard.
    const overviewOuter =
      OVERVIEW_MIN_PX + Math.max(0, overviewEl.offsetWidth - overviewEl.clientWidth);
    const maxPx = Math.max(
      minPx,
      layoutWidth - resizerWidth - overviewOuter - 2 * layoutGapPx() - 1,
    );
    const clamped = Math.min(Math.max(px, minPx), maxPx);
    draggedPx = clamped;
    // `0 0 <px>` (not just flex-basis) zeroes out grow/shrink on this
    // pane specifically, so the drag result is exactly what was set --
    // the overview pane's own flex:1 absorbs whatever space is left.
    radarEl.style.flex = `0 0 ${clamped}px`;
    resizer.setAttribute(
      'aria-valuenow',
      Math.round((clamped / (layoutWidth - resizerWidth)) * 100),
    );
    scheduleInvalidate();
  }

  let dragging = false;
  resizer.addEventListener('pointerdown', (event) => {
    dragging = true;
    resizer.setPointerCapture(event.pointerId);
  });
  resizer.addEventListener('pointermove', (event) => {
    if (!dragging) return;
    setMapBasisPx(event.clientX - layout.getBoundingClientRect().left);
  });
  resizer.addEventListener('pointerup', (event) => {
    dragging = false;
    resizer.releasePointerCapture(event.pointerId);
  });

  resizer.addEventListener('keydown', (event) => {
    const currentPx = radarEl.getBoundingClientRect().width;
    if (event.key === 'ArrowLeft') {
      setMapBasisPx(currentPx - KEYBOARD_STEP_PX);
      event.preventDefault();
    } else if (event.key === 'ArrowRight') {
      setMapBasisPx(currentPx + KEYBOARD_STEP_PX);
      event.preventDefault();
    }
  });

  // The way back to the default split.
  resizer.addEventListener('dblclick', () => {
    draggedPx = null;
    radarEl.style.flex = '';
    scheduleInvalidate();
  });

  // When the layout stacks (narrow screen), a fixed region width would
  // overflow; drop it and restore it if the layout widens again.
  const narrowQuery = window.matchMedia('(max-width: 40rem)');
  function applyNarrowState() {
    if (narrowQuery.matches) {
      radarEl.style.flex = '';
    } else if (draggedPx !== null) {
      setMapBasisPx(draggedPx);
    }
  }
  narrowQuery.addEventListener('change', applyNarrowState);

  new ResizeObserver(() => {
    applyNarrowState();
    scheduleInvalidate();
  }).observe(layout);

  return {
    // Re-clamp the stored width after the region's own minimum changed
    // (the trace panel opening or closing), so the drag state and the
    // rendered width agree.
    reclamp() {
      if (draggedPx !== null) setMapBasisPx(draggedPx);
    },
  };
})();

// The handle between the radargram and the trace panel. It keeps its 8px in
// the flex line when hidden (`.is-hidden` uses `visibility`), so hiding it
// cannot change whether the trace panel wraps below the radargram.
const traceResizer = (function setupTraceResizer() {
  const region = document.getElementById('radar-region');
  const resizer = document.getElementById('trace-resizer');
  const mapEl = document.getElementById('map');
  const traceEl = document.getElementById('trace-view');
  const MAP_MIN_PX = 200;
  const TRACE_MIN_PX = 160;
  const KEYBOARD_STEP_PX = 24;
  let draggedPx = null;

  function gapPx() {
    const gap = parseFloat(getComputedStyle(region).columnGap);
    return Number.isFinite(gap) ? gap : 16;
  }

  // True when the region is too narrow for map and trace side by side, so
  // the trace has dropped below and a horizontal handle makes no sense.
  function wrapped() {
    return mapEl.offsetTop !== traceEl.offsetTop;
  }

  function refresh() {
    resizer.classList.toggle('is-hidden', traceEl.hidden || wrapped());
  }

  function setMapPx(px) {
    const regionWidth = region.getBoundingClientRect().width;
    const resizerWidth = resizer.getBoundingClientRect().width;
    // Both panes' *outer* minima: each has a 1px border that
    // `box-sizing: content-box` keeps outside the flex-basis, so the line
    // is 2px wider on each side than the basis suggests. Missing that was
    // what wrapped the trace below the radargram at the drag's limit.
    const traceOuter =
      TRACE_MIN_PX + Math.max(0, traceEl.offsetWidth - traceEl.clientWidth);
    const mapOuter = Math.max(0, mapEl.offsetWidth - mapEl.clientWidth);
    const maxPx = Math.max(
      MAP_MIN_PX,
      regionWidth - resizerWidth - traceOuter - mapOuter - 2 * gapPx() - 1,
    );
    const clamped = Math.min(Math.max(px, MAP_MIN_PX), maxPx);
    draggedPx = clamped;
    mapEl.style.flex = `0 0 ${clamped}px`;
    resizer.setAttribute(
      'aria-valuenow',
      Math.round((clamped / Math.max(1, regionWidth - resizerWidth)) * 100),
    );
    scheduleInvalidate();
  }

  let dragging = false;
  resizer.addEventListener('pointerdown', (event) => {
    dragging = true;
    resizer.setPointerCapture(event.pointerId);
  });
  resizer.addEventListener('pointermove', (event) => {
    if (!dragging) return;
    setMapPx(event.clientX - region.getBoundingClientRect().left);
  });
  resizer.addEventListener('pointerup', (event) => {
    dragging = false;
    resizer.releasePointerCapture(event.pointerId);
  });

  resizer.addEventListener('keydown', (event) => {
    const currentPx = mapEl.getBoundingClientRect().width;
    if (event.key === 'ArrowLeft') {
      setMapPx(currentPx - KEYBOARD_STEP_PX);
      event.preventDefault();
    } else if (event.key === 'ArrowRight') {
      setMapPx(currentPx + KEYBOARD_STEP_PX);
      event.preventDefault();
    }
  });

  resizer.addEventListener('dblclick', () => {
    draggedPx = null;
    mapEl.style.flex = '';
    scheduleInvalidate();
  });

  new ResizeObserver(() => {
    if (draggedPx !== null && !wrapped()) setMapPx(draggedPx);
    refresh();
    scheduleInvalidate();
  }).observe(region);
  refresh();

  return {
    // Called by the trace panel's own toggle, which is the only thing that
    // decides whether there is a trace to resize against.
    setEnabled(on) {
      resizer.hidden = !on;
      refresh();
    },
  };
})();

// The radargram's own size can change without the region's (a phone's
// address bar hiding changes its height, not the region's width), and
// Leaflet has to be told about any of it.
new ResizeObserver(() => scheduleInvalidate()).observe(document.getElementById('map'));

let ownTrack = null;
const cursorMarker = L.circleMarker([0, 0], {
  color: RIDAL.cursorColor,
  radius: RIDAL.cursorRadius,
}).addTo(overviewMap);
cursorMarker.setStyle({ opacity: 0 });

const trackToLatLngs = RIDAL.trackToLatLngs;

function fitOverviewToTrack(track) {
  const lines = trackToLatLngs(track);
  const points = lines.flat();
  if (points.length > 0) {
    overviewMap.fitBounds(points);
  } else {
    overviewMap.setView([0, 0], 2);
  }
}

RIDAL.fetchJson(RIDAL.apiPath("datasets", RADARGRAM_ID, "track"))
  .then((track) => {
    ownTrack = track;
    trackToLatLngs(track).forEach((latlngs) => {
      L.polyline(latlngs, {
        color: RIDAL.trackColor,
        weight: RIDAL.trackFocusWeight,
      }).addTo(overviewMap);
    });
    fitOverviewToTrack(track);
  })
  .catch((error) => {
    // Without the track there is no cursor sync and no map extent, so
    // this is worth saying out loud rather than leaving an empty map.
    RIDAL.reportError('overview-map', `Could not load this radargram's track: ${error.message}`);
    overviewMap.setView([0, 0], 2);
  });

if (GROUP) {
  RIDAL.fetchJson(RIDAL.apiPath("groups", GROUP, "tracks"))
    .then((siblings) => {
      for (const [siblingId, info] of Object.entries(siblings)) {
        if (siblingId === RADARGRAM_ID) continue;
        const pairs = trackToLatLngs(info.track).map((latlngs) => {
          const hit = RIDAL.hitLine(latlngs)
            // A function, not a string: the profile select changes the
            // radargram without reloading, so the popup has to be built
            // when it opens rather than when the track is drawn.
            .bindPopup(() =>
              RIDAL.popupContent(siblingId, info.effective_label, currentProfile()),
            )
            .addTo(overviewMap);
          const visible = L.polyline(latlngs, {
            color: RIDAL.siblingColor,
            weight: RIDAL.siblingWeight,
            opacity: RIDAL.siblingOpacity,
            interactive: false,
          }).addTo(overviewMap);
          return { visible, hit };
        });
        RIDAL.bindTrackHighlight(pairs, null, RIDAL.siblingWeight, RIDAL.siblingFocusWeight);
      }
    })
    .catch((error) => {
      // Sibling tracks are context, not the main content: the viewer is
      // still usable without them, so this is a note rather than a
      // replacement for the map.
      RIDAL.reportError('overview-map', `Could not load sibling tracks: ${error.message}`);
    });
}

// --- Cursor sync: mousemove over the radargram viewer moves a marker on
// the overview map, using the same trace-indexed lookup as
// Track::locate_trace (Rust reference implementation in
// src/server/track.rs), so this stays exact regardless of standstills
// or uneven vertex spacing -- the whole reason track.rs stores trace
// indices instead of assuming uniform spacing. ---
function locateInVertices(vertices, traceIndex) {
  if (vertices.length === 0) return null;
  if (vertices.length === 1) return [vertices[0].lat, vertices[0].lon];
  let pos = vertices.findIndex((v) => v.trace_index >= traceIndex);
  let a, b;
  if (pos === -1) { a = vertices.length - 2; b = vertices.length - 1; }
  else if (pos === 0) { a = 0; b = 1; }
  else { a = pos - 1; b = pos; }
  const va = vertices[a], vb = vertices[b];
  const span = vb.trace_index - va.trace_index;
  const t = span > 0 ? Math.min(1, Math.max(0, (traceIndex - va.trace_index) / span)) : 0;
  return [va.lat + t * (vb.lat - va.lat), va.lon + t * (vb.lon - va.lon)];
}

function locateTrace(track, traceIndex) {
  for (const seg of track.segments) {
    if (traceIndex >= seg.trace_start - 1e-6 && traceIndex <= seg.trace_end + 1e-6) {
      return locateInVertices(seg.vertices, traceIndex);
    }
  }
  return null;
}

// --- Axes (distance/TWTT/depth) for the readout, fetched once. Each
// axis degrades independently to null server-side (`/axes`'s contract)
// for fixtures that never wrote it, so the readout below must check
// each one rather than assuming all-or-nothing. ---
let axes = null;
RIDAL.fetchJson(RIDAL.apiPath("datasets", RADARGRAM_ID, "axes"))
  .then((a) => { axes = a; })
  .catch((error) => {
    // The readout degrades to trace-only, which is still useful, so this
    // goes to the console rather than the page.
    console.warn(`Could not load axes: ${error.message}`);
  });

function axisValue(array, index) {
  if (!array) return null;
  const i = Math.round(index);
  if (i < 0 || i >= array.length) return null;
  return array[i];
}

// --- Trace view (#181) ----------------------------------------------------
//
// One source trace beside the radargram. Rendering is treated as expensive,
// which is the issue's explicit choice: the panel does not follow the
// cursor, so clicking the radargram selects the trace under the click and
// that single column is fetched and drawn. The horizontal cursor line still
// follows the mouse -- that is a cheap canvas overlay, not a re-read, and
// is what makes a reflection in the radargram relatable to a point on the
// trace.
//
// The vertical axis is the radargram's own visible TWTT range. Each canvas
// row is converted to a *source* sample through `shiftAt` at the selected
// trace, so the panel stays aligned in the topographically corrected view
// instead of being disabled there (#181's preferred outcome). No unit is
// put on the amplitude axis: the processed amplitudes are gain-dependent.
const traceView = (() => {
  const toggle = document.getElementById('trace-toggle');
  const panel = document.getElementById('trace-view');
  const canvas = document.getElementById('trace-canvas');
  const label = document.getElementById('trace-label');
  const hint = document.getElementById('trace-hint');
  const zoomIn = document.getElementById('trace-zoom-in');
  const zoomOut = document.getElementById('trace-zoom-out');
  const mapEl = document.getElementById('map');
  const regionEl = document.getElementById('radar-region');
  const ctx = canvas.getContext('2d');

  // Trace index -> column. Bounded so a long session clicking across a
  // 12000-trace radargram does not accumulate every column it read; the
  // least-recently used is dropped.
  const CACHE_LIMIT = 24;
  const columns = new Map();

  const AMPLITUDE_STEP = 1.25;
  // Matches Leaflet's own CSS zoom transition, so the trace lands with the
  // radargram rather than snapping into place after it.
  const ZOOM_ANIMATION_MS = 250;

  let on = false;
  let selected = null;
  let column = null;
  // The selected column's largest |amplitude|. The drawing scale is
  // anchored to this, not to whatever is currently on screen, so panning
  // and vertical zooming do not rescale the trace -- changing the width is
  // an explicit act (the scroll wheel or the +/- buttons).
  let columnPeak = 1;
  // The raster row the cursor is on, not the source sample: in the
  // corrected view the two differ per trace, and the line should mark the
  // same *screen* height in both panels.
  let cursorRow = null;
  let amplitudeZoom = 1;
  // Bumped per selection so a slow fetch cannot overwrite a newer one.
  let pending = 0;
  let cursorQueued = false;
  let drawQueued = false;
  let animationRaf = 0;

  toggle.hidden = false;

  // A vertical marker on the radargram at the selected trace, so it is
  // obvious where the trace panel is reading from. It lives in a pane of
  // its own, above the radargram images and the pick lines.
  map.createPane('radargram-trace').style.zIndex = 403;
  let traceLine = null;
  function updateTraceLine() {
    if (!on || selected === null) {
      if (traceLine) {
        map.removeLayer(traceLine);
        traceLine = null;
      }
      return;
    }
    // A constant longitude spans every raster row; `rasterHeight` and the
    // vertical scale are read live so the line follows the topographic
    // shear and the horizontal-scale change.
    const lng = selected * G.rasterScale * (window.RIDAL_XSCALE || 1);
    const latlngs = [
      L.latLng(0, lng),
      L.latLng(-G.rasterHeight * G.verticalRasterScale, lng),
    ];
    if (traceLine) {
      traceLine.setLatLngs(latlngs);
    } else {
      traceLine = L.polyline(latlngs, {
        color: '#000',
        weight: 2,
        opacity: 0.85,
        interactive: false,
        pane: 'radargram-trace',
      }).addTo(map);
    }
  }
  // Called by `redrawOverlays` after a topographic or horizontal-scale
  // change, so the marker is re-projected through the new geometry.
  window.RIDAL_REDRAW_TRACE = updateTraceLine;

  function setOn(value) {
    on = value;
    panel.hidden = !on;
    // The region's own minimum changes with the trace panel: without the
    // class, the split resizer could shrink the region to the radargram's
    // minimum alone and push the trace onto its own row.
    regionEl.classList.toggle('has-trace', on);
    splitResizer.reclamp();
    toggle.setAttribute('aria-pressed', String(on));
    toggle.textContent = on ? 'Hide trace view' : 'Trace view';
    traceResizer.setEnabled(on);
    updateTraceLine();
    // The radargram's width changes when the panel appears and Leaflet has
    // to be told. The region's observer would catch it a frame later, which
    // can leave a gutter of stale tiles.
    map.invalidateSize({ pan: false });
    if (on) draw();
  }

  toggle.addEventListener('click', () => setOn(!on));
  zoomIn.addEventListener('click', () => {
    amplitudeZoom *= AMPLITUDE_STEP;
    draw();
  });
  zoomOut.addEventListener('click', () => {
    amplitudeZoom /= AMPLITUDE_STEP;
    draw();
  });
  canvas.addEventListener('wheel', (event) => {
    event.preventDefault();
    amplitudeZoom *= event.deltaY < 0 ? AMPLITUDE_STEP : 1 / AMPLITUDE_STEP;
    draw();
  }, { passive: false });

  // Dragging the trace pans the radargram vertically, so the two stay
  // synced while the user looks up and down the trace.
  let panning = null;
  canvas.addEventListener('pointerdown', (event) => {
    if (!on || selected === null || !column) return;
    panning = { y: event.clientY };
    canvas.setPointerCapture(event.pointerId);
    event.preventDefault();
  });
  canvas.addEventListener('pointermove', (event) => {
    if (!panning) return;
    const dy = event.clientY - panning.y;
    if (dy === 0) return;
    panning.y = event.clientY;
    // Negative offset: the content follows the drag (drag down reveals
    // earlier samples), which is what a map's own drag does.
    map.panBy([0, -dy], { animate: false });
  });
  canvas.addEventListener('pointerup', (event) => {
    if (!panning) return;
    panning = null;
    if (canvas.hasPointerCapture(event.pointerId)) {
      canvas.releasePointerCapture(event.pointerId);
    }
  });

  function columnFor(trace) {
    if (columns.has(trace)) {
      const value = columns.get(trace);
      // Refresh recency.
      columns.delete(trace);
      columns.set(trace, value);
      return Promise.resolve(value);
    }
    const url = RIDAL.apiPath("datasets", RADARGRAM_ID, "traces", trace);
    return RIDAL.fetchJson(url)
      .catch((error) => {
        // A GET is idempotent, and a transport-level failure (no HTTP
        // status) can be transient -- a browser stretched thin by the
        // radargram's own tile requests, for instance. Retry once after a
        // short pause; an HTTP status is the server's answer and is not
        // retried.
        if (error.status !== undefined) throw error;
        return new Promise((resolve) => setTimeout(resolve, 250)).then(() =>
          RIDAL.fetchJson(url),
        );
      })
      .then((data) => {
        const value = Float32Array.from(data.amplitude);
        columns.set(trace, value);
        if (columns.size > CACHE_LIMIT) {
          columns.delete(columns.keys().next().value);
        }
        return value;
      });
  }

  function clearError() {
    panel.querySelectorAll('.error-overlay').forEach((box) => box.remove());
  }

  function peakOf(values) {
    let peak = 0;
    for (let i = 0; i < values.length; i++) {
      const a = Math.abs(values[i]);
      if (a > peak) peak = a;
    }
    return peak > 0 ? peak : 1;
  }

  function select(trace) {
    selected = Math.max(0, Math.min(SOURCE_WIDTH - 1, Math.round(trace)));
    cursorRow = null;
    hint.hidden = true;
    clearError();
    label.textContent = `trace: ${selected}`;
    updateTraceLine();
    const token = ++pending;
    columnFor(selected)
      .then((value) => {
        // A later click may have landed while this fetch was in flight.
        if (token !== pending) return;
        column = value;
        columnPeak = peakOf(value);
        draw();
      })
      .catch((error) => {
        if (token !== pending) return;
        RIDAL.reportError('trace-view', `Could not load trace ${selected}: ${error.message}`);
      });
  }

  // The source-sample span the radargram currently shows, over the
  // selected trace. `rasterRow = sample + shiftAt(trace)`, so inverting at
  // one trace gives an exact window even in the corrected view.
  function visibleSampleRange() {
    const bounds = map.getBounds();
    const rows = [
      -bounds.getNorth() / G.verticalRasterScale,
      -bounds.getSouth() / G.verticalRasterScale,
    ];
    const shift = shiftAt(selected);
    return [Math.min(rows[0], rows[1]) - shift, Math.max(rows[0], rows[1]) - shift];
  }

  // The range the radargram will show once the in-flight zoom finishes.
  // Leaflet fires `zoomanim` at the *start* of the CSS transition and
  // `zoomend` at the end, so the target has to be projected from the
  // event's own centre/zoom rather than read from `getBounds()`.
  function targetRangeForZoom(event) {
    const size = map.getSize();
    const centerPx = map.project(event.center, event.zoom);
    const half = size.divideBy(2);
    const nw = map.unproject(centerPx.subtract(half), event.zoom);
    const se = map.unproject(centerPx.add(half), event.zoom);
    const shift = shiftAt(selected);
    const rows = [-nw.lat / G.verticalRasterScale, -se.lat / G.verticalRasterScale];
    return [Math.min(rows[0], rows[1]) - shift, Math.max(rows[0], rows[1]) - shift];
  }

  function stopAnimation() {
    if (animationRaf) {
      cancelAnimationFrame(animationRaf);
      animationRaf = 0;
    }
  }

  map.on('zoomanim', (event) => {
    if (!on || selected === null || !column) return;
    const from = visibleSampleRange();
    const to = targetRangeForZoom(event);
    stopAnimation();
    const start = performance.now();
    const step = (now) => {
      const t = Math.min(1, (now - start) / ZOOM_ANIMATION_MS);
      // Smoothstep, close enough to Leaflet's ease that the two arrive
      // together without a visible lag.
      const s = t * t * (3 - 2 * t);
      draw([from[0] + (to[0] - from[0]) * s, from[1] + (to[1] - from[1]) * s]);
      animationRaf = t < 1 ? requestAnimationFrame(step) : 0;
    };
    animationRaf = requestAnimationFrame(step);
  });

  function fitCanvas() {
    const ratio = window.devicePixelRatio || 1;
    const width = canvas.clientWidth;
    const height = canvas.clientHeight;
    if (canvas.width !== Math.round(width * ratio) ||
        canvas.height !== Math.round(height * ratio)) {
      canvas.width = Math.round(width * ratio);
      canvas.height = Math.round(height * ratio);
    }
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    return { width, height };
  }

  // `rangeOverride` is used by the zoom animation, which draws an
  // interpolated window; every other caller lets it read the live one.
  function draw(rangeOverride) {
    if (!on || selected === null || !column) return;
    const { width, height } = fitCanvas();
    if (width <= 0 || height <= 0) return;
    ctx.clearRect(0, 0, width, height);

    const [sampleTop, sampleBottom] = rangeOverride || visibleSampleRange();
    const span = sampleBottom - sampleTop;
    if (!(span > 0)) return;

    const first = Math.max(0, Math.floor(sampleTop));
    const last = Math.min(column.length - 1, Math.ceil(sampleBottom));
    if (last < first) return;

    // Fixed scale, anchored to the whole trace: the largest |amplitude| of
    // the *selected column* maps to half the canvas width, times the user's
    // zoom. Deliberately not the visible window's peak -- that made the
    // trace breathe wider and narrower as the user panned.
    const half = (width / 2) * amplitudeZoom;
    const xOf = (amplitude) => width / 2 + (amplitude / columnPeak) * half;
    const yOf = (sample) => ((sample - sampleTop) / span) * height;

    const accent =
      getComputedStyle(document.documentElement).getPropertyValue('--color-accent').trim() ||
      '#1f6f8b';

    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, width, height);
    ctx.clip();

    // Zero line.
    ctx.strokeStyle = 'rgba(127, 127, 127, 0.5)';
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(width / 2, 0);
    ctx.lineTo(width / 2, height);
    ctx.stroke();

    // The trace itself.
    ctx.strokeStyle = accent;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(xOf(column[first]), yOf(first));
    for (let s = first + 1; s <= last; s++) {
      ctx.lineTo(xOf(column[s]), yOf(s));
    }
    ctx.stroke();
    ctx.restore();

    // Where on the trace the cursor is. Converted through the selected
    // trace's own shift, so the line sits at the same height as the
    // cursor in the radargram.
    if (cursorRow !== null) {
      const cursorSample = cursorRow - shiftAt(selected);
      if (cursorSample >= sampleTop && cursorSample <= sampleBottom) {
        ctx.strokeStyle = accent;
        ctx.globalAlpha = 0.5;
        ctx.beginPath();
        ctx.moveTo(0, yOf(cursorSample));
        ctx.lineTo(width, yOf(cursorSample));
        ctx.stroke();
        ctx.globalAlpha = 1;
      }
    }
  }

  // Clicking the radargram chooses the trace. Picking owns clicks while it
  // is active, so selecting a trace then would silently move the trace
  // panel instead of placing a vertex.
  map.on('click', (event) => {
    if (!on || mapEl.classList.contains('picking')) return;
    const [trace, sample] = RIDAL_TO_INDEX(event.latlng);
    if (trace < 0 || trace >= SOURCE_WIDTH) return;
    // The corrected view's no-data wedge is not a trace position.
    if (sample < 0 || sample > G.sourceHeight) return;
    select(trace);
  });

  // A pan, zoom, topo toggle or scale change moves the visible sample
  // window, so the panel has to be redrawn to stay in sync. `move` covers
  // the continuous part of a pan (and the trace panel's own drag-to-pan);
  // the end events settle the final, authoritative window and cancel any
  // zoom animation still running.
  map.on('move', () => {
    if (animationRaf || !on || selected === null) return;
    if (drawQueued) return;
    drawQueued = true;
    requestAnimationFrame(() => {
      drawQueued = false;
      draw();
    });
  });
  map.on('moveend zoomend', () => {
    stopAnimation();
    draw();
  });
  window.addEventListener('resize', () => {
    if (on) draw();
  });

  function setCursorRow(row) {
    cursorRow = row;
    if (!on || selected === null || cursorQueued) return;
    cursorQueued = true;
    requestAnimationFrame(() => {
      cursorQueued = false;
      draw();
    });
  }

  return { setCursorRow };
})();

const readout = document.getElementById('cursor-readout');
// Seeded, and never blanked below, so the readout always occupies exactly
// one line. An empty readout used to take no width, sit on the controls
// row, and then wrap to a row of its own the moment it was populated --
// growing the whole controls block and shifting the radargram down by a
// line. On a phone that happens on every tap: moving a finger from the map
// to a button fires `mouseout` (blank, shift up), tapping the map fires
// `mousemove` (populate, shift down). It made the first tap on any control
// land on whatever had just moved out from under it, and put a placed
// vertex a line higher than where it was tapped.
readout.textContent = `trace - / ${SOURCE_WIDTH}`;
map.on('mousemove', (event) => {
  const viewerX = event.latlng.lng / xScale;
  const viewerY = -event.latlng.lat;
  if (viewerX < 0 || viewerX > G.rasterWidth || viewerY < 0 || viewerY > G.rasterHeight) {
    cursorMarker.setStyle({ opacity: 0 });
    readout.textContent = '';
    return;
  }
  const traceIndex = viewerX / G.rasterScale;
  const rasterRow = viewerY / G.verticalRasterScale;
  // Distance/TWTT/depth are all indexed by *source* sample, so a raster
  // row in the corrected view has to invert the shear first -- the
  // cursor-sync twin of `toIndex` in picker.js.
  const sampleIndex = rasterRow - shiftAt(traceIndex);

  // Mark the same screen height in the trace panel. The row, not the
  // sample: the panel re-applies its own trace's shift (#181).
  traceView.setCursorRow(rasterRow);

  let text = `trace ${Math.round(traceIndex)} / ${SOURCE_WIDTH}`;
  if (axes) {
    const distance = axisValue(axes.distance, traceIndex);
    const twtt = axisValue(axes.twtt, sampleIndex);
    const depth = axisValue(axes.depth, sampleIndex);
    // Labelled like every other term: distance and depth are both in
    // metres, so an unlabelled one next to `depth` is two numbers in the
    // same unit with nothing saying which is which. Abbreviated to match
    // `elev.` and keep the line from wrapping on a phone.
    if (distance !== null) text += ` · dist. ${distance.toFixed(1)} m`;
    if (twtt !== null) text += ` · TWTT ${twtt.toFixed(1)} ns`;
    if (depth !== null) text += ` · depth ${depth.toFixed(1)} m`;
    // Point elevation = this trace's own surface elevation minus this
    // sample's depth -- shown in every view, not only the corrected one,
    // since the point of it is to let someone read off a radargram's raw
    // `elevation` values (spikes included) well enough to set a trusted
    // range for the corrected view in the catalog's properties dialog.
    // `axes.elevation` is per-*trace*, so it is looked up by trace index
    // even though `axisValue` is the same helper the per-sample axes use.
    const surfaceElevation = axisValue(axes.elevation, traceIndex);
    if (surfaceElevation !== null && depth !== null) {
      text += ` · elev. ${(surfaceElevation - depth).toFixed(1)} m`;
    }
  }
  readout.textContent = text;

  if (ownTrack) {
    const pos = locateTrace(ownTrack, traceIndex);
    if (pos) {
      cursorMarker.setLatLng(pos);
      cursorMarker.setStyle({ opacity: 1 });
    }
  }
});
map.on('mouseout', () => {
  cursorMarker.setStyle({ opacity: 0 });
  // The last reading deliberately stays. Blanking it resized the controls
  // block (see the seed above), and keeping it is better anyway: on a touch
  // screen the value is only readable *after* the finger lifts.
});

// --- Metadata dialog: a button opening a <dialog> with the server's
// curated, human-readable attribute view (prettified labels, merged
// units, rounded floats -- see routes.rs::build_metadata_entries),
// plus the processing steps/log in their own <details>. ---
const dialog = document.getElementById('metadata-dialog');
document.getElementById('metadata-button').addEventListener('click', () => {
  RIDAL.fetchJson(RIDAL.apiPath("datasets", RADARGRAM_ID, "attributes"))
    .then((data) => {
      const tbody = document.querySelector('#metadata-table tbody');
      tbody.innerHTML = '';
      data.entries.forEach(({ label, value }) => {
        const row = document.createElement('tr');
        const keyCell = document.createElement('th');
        keyCell.textContent = label;
        const valCell = document.createElement('td');
        valCell.textContent = value;
        row.append(keyCell, valCell);
        tbody.appendChild(row);
      });

      const stepsList = document.getElementById('processing-steps-list');
      stepsList.innerHTML = '';
      data.processing_steps.forEach((step) => {
        const li = document.createElement('li');
        li.textContent = step;
        stepsList.appendChild(li);
      });

      // The log is `step (duration: Xs):\tdetail\n...` -- split on
      // newlines, then turn each step's embedded tab into its own
      // indented line, so it renders as one line per step detail
      // instead of collapsing into an unreadable run-on paragraph.
      const logLines = data.processing_log
        .split('\n')
        .map((line) => line.replace(/\t/g, '\n  '));
      document.getElementById('processing-log').textContent = logLines.join('\n');

      // Its own <details>, not a metadata-table row: an acquisition
      // that merged many inputs can have an arbitrarily long list of
      // arbitrarily long paths.
      const filepathsList = document.getElementById('filepaths-list');
      filepathsList.innerHTML = '';
      data.original_filepaths.forEach((path) => {
        const li = document.createElement('li');
        li.textContent = path;
        filepathsList.appendChild(li);
      });

      dialog.showModal();
    })
    .catch((error) => {
      // The button did nothing visible on failure before; now the
      // dialog opens and says why, which is the whole point of having a
      // structured error envelope.
      const tbody = document.querySelector('#metadata-table tbody');
      tbody.innerHTML = '';
      const row = document.createElement('tr');
      const cell = document.createElement('td');
      cell.colSpan = 2;
      cell.className = 'error-text';
      cell.textContent = `Could not load metadata: ${error.message}`;
      row.appendChild(cell);
      tbody.appendChild(row);
      dialog.showModal();
    });
});
document.getElementById('metadata-close').addEventListener('click', () => dialog.close());

/* --- Downloads -----------------------------------------------------------
 *
 * Owned here rather than in picker.js: three of the five need no project
 * and no write access, so they have to work on a read-only catalog where
 * the picker never initialises at all.
 *
 * Each is a plain navigation to an endpoint that sets
 * `Content-Disposition: attachment`, so the browser saves the file and the
 * page stays where it is -- no blob building, and a failure lands on the
 * server's own error envelope rather than being swallowed.
 */
(function setupDownloads() {
  const menu = document.getElementById('download-menu');
  if (!menu) return;

  const datasetUrl = RIDAL.apiPath("datasets", RADARGRAM_ID);
  const picksUrl = `${datasetUrl}/interpretations/${CFG.user}`;
  /* Fetched rather than navigated to, so a refusal is shown on this page
   * rather than throwing the viewer away to render the error envelope as
   * a document. `dl-radargram` is the exception and says why. */
  const go = (url) => {
    menu.open = false;
    return RIDAL.download(url, 'download-error');
  };

  /* The two pick downloads are derived from what is *saved*. Offering them
   * over unsaved edits would hand back something that quietly disagrees
   * with what is on screen, so they say so instead. */
  function picksAreStale() {
    if (window.RIDAL_PICKS_DIRTY) {
      RIDAL.reportError(
        'map',
        'Save your picks first -- a download is built from the saved ' +
          'interpretation, not from what is on screen.',
      );
      menu.open = false;
      return true;
    }
    return false;
  }

  const bind = (id, handler) => {
    const button = document.getElementById(id);
    if (button) button.addEventListener('click', handler);
  };

  bind('dl-track', () => go(`${datasetUrl}/track.geojson`));
  /* The one download left as a navigation. A radargram reaches 145 MB and
   * the server streams it precisely so nothing holds it whole, which
   * fetching into a blob here would undo. It also has no failure a person
   * can act on: the permission cases never reach the menu. */
  bind('dl-radargram', () => {
    menu.open = false;
    window.location.href = `${datasetUrl}/download`;
  });
  bind('dl-raw', () => {
    if (!picksAreStale()) go(`${picksUrl}/raw`);
  });

  // --- Layer points (the level 2 product) ---
  const pointsDialog = document.getElementById('download-dialog');
  bind('dl-points', () => {
    if (picksAreStale()) return;
    menu.open = false;
    pointsDialog.showModal();
  });
  document
    .getElementById('download-close')
    .addEventListener('click', () => pointsDialog.close());
  document.getElementById('download-go').addEventListener('click', () => {
    const spacing = document.getElementById('download-spacing').value;
    // One select covers both the file format and its coordinates: "GeoJSON
    // in native coordinates" is a single choice to a user even though it is
    // two parameters on the wire.
    const choice = document.getElementById('download-format').value;
    const format = choice === 'csv' ? 'csv' : 'geojson';
    const crs = choice === 'geojson-native' ? '&crs=native' : '';
    // Admin only, and the server enforces the role regardless of the markup.
    const everyUser = document.getElementById('download-every-user');
    const every = everyUser && everyUser.checked ? '&every_user=true' : '';
    pointsDialog.close();
    go(
      `${picksUrl}/level2?spacing=${encodeURIComponent(spacing)}` +
        `&format=${encodeURIComponent(format)}${crs}${every}`,
    );
  });

  // --- Derived layer points ---
  const derivedDialog = document.getElementById('derived-download-dialog');
  bind('dl-derived-points', () => {
    menu.open = false;
    derivedDialog.showModal();
  });
  document
    .getElementById('derived-download-close')
    .addEventListener('click', () => derivedDialog.close());
  document
    .getElementById('derived-download-go')
    .addEventListener('click', () => {
      const spacing = document.getElementById('derived-download-spacing').value;
      const choice = document.getElementById('derived-download-format').value;
      const format = choice === 'csv' ? 'csv' : 'geojson';
      const crs = choice === 'geojson-native' ? '&crs=native' : '';
      const include = document.getElementById('derived-download-include-unlisted').checked
        ? '&include_unlisted=true'
        : '';
      derivedDialog.close();
      go(
        `${datasetUrl}/derived/level2?spacing=${encodeURIComponent(spacing)}` +
          `&format=${encodeURIComponent(format)}${crs}${include}`,
      );
    });

  // --- Rendered image ---
  const imageDialog = document.getElementById('image-dialog');
  const widthSelect = document.getElementById('image-width');
  const formatSelect = document.getElementById('image-format');
  const qualityField = document.getElementById('image-quality-field');
  const qualitySelect = document.getElementById('image-quality');
  const estimate = document.getElementById('image-estimate');

  /* Offered widths, smallest first, ending at one pixel per trace.
   *
   * Full resolution is the default and the last entry. The viewer renders
   * 1:1, so "what the viewer shows" and "full resolution" are now the same
   * option; there used to be a separate entry for the viewer's capped
   * raster, which no longer exists.
   *
   * Width is *not* a speed dial. Rendering reads the whole source array
   * whichever width is asked for, so the time barely moves with it: on a
   * release build, a 12187x3678 radargram from a 145 MB file took 1.9 s at
   * 900 px and 2.4 s at full resolution, and 6 ms once cached. The choice
   * here is about file size and detail, which is what the estimate says. */
  function widthOptions() {
    const presets = [1000, 2000, 4000, 8000, 16000]
      .filter((w) => w > 0 && w < SOURCE_WIDTH)
      .sort((a, b) => a - b);
    return [
      ...presets.map((w) => new Option(`${w} px`, String(w))),
      new Option(`${SOURCE_WIDTH} px - full resolution`, String(SOURCE_WIDTH)),
    ];
  }

  function describeChoice() {
    const width = Number(widthSelect.value) || SOURCE_WIDTH;
    // `G.rasterHeight`, not the source height: the corrected view's
    // download is the taller, sheared raster, and the estimate should
    // match what `G.view` above will actually ask the server for.
    const height = Math.max(1, Math.round((G.rasterHeight * width) / SOURCE_WIDTH));
    const megapixels = (width * height) / 1e6;
    // Deliberately about size rather than time. An earlier version warned
    // that large widths were slow, from timings taken on a debug build --
    // they were 15 to 35 times the real figure, and the warning would have
    // steered people away from full resolution for no reason.
    estimate.textContent =
      `${width} \u00d7 ${height} px (${megapixels.toFixed(1)} MP). ` +
      (formatSelect.value === 'jpeg'
        ? 'JPEG is much smaller but lossy, and cannot exceed 65535 px.'
        : 'PNG is lossless; at this size the file may be tens of megabytes.');
  }

  widthSelect.replaceChildren(...widthOptions());
  // Full resolution: the viewer no longer downscales, so defaulting to
  // anything less would hand back less than what is on screen.
  widthSelect.selectedIndex = widthSelect.options.length - 1;
  describeChoice();

  widthSelect.addEventListener('change', describeChoice);
  formatSelect.addEventListener('change', () => {
    qualityField.hidden = formatSelect.value !== 'jpeg';
    describeChoice();
  });

  bind('dl-image', () => {
    menu.open = false;
    // Re-estimated on open, not only on width/format change: the
    // corrected view can have been toggled since the dialog last
    // recomputed, which changes the height half of the estimate.
    describeChoice();
    imageDialog.showModal();
  });
  document
    .getElementById('image-close')
    .addEventListener('click', () => imageDialog.close());
  document.getElementById('image-go').addEventListener('click', () => {
    const params = new URLSearchParams({
      profile: currentProfile(),
      width: widthSelect.value,
      format: formatSelect.value,
    });
    if (formatSelect.value === 'jpeg') params.set('quality', qualitySelect.value);
    // The same cache-busting the chunk URLs carry, and for the same
    // reason: `RIDAL.download` fetches, the image response sets no
    // validators, and after an elevation-window edit this URL is otherwise
    // byte-identical while the server now renders a different height.
    if (G.view === 'topo' && G.fingerprint) params.set('fp', G.fingerprint);
    imageDialog.close();
    // `G.view`, not a fixed "standard": the downloaded image matches
    // whatever is on screen, corrected view included (#168).
    go(`${datasetUrl}/views/${G.view}/image?${params}`);
  });
})();

/* --- Hide the radargram (#230) --------------------------------------------
 *
 * Picks, derived layers and fills live in their own Leaflet panes above the
 * image chunks, so making the chunks transparent leaves exactly them. Useful
 * where a line runs along the reflector it follows in a similar colour and
 * disappears into it.
 *
 * Deliberately not persisted: this is a momentary "let me see my lines"
 * action, and a viewer that opened with no radargram and no explanation
 * would read as a broken render.
 */
(function setupRadargramToggle() {
  const toggle = document.getElementById('radargram-toggle');
  if (!toggle) return;

  toggle.hidden = false;
  toggle.addEventListener('click', () => {
    // `radargramVisible` is the one copy of this state, because
    // `addChunksInView` reads it too; a second flag here could disagree
    // with what a newly placed chunk is given.
    setRadargramVisible(!radargramVisible);
    toggle.textContent = radargramVisible ? 'Hide radargram' : 'Show radargram';
    toggle.setAttribute('aria-pressed', String(radargramVisible));
  });
})();

/* --- Topographic correction (#168) ---------------------------------------
 *
 * A render-time-only vertical shear of the existing `data` array -- see
 * `src/render/topo.rs`'s module docs for the geometry. Nothing here is
 * precomputed or stored; toggling the checkbox changes which view the
 * viewer requests chunks/overviews from and updates the shared
 * `window.RIDAL_GEOMETRY` picker.js reads for placing and drawing picks.
 */
(function setupTopoView() {
  const toggle = document.getElementById('topo-toggle');
  const row = document.getElementById('topo-toggle-row');
  const recentreButton = document.getElementById('topo-recentre');
  if (!toggle) return;

  const geometryUrl = RIDAL.apiPath("datasets", RADARGRAM_ID, "views", "topo", "geometry");

  /* Fetch and validate the geometry. Returns the parsed body on success;
   * on failure, disables the checkbox with the reason as its `title` and
   * returns null -- the rule that unavailability must never be silent.
   *
   * How loudly depends on what can fix it, which the server says in the
   * error `code`:
   *
   * - `topo_window_invalid` -- the project's configured floor/cap is what
   *   excludes the data. Somebody set that, it is wrong, and editing it
   *   fixes it, so it gets a visible warning naming the reason. A
   *   checkbox that merely refuses to enable is how this was reported:
   *   the cause was real, actionable, and only in a tooltip.
   * - anything else -- the radargram itself cannot support the view (no
   *   `elevation`, no `depth`). Nothing to act on, so the disabled
   *   checkbox explaining itself on hover is the whole story; a banner on
   *   every such file would be noise.
   */
  const WARNING_HOST = 'download-error';

  /* Explain the control's state on both halves of it. Leaving the reason
   * only on the label meant the checkbox itself kept whatever the template
   * rendered ("Checking availability…") forever, so hovering the thing you
   * just failed to tick explained nothing. */
  function setReason(reason) {
    row.title = reason;
    toggle.title = reason;
  }

  async function fetchGeometry() {
    try {
      const response = await fetch(geometryUrl);
      if (!response.ok) {
        const failure = await response.json().catch(() => null);
        const reason =
          failure?.error?.message || RIDAL.upstreamMessage(response.status);
        const recoverable = failure?.error?.code === 'topo_window_invalid';
        toggle.checked = false;
        // A bad window stays *enabled*: the fix is in the catalog's
        // properties dialog, the geometry is re-fetched on every toggle,
        // and so ticking the box again is how you find out you fixed it.
        // Disabling it would leave the one recoverable failure with no way
        // to retry short of a reload. An unsupported file has nothing to
        // retry and stays disabled.
        toggle.disabled = !recoverable;
        setReason(reason);
        if (recoverable) {
          RIDAL.reportProblem(
            WARNING_HOST,
            `Topographic correction is unavailable: ${reason}`,
          );
        }
        return null;
      }
      setReason('');
      toggle.disabled = false;
      return await response.json();
    } catch (error) {
      toggle.checked = false;
      // A transport failure is recoverable too -- the server may simply
      // have been restarting -- so the control stays usable.
      toggle.disabled = false;
      setReason(`Could not check availability: ${error.message}`);
      return null;
    }
  }

  const mapEl = document.getElementById('map');

  /* Say what the correction did to this radargram's elevations.
   *
   * Every one of these changes the geometry on screen, and #168's rule is
   * that none of them may be silent -- a reader has to be able to tell a
   * surface they are looking at from one this view invented. Previously
   * only `suspect` was surfaced, so an interpolated gap, a clamped spike
   * or a cropped floor all passed without a word.
   *
   * One combined message rather than one per condition: `reportProblem`
   * replaces the host's contents, so separate calls would leave only
   * whichever fired last. */
  function reportDiagnostics(d) {
    if (!d) return;
    const notes = [];
    if (d.suspect) {
      notes.push(
        "this radargram's elevation spread looks like it may contain GPS spikes " +
          'rather than real topography',
      );
    }
    if (d.interpolated_count > 0) {
      notes.push(
        `${d.interpolated_count} trace${d.interpolated_count === 1 ? '' : 's'} had no ` +
          'elevation and were interpolated from their neighbours',
      );
    }
    if (d.clamped_count > 0) {
      notes.push(
        `${d.clamped_count} trace${d.clamped_count === 1 ? '' : 's'} sat above the ` +
          'surface cap and were flattened to it',
      );
    }
    if (d.cropped_rows > 0) {
      notes.push(`the floor is cropping ${d.cropped_rows} rows off the bottom`);
    }
    if (notes.length === 0) return;
    RIDAL.reportProblem(
      WARNING_HOST,
      `Topographic correction: ${notes.join('; ')}. ` +
        "The catalog page's properties dialog sets the floor and surface cap.",
      'note',
    );
  }

  /* Apply a fetched geometry (or its absence, for turning the view back
   * off) to the shared state, in the order #168 specifies: update the
   * shared geometry state and view name, rebuild chunk overlays and
   * bounds, redraw picks, then recentre. Any other order draws something
   * through a mapping that no longer applies.
   *
   * "Recentre" here means keeping the same underlying (trace,
   * source-sample) point under the same screen position, not refitting
   * the view -- refitting to the data band on the way in and resetting to
   * the start of the radargram on the way out (the previous behaviour)
   * both threw away where the person doing the toggling was actually
   * looking. Only the vertical placement needs correcting for the shear;
   * the horizontal (trace) mapping and the zoom level are untouched by
   * this view, so `lng`/zoom are carried straight through. */
  function applyView(on, geometry) {
    const center = map.getCenter();
    const trace = center.lng / xScale / G.rasterScale;
    const oldRasterRow = -center.lat / G.verticalRasterScale;
    const sourceSample = oldRasterRow - shiftAt(trace);

    if (on && geometry) {
      G.view = "topo";
      G.rasterHeight = geometry.raster_height;
      G.nRows = Math.ceil(geometry.raster_height / CHUNK_SIZE);
      G.shift = Float32Array.from(geometry.shift);
      G.fingerprint = geometry.fingerprint;
      // Matches `renderer.rs`'s `PAD_VALUE` exactly, so the wedge of
      // genuine no-data above the sheared surface blends into the map's
      // own background instead of showing a hard-edged rectangle against
      // it (#168 feedback).
      mapEl.classList.add('map-topo');
      reportDiagnostics(geometry.diagnostics);
    } else {
      G.view = "standard";
      G.rasterHeight = CFG.viewerHeight;
      G.nRows = CFG.nRows;
      G.shift = null;
      G.fingerprint = null;
      mapEl.classList.remove('map-topo');
    }
    recentreButton.hidden = G.view !== "topo";

    const newRasterRow = sourceSample + shiftAt(trace);
    const newLat = -newRasterRow * G.verticalRasterScale;
    map.setView([newLat, center.lng], map.getZoom(), { animate: false });

    loadChunks(map, currentProfile(), xScale);
    redrawOverlays();
  }

  /* Fit the view vertically to the data band over the traces currently on
   * screen -- the way back from panning into empty space above or below
   * the corrected surface (#168). */
  function recentreToData() {
    if (G.view !== "topo" || !G.shift) return;
    const bounds = map.getBounds();
    const traceLo = Math.max(0, Math.floor(bounds.getWest() / xScale / G.rasterScale));
    const traceHi = Math.min(
      G.sourceWidth - 1,
      Math.ceil(bounds.getEast() / xScale / G.rasterScale),
    );
    let lo = Infinity;
    let hi = -Infinity;
    for (let t = traceLo; t <= traceHi; t++) {
      const s = G.shift[t];
      if (s < lo) lo = s;
      if (s > hi) hi = s;
    }
    if (!isFinite(lo)) return;
    // Clamped to the rows the raster actually has. A configured floor
    // crops the corrected raster, so `shift + sourceHeight` can reach well
    // past its bottom -- and fitting to rows that are never rendered would
    // park the view in empty space, which is the opposite of what this
    // button is for.
    const bandTop = Math.max(0, lo);
    const bandBottom = Math.min(hi + G.sourceHeight, G.rasterHeight);
    if (bandBottom <= bandTop) return;
    map.fitBounds(
      [
        [-bandBottom, bounds.getWest()],
        [-bandTop, bounds.getEast()],
      ],
      { animate: false },
    );
  }

  toggle.addEventListener('change', async () => {
    const on = toggle.checked;
    toggle.disabled = true;
    // Re-fetched on every toggle, not just the first: an elevation-range
    // edit made in the catalog's properties dialog since page load must
    // take effect the next time this view is turned on.
    const geometry = on ? await fetchGeometry() : null;
    if (on && !geometry) {
      toggle.checked = false;
      toggle.disabled = false;
      return;
    }
    applyView(on, geometry);
    toggle.disabled = false;
  });

  recentreButton.addEventListener('click', recentreToData);

  // Availability check on load, so the checkbox starts in its correct
  // state (disabled with a reason, or enabled) rather than offering a
  // view that will only fail on the first toggle.
  fetchGeometry();
})();
