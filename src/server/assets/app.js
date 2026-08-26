/* Ridal web GUI shared constants and helpers (#115, #121).
 *
 * First-party, embedded in the binary via assets.rs, loaded from
 * base.html.jinja after leaflet.js so both `L` and `RIDAL` are defined
 * before any per-page script runs. A classic script defining one frozen
 * global -- no modules, no build step (#120: production must not require a
 * separate Node dev server).
 *
 * Deliberately NOT under assets/vendor/ -- scripts/vendor_leaflet.sh does
 * `rm -rf` on that directory.
 *
 * These colours are literal hex rather than CSS custom properties on
 * purpose: they are drawn onto satellite imagery, which looks the same in
 * either page theme, so they must not follow prefers-color-scheme.
 */

const RIDAL = Object.freeze({
  // A radargram's own track, on the index group maps and the viewer.
  trackColor: "#e63",
  trackWeight: 3,
  // The viewer draws its *own* track heavier than the index does, to
  // distinguish it from the sibling tracks beside it. Previously this was
  // an accidental 3-vs-4 discrepancy between two copy-pasted blocks; it is
  // now a named, intentional distinction.
  trackFocusWeight: 4,

  // Other radargrams in the same group, shown for context on the viewer.
  siblingColor: "#bbb",
  siblingWeight: 3,
  siblingOpacity: 0.7,

  // Marker tracking the cursor's trace position along the track.
  cursorColor: "#ff3b30",
  cursorRadius: 6,

  tileUrl:
    "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
  tileAttribution: "Esri",
  tileMaxZoom: 18,

  /** Add the shared basemap layer to `map` and return it. */
  basemap(map) {
    L.tileLayer(RIDAL.tileUrl, {
      maxZoom: RIDAL.tileMaxZoom,
      attribution: RIDAL.tileAttribution,
    }).addTo(map);
    return map;
  },

  /** Latitude/longitude pairs for every vertex, per track segment. */
  trackToLatLngs(track) {
    return track.segments.map((seg) => seg.vertices.map((v) => [v.lat, v.lon]));
  },
});
