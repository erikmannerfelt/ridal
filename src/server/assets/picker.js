/* Picking reflectors on the radargram (#102).
 *
 * First-party, embedded via assets.rs, loaded after viewer.js so `L`,
 * `RIDAL` and `window.RIDAL_VIEWER` all exist. Deliberately NOT under
 * assets/vendor/ -- scripts/vendor_leaflet.sh does `rm -rf` on that
 * directory.
 *
 * Built directly on Leaflet rather than on Leaflet.Draw. The interaction a
 * horizon picker needs is narrow -- extend a line, drag a vertex, undo,
 * finish, reassign, split -- while Leaflet.Draw brings a shape palette and
 * a modal toolbar that would mostly be hidden, and has been unmaintained
 * since before Leaflet 1.9.
 *
 * Coordinates: the map is L.CRS.Simple over the *viewer raster*, while
 * picks are stored in source trace/sample index space. The conversion is
 * redone here rather than imported from viewer.js, because both are plain
 * scripts with no module boundary between them (#120: no build step).
 *
 * Touch first: the field device is a phone. Vertex handles are `L.marker`s,
 * not `L.circleMarker`s, because only markers are draggable and only they
 * get a real touch target. Every gesture works with one finger -- there is
 * no right-click and no hover anywhere in here.
 */

/* Everything below is wrapped in an IIFE. This is load-bearing, not style:
 * classic scripts share one global scope, so a top-level `const CFG` here
 * collided with viewer.js's own `const CFG` and made this entire file fail
 * to parse -- silently, taking every picking control with it. Nothing in
 * here may be declared at top level; reach other scripts through explicit
 * `window.*` properties instead. `assets.rs` has a test that catches a
 * recurrence. */
(function () {
  "use strict";

  const CFG = window.RIDAL_VIEWER;

  /** Split a line's coordinates at an interior vertex.
   *
   * Returns `[head, tail]`, or `null` if the index is not interior.
   *
   * The split vertex belongs to **both** halves. A horizon split into two
   * lines should still cover every position it covered before; dropping the
   * shared vertex from one side would leave a gap exactly where the user
   * tapped. So the vertex count goes up by one -- that is the split, not a
   * duplicated line.
   *
   * Splitting at an end is refused rather than clamped, because it would
   * leave a one-vertex "line", which is not a line and cannot be exported.
   */
  function splitCoordinates(coordinates, vertexIndex) {
    if (vertexIndex <= 0 || vertexIndex >= coordinates.length - 1) return null;
    return [
      coordinates.slice(0, vertexIndex + 1),
      coordinates.slice(vertexIndex),
    ];
  }

  /** Join two lines end to end.
   *
   * `aAtStart` / `bAtStart` say which end of each line is being joined.
   * Each line is oriented so the meeting ends face each other, then `a`'s
   * own endpoint is dropped and `b`'s is kept -- the two are near each
   * other but not identical, and keeping one of them is what makes this a
   * join rather than a line with a tiny kink in it.
   *
   * The result therefore has `a.length + b.length - 1` vertices. Anything
   * else means a vertex was duplicated or lost.
   *
   * The inverse of `splitCoordinates`, and deliberately as literal about
   * it: splitting at vertex `i` and rejoining the halves returns the
   * original line.
   */
  function joinCoordinates(a, b, aAtStart, bAtStart) {
    // Orient `a` so its joining end is last, and `b` so its joining end is
    // first. Reversing a line is not a change of interpretation -- it is
    // the same reflector recorded in the other direction.
    const head = aAtStart ? a.slice().reverse() : a.slice();
    const tail = bAtStart ? b.slice() : b.slice().reverse();
    return [...head.slice(0, -1), ...tail];
  }

  /** Every vertex at which a line stops advancing in trace.
   *
   * The client-side twin of `overhang_at` in `src/interp/checks.rs`, which
   * returns only the first. This returns all of them, because they are
   * drawn: a line that doubles back three times gets three markers, so the
   * problem is visible rather than merely described.
   *
   * Direction comes from the first and last vertex, matching the server, so
   * a line drawn right-to-left is not an overhang -- it is the same
   * interpretation recorded in the opposite order.
   */
  function overhangIndices(coordinates) {
    const traces = coordinates.map((c) => c[0]);
    if (traces.length < 2) return [];
    const descending = traces[traces.length - 1] < traces[0];
    const offending = [];
    for (let i = 1; i < traces.length; i++) {
      const advances = descending
        ? traces[i] < traces[i - 1]
        : traces[i] > traces[i - 1];
      if (!advances) offending.push(i);
    }
    return offending;
  }

  if (CFG.writable) {
    initPicker();
  }

  function initPicker() {
    const DEFAULT_COLOR = "#ffcc00";

    const map = window.RIDAL_MAP;

    const layerSelect = document.getElementById("pick-layer");
    const toggleButton = document.getElementById("pick-toggle");
    const undoButton = document.getElementById("pick-undo");
    const finishButton = document.getElementById("pick-finish");
    const saveButton = document.getElementById("pick-save");
    const statusEl = document.getElementById("pick-status");
    const errorBox = document.getElementById("pick-error");
    const selectionBox = document.getElementById("pick-selection");
    const selectedLayer = document.getElementById("pick-selected-layer");
    const deleteButton = document.getElementById("pick-delete");
    const selectionHint = document.getElementById("pick-selection-hint");
    const layerSwatch = document.getElementById("pick-layer-swatch");
    const selectedSwatch = document.getElementById("pick-selected-swatch");
    const visibilityButton = document.getElementById("pick-visibility");

    /** Stored features, as gprinterp features in index space. */
    let features = [];
    /** ETag of the document these came from, or null if none is stored yet. */
    let etag = null;
    /** The document as it was read, so members this editor does not model
     * survive a save. gprinterp requires unknown fields to round-trip, and
     * rebuilding the document from its handful of known keys deleted them
     * silently on the first browser edit. */
    let loaded = null;
    let layers = [];
    let picking = false;
    let dirty = false;
    /** The line being drawn: array of [trace, sample], or null. */
    let draft = null;
    /** Index into `features` of the selected line, or null. */
    let selected = null;

    let drawnLines = [];
    /** Layer ids switched off in the layer panel (#209).
     *
     * Picker.js owns the caller's *editable* lines, so the panel hides one of
     * these rather than drawing a second read-only copy of the same pick. A
     * layer not in the set is visible. */
    const hiddenLayers = new Set();
    let draftLine = null;
    let handles = [];
    let overhangMarkers = [];
    let nextId = 1;
    /** Whether the stored lines are drawn at all (#143).
     *
     * Starts from this person's settings and changes from the toolbar
     * without saving: hiding the picks to read the radargram underneath is
     * something you do for a minute, not a preference you are declaring.
     * The setting is what it *starts* as.
     *
     * Only the stored lines are affected. The draft, its handles and the
     * overhang markers belong to an edit in progress, and an edit in
     * progress forces this back on -- see `revealPicks`. */
    let picksVisible = CFG.showPicks !== false;

    const showError = (message) => {
      errorBox.textContent = message;
      errorBox.classList.remove("toast-info");
      errorBox.hidden = false;
    };
    /** Same toast, said calmly.
     *
     * Worth having because the toast is `position: fixed` -- it costs no
     * layout, so an explanation can appear exactly when it is relevant and
     * vanish when it is not, without moving anything. */
    const showInfo = (message) => {
      errorBox.textContent = message;
      errorBox.classList.add("toast-info");
      errorBox.hidden = false;
    };
    const clearError = () => {
      errorBox.hidden = true;
      errorBox.textContent = "";
      errorBox.classList.remove("toast-info");
    };

    // --- Coordinate conversion ----------------------------------------------
    //
    // Reads `window.RIDAL_GEOMETRY` live on every call rather than a
    // `RASTER_SCALE`/`VERTICAL_RASTER_SCALE` captured once at
    // initialization (#168): toggling the topographically corrected view
    // changes the raster height and the raster<->source-sample mapping,
    // and capturing either at load would leave existing markers drawing
    // through one mapping while newly placed picks are stored through
    // another -- corrupting picks silently, which is exactly what "always
    // store picks in source coordinates" exists to prevent.
    //
    // The corrected view's extra step is `shiftAt` (defined in
    // viewer.js): `toIndex` inverts it to recover the *source* sample a
    // click landed on, `toLatLng` re-applies it to place a stored
    // source-space pick back on the sheared raster. Both are no-ops
    // outside that view, so the standard view's math is unchanged.

    function toIndex(latlng) {
      const scale = window.RIDAL_XSCALE || 1;
      const g = window.RIDAL_GEOMETRY;
      const trace = latlng.lng / scale / g.rasterScale;
      const rasterRow = -latlng.lat / g.verticalRasterScale;
      return [trace, rasterRow - shiftAt(trace)];
    }

    function toLatLng(trace, sample) {
      const scale = window.RIDAL_XSCALE || 1;
      const g = window.RIDAL_GEOMETRY;
      const rasterRow = sample + shiftAt(trace);
      return [-rasterRow * g.verticalRasterScale, trace * g.rasterScale * scale];
    }

    /** Show the colour a layer's lines are actually drawn in.
     *
     * Deliberately `colorFor` rather than the layer's stored colour: an
     * undefined layer has no colour and falls back to the picking default,
     * and the swatch should say what will appear on the radargram rather
     * than what the vocabulary happens to record. */
    function paintSwatch(element, label) {
      if (!element) return;
      element.style.background = colorFor(label);
    }

    /* Is this position inside the source array?
     *
     * Exclusive at the top: indices are zero-based, so `trace ===
     * sourceWidth` is one past the last trace. It used to pass a `>` check
     * and be stored, and level 2 then clamped the axis lookup while keeping
     * the out-of-range index -- a point that looks real and is not.
     *
     * Used by the click and the drag paths alike. The map can be panned
     * beyond the raster, so a vertex can be dropped outside it. */
    const inBounds = (trace, sample) =>
      trace >= 0 &&
      trace < CFG.sourceWidth &&
      sample >= 0 &&
      sample < CFG.sourceHeight;

    const layerFor = (label) => layers.find((l) => l.id === label);
    const colorFor = (label) => (layerFor(label) || {}).color || DEFAULT_COLOR;
    const allowsOverhangs = (label) =>
      Boolean((layerFor(label) || {}).allow_overhangs);

    /** What to call a layer in the interface.
     *
     * Picks store the layer *id* -- `bed_no_temperate` -- while the person
     * reads `name`. Falls back to the id for a label the vocabulary does not
     * define, which is a real case: `allowsOverhangs` treats it as one, and a
     * document can name a layer this project has since removed. */
    const layerName = (label) => {
      const layer = layerFor(label);
      return (layer && layer.name) || label || "unlabelled";
    };

    function newFeature(coordinates, label) {
      return {
        type: "Feature",
        geometry: { type: "LineString", coordinates },
        properties: { id: `f-${Date.now()}-${nextId++}`, label },
      };
    }

    // --- Handles -------------------------------------------------------------

    /** A draggable vertex handle.
     *
     * `L.marker` with a `divIcon` rather than `L.circleMarker`: circle
     * markers cannot be dragged at all, and an SVG circle is a poor touch
     * target. The icon is sized in CSS so it can grow on coarse pointers.
     */
    function makeHandle(coordinates, index, label, kind, onTap, featureIndex) {
      const [trace, sample] = coordinates[index];
      const marker = L.marker(toLatLng(trace, sample), {
        draggable: true,
        keyboard: false,
        icon: L.divIcon({
          className: `pick-handle pick-handle-${kind}`,
          iconSize: [HANDLE_PX, HANDLE_PX],
          iconAnchor: [HANDLE_PX / 2, HANDLE_PX / 2],
        }),
      }).addTo(map);
      marker.setZIndexOffset(1000);

      marker.on("dragend", () => {
        const before = coordinates[index];
        const dropped = marker.getLatLng();

        // Dropped onto one of its own neighbours: the two would be on top
        // of each other, so the intent is to get rid of this one. Checked
        // before the join below because a neighbour on the same line is the
        // nearer, more local target -- joining is about a *different* line.
        if (droppedOnNeighbour(coordinates, index, dropped)) {
          removeVertex(coordinates, index);
          return;
        }

        // Dragging an end of a stored line onto the end of another is how
        // two lines are joined back together -- the inverse of tapping a
        // middle vertex to split one.
        const isEnd = index === 0 || index === coordinates.length - 1;
        if (featureIndex !== undefined && featureIndex !== null && isEnd) {
          const target = findJoinTarget(featureIndex, dropped);
          if (target) {
            confirmJoinWith(featureIndex, index === 0, target, dropped);
            return;
          }
        }

        const [newTrace, newSample] = toIndex(dropped);
        if (!inBounds(newTrace, newSample)) {
          // Dropped off the radargram. Put the marker back rather than
          // storing a position outside the data.
          redraw();
          return;
        }
        // Preserve any third element GeoJSON allows, rather than truncating
        // a position this viewer did not author.
        coordinates[index] = [newTrace, newSample, ...before.slice(2)];

        if (!allowsOverhangs(label) && overhangIndices(coordinates).length > 0) {
          coordinates[index] = before;
          showError(
            "Moving that vertex there would make the line double back, so it " +
              "would have two depths at one position. Move it somewhere the " +
              "line keeps advancing, or allow overhangs on this layer.",
          );
        } else {
          clearError();
          markDirty();
        }
        redraw();
      });

      marker.on("click", (event) => {
        L.DomEvent.stopPropagation(event);
        onTap();
      });
      return marker;
    }

    /** How near, in screen pixels, a vertex has to be dropped to a
     * neighbour to be removed, and an endpoint to another line's endpoint
     * to be joined. Screen pixels rather than trace indices: the tolerance
     * should be a fingertip regardless of zoom or horizontal stretch. */
    const SNAP_RADIUS_PX = 24;

    const COARSE_POINTER = window.matchMedia("(pointer: coarse)").matches;

    /* Marker sizes live here, not in CSS.
     *
     * Leaflet writes `iconAnchor` as an *inline* margin, so a stylesheet
     * rule cannot move the anchor -- but `width: ... !important` does beat
     * Leaflet's inline size. Overriding the size in a media query therefore
     * grew each marker from its top-left while it stayed anchored as if it
     * were still the smaller size, leaving every handle a few pixels below
     * the line it belonged to. Sizing them here lets Leaflet derive the
     * anchor from the size, which is the only way the two stay consistent. */
    const HANDLE_PX = COARSE_POINTER ? 24 : 16;
    const MIDPOINT_PX = COARSE_POINTER ? 36 : 26;
    const OVERHANG_PX = COARSE_POINTER ? 26 : 18;

    /** A segment shorter than this on screen gets no midpoint handle.
     *
     * Erik's suggestion, and it is better than the fixed vertex cap it
     * replaces: midpoints appear only where there is room to use them, so
     * zooming out thins them out on its own and they never crowd the
     * vertices they sit between. Roughly two handle widths, so a midpoint
     * and its two neighbours cannot overlap. */
    const MIN_SEGMENT_PX_FOR_MIDPOINT = COARSE_POINTER ? 72 : 44;

    function pixelsApart(latlng, [trace, sample]) {
      return map
        .latLngToContainerPoint(latlng)
        .distanceTo(map.latLngToContainerPoint(toLatLng(trace, sample)));
    }

    /** Whether `latlng` lands on the vertex before or after `index`. */
    function droppedOnNeighbour(coordinates, index, latlng) {
      return [index - 1, index + 1].some(
        (i) =>
          i >= 0 &&
          i < coordinates.length &&
          pixelsApart(latlng, coordinates[i]) <= SNAP_RADIUS_PX,
      );
    }

    /** Drop a vertex from a line.
     *
     * Refused rather than clamped when it would leave fewer than two
     * vertices: one point is not a line, cannot be exported, and there is
     * no way back from it. Deleting the whole line is a separate,
     * deliberate button.
     *
     * Says what happened, because a vertex vanishing under a finger is
     * otherwise indistinguishable from a mis-drag -- and there is no undo. */
    function removeVertex(coordinates, index) {
      if (coordinates.length <= 2) {
        showError(
          "A line needs at least two vertices, so this one cannot be removed. " +
            "Use Delete line if you meant to remove the whole line.",
        );
        redraw();
        return;
      }
      coordinates.splice(index, 1);
      markDirty();
      redraw();
      showInfo("Vertex removed -- it was dropped onto its neighbour.");
    }

    /** The small handle between two vertices that inserts a third.
     *
     * Leaflet.Draw's pattern, and the reason it works is that one gesture
     * covers both intents: a tap drops a vertex at the midpoint, while
     * pressing and dragging creates it and positions it in the same motion,
     * with no intermediate state to undo.
     *
     * The insert itself can never create an overhang -- the midpoint of two
     * points is strictly between them -- so only the drag needs validating.
     */
    function makeMidpoint(coordinates, index, label) {
      const [aTrace, aSample] = coordinates[index];
      const [bTrace, bSample] = coordinates[index + 1];
      const midpoint = [(aTrace + bTrace) / 2, (aSample + bSample) / 2];

      // The touch target is the full icon; the dot inside it is what you
      // see. They were the same element before, at 18px on a phone against
      // a vertex handle's 24px, and a finger drag whose first sample lands
      // a few pixels off then goes to the map instead of the marker --
      // which is why a midpoint could be tapped but not dragged. A tap is
      // one point and forgiving; a drag is not.
      const marker = L.marker(toLatLng(midpoint[0], midpoint[1]), {
        draggable: true,
        keyboard: false,
        icon: L.divIcon({
          className: "pick-midpoint",
          html: '<i class="pick-midpoint-dot"></i>',
          iconSize: [MIDPOINT_PX, MIDPOINT_PX],
          iconAnchor: [MIDPOINT_PX / 2, MIDPOINT_PX / 2],
        }),
      }).addTo(map);
      // Below the real vertices, so where the two overlap the vertex wins.
      marker.setZIndexOffset(900);
      // Hover-only affordance: on a touch screen the tooltip opens on the
      // same tap that adds the vertex, so it is noise at best.
      if (!COARSE_POINTER) {
        marker.bindTooltip("Add vertex here");
      }

      // Inserted on `dragstart` so the drag is already moving a real
      // vertex, exactly as if it had been there all along. Deliberately no
      // redraw until the drag ends -- rebuilding the handles mid-drag would
      // destroy the marker being dragged.
      let dragging = false;
      marker.on("dragstart", () => {
        dragging = true;
        // The tooltip sits exactly where the vertex is being aimed, so it
        // is hidden for the duration. Closing it is not enough: Leaflet
        // reopens a bound tooltip on `mouseover` and the pointer stays over
        // the marker for the whole drag, so it is unbound instead. `redraw`
        // on `dragend` rebuilds the marker, tooltip and all.
        marker.unbindTooltip();
        coordinates.splice(index + 1, 0, midpoint.slice());
      });

      marker.on("dragend", () => {
        const [trace, sample] = toIndex(marker.getLatLng());
        coordinates[index + 1] = [trace, sample];
        if (!allowsOverhangs(label) && overhangIndices(coordinates).length > 0) {
          coordinates.splice(index + 1, 1);
          showError(
            "A vertex there would make the line double back, so it would have " +
              "two depths at one position. Nothing was added.",
          );
        } else {
          clearError();
          markDirty();
        }
        dragging = false;
        redraw();
      });

      marker.on("click", (event) => {
        L.DomEvent.stopPropagation(event);
        // Leaflet can fire a click after a drag; the drag already did the
        // work.
        if (dragging) return;
        coordinates.splice(index + 1, 0, midpoint.slice());
        clearError();
        markDirty();
        redraw();
      });

      return marker;
    }

    // --- Editing a stored line ----------------------------------------------

    function select(index) {
      selected = index;
      redraw();
    }

    function deselect() {
      if (selected === null) return;
      selected = null;
      redraw();
    }

    /** The nearest joinable endpoint to `latlng`, or null.
     *
     * Restricted to the same layer: joining a "bed" line to an "internal"
     * one would have to silently pick a label for the result, and picking
     * either is wrong. Restricted to *other* lines: dragging a line's start
     * onto its own end would close a loop, which is never a function of
     * trace. */
    function findJoinTarget(featureIndex, latlng) {
      const label = features[featureIndex].properties?.label;
      const point = map.latLngToContainerPoint(latlng);
      let best = null;
      features.forEach((feature, index) => {
        if (index === featureIndex) return;
        if ((feature.properties?.label ?? null) !== (label ?? null)) return;
        const coordinates = feature.geometry.coordinates;
        for (const atStart of [true, false]) {
          const [trace, sample] = atStart
            ? coordinates[0]
            : coordinates[coordinates.length - 1];
          const distance = point.distanceTo(
            map.latLngToContainerPoint(toLatLng(trace, sample)),
          );
          if (distance <= SNAP_RADIUS_PX && (!best || distance < best.distance)) {
            best = { index, atStart, distance };
          }
        }
      });
      return best;
    }

    /** Merge the dragged line into the one whose endpoint it was dropped on.
     *
     * Two features out, one in, via a `splice` pair that removes the higher
     * index first so the lower one is still valid -- the same discipline as
     * `splitSelectedAt`, and for the same reason: a merge written as "add
     * the joined line, then remove the two originals" leaves an original
     * behind whenever a removal is skipped. */
    function joinWith(featureIndex, draggedAtStart, target) {
      const source = features[featureIndex];
      const other = features[target.index];
      const label = source.properties?.label;

      const merged = joinCoordinates(
        source.geometry.coordinates,
        other.geometry.coordinates,
        draggedAtStart,
        target.atStart,
      );

      if (!allowsOverhangs(label) && overhangIndices(merged).length > 0) {
        showError(
          "Joining those two lines would double back, so the result would have " +
            "two depths at one position. They probably need joining at their " +
            "other ends, or they overlap along the profile.",
        );
        redraw();
        return;
      }

      const low = Math.min(featureIndex, target.index);
      const high = Math.max(featureIndex, target.index);
      features.splice(high, 1);
      features.splice(low, 1, newFeature(merged, label));

      // Select the result rather than dropping the selection: the user is
      // looking at what they just made, and its vertices are what they will
      // want to adjust next.
      selected = low;
      clearError();
      markDirty();
      redraw();
    }

    /** The confirmation both destructive line edits ask for.
     *
     * Splitting and joining are each triggered by a gesture aimed at a
     * vertex -- a tap that could have been the start of a drag, or a drop
     * that could have been a plain move -- and each is awkward to undo by
     * hand on a long horizon. So both ask, and they ask identically.
     *
     * A popup at the vertex rather than a prompt in the selection panel
     * above the map: the gesture landed here, and a question that appears
     * above the radargram is a question that gets answered without being
     * read. Not `window.confirm`, which blocks the Chromium harness and
     * reads as a browser dialog rather than part of the page -- the same
     * reason the layer panel builds its delete confirmation by hand.
     *
     * A standalone popup, not `marker.bindPopup`: binding also installs
     * Leaflet's own click-to-toggle on the marker, so a second tap on a
     * handle closed the popup this had just opened and the prompt never
     * reappeared. `openOn` also closes any popup already up, so two vertices
     * cannot both be asking at once.
     *
     * `onDismiss` runs whenever the popup goes away without the action being
     * taken -- Cancel, a click on the map, Escape. The join needs it: its
     * drag has already moved the marker while `coordinates` still holds the
     * old position, so anything short of a confirmation has to put it back.
     */
    function confirmAt(latlng, { name, question, verb, onConfirm, onDismiss }) {
      let confirmed = false;
      const content = document.createElement("div");
      content.className = "pick-confirm";
      const text = document.createElement("span");
      text.textContent = question;
      const yes = document.createElement("button");
      yes.type = "button";
      yes.id = `pick-${name}-confirm`;
      yes.textContent = verb;
      yes.addEventListener("click", () => {
        confirmed = true;
        map.closePopup();
        onConfirm();
      });
      const no = document.createElement("button");
      no.type = "button";
      no.id = `pick-${name}-cancel`;
      no.textContent = "Cancel";
      no.addEventListener("click", () => map.closePopup());
      content.append(text, yes, no);
      const popup = L.popup({ closeButton: false, autoPan: false })
        .setLatLng(latlng)
        .setContent(content)
        .openOn(map);
      if (onDismiss) {
        popup.on("remove", () => {
          if (!confirmed) onDismiss();
        });
      }
    }

    /** Ask before splitting, anchored at the vertex that was tapped. */
    function confirmSplitAt(vertexIndex, marker) {
      confirmAt(marker.getLatLng(), {
        name: "split",
        question: "Split the line here?",
        verb: "Split",
        onConfirm: () => splitSelectedAt(vertexIndex),
      });
    }

    /** Ask before joining, anchored where the end was dropped.
     *
     * Nothing has been written when this is called -- the dragged vertex's
     * new position is not stored until the move is applied, and a join
     * replaces both lines outright -- so dismissing only has to redraw for
     * the marker to snap back to where it came from. */
    function confirmJoinWith(featureIndex, draggedAtStart, target, droppedAt) {
      confirmAt(droppedAt, {
        name: "join",
        question: "Join these two lines?",
        verb: "Join",
        onConfirm: () => joinWith(featureIndex, draggedAtStart, target),
        onDismiss: redraw,
      });
    }

    /** Replace the selected line with the two halves of a split.
     *
     * One `splice` that removes exactly one feature and inserts exactly
     * two. Written this way on purpose: a split implemented as "add both
     * halves, then remove the original" leaves the original behind whenever
     * the removal is skipped or the handler fires twice, which is how a
     * split turns into duplicate overlapping lines. */
    function splitSelectedAt(vertexIndex) {
      const feature = features[selected];
      const halves = splitCoordinates(feature.geometry.coordinates, vertexIndex);
      if (halves === null) {
        showError(
          "Tap a vertex in the middle of the line to split it -- splitting at " +
            "an end would leave a line with a single vertex.",
        );
        return;
      }
      const label = feature.properties && feature.properties.label;
      features.splice(
        selected,
        1,
        newFeature(halves[0], label),
        newFeature(halves[1], label),
      );
      selected = null;
      clearError();
      markDirty();
      redraw();
    }

    /** Delete the selected line.
     *
     * Does nothing without a selection, rather than deleting a line the
     * user cannot see. `features.splice(null, 1)` coerces `null` to `0` and
     * quietly removes the *first* line, which is what this did when the
     * panel was reachable with nothing selected -- once per press.
     *
     * Deleting "the last line" instead was the alternative, but a
     * destructive action needs a visible target: there is no way to show
     * which line "the last one" is, so a confirmation could not name what
     * was about to be lost. Every control in a panel headed "Selected line"
     * acts on the selection, or not at all. */
    function deleteSelected() {
      if (selected === null) return;
      features.splice(selected, 1);
      selected = null;
      markDirty();
      redraw();
    }

    // --- Rendering -----------------------------------------------------------

    function redraw() {
      drawnLines.forEach((line) => map.removeLayer(line));
      drawnLines = [];
      // Hidden means not drawn, rather than drawn transparently: a
      // zero-opacity line still swallows taps aimed at the radargram, and
      // "let me see the data" should not leave invisible obstacles on it.
      // The features themselves are untouched, so showing them again is a
      // redraw and nothing more.
      const shown = picksVisible ? features : [];
      shown.forEach((feature, index) => {
        const label = feature.properties && feature.properties.label;
        // A layer switched off in the panel is not drawn at all, for the same
        // reason hiding the picks is not drawn transparently: a zero-opacity
        // line still swallows taps aimed at the radargram.
        if (hiddenLayers.has(label)) return;
        const isSelected = index === selected;
        const points = feature.geometry.coordinates.map(([t, s]) => toLatLng(t, s));

        // The wide, invisible companion goes down first so the visible line
        // draws over it, and carries all the interaction: the visible line
        // is non-interactive, so it cannot swallow a tap meant for the
        // easier target.
        const hit = RIDAL.hitLine(points, "radargram-lines").addTo(map);
        // A text node, not a string: Leaflet assigns a string tooltip with
        // innerHTML, and `label` is free text from the stored document.
        // `layerName` rather than `label` so a line in `bed_no_temperate`
        // reads as "Glacier bed", the way the layer dropdown names it.
        hit.bindTooltip(
          document.createTextNode(
            `${layerName(label)} (${feature.geometry.coordinates.length} vertices)`,
          ),
        );
        hit.on("click", (event) => {
          // While picking, a tap over an existing line is still a new
          // vertex -- lines must not become holes in the drawing surface.
          if (picking) return;
          L.DomEvent.stopPropagation(event);
          select(index === selected ? null : index);
        });

        const line = L.polyline(points, {
          color: colorFor(label),
          weight: isSelected ? 5 : 3,
          opacity: isSelected ? 1 : 0.85,
          interactive: false,
          pane: "radargram-lines",
        }).addTo(map);

        drawnLines.push(hit, line);
      });
      redrawHandles();
      redrawOverhangs();
      updateSelectionPanel();
      updateStatus();
    }

    /** Handles for whichever line is being edited: the draft, or the
     * selected stored line. Only one set exists at a time, so a tap on a
     * handle is never ambiguous. */
    function redrawHandles() {
      if (draftLine) {
        map.removeLayer(draftLine);
        draftLine = null;
      }
      handles.forEach((handle) => map.removeLayer(handle));
      handles = [];

      if (draft && draft.length) {
        const label = layerSelect.value;
        if (draft.length > 1) {
          draftLine = L.polyline(
            draft.map(([t, s]) => toLatLng(t, s)),
            {
              color: colorFor(label),
              weight: 3,
              dashArray: "6 4",
              pane: "radargram-lines",
            },
          ).addTo(map);
        }
        handles = draft.map((_, index) =>
          makeHandle(draft, index, label, "draft", () => {
            // Tap removes. There is no right-click on a phone, and Undo
            // only ever reaches the last vertex.
            draft.splice(index, 1);
            clearError();
            redrawHandles();
            redrawOverhangs();
            updateStatus();
          }),
        );
        return;
      }

      if (selected === null) return;
      const feature = features[selected];
      const label = feature.properties && feature.properties.label;
      const coordinates = feature.geometry.coordinates;
      handles = coordinates.map((_, index) => {
        const interior = index > 0 && index < coordinates.length - 1;
        const handle = makeHandle(
          coordinates,
          index,
          label,
          interior ? "interior" : "end",
          () => {
            // `handle` is assigned by the time a tap can reach this.
            if (interior) confirmSplitAt(index, handle);
          },
          selected,
        );
        handle.bindTooltip(
          interior ? "Drag to move, tap to split" : "Drag to move",
        );
        // Once the handle is being dragged the tooltip sits exactly where
        // the vertex is being aimed, and what is happening is already
        // obvious. Unbound rather than closed for the same reason as the
        // midpoint: `mouseover` would reopen it mid-drag. `dragend` calls
        // `redraw`, which rebuilds every handle with its tooltip.
        handle.on("dragstart", () => handle.unbindTooltip());
        return handle;
      });

      // A midpoint per segment, but only where one is usable: long enough
      // on screen to aim at, and actually in view. A horizon with hundreds
      // of vertices therefore costs nothing until it is zoomed into, and
      // then only for the part being looked at.
      const view = map.getBounds();
      for (let index = 0; index < coordinates.length - 1; index++) {
        const a = toLatLng(coordinates[index][0], coordinates[index][1]);
        const b = toLatLng(coordinates[index + 1][0], coordinates[index + 1][1]);
        if (!view.intersects(L.latLngBounds(a, b))) continue;
        const lengthPx = map
          .latLngToContainerPoint(a)
          .distanceTo(map.latLngToContainerPoint(b));
        if (lengthPx < MIN_SEGMENT_PX_FOR_MIDPOINT) continue;
        handles.push(makeMidpoint(coordinates, index, label));
      }
    }

    /** A marker at every vertex where a line doubles back.
     *
     * Drawn for *all* lines, including layers that allow overhangs: an
     * intentional overhang is still worth seeing, and a line saved before
     * the rule existed would otherwise look fine while quietly failing to
     * export at even spacing. */
    function redrawOverhangs() {
      overhangMarkers.forEach((marker) => map.removeLayer(marker));
      overhangMarkers = [];

      // Hidden means hidden (#143): a marker and its tooltip left floating
      // over a radargram whose picks were switched off is exactly the view
      // the toggle exists to give. The draft is not a stored pick and is
      // always marked -- and starting to draw one reveals the rest anyway.
      const lines = (picksVisible ? features : []).map((f) => [
        f.geometry.coordinates,
        (f.properties && f.properties.label) || null,
      ]);
      if (draft && draft.length) lines.push([draft, layerSelect.value]);

      for (const [coordinates, label] of lines) {
        const allowed = allowsOverhangs(label);
        for (const index of overhangIndices(coordinates)) {
          const [trace, sample] = coordinates[index];
          overhangMarkers.push(
            L.marker(toLatLng(trace, sample), {
              keyboard: false,
              icon: L.divIcon({
                className: `pick-overhang${allowed ? " pick-overhang-allowed" : ""}`,
                iconSize: [OVERHANG_PX, OVERHANG_PX],
                iconAnchor: [OVERHANG_PX / 2, OVERHANG_PX / 2],
              }),
            })
              .addTo(map)
              .bindTooltip(
                document.createTextNode(
                  allowed
                    ? `Overhang at vertex ${index}, allowed on "${label}". This ` +
                        "layer exports as picked vertices, not evenly spaced."
                    : `Overhang at vertex ${index}: the line doubles back here, ` +
                        "so it has two depths at one position.",
                ),
              ),
          );
        }
      }
    }

    function updateSelectionPanel() {
      const active = selected !== null;
      selectionBox.hidden = !active;
      // Not only hidden: a disabled button cannot be activated even if a
      // future style rule makes the panel visible again, which is the way
      // this failed the first time.
      deleteButton.disabled = !active;
      if (!active) return;
      const feature = features[selected];
      const label = feature.properties && feature.properties.label;
      selectedLayer.value = label || "";
      paintSwatch(selectedSwatch, label);
      const many = feature.geometry.coordinates.length > 2;
      selectionHint.textContent =
        "Drag a vertex to move it, or onto its neighbour to remove it. " +
        "Tap a small handle between two vertices to add one. " +
        (many ? "Tap a middle vertex to split. " : "") +
        "Drop an end onto another line's end in the same layer to join them.";
    }

    function countOverhangs() {
      let total = features.reduce(
        (sum, f) => sum + overhangIndices(f.geometry.coordinates).length,
        0,
      );
      if (draft) total += overhangIndices(draft).length;
      return total;
    }

    function updateStatus() {
      const parts = [
        `${features.length} line${features.length === 1 ? "" : "s"}`,
      ];
      if (draft && draft.length) parts.push(`drawing: ${draft.length}`);
      const overhangs = countOverhangs();
      if (overhangs) {
        parts.push(`${overhangs} overhang${overhangs === 1 ? "" : "s"}`);
      }
      parts.push(dirty ? "unsaved" : "saved");
      statusEl.textContent = parts.join(" · ");
      statusEl.classList.toggle("dirty", dirty);

      // A carried document has something to save even with nothing edited:
      // adopting it onto this revision is the whole action, and the button
      // says so rather than sitting greyed out next to a banner explaining
      // that the picks are from an earlier version.
      // `refused` has no carried document to adopt, so offering the
      // button there could only ever produce `cannot_be_carried`. A
      // control that can only fail is worse than an absent one.
      const adoptable =
        carriedReport &&
        carriedReport.severity !== "current" &&
        carriedReport.severity !== "refused";
      saveButton.disabled = !dirty && !adoptable;
      saveButton.textContent = adoptable && !dirty ? "Adopt to this version…" : "Save";
      undoButton.disabled = !draft || draft.length === 0;
      finishButton.disabled = !draft || draft.length < 2;
      // Read by the download menu in viewer.js, which owns downloading now:
      // a level 2 export is derived from what is *saved*, so offering one
      // over unsaved edits would hand back the wrong thing silently.
      window.RIDAL_PICKS_DIRTY = dirty;
      // Naming the count ties the button to the line in progress. "Finish
      // line" on its own reads as a mode switch, which is what made it
      // hard to guess what it would do.
      finishButton.textContent =
        draft && draft.length ? `Finish line (${draft.length})` : "Finish line";
    }

    function markDirty() {
      dirty = true;
      updateStatus();
    }

    // --- Drawing --------------------------------------------------------------

    /** Draw the stored lines, or stop drawing them (#143). */
    function setPicksVisible(visible) {
      picksVisible = visible;
      visibilityButton.textContent = visible ? "Hide picks" : "Show picks";
      visibilityButton.setAttribute("aria-pressed", String(visible));
      if (!visible) {
        // A selection you cannot see is a delete button pointed at
        // something invisible, so hiding clears it.
        deselect();
      }
      redraw();
    }

    /** Bring the picks back because something is about to change them.
     *
     * Called wherever an edit begins. Editing what is not on screen is the
     * one case where the toggle must lose: the person is no longer reading
     * the radargram, and a line that appears out of nowhere on save is
     * worse than one that reappears when picking starts. */
    function revealPicks() {
      if (!picksVisible) setPicksVisible(true);
    }

    function setPicking(on) {
      if (on) revealPicks();
      picking = on;
      toggleButton.textContent = on ? "Stop picking" : "Start picking";
      toggleButton.setAttribute("aria-pressed", String(on));
      document.getElementById("map").classList.toggle("picking", on);
      if (on) {
        deselect();
        // Every tap extends the *same* line until it is finished, which is
        // not guessable from a toolbar of buttons. Said once, when it
        // becomes relevant, and cleared by the first tap.
        showInfo(
          "Tap the radargram to add points to one line. Finish line ends it, " +
            "so the next tap starts a separate line.",
        );
      } else {
        finishLine();
        clearError();
      }
    }

    function finishLine() {
      if (!draft || draft.length < 2) {
        draft = null;
        redrawHandles();
        redrawOverhangs();
        updateStatus();
        return;
      }
      features.push(newFeature(draft, layerSelect.value));
      draft = null;
      markDirty();
      redraw();
    }

    map.on("click", (event) => {
      if (!picking) {
        deselect();
        return;
      }
      if (!layerSelect.value) {
        showError("Choose a layer before picking.");
        return;
      }
      const [trace, sample] = toIndex(event.latlng);
      if (!inBounds(trace, sample)) return;

      // Test the whole candidate line, not just this vertex against the
      // previous one. Direction is a property of the line as a whole, and
      // checking pairwise let one stray vertex flip the perceived direction
      // and then reject every later point as an overhang.
      const candidate = draft
        ? draft.concat([[trace, sample]])
        : [[trace, sample]];
      if (
        !allowsOverhangs(layerSelect.value) &&
        overhangIndices(candidate).length > 0
      ) {
        showError(
          "That point would make the line double back, so it would have two " +
            "depths at one position. Carry on in the direction you started, " +
            "finish this line and begin another, or allow overhangs on this " +
            "layer.",
        );
        return;
      }
      // Clears the "how this works" hint too, on the first tap that proves
      // it was read.
      clearError();
      draft = candidate;
      redrawHandles();
      redrawOverhangs();
      updateStatus();
    });

    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        if (picking) finishLine();
        else deselect();
      }
      if (
        event.key === "z" &&
        (event.ctrlKey || event.metaKey) &&
        draft &&
        draft.length
      ) {
        event.preventDefault();
        draft.pop();
        redrawHandles();
        redrawOverhangs();
        updateStatus();
      }
      if (event.key === "s" && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        save();
      }
    });

    window.RIDAL_REDRAW_PICKS = redraw;

    /* Show or hide one of the caller's own layers (#209).
     *
     * The layer panel calls this so that toggling a layer removes the actual
     * editable lines from the map rather than drawing a read-only copy
     * underneath them. `window.*` because the panel is another classic script
     * with no module boundary. */
    window.RIDAL_SET_LAYER_VISIBLE = function (label, visible) {
      if (visible) {
        hiddenLayers.delete(label);
      } else {
        hiddenLayers.add(label);
      }
      redraw();
    };

    // Midpoint visibility depends on zoom and pan, so the handles are
    // rebuilt when the view settles. `moveend`/`zoomend` rather than
    // `move`/`zoom`: rebuilding markers on every frame of a pan would be
    // both wasteful and visibly jumpy.
    map.on("moveend zoomend", redrawHandles);

    // The template renders the label and `aria-pressed` from the same
    // setting this reads, so the page is never briefly wrong -- including
    // with JavaScript disabled. Re-applied here anyway, because that
    // agreement is an invariant rather than a coincidence.
    visibilityButton.textContent = picksVisible ? "Hide picks" : "Show picks";
    visibilityButton.setAttribute("aria-pressed", String(picksVisible));
    visibilityButton.addEventListener("click", () => setPicksVisible(!picksVisible));
    toggleButton.addEventListener("click", () => setPicking(!picking));
    undoButton.addEventListener("click", () => {
      if (draft && draft.length) {
        draft.pop();
        clearError();
        redrawHandles();
        redrawOverhangs();
        updateStatus();
      }
    });
    finishButton.addEventListener("click", finishLine);
    saveButton.addEventListener("click", save);
    layerSelect.addEventListener("change", () => {
      paintSwatch(layerSwatch, layerSelect.value);
      redrawHandles();
      redrawOverhangs();
      updateStatus();
    });
    deleteButton.addEventListener("click", deleteSelected);
    selectedLayer.addEventListener("change", () => {
      if (selected === null) return;
      features[selected].properties.label = selectedLayer.value;
      paintSwatch(selectedSwatch, selectedLayer.value);
      markDirty();
      redraw();
    });

    window.addEventListener("beforeunload", (event) => {
      if (dirty) event.preventDefault();
    });

    // --- Persistence ----------------------------------------------------------

    const documentUrl = RIDAL.apiPath(
      "datasets",
      CFG.radargramId,
      "interpretations",
      CFG.user,
    );

    /** Adopt the carried view as this user's interpretation on this revision.
     *
     * Confirmed first, and once: it overwrites coordinates somebody drew
     * with coordinates derived from them. The archive makes that
     * reversible, and the confirmation is what makes it deliberate -- the
     * whole point is that a person looked and agreed, so the moment of
     * agreeing should be visible rather than implied by a Save click. */
    async function adopt() {
      const dropped =
        (carriedReport && carriedReport.dropped && carriedReport.dropped.length) || 0;
      const lost =
        dropped > 0
          ? `\n\n${dropped} line(s) fall outside this version and will not be ` +
            "included. They stay in the archived copy."
          : "";
      const edited = dirty ? "\n\nYour edits are included." : "";
      const agreed = window.confirm(
        "Adopt these picks onto the current version?\n\n" +
          "They were drawn on an earlier version and carried here for display. " +
          "Adopting records them as yours on this version, and notes in the file " +
          "that they were carried rather than drawn.\n\n" +
          "The version as drawn is archived first, so this can be undone." +
          edited +
          lost,
      );
      if (!agreed) return;
      // Adopting rewrites what is stored, so the picks come back into view
      // for the same reason picking does (#143): a change this consequential
      // should not land on a screen showing none of it.
      revealPicks();

      try {
        // What is on screen, edits and all -- an edit made over a carried
        // view is already in this revision's index space. `onto` says
        // which revision this page believes it is looking at, so a tab
        // left open across a replace fails loudly instead of writing
        // coordinates nobody validated against the file now on disk.
        // Conditional, exactly as a save is. Without it, adopting from a
        // page that loaded before another tab saved would overwrite the
        // newer document -- archived, but silently replaced -- where a
        // save in the same position returns 412 and says so.
        const headers = { "Content-Type": "application/json" };
        if (etag) headers["If-Match"] = etag;
        const response = await fetch(
          `${documentUrl}/promote?onto=${encodeURIComponent(CFG.revisionId)}`,
          { method: "POST", headers, body: JSON.stringify(buildBody()) },
        );
        if (response.status === 412) {
          showError(
            "These picks were changed somewhere else while this page was open. " +
              "Reload to see the saved version -- nothing here is lost until you do.",
          );
          return;
        }
        if (!response.ok) {
          const failure = await response.json().catch(() => null);
          showError(
            failure?.error?.message || `Could not adopt these picks (${response.status}).`,
          );
          return;
        }
      } catch (error) {
        showError(`Could not adopt these picks: ${error.message}`);
        return;
      }
      // Reloaded rather than patched: the document now belongs to this
      // revision, so the banner, the etag and the save guard all change.
      dirty = false;
      window.location.reload();
    }

    /** The document as this page would store it: what is on screen.
     *
     * Shared by saving and adopting. Adopting used to rebuild the carried
     * coordinates on the server and ignore this, which threw away any edit
     * made on top of the carried view -- and an edit made on top is
     * already in *this* revision's index space, because it was made by
     * dragging a vertex over this revision. There is nothing to carry
     * about it. The person looked at the carried picks, adjusted them, and
     * both halves of that are what they meant.
     */
    function buildBody() {
      return {
          // Spread first, so anything this editor does not model is carried
          // through, then override only the fields it owns.
          ...(loaded || {}),
          schema: "gprinterp",
          schema_version: "0.1",
          key: CFG.radargramId,
          date_modified: new Date().toISOString(),
          source: {
            ...((loaded && loaded.source) || {}),
            id: CFG.radargramId,
            // The revision the picks were drawn against. Without it a
            // document authored here can never trigger the reprocessing
            // warning gprinterp SPEC 6.3 exists for: equal trace and sample
            // counts are explicitly not evidence that two revisions agree on
            // what an index means.
            revision_id: CFG.revisionId,
            n_traces: CFG.sourceWidth,
            n_samples: CFG.sourceHeight,
          },
          // What lets these picks be carried onto a differently processed
          // version of the same radargram (gprinterp SPEC 8.1). Without it a
          // consumer has a coordinate and no mapping to evaluate it through,
          // and 8.1 forbids falling back to the raw index -- so a document
          // with no axes is stuck on the one revision forever.
          //
          // Built by the server, which is the only thing that has read the
          // radargram. Null when it cannot describe its axes, and then the
          // key is left out entirely: half an axis block would invite a
          // consumer to believe it had a mapping.
          // The *axes* are replaced and the rest of `coordinates` is kept.
          // gprinterp puts `space` and `convention` in the same object, and
          // the editor's contract is to carry through what it does not model
          // -- overwriting the whole thing would drop a producer's
          // conventions on the first save made here.
          //
          // `undefined` when there are no axes, rather than omitting the key,
          // because the spread above carried through whatever the loaded
          // document had. Those axes describe the revision it was drawn on
          // and `source` two lines up now names this one, so keeping them
          // would pair one revision's mapping with another's id -- a worse
          // lie than having no mapping. JSON.stringify drops the key.
          coordinates: CFG.axes
            ? { ...((loaded && loaded.coordinates) || {}), axes: CFG.axes }
            : undefined,
          features,
      };
    }

    async function save() {
      finishLine();
      clearError();

      // Adopting comes first, and not only because a carried document
      // cannot be saved: with nothing edited there is no `dirty` to gate
      // on, and this is exactly the case where the button is offered.
      //
      // An *absent* revision is not a current one either. A document that
      // never said what it was drawn on -- written before the picker
      // recorded it, or produced by another tool -- must not be silently
      // relabelled with this one.
      const drawnOnRevision =
        (loaded && loaded.source && loaded.source.revision_id) || null;
      if (loaded && drawnOnRevision !== CFG.revisionId) {
        await adopt();
        return;
      }
      if (!dirty) return;

      // Past here the document belongs to the revision on screen, because
      // the branch above sends every other case to `adopt()`. That branch
      // is load-bearing: writing these coordinates under the current
      // revision's id without re-anchoring them is precisely the
      // cross-revision mistake the axes exist to prevent, and this editor
      // would be the source of it.
      const body = buildBody();
      const headers = { "Content-Type": "application/json" };
      // Conditional either way. Without the absent case, two tabs that both
      // loaded a document which did not exist yet would both save, and the
      // later one would silently discard the other's first edit.
      if (etag) {
        headers["If-Match"] = etag;
      } else {
        headers["If-None-Match"] = "*";
      }

      try {
        const response = await fetch(documentUrl, {
          method: "PUT",
          headers,
          body: JSON.stringify(body),
        });
        if (response.status === 412) {
          showError(
            "These picks were changed somewhere else while this page was open. " +
              "Reload to see the saved version -- nothing here is lost until you do.",
          );
          return;
        }
        if (!response.ok) {
          const failure = await response.json().catch(() => null);
          showError(
            failure?.error?.message || `Could not save (${response.status}).`,
          );
          return;
        }
        etag = response.headers.get("ETag");
        dirty = false;
        updateStatus();
        // The derived lines are computed from the *saved* picks, so they are
        // stale the moment this succeeds. Force a refetch rather than let the
        // panel redraw its cached values, which are the pre-save ones.
        if (window.RIDAL_REDRAW_DERIVED) window.RIDAL_REDRAW_DERIVED(true);
      } catch (error) {
        showError(`Could not save: ${error.message}`);
      }
    }

    /** Load whatever is already stored for this radargram.
     *
     * Runs on page load, unconditionally: opening a radargram shows the
     * picks that exist for it, rather than an empty canvas that would
     * invite redoing work someone has already done. */
    /* --- The carry banner (#148) ------------------------------------------
     *
     * Standing, not transient. It describes what is on screen right now,
     * and it stays for as long as that is true.
     */
    /** The last carry report, so adopting can say what it would leave out. */
    let carriedReport = null;

    const carryBanner = document.getElementById("carry-banner");
    const carryTier = document.getElementById("carry-tier");
    const carryHeadline = document.getElementById("carry-headline");
    const carryDetail = document.getElementById("carry-detail");
    const carryDetailBody = document.getElementById("carry-detail-body");

    const TIER_LABEL = {
      carried: "Carried from an earlier version",
      approximate: "Approximate",
      partial: "Incomplete",
      refused: "Cannot be shown here",
    };

    function hideCarryBanner() {
      if (carryBanner) carryBanner.hidden = true;
    }

    function showCarryBanner(report) {
      if (!carryBanner) return;
      carryBanner.hidden = false;
      carryBanner.classList.toggle("is-refused", report.severity === "refused");
      carryTier.textContent = TIER_LABEL[report.severity] || "Carried";
      carryHeadline.textContent = report.headline || "";

      // The numbers behind the sentence, for whoever wants them. Folded
      // away by default: the headline is what most people need, and a
      // banner that opens with a table of statistics is a banner people
      // learn to skip.
      const rows = [];
      if (report.from_revision) {
        rows.push(["Drawn on", report.from_revision]);
      }
      rows.push(["Showing on", report.to_revision]);
      if (report.x_anchor && report.y_anchor) {
        rows.push(["Carried through", `${report.x_anchor} and ${report.y_anchor}`]);
      }
      if (report.moved) {
        const m = report.moved;
        rows.push([
          "Moved (median)",
          `${m.median_traces.toFixed(2)} traces, ${m.median_samples.toFixed(2)} samples`,
        ]);
        rows.push([
          "Moved (worst)",
          `${m.worst_traces.toFixed(2)} traces, ${m.worst_samples.toFixed(2)} samples`,
        ]);
      }
      if (report.dropped && report.dropped.length > 0) {
        rows.push([
          "Left out",
          report.dropped
            .map((d) => d.label || d.id || `line ${d.index + 1}`)
            .join(", "),
        ]);
      }
      if (report.refusal) rows.push(["Reason", report.refusal]);

      const dl = document.createElement("dl");
      for (const [term, value] of rows) {
        const dt = document.createElement("dt");
        dt.textContent = term;
        const dd = document.createElement("dd");
        dd.textContent = value;
        dl.append(dt, dd);
      }
      carryDetailBody.replaceChildren(dl);
      carryDetail.hidden = rows.length === 0;
    }

    /** The stored picks as they should be drawn on this revision.
     *
     * Returns the document to draw, or `null` when there is nothing to
     * draw -- in which case the banner already says why, and the caller
     * should stop rather than fall back to the stored coordinates. Falling
     * back is precisely the raw-index mistake §8.1 forbids. */
    async function loadCarried() {
      try {
        const response = await fetch(`${documentUrl}/carried`);
        if (!response.ok) {
          showError(`Could not check this against the current version (${response.status}).`);
          return null;
        }
        const body = await response.json();
        carriedReport = body.report;
        showCarryBanner(body.report);
        if (!body.document) {
          features = [];
          redraw();
          return null;
        }
        return body.document;
      } catch (error) {
        showError(`Could not check this against the current version: ${error.message}`);
        return null;
      }
    }

    async function load() {
      try {
        const response = await fetch(documentUrl);
        if (response.status === 404) {
          features = [];
          etag = null;
          redraw();
          return;
        }
        if (!response.ok) {
          const failure = await response.json().catch(() => null);
          showError(
            failure?.error?.message ||
              `Could not load picks (${response.status}).`,
          );
          return;
        }
        etag = response.headers.get("ETag");
        const body = await response.json();
        loaded = body;

        // Drawn on an earlier revision? Then what is stored indexes a grid
        // this radargram no longer has, and drawing it here would put the
        // picks wherever the two revisions happen to disagree -- the raw
        // index fallback gprinterp SPEC §8.1 forbids. Ask the server to
        // carry them across, and draw that instead.
        //
        // Nothing is written either way: the carried view is a view, and
        // the save guard below refuses a document whose revision is not
        // the one on screen. The banner says which is being shown.
        // Same rule as the save guard: an absent revision is not a
        // current one. The server only calls a document `current` on an
        // exact match, so anything else -- including nothing at all --
        // goes through the carry rather than being drawn straight against
        // this grid, which is the raw-index fallback §8.1 forbids.
        const drawnOn = (body.source && body.source.revision_id) || null;
        let shown = body;
        if (drawnOn !== CFG.revisionId) {
          const carried = await loadCarried();
          if (!carried) return;
          shown = carried;
        } else {
          carriedReport = null;
          hideCarryBanner();
        }

        const all = shown.features || [];
        features = all.filter(
          (f) => f.geometry && f.geometry.type === "LineString",
        );
        if (features.length !== all.length) {
          showError(
            "This interpretation contains geometry other than lines, which this " +
              "viewer cannot edit. Saving here would drop it, so editing is " +
              "disabled for safety.",
          );
          saveButton.disabled = true;
          toggleButton.disabled = true;
          return;
        }
        redraw();
      } catch (error) {
        showError(`Could not load picks: ${error.message}`);
      }
    }

    async function loadLayers() {
      try {
        const body = await RIDAL.fetchJson("/api/v1/layers");
        layers = body.layers || [];
      } catch (error) {
        console.warn(`Could not load layers: ${error.message}`);
        layers = [];
      }
      // Plain text, deliberately. Putting the colour inside the list was
      // tried and reverted: an `<option>` cannot contain markup, so the
      // only ways in are tinting the whole label -- which several platforms
      // ignore -- or a square glyph, which renders as an empty box wherever
      // the font lacks it. Both leave the colour *less* legible than not
      // showing it at all. The swatch beside the select is the one that
      // works everywhere.
      const options = layers.length
        ? layers.map((layer) => {
            const option = document.createElement("option");
            option.value = layer.id;
            option.textContent = layer.name || layer.id;
            return option;
          })
        : [new Option("No layers defined - add one on the Layers page", "")];
      layerSelect.replaceChildren(...options);
      selectedLayer.replaceChildren(...options.map((o) => o.cloneNode(true)));
      paintSwatch(layerSwatch, layerSelect.value);
    }

    // Layers first: colours and overhang permissions are needed before the
    // stored picks can be drawn correctly.
    loadLayers().then(load);
  }
})();
