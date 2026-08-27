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
  // Weight while a sibling's track is hovered or its popup is open --
  // mirrors trackFocusWeight's role for the index page's own tracks.
  siblingFocusWeight: 5,

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

  /** One track's popup content: a link wrapping both the label and a
   * lazy-loaded overview thumbnail, so clicking the image navigates just
   * like clicking the label does -- matching PFA_website's
   * overview_map.js, but with the image inside the anchor rather than
   * beside it. The near-opaque popup background that makes this legible
   * over arbitrary basemap imagery comes from app.css's
   * .leaflet-popup-content-wrapper rule, not from anything here. */
  popupContent(radargramId, label) {
    return (
      `<a class="popup-link" href="/view/${radargramId}">` +
      `${label}` +
      `<img class="popup-thumb" src="/api/v1/datasets/${radargramId}/views/standard/overview" ` +
      'loading="lazy" alt="">' +
      '</a>'
    );
  },

  /** Wire up a track's hover/popup highlighting, and -- if `card` is
   * given -- two-way highlighting with its catalog card: hovering either
   * the track or the card highlights both, and the track's own popup
   * being open counts as "highlighted" too (PFA_website's
   * popupopen/popupclose pattern), so the two highlight sources agree
   * rather than fighting over the layer's weight when one ends before
   * the other. `layers` is an array because one track can be several
   * polyline segments. */
  bindTrackHighlight(layers, card, baseWeight, focusWeight) {
    let hovered = false;
    let popupOpen = false;
    const apply = () => {
      const on = hovered || popupOpen;
      layers.forEach((layer) => {
        layer.setStyle({ weight: on ? focusWeight : baseWeight });
        if (on) layer.bringToFront();
      });
      if (card) card.classList.toggle("is-hovered", on);
    };
    layers.forEach((layer) => {
      layer.on("mouseover", () => { hovered = true; apply(); });
      layer.on("mouseout", () => { hovered = false; apply(); });
      layer.on("popupopen", () => { popupOpen = true; apply(); });
      layer.on("popupclose", () => { popupOpen = false; apply(); });
    });
    if (card) {
      card.addEventListener("mouseenter", () => { hovered = true; apply(); });
      card.addEventListener("mouseleave", () => { hovered = false; apply(); });
    }
  },
});
