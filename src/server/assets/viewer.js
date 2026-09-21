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

function addChunksInView(profile, scale) {
  for (const [x, y] of chunksInView(scale)) {
    const key = `${x},${y}`;
    if (chunksAdded.has(key)) continue;
    chunksAdded.add(key);
    const layer = L.imageOverlay(chunkImage(x, y, profile), chunkBounds(x, y, scale)).addTo(map);
    chunkLayers.push(layer);
  }
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

// --- Resizable split between the radargram and overview map. Only
// meaningful when the two are actually laid out side by side --
// `.layout` is flex-wrap: wrap, so on a narrow screen they stack, at
// which point a horizontal drag handle makes no sense and is hidden.
// Detected exactly (offsetTop equality), not guessed via a media-query
// breakpoint, since the wrap point depends on both panes' flex-basis. ---
(function setupSplitResizer() {
  const layout = document.getElementById('viewer-layout');
  const resizer = document.getElementById('split-resizer');
  const mapEl = document.getElementById('map');
  const overviewEl = document.getElementById('overview-map');
  const MIN_PANE_PX = 200;
  const KEYBOARD_STEP_PX = 24;

  function isSideBySide() {
    return mapEl.offsetTop === overviewEl.offsetTop;
  }

  // Leaflet does not re-lay its tiles when its container is resized by
  // something other than a window resize event it listens for itself --
  // it has to be told. Throttled to one call per frame since pointermove
  // fires far more often than the browser can usefully repaint.
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

  function updateSideBySideState() {
    const sideBySide = isSideBySide();
    // `.is-hidden` only flips visibility, never `display` -- see the
    // rule in app.css for why removing the handle from flow makes the
    // layout oscillate across the wrap threshold.
    resizer.classList.toggle('is-hidden', !sideBySide);
    if (!sideBySide) {
      // Clear the override so the CSS defaults resume when the layout
      // wraps back to stacked -- otherwise a resize made while wide
      // would stick around, meaninglessly, once stacked.
      mapEl.style.flex = '';
    }
  }

  function setMapBasisPx(px) {
    const layoutWidth = layout.getBoundingClientRect().width;
    const resizerWidth = resizer.getBoundingClientRect().width;
    const maxPx = Math.max(MIN_PANE_PX, layoutWidth - resizerWidth - MIN_PANE_PX);
    const clamped = Math.min(Math.max(px, MIN_PANE_PX), maxPx);
    // `0 0 <px>` (not just flex-basis) zeroes out grow/shrink on this
    // pane specifically, so the drag result is exactly what was set --
    // the overview pane's own flex:1 absorbs whatever space is left.
    mapEl.style.flex = `0 0 ${clamped}px`;
    resizer.setAttribute(
      'aria-valuenow',
      Math.round((clamped / (layoutWidth - resizerWidth)) * 100),
    );
    scheduleInvalidate();
  }

  let dragging = false;
  resizer.addEventListener('pointerdown', (event) => {
    if (!isSideBySide()) return;
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
    if (!isSideBySide()) return;
    const currentPx = mapEl.getBoundingClientRect().width;
    if (event.key === 'ArrowLeft') {
      setMapBasisPx(currentPx - KEYBOARD_STEP_PX);
      event.preventDefault();
    } else if (event.key === 'ArrowRight') {
      setMapBasisPx(currentPx + KEYBOARD_STEP_PX);
      event.preventDefault();
    }
  });

  updateSideBySideState();
  new ResizeObserver(() => {
    updateSideBySideState();
    scheduleInvalidate();
  }).observe(layout);

  // The map pane gets its own observer, deliberately not the one above:
  // that callback writes `mapEl.style.flex`, so pointing it at `mapEl`
  // would let it feed itself. This one only tells Leaflet the pane
  // resized, which is what a phone's address bar hiding does.
  new ResizeObserver(() => scheduleInvalidate()).observe(mapEl);
})();

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

  let text = `trace ${Math.round(traceIndex)} / ${SOURCE_WIDTH}`;
  if (axes) {
    const distance = axisValue(axes.distance, traceIndex);
    const twtt = axisValue(axes.twtt, sampleIndex);
    const depth = axisValue(axes.depth, sampleIndex);
    if (distance !== null) text += ` · ${distance.toFixed(1)} m`;
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
          failure?.error?.message || `Could not check availability (${response.status}).`;
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
