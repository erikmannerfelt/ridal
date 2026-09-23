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

  /* The basemaps this page may draw on, as the server resolved them (#177):
   * the project's own, with Ridal's built-in ESRI World Imagery first unless
   * the project switched it off. Every optional value arrives resolved, so
   * there are no defaults here to drift from the ones in basemaps.rs.
   *
   * Delivered as a body attribute rather than an inline script, so a name or
   * URL containing `</script>` is inert -- minijinja escapes an attribute
   * value, and the browser un-escapes it before JSON.parse sees it.
   *
   * The literal below is the fallback for a page with no attribute at all:
   * the settings and layers pages have no map, and a page rendered by an
   * older template would otherwise have no basemap whatsoever. It is the
   * one place the built-in is written twice, which the Rust test
   * `the_built_in_matches_the_fallback_in_app_js` pins. */
  basemaps: (function readBasemaps() {
    const BUILT_IN = [
      {
        id: "esri-world-imagery",
        name: "ESRI World Imagery",
        url: "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        attribution: "Esri",
        attribution_url: null,
        tile_size: 256,
        max_zoom: 18,
        zoom_offset: 0,
        subdomains: null,
      },
    ];
    const raw = document.body.dataset.basemaps;
    if (!raw) return BUILT_IN;
    try {
      const parsed = JSON.parse(raw);
      // An empty list would leave every map blank, which is a worse answer
      // to a broken config than ignoring it.
      return Array.isArray(parsed) && parsed.length > 0 ? parsed : BUILT_IN;
    } catch {
      return BUILT_IN;
    }
  })(),

  /* Which of them a map opens with: the server's cascade of this person's
   * preference, the project default and the first offered. Empty on a page
   * that carries no basemaps, where `basemap()` falls back to the first. */
  activeBasemap: document.body.dataset.basemap || "",

  /* The vector overlays this project defines (#177), in the order the layer
   * control should list them. Empty unless a project defined some, which is
   * every project until someone does.
   *
   * Delivered the same way the basemaps are, and read the same way: a
   * broken attribute costs the overlays rather than the page. */
  overlays: (function readOverlays() {
    const raw = document.body.dataset.overlays;
    if (!raw) return [];
    try {
      const parsed = JSON.parse(raw);
      return Array.isArray(parsed) ? parsed : [];
    } catch {
      return [];
    }
  })(),

  /** Put `message` into `element`, honouring blank-line paragraph breaks.
   *
   * `element.textContent = message` is right for a sentence and wrong for
   * anything longer: a blank line between paragraphs collapses to a single
   * space, so a message written to hold apart what happened, who can fix it,
   * and what the reader should do next arrives as one undifferentiated block
   * (#248). Assigning `innerHTML` instead would mean trusting every caller's
   * message as markup, and several carry a server-supplied string.
   *
   * A single paragraph sets one text node, exactly as before, so this is a
   * no-op for the short messages that make up nearly every call.
   *
   * The host must be able to hold block children -- `.warning` as a <div>,
   * not as a <p>. A <p> cannot legally contain one. */
  setMessage(element, message) {
    element.replaceChildren();
    const text = message == null ? "" : String(message);
    const paragraphs = text.split("\n\n");
    if (paragraphs.length === 1) {
      element.textContent = text;
      return;
    }
    for (const paragraph of paragraphs) {
      const line = document.createElement("p");
      line.textContent = paragraph;
      element.appendChild(line);
    }
  },

  /** The message for an error response that Ridal did not write.
   *
   * Every Ridal route answers a failure with the same envelope (#120), so a
   * failure carrying no envelope came from something *between* the browser
   * and Ridal. That is almost always the reverse proxy the README recommends
   * putting in front of a served project, and the status says which way it
   * went wrong:
   *
   * - 413: the proxy's own request body limit is below the size of a
   *   radargram, so the upload was refused before Ridal saw a byte of it
   *   (#248). nginx defaults this to 1 MB, which every radargram exceeds.
   * - 502/503/504: the proxy is answering, but Ridal behind it is not.
   *
   * Whoever hits this is typically someone uploading a radargram, with no
   * reason to know what a reverse proxy is. "Could not add it (413)." sends
   * them to every wrong conclusion available -- that the file is corrupt,
   * that it needs re-exporting, trimming, or simply retrying -- and none of
   * those can resolve it, because nothing about the request was wrong.
   *
   * So the message names the one action that does resolve it: hand it to
   * whoever runs the server. The administrator's jargon is kept, since they
   * need it to act, but it is quarantined in its own paragraph addressed to
   * them. Read without that paragraph, the first and last still carry the
   * whole thing -- this is the server's setup rather than anything you did,
   * and here is whether trying again is worth your time. */
  upstreamMessage(status) {
    // The third element is the closing advice, which cannot be shared: a
    // 413 is settled until somebody changes the configuration, while a 502
    // is often a restart in progress and worth another try in a minute.
    // Telling someone to stop retrying something that would succeed on the
    // next attempt is its own wrong answer.
    const [what, fix, next] =
      status === 413
        ? [
            "rejected it as too large",
            "the maximum request body size is smaller than a radargram and " +
              "needs raising (in nginx, client_max_body_size)",
            "Retrying, re-exporting or shrinking the file will not help.",
          ]
        : status === 502 || status === 503 || status === 504
          ? [
              "could not reach Ridal itself",
              "Ridal is not answering the proxy. It may be stopped or " +
                "restarting, or the address the proxy forwards to may be wrong",
              "This one may be temporary, so it is worth trying again in a " +
                "minute. If it keeps happening, it needs fixing on the server.",
            ]
          : [
              `refused it (HTTP ${status})`,
              "it is rejecting requests before they reach Ridal",
              "Trying again is unlikely to help until that is changed.",
            ];
    return (
      "This is a problem with how this Ridal server is set up, not with " +
      `anything you did. The web server in front of Ridal ${what}, so your ` +
      "request never reached Ridal.\n\n" +
      "Please pass this on to whoever administers this Ridal server: " +
      `${fix}.\n\n${next}`
    );
  },

  /** Fetch JSON, turning a non-2xx response into a rejection carrying
   * the server's own message.
   *
   * Every API route answers failures with the same envelope (#120):
   * `{"error": {"code", "message"}}`. A bare `fetch(...).then(r =>
   * r.json())` throws that away -- a 500 parses as JSON perfectly well,
   * so the caller silently proceeds with an object that has no `track`
   * or `entries` field and fails later, somewhere unrelated. This
   * surfaces the message the server already took the trouble to write.
   *
   * The `code` is attached to the Error so a caller can branch on it
   * without string-matching the human-readable message. */
  async fetchJson(url, options) {
    let response;
    try {
      response = await fetch(url, options);
    } catch (networkError) {
      // fetch() rejects only on network-level failure, where there is no
      // response and therefore no envelope to read.
      throw new Error(`network request failed (${networkError.message})`);
    }

    let body = null;
    try {
      body = await response.json();
    } catch {
      // A non-JSON body is itself the problem when the status is bad;
      // when the status is fine it means the route broke its contract.
      if (!response.ok) {
        // Ridal always answers a failure with an envelope, so a bad status
        // that did not parse as JSON was written by something in front of
        // it rather than by a route here (#248).
        const error = new Error(RIDAL.upstreamMessage(response.status));
        error.code = 'upstream';
        error.status = response.status;
        throw error;
      }
      throw new Error('response was not valid JSON');
    }

    if (!response.ok) {
      const envelope = body && body.error;
      const error = new Error(
        (envelope && envelope.message) || `HTTP ${response.status}`,
      );
      error.code = (envelope && envelope.code) || null;
      error.status = response.status;
      throw error;
    }
    return body;
  },

  /** Show `message` over the element with id `hostId`, as a dismissible
   * overlay.
   *
   * Failures used to be invisible: a failed `/track` left a blank map
   * that reads as "no data here" rather than "the request failed". The
   * overlay sits on the element that would otherwise be mysteriously
   * empty, so the explanation is where the user is already looking. */
  reportError(hostId, message) {
    const host = document.getElementById(hostId);
    if (!host) {
      console.error(message);
      return;
    }
    // The host is usually a Leaflet container, which is positioned;
    // guard the case where it is not so the overlay cannot escape it.
    if (getComputedStyle(host).position === 'static') {
      host.style.position = 'relative';
    }
    const box = document.createElement('div');
    box.className = 'error-overlay';
    box.setAttribute('role', 'status');

    const text = document.createElement('span');
    text.textContent = message;

    const dismiss = document.createElement('button');
    dismiss.type = 'button';
    dismiss.className = 'error-overlay-dismiss';
    dismiss.textContent = '×';
    dismiss.setAttribute('aria-label', 'Dismiss');
    dismiss.addEventListener('click', () => box.remove());

    box.append(text, dismiss);
    host.appendChild(box);
  },

  /** Show `message` as a dismissible box inside `hostId`.
   *
   * The sibling of `reportError`, for hosts that are not maps. That one
   * positions itself absolutely over a container that would otherwise be
   * mysteriously empty; this one is an ordinary block that pushes the page
   * down, because the things it reports on -- a download that did not
   * happen -- have no empty container to sit over.
   *
   * `tone` is 'problem' or 'note': a download that failed, or one that
   * succeeded with a caveat worth reading. */
  reportProblem(hostId, message, tone) {
    const host = document.getElementById(hostId);
    if (!host) {
      console.error(message);
      return;
    }
    const box = document.createElement('div');
    box.className = 'warning page-problem';
    box.setAttribute('role', 'status');
    if (tone === 'note') box.classList.add('page-problem-note');

    const text = document.createElement('span');
    text.textContent = message;

    const dismiss = document.createElement('button');
    dismiss.type = 'button';
    dismiss.className = 'error-overlay-dismiss';
    dismiss.textContent = '×';
    dismiss.setAttribute('aria-label', 'Dismiss');
    dismiss.addEventListener('click', () => box.remove());

    box.append(text, dismiss);
    host.replaceChildren(box);
  },

  /** Download `url`, reporting a failure into `hostId` instead of
   * navigating to it.
   *
   * `window.location.href = url` is the obvious way to start a download
   * and it has one bad failure mode: when the server refuses, the browser
   * leaves the page and renders the JSON error envelope as the document.
   * Asking for layer points of a radargram nobody has interpreted yet
   * threw away the viewer and replaced it with `{"error":{...}}`, which
   * is a poor way to learn something as ordinary as "there are no picks
   * here".
   *
   * Fetching instead keeps the page, so the refusal can be shown where
   * the user already is -- and lets the `Warning` header be read, which a
   * navigation silently discarded. The server sets it on a merged export
   * that omitted radargrams nobody has picked yet, and "this file looks
   * complete and is not" is exactly the thing worth surfacing.
   *
   * Deliberately not used for the radargram NetCDF: those reach 145 MB
   * and are streamed by the server precisely so nothing has to hold them
   * whole, which a blob here would undo. That download has no failure
   * mode a person can act on anyway -- the permission cases are gone from
   * the menu before they can be clicked. */
  async download(url, hostId) {
    let response;
    try {
      response = await fetch(url);
    } catch (networkError) {
      RIDAL.reportProblem(
        hostId,
        `Could not reach the server: ${networkError.message}`,
      );
      return;
    }

    if (!response.ok) {
      let message = `The download failed (${response.status}).`;
      try {
        const body = await response.json();
        if (body && body.error && body.error.message) message = body.error.message;
      } catch {
        // A non-JSON body from a failing download is not worth reporting
        // over the status code it came with.
      }
      RIDAL.reportProblem(hostId, message);
      return;
    }

    const blob = await response.blob();
    const href = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = href;
    anchor.download = RIDAL.filenameFrom(response.headers.get('content-disposition'));
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    // Next tick: revoking synchronously cancels the download in Safari.
    setTimeout(() => URL.revokeObjectURL(href), 0);

    const caveat = response.headers.get('warning');
    if (caveat) {
      // Said to have downloaded, because the box otherwise reads as a
      // failure: the caveat alone gives no clue that a file just arrived.
      RIDAL.reportProblem(
        hostId,
        `Downloaded. ${RIDAL.warningText(caveat)}`,
        'note',
      );
    }
  },

  /** The filename a `Content-Disposition` asks for, or '' to let the
   * browser derive one from the URL.
   *
   * Every one Ridal sends is built from validated slugs, so this does not
   * try to handle the quoting and encoding the header allows in general. */
  filenameFrom(disposition) {
    const match = /filename="([^"]*)"/.exec(disposition || '');
    return match ? match[1] : '';
  },

  /** The human part of an HTTP `Warning` header, as a sentence.
   *
   * Ridal sends `199 ridal "<text>"`; the code and agent are ceremony the
   * reader does not need. The capital is applied here rather than at the
   * source because one of these texts is shared with the CLI, which
   * prints it mid-sentence after a prefix of its own. */
  warningText(header) {
    const match = /^\s*\d{3}\s+\S+\s+"(.*)"\s*$/.exec(header);
    const text = match ? match[1] : header;
    return text.charAt(0).toUpperCase() + text.slice(1);
  },

  /** Derive an id from a display name, the way Ridal derives one from a
   * filename.
   *
   * The client-side twin of `sanitize_to_slug` in `src/identity.rs`, and
   * deliberately the same rules: lowercase, Nordic letters transliterated
   * (Drønbreen -> dronbreen, which matters in the places this tool is used),
   * runs of anything else collapsed to `-`, separators trimmed off both
   * ends. An all-punctuation name gives `""`, which the caller must treat as
   * "no id could be derived" rather than as an id.
   *
   * Here rather than on the server because it is shown while typing: the
   * settings page puts the derived id in the id box's placeholder, so what
   * gets stored is what was on screen. `no_two_scripts_on_a_page_declare_
   * the_same_global` in assets.rs keeps this from colliding with anything. */
  slugify(name) {
    const nordic = { "ø": "o", "æ": "ae", "å": "aa" };
    let out = "";
    let lastWasSeparator = false;
    for (const character of String(name).toLowerCase()) {
      if (nordic[character]) {
        out += nordic[character];
        lastWasSeparator = false;
      } else if (/[a-z0-9_-]/.test(character)) {
        out += character;
        lastWasSeparator = character === "-" || character === "_";
      } else if (!lastWasSeparator) {
        out += "-";
        lastWasSeparator = true;
      }
    }
    return out.replace(/^[-_]+/, "").replace(/[-_]+$/, "");
  },

  /** Text as HTML, for the two places Leaflet insists on markup.
   *
   * Leaflet's attribution control writes its content with `innerHTML`, so a
   * credit line is a scripting primitive unless it is escaped -- and a
   * project's basemaps are editable by an `operator`, a role below `admin`.
   * Escaping here rather than refusing angle brackets server-side keeps
   * "Kartverket <kartverket.no>" a legal thing to write. */
  escapeHtml(text) {
    return String(text)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  },

  /** One basemap's attribution, as the markup Leaflet wants.
   *
   * A link when the provider's terms ask for one -- OpenStreetMap's do --
   * built here from a scheme-checked URL rather than accepted as markup.
   * New tab, so reading the terms does not take someone out of the viewer
   * mid-pick. */
  attributionHtml(entry) {
    if (!entry.attribution) return "";
    const text = RIDAL.escapeHtml(entry.attribution);
    if (!entry.attribution_url) return text;
    const href = RIDAL.escapeHtml(entry.attribution_url);
    return `<a href="${href}" target="_blank" rel="noopener noreferrer">${text}</a>`;
  },

  /** Add this page's basemaps to `map` and return it.
   *
   * Every offered basemap becomes a layer, but only the active one is added:
   * the others exist so the control can switch to them, and adding them all
   * would have every map fetching every provider's tiles at once.
   *
   * The control appears only when there is something to switch between. A
   * project that never defined a basemap therefore looks exactly as it did
   * before #177, rather than growing a menu with one item in it. */
  basemap(map, hostId) {
    // A null-prototype dictionary, not `{}`: these keys are free-text
    // names, and a basemap called `__proto__` would hit the prototype
    // setter instead of becoming an entry -- the layer would simply not be
    // in the control, with nothing anywhere saying why.
    const layers = Object.create(null);
    const labels = new Set();
    let active = null;

    for (const entry of RIDAL.basemaps) {
      const layer = L.tileLayer(entry.url, {
        maxZoom: entry.max_zoom,
        tileSize: entry.tile_size,
        zoomOffset: entry.zoom_offset,
        // Leaflet's own default, restated: passing null would make `{s}`
        // resolve to nothing rather than to a host.
        subdomains: entry.subdomains || "abc",
        attribution: RIDAL.attributionHtml(entry),
      });
      const label = RIDAL.uniqueLabel(labels, entry);
      layers[label] = layer;
      if (active === null || entry.id === RIDAL.activeBasemap) active = layer;
    }

    if (active) active.addTo(map);

    const overlays = RIDAL.overlayLayers(map, hostId);
    // The control appears when there is something to choose: a second
    // basemap, or any overlay. A project that defined neither looks exactly
    // as it did before #177, rather than growing a menu with one item.
    if (Object.keys(layers).length > 1 || Object.keys(overlays).length > 0) {
      L.control.layers(layers, overlays, { collapsed: true }).addTo(map);
    }
    return map;
  },

  /** This page's overlays, as layers keyed by the name to show in the
   * control (#177).
   *
   * Empty and lazy: each one is an `L.layerGroup` with nothing in it until
   * somebody switches it on, at which point the GeoJSON is fetched once and
   * its features are added. An overlay nobody looks at therefore costs a
   * page one empty object, which is what makes a project able to define
   * several without making every map slow.
   *
   * None of them is added to the map here -- overlays start off, because the
   * maps exist to show where the radargrams are and an overlay is context
   * around that. */
  overlayLayers(map, hostId) {
    const layers = Object.create(null);
    const labels = new Set();

    for (const overlay of RIDAL.overlays) {
      const group = L.layerGroup();
      let state = "empty";

      group.on("add", () => {
        if (state !== "empty") return;
        state = "loading";
        RIDAL.loadOverlay(overlay)
          .then((features) => {
            state = "loaded";
            features.addTo(group);
          })
          .catch((error) => {
            // Back to empty, so switching the overlay off and on again is a
            // retry rather than a silent no-op -- a CORS failure or a
            // moved file is exactly the kind of thing someone fixes and
            // tries again.
            state = "empty";
            RIDAL.reportError(
              hostId || map.getContainer().id,
              `Could not draw "${overlay.name}": ${error.message}`,
            );
          });
      });

      layers[RIDAL.uniqueLabel(labels, overlay)] = group;
    }
    return layers;
  },

  /** A layer-control label for `entry` that is escaped, unique, and taken.
   *
   * Two things at once, because both are about the same string:
   *
   * Leaflet writes a layer's name into the control with `innerHTML` (see
   * `_addItem` in the vendored build), so a project naming a basemap
   * `<img src=x onerror=...>` would run it on every map. The name is
   * escaped for the same reason the attribution is.
   *
   * And the control is keyed by that label, so two entries sharing one
   * would silently become one layer. Disambiguating with the id is not
   * enough on its own -- `A`/`one`, `A`/`two` and a third entry actually
   * named `A (two)` all collide -- so this keeps suffixing until the label
   * is genuinely unused, and records what it took. */
  uniqueLabel(taken, entry) {
    const name = RIDAL.escapeHtml(entry.name);
    let label = name;
    if (taken.has(label)) label = `${name} (${RIDAL.escapeHtml(entry.id)})`;
    let attempt = 2;
    while (taken.has(label)) {
      label = `${name} (${RIDAL.escapeHtml(entry.id)} ${attempt})`;
      attempt += 1;
    }
    taken.add(label);
    return label;
  },

  /** Fetch one overlay's GeoJSON and build its layer.
   *
   * Fetched by the browser rather than proxied by Ridal, which keeps an
   * HTTP client out of the binary and keeps the server from making requests
   * to addresses a project member typed. The cost is that a host which does
   * not allow cross-origin reads cannot be used, and that is said plainly
   * rather than left as an empty layer. */
  async loadOverlay(overlay) {
    let response;
    try {
      response = await fetch(overlay.url);
    } catch (networkError) {
      // A CORS refusal reaches script as an indistinguishable network
      // error, so the message has to name both possibilities rather than
      // guess. `${networkError.message}` is usually "Failed to fetch",
      // which on its own tells nobody anything.
      throw new Error(
        "the file could not be fetched. Either it is unreachable, or the " +
          "site hosting it does not allow this page to read it (CORS).",
      );
    }
    if (!response.ok) {
      throw new Error(`the file could not be fetched (HTTP ${response.status}).`);
    }

    let data;
    try {
      data = await response.json();
    } catch {
      throw new Error("the file is not valid JSON, so it is not GeoJSON.");
    }
    if (!data || typeof data !== "object" || !data.type) {
      throw new Error("the file is JSON but not GeoJSON: it has no `type`.");
    }

    const wrongCrs = RIDAL.nonWgs84Reason(data);
    if (wrongCrs) throw new Error(wrongCrs);

    return L.geoJSON(data, {
      style: () => ({ color: overlay.color, weight: 3, fillOpacity: 0.15 }),
      // Points as circle markers rather than Leaflet's default pin: the pin
      // needs an image asset and points here are usually stakes or samples,
      // which read better as small marks than as map pins.
      pointToLayer: (_feature, latlng) =>
        L.circleMarker(latlng, {
          radius: 5,
          color: overlay.color,
          weight: 2,
          fillColor: overlay.color,
          fillOpacity: 0.6,
        }),
      onEachFeature: (feature, layer) => {
        const content = RIDAL.overlayPopup(overlay, feature);
        if (content) layer.bindPopup(content);
      },
    });
  },

  /** Why this GeoJSON cannot be drawn as it is, or `null` if it can.
   *
   * Leaflet reads GeoJSON as RFC 7946 does: longitude and latitude in
   * WGS84. A projected file is not detected by Leaflet at all -- it draws
   * eastings as degrees and puts Svalbard somewhere past the date line,
   * with nothing on screen to say why. Ridal does not reproject (yet), so
   * the least confusing thing it can do is refuse and say what is wrong.
   *
   * Two checks, because a file can be wrong in two ways: it may *declare* a
   * CRS (the pre-2016 GeoJSON member, which RFC 7946 removed but exporters
   * still write), or it may simply carry coordinates no degree can hold. */
  nonWgs84Reason(data) {
    const convert =
      " Reproject it to WGS84 first, for example with " +
      "`ogr2ogr -t_srs EPSG:4326 wgs84.geojson yours.geojson`.";

    // Both forms the 2008 spec allowed: a named CRS, and a link to one.
    // An exporter that writes only the `href` would otherwise walk past
    // this check and be drawn as degrees.
    const properties = (data.crs && data.crs.properties) || {};
    const declared = properties.name || properties.href;
    if (declared && !/(CRS84|EPSG:*0*4326)/i.test(String(declared))) {
      return `the file declares the coordinate system ${declared}, and Ridal draws WGS84 only.${convert}`;
    }

    // Every coordinate, not just the first: a file can open with a
    // plausible point and carry a projected one further in, and "Ridal
    // draws WGS84 only" should be a property of the file rather than of
    // its first vertex. Stops at the first bad one, so a good file costs
    // one pass and a bad one usually much less.
    const outlier = RIDAL.firstCoordinateOutsideDegrees(data);
    if (outlier) {
      return (
        `it has coordinates that are not degrees -- ${outlier[0]}, ` +
        `${outlier[1]} -- so the file is in a projected coordinate ` +
        `system, and Ridal draws WGS84 only.${convert}`
      );
    }
    return null;
  },

  /** The first `[x, y]` in a GeoJSON object that no degree can hold, or
   * null if every coordinate could be WGS84.
   *
   * Walks the nested arrays a geometry can be rather than switching on
   * every geometry type, so a GeometryCollection inside a Feature inside a
   * FeatureCollection is handled without knowing those exist. */
  firstCoordinateOutsideDegrees(data) {
    const walk = (node) => {
      if (!node || typeof node !== "object") return null;
      if (Array.isArray(node)) {
        if (typeof node[0] === "number" && typeof node[1] === "number") {
          return Math.abs(node[0]) > 180 || Math.abs(node[1]) > 90
            ? [node[0], node[1]]
            : null;
        }
        for (const child of node) {
          const found = walk(child);
          if (found) return found;
        }
        return null;
      }
      for (const key of ["coordinates", "geometry", "geometries", "features"]) {
        if (node[key]) {
          const found = walk(node[key]);
          if (found) return found;
        }
      }
      return null;
    };
    return walk(data);
  },

  /** One overlay feature's popup, as DOM nodes, or null when there is
   * nothing to say about it.
   *
   * `name_field` names the property holding the feature's name -- the
   * generalisation of PFA_website's hardcoded `properties.Stake` -- and is
   * inserted as text, so a name containing markup is shown rather than run.
   *
   * `description_field` is deliberately the opposite: it is documented as
   * HTML, because a description that cannot carry a link or a table is not
   * much of a description. It is scrubbed first (see `cleanHtml`). The
   * scrub is defence in depth, not the guarantee -- the guarantee is that
   * an operator chose the URL. */
  overlayPopup(overlay, feature) {
    const properties = (feature && feature.properties) || {};
    const name = overlay.name_field ? properties[overlay.name_field] : null;
    const description = overlay.description_field
      ? properties[overlay.description_field]
      : null;
    if (
      (name === undefined || name === null || name === "") &&
      (description === undefined || description === null || description === "")
    ) {
      return null;
    }

    const box = document.createElement("div");
    box.className = "overlay-popup";
    if (name !== undefined && name !== null && name !== "") {
      const heading = document.createElement("strong");
      heading.textContent = String(name);
      box.appendChild(heading);
    }
    if (description !== undefined && description !== null && description !== "") {
      const body = document.createElement("div");
      body.className = "overlay-popup-body";
      body.append(...RIDAL.cleanHtml(String(description)));
      box.appendChild(body);
    }
    return box;
  },

  /** Parse `html` and return its nodes with the obvious ways to run
   * something removed.
   *
   * Parsed with `DOMParser` rather than assigned to `innerHTML`, so nothing
   * is ever live in this document while it is being inspected: a
   * `<img onerror>` in a detached parse does not fire. Then the elements
   * that execute (`script`, `iframe`, and friends), every `on*` handler and
   * every non-`http(s)` URL are dropped.
   *
   * An allowlist would be stricter, and a sanitiser library stricter still.
   * This is the proportionate version: the HTML comes from a file whose
   * address a project operator typed, which is a person who can already
   * change the project's settings. */
  cleanHtml(html) {
    const parsed = new DOMParser().parseFromString(html, "text/html");
    const forbidden = "script,style,iframe,object,embed,link,meta,base,form";
    parsed.body.querySelectorAll(forbidden).forEach((node) => node.remove());
    parsed.body.querySelectorAll("*").forEach((node) => {
      for (const attribute of [...node.attributes]) {
        const name = attribute.name.toLowerCase();
        const value = attribute.value.trim().toLowerCase();
        const isUrl = name === "href" || name === "src" || name === "xlink:href";
        if (
          name.startsWith("on") ||
          (isUrl && !/^(https?:|mailto:|#|\/|\.)/.test(value))
        ) {
          node.removeAttribute(attribute.name);
        }
      }
      if (node.tagName === "A") {
        // A link out of a popup opens beside the viewer rather than
        // replacing it, and does not hand the destination this page.
        node.setAttribute("target", "_blank");
        node.setAttribute("rel", "noopener noreferrer");
      }
    });
    return [...parsed.body.childNodes];
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
  /** A track popup: the radargram's label, a thumbnail, and a link to it.
   *
   * `profile` applies to *both* -- the link, so arriving at the radargram
   * keeps the profile being browsed in, and the thumbnail, which is a
   * render and otherwise comes back in the default profile regardless of
   * what the rest of the page is showing.
   *
   * Callers should pass this to `bindPopup` as a function rather than a
   * string, so the profile is read when the popup opens. The viewer's
   * profile can change without a page reload, and a popup built at load
   * time would keep showing the profile that was active then. */
  /** Build an API path with every segment percent-encoded.
   *
   * The values that reach these today are server-rendered slugs --
   * validated `RadargramId`, `GroupId`, `UserId`, and a profile name from
   * a select whose options the server wrote. So this is not closing a
   * live hole. It is that an un-encoded URL segment is a latent bug
   * rather than a safe assumption: one containing a slash, `?` or `#`
   * silently addresses a different resource than intended, and the next
   * value routed through here may not come from the server.
   *
   * Encoding at construction means no caller has to know where its value
   * came from, which is the only version of this that stays true.
   */
  apiPath(...segments) {
    return `/api/v1/${segments.map((s) => encodeURIComponent(s)).join("/")}`;
  },

  /** Build a track popup as DOM nodes rather than an HTML string.
   *
   * Leaflet assigns a string popup with `innerHTML` and appends an element
   * with `appendChild` (see `_updateContent` in the vendored build), so
   * returning a node means nothing here is ever parsed as HTML. That
   * removes the escaping question rather than answering it: `label` is
   * free text from a radargram's metadata, and the profile comes from a
   * <select> whose value a scanner cannot prove is constrained.
   *
   * Callers should pass this to `bindPopup` as a function rather than a
   * string, so the profile is read when the popup opens. The viewer's
   * profile can change without a page reload, and a popup built at load
   * time would keep showing the profile that was active then. */
  popupContent(radargramId, label, profile) {
    const query = profile ? `?profile=${encodeURIComponent(profile)}` : "";
    const id = encodeURIComponent(radargramId);

    const link = document.createElement("a");
    link.className = "popup-link";
    link.href = `/view/${id}${query}`;
    // A text node: markup in a display name is shown, never run.
    link.appendChild(document.createTextNode(label));

    const thumb = document.createElement("img");
    thumb.className = "popup-thumb";
    thumb.src = `${RIDAL.apiPath("datasets", radargramId, "views", "standard", "overview")}${query}`;
    thumb.loading = "lazy";
    thumb.alt = "";
    link.appendChild(thumb);

    return link;
  },

  /** Wire up a track's hover/popup highlighting, and -- if `card` is
   * given -- two-way highlighting with its catalog card: hovering either
   * the track or the card highlights both, and the track's own popup
   * being open counts as "highlighted" too (PFA_website's
   * popupopen/popupclose pattern), so the two highlight sources agree
   * rather than fighting over the layer's weight when one ends before
   * the other. `layers` is an array because one track can be several
   * polyline segments. */
  /** How wide, in pixels, the invisible strip along a line that accepts a
   * tap or hover.
   *
   * A Leaflet polyline is only interactive within its own stroke, so a 3px
   * track has a 3px target -- unusable with a finger and fiddly with a
   * mouse. Every interactive line therefore gets a transparent companion of
   * this width. Roughly a fingertip on touch, a comfortable aim otherwise. */
  hitWidth: window.matchMedia("(pointer: coarse)").matches ? 34 : 14,

  /** A transparent, interactive companion for `latlngs`.
   *
   * Add it *before* the visible line so the visible one draws on top, and
   * put every handler on this rather than on the line it shadows -- a
   * visible line left interactive would swallow events aimed at the easier
   * target. */
  hitLine(latlngs, pane) {
    return L.polyline(latlngs, {
      className: "hit-line",
      weight: RIDAL.hitWidth,
      opacity: 0,
      interactive: true,
      // The picker passes its own line pane so the wide tap target shares the
      // same z-order as the line it shadows; everywhere else keeps the
      // default. See `viewer.js`'s pane setup (#209).
      pane: pane || "overlayPane",
    });
  },

  /** Two-way hover/popup highlighting for a set of track lines.
   *
   * Takes `{ visible, hit }` pairs: events come from the wide companion,
   * while the weight change is applied to the line that can actually be
   * seen. */
  bindTrackHighlight(pairs, card, baseWeight, focusWeight) {
    let hovered = false;
    let popupOpen = false;
    const apply = () => {
      const on = hovered || popupOpen;
      pairs.forEach(({ visible, hit }) => {
        visible.setStyle({ weight: on ? focusWeight : baseWeight });
        if (on) {
          // Order matters: the companion first, so the visible line still
          // ends up above it.
          hit.bringToFront();
          visible.bringToFront();
        }
      });
      if (card) card.classList.toggle("is-hovered", on);
    };
    pairs.forEach(({ hit }) => {
      const layer = hit;
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

  /** The derived-item editor, shared by the viewer panel and the /layers page.
   *
   * Built once, lazily, and appended to `<body>`, so both pages use the same
   * dialog and the same save path. Two editors that drift apart is a worse
   * outcome than one that is slightly awkward in both places.
   *
   * The one thing that cannot be shared is the live preview, which needs a
   * radargram: the caller passes `preview`, and when it is absent the dialog
   * says there is nothing to preview against rather than looking broken.
   *
   * `open({ item, items, unusable, canRelease, preview, onSaved, onClose })`
   * where `item` is an existing item or `null`, `items` is every item the
   * caller can see (the save is a full PUT of that set), and `preview` is
   * `async (expression, unit) => ({ kind, unit })` or `null`.
   *
   * `audience` is a permission control, not a display option: the release
   * checkbox is shown only to a caller who may set it, and labelled as
   * publishing other contributors' work. `put_derived` requires `Role::Admin`
   * for it, and that server check is the real gate; a 403 it returns is shown
   * verbatim rather than folded into a generic failure.
   */
  derivedEditor: (function () {
    // Must match `interp::derive`'s registered functions, or the highlighter
    // colours a name the evaluator does not know.
    const BUILTINS = [
      "count", "median", "mean", "std", "nmad", "percentile",
      "min", "max", "concatenate", "shallowest",
      "deepest", "clamp", "where",
    ];
    const KEYWORDS = ["if", "else", "true", "false", "NaN"];
    // What the colour picker shows for an item that has none of its own.
    // Mirrors the `value` in the markup below rather than interpolating into
    // it: that block is deliberately static so it is trivial to audit, and
    // `open` assigns the picker on every open anyway.
    const NEW_ITEM_COLOR = "#ffcc00";
    const HEX_COLOR = /^#[0-9a-fA-F]{6}$/;
    const UNITS = [
      ["meters", "metres"],
      ["nanoseconds", "nanoseconds"],
      ["samples", "samples"],
      ["dimensionless", "dimensionless"],
    ];

    let dialog = null;
    let fields = null;
    let options = null;
    let editingId = null;
    let originalAudience = "own_picks";
    let originalScope = "project";
    let previewTimer = null;
    // Bumped whenever a preview must no longer be trusted (a save started or
    // failed), so a slower in-flight preview cannot overwrite a save error.
    let previewGeneration = 0;

    function build() {
      dialog = document.createElement("dialog");
      dialog.id = "derived-editor";
      // Static markup only; every value that comes from a document is set
      // through `.value`/`.textContent` below, never interpolated here.
      dialog.innerHTML = `
        <h2 id="derived-editor-title">New derived expression</h2>
        <p class="hint" id="derived-editor-hint"></p>
        <div class="add-layer-fields">
          <label>Name <input id="derived-name" type="text" autocomplete="off"></label>
          <label>Id <input id="derived-id" type="text" autocomplete="off" spellcheck="false"></label>
          <label>Unit <select id="derived-unit">${UNITS.map(
            ([value, label]) => `<option value="${value}">${label}</option>`,
          ).join("")}</select></label>
          <label class="checkbox"><input id="derived-listed" type="checkbox" checked> Show in the viewer's layer list</label>
          <label class="checkbox"><input id="derived-show" type="checkbox"> Draw it when the viewer opens</label>
        </div>
        <div class="editor-input">
          <pre id="derived-highlight" aria-hidden="true"></pre>
          <textarea id="derived-expression" rows="4" spellcheck="false"
                    list="derived-suggestions" autocomplete="off"></textarea>
          <datalist id="derived-suggestions"></datalist>
        </div>
        <p class="editor-status" id="derived-status" role="status" aria-live="polite"></p>
        <p class="editor-warning" id="derived-unusable" hidden></p>
        <div class="add-layer-fields">
          <label>Colour <input id="derived-color" type="text" placeholder="#rrggbb" autocomplete="off"></label>
          <label>Pick <input id="derived-color-picker" type="color" value="#ffcc00"></label>
          <label class="checkbox"><input id="derived-no-color" type="checkbox"> No colour</label>
        </div>
        <fieldset id="derived-range-fields">
          <label class="checkbox"><input id="derived-range" type="checkbox"> Range fill</label>
          <div class="add-layer-fields">
            <label>Target <select id="derived-range-target"></select></label>
            <label>Colour <input id="derived-range-color" type="text" placeholder="optional" autocomplete="off"></label>
            <label>Pick <input id="derived-range-picker" type="color" value="#888888"></label>
            <label>Opacity <input id="derived-range-opacity" type="number" min="0" max="1" step="0.05" value="0.25"></label>
          </div>
        </fieldset>
        <label class="checkbox" id="derived-audience-row" hidden>
          <input id="derived-release" type="checkbox">
          Visible to everyone (publishes every contributor's picks in aggregate)
        </label>
        <p>
          <button id="derived-save" type="button">Save</button>
          <button id="derived-cancel" type="button">Close</button>
        </p>`;
      document.body.appendChild(dialog);
      fields = {
        title: dialog.querySelector("#derived-editor-title"),
        hint: dialog.querySelector("#derived-editor-hint"),
        name: dialog.querySelector("#derived-name"),
        id: dialog.querySelector("#derived-id"),
        unit: dialog.querySelector("#derived-unit"),
        show: dialog.querySelector("#derived-show"),
        listed: dialog.querySelector("#derived-listed"),
        expression: dialog.querySelector("#derived-expression"),
        highlight: dialog.querySelector("#derived-highlight"),
        status: dialog.querySelector("#derived-status"),
        unusable: dialog.querySelector("#derived-unusable"),
        suggestions: dialog.querySelector("#derived-suggestions"),
        color: dialog.querySelector("#derived-color"),
        colorPicker: dialog.querySelector("#derived-color-picker"),
        noColor: dialog.querySelector("#derived-no-color"),
        range: dialog.querySelector("#derived-range"),
        rangeFields: dialog.querySelector("#derived-range-fields"),
        rangeTarget: dialog.querySelector("#derived-range-target"),
        rangeColor: dialog.querySelector("#derived-range-color"),
        rangePicker: dialog.querySelector("#derived-range-picker"),
        rangeOpacity: dialog.querySelector("#derived-range-opacity"),
        audienceRow: dialog.querySelector("#derived-audience-row"),
        release: dialog.querySelector("#derived-release"),
        save: dialog.querySelector("#derived-save"),
        cancel: dialog.querySelector("#derived-cancel"),
      };

      fields.name.addEventListener("input", () => {
        if (!editingId) fields.id.value = sanitizeId(fields.name.value);
      });
      fields.color.addEventListener("input", () => {
        if (HEX_COLOR.test(fields.color.value.trim())) {
          fields.colorPicker.value = fields.color.value.trim();
        }
      });
      fields.colorPicker.addEventListener("input", () => {
        fields.color.value = fields.colorPicker.value;
        fields.noColor.checked = false;
      });
      fields.rangePicker.addEventListener("input", () => {
        fields.rangeColor.value = fields.rangePicker.value;
      });
      fields.noColor.addEventListener("change", syncColorEnabled);
      fields.range.addEventListener("change", syncRangeEnabled);
      fields.expression.addEventListener("input", () => {
        syncHighlight();
        schedulePreview();
      });
      fields.expression.addEventListener("scroll", syncHighlight);
      fields.unit.addEventListener("change", schedulePreview);
      fields.save.addEventListener("click", save);
      fields.cancel.addEventListener("click", close);
      // Native Escape close; make sure the preview line goes with it.
      dialog.addEventListener("close", clearPreview);
      return dialog;
    }

    /* Enable or disable the colour inputs to match the "No colour" box.
     *
     * Wired to the checkbox, not only called when the dialog opens: an item
     * with no colour opens with the box checked and both inputs disabled, so
     * without a listener here unchecking it left them greyed out for the rest
     * of the dialog's life and the item could never be given a colour at all.
     *
     * Seeding the text field from the picker when it is empty is the other
     * half of that: `save` reads the *text* field, so unchecking the box and
     * pressing Save without touching the picker would otherwise write no
     * colour back while the picker sat there showing one.
     */
    function syncColorEnabled() {
      const on = !fields.noColor.checked;
      fields.color.disabled = !on;
      fields.colorPicker.disabled = !on;
      if (on && fields.color.value.trim() === "") {
        fields.color.value = fields.colorPicker.value;
      }
    }

    function syncRangeEnabled() {
      const on = fields.range.checked;
      fields.rangeFields.classList.toggle("is-disabled", !on);
      fields.rangeTarget.disabled = !on;
      fields.rangeColor.disabled = !on;
      fields.rangePicker.disabled = !on;
      fields.rangeOpacity.disabled = !on;
    }

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
      fields.highlight.innerHTML = `${highlight(fields.expression.value)}\n`;
      fields.highlight.scrollTop = fields.expression.scrollTop;
      fields.highlight.scrollLeft = fields.expression.scrollLeft;
    }

    function clearPreview() {
      if (previewTimer) {
        clearTimeout(previewTimer);
        previewTimer = null;
      }
      // The caller's way to drop its preview line. Called here rather than in
      // `close` so an invalid expression clears it too, not just a close.
      if (options && options.onClose) options.onClose();
    }

    function schedulePreview() {
      if (previewTimer) clearTimeout(previewTimer);
      if (!options || !options.preview) return;
      const generation = ++previewGeneration;
      previewTimer = setTimeout(() => runPreview(generation), 300);
    }

    async function runPreview(generation) {
      if (!options || !options.preview) return;
      const expression = fields.expression.value.trim();
      if (!expression) {
        fields.status.textContent = "";
        fields.status.classList.remove("editor-error");
        if (options.onClose) options.onClose();
        return;
      }
      let body;
      try {
        body = await options.preview(expression, fields.unit.value);
      } catch (error) {
        // An invalid expression clears the previous preview rather than
        // leaving a stale line on screen pretending to be the current one.
        if (generation !== previewGeneration) return;
        clearPreview();
        fields.status.textContent = error.message;
        fields.status.classList.add("editor-error");
        return;
      }
      if (generation !== previewGeneration) return;
      fields.status.classList.remove("editor-error");
      const kindLabel = body.kind === "layer" ? "layer (a line)" : "attribute";
      fields.status.textContent = `${kindLabel} · ${body.unit || "no unit"}`;
    }

    function buildSuggestions() {
      const names = [
        ...(options.layerIds || []),
        ...options.items.map((item) => item.id),
        ...BUILTINS,
      ];
      fields.suggestions.replaceChildren(
        ...names.map((name) => {
          const option = document.createElement("option");
          option.value = name;
          return option;
        }),
      );
    }

    function buildTargets(current) {
      // `"layer"` is what `Kind` serialises to -- an item that is a line on
      // the radargram, as opposed to an `"attribute"`, which has no position
      // to fill toward. There has never been a `"position"`, so this filter
      // used to match nothing and the dropdown offered only `None`.
      const targets = options.items.filter(
        (item) => item.kind === "layer" && item.id !== editingId,
      );
      const choices = [["", "None"]].concat(
        targets.map((item) => [item.id, item.name || item.id]),
      );
      // A target the caller cannot see (or a non-position one) is not offered,
      // but an existing one is kept as an option so saving does not silently
      // drop the range.
      if (current && !choices.some(([value]) => value === current)) {
        choices.push([current, `${current} (current)`]);
      }
      fields.rangeTarget.replaceChildren(
        ...choices.map(([value, label]) => {
          const option = document.createElement("option");
          option.value = value;
          option.textContent = label;
          return option;
        }),
      );
    }

    function open(newOptions) {
      if (!dialog) build();
      options = newOptions;
      const item = newOptions.item;
      editingId = item ? item.id : null;
      originalAudience = item ? item.audience : "own_picks";
      originalScope = item ? item.scope : "project";

      fields.title.textContent = item
        ? `Edit '${item.name || item.id}'`
        : "New derived expression";
      fields.hint.textContent = newOptions.preview
        ? "A Rhai expression over the layer ids and derived items. The line is previewed on the radargram as you type, without saving."
        : "A Rhai expression over the layer ids and derived items. There is no radargram on this page, so there is no live preview; the expression is checked when you save.";
      fields.name.value = item ? item.name : "";
      fields.id.value = item ? item.id : "";
      // Ids are immutable (#206): shown but not editable when editing.
      fields.id.readOnly = Boolean(item);
      fields.unit.value = item ? item.unit : "meters";
      fields.show.checked = item ? Boolean(item.show) : false;
      fields.listed.checked = item ? item.listed !== false : true;
      fields.expression.value = item ? item.expression : "";
      fields.color.value = item && item.color ? item.color : "";
      fields.noColor.checked = !(item && item.color);
      // The dialog is built once and reused, so the picker keeps whatever
      // the last item left in it unless it is reset here. Left alone, the
      // swatch disagreed with the text field, and clicking it overwrote the
      // item's real colour with the previous item's.
      fields.colorPicker.value =
        item && item.color && HEX_COLOR.test(item.color)
          ? item.color
          : NEW_ITEM_COLOR;
      syncColorEnabled();
      const fill = item && item.fill_to ? item.fill_to : null;
      fields.range.checked = Boolean(fill);
      fields.rangeColor.value = fill && fill.color ? fill.color : "";
      fields.rangeOpacity.value = fill && fill.opacity != null ? fill.opacity : 0.25;
      buildTargets(fill ? fill.target : null);
      fields.rangeTarget.value = fill ? fill.target : "";
      syncRangeEnabled();
      fields.release.checked = originalAudience === "released";
      fields.audienceRow.hidden = !newOptions.canRelease;
      fields.unusable.hidden = !newOptions.unusable || newOptions.unusable.length === 0;
      fields.unusable.textContent =
        newOptions.unusable && newOptions.unusable.length
          ? `These layer ids cannot be used in an expression (a hyphen parses as a minus): ${newOptions.unusable.join(", ")}.`
          : "";
      previewGeneration++;
      fields.status.textContent = "";
      fields.status.classList.remove("editor-error");
      buildSuggestions();
      syncHighlight();
      if (typeof dialog.showModal === "function") dialog.showModal();
      else dialog.setAttribute("open", "");
      if (newOptions.preview) schedulePreview();
    }

    function close() {
      if (!dialog) return;
      // The `close` event does the cleanup, so an Escape and this button take
      // exactly the same path.
      if (typeof dialog.close === "function") {
        dialog.close();
      } else {
        clearPreview();
        dialog.removeAttribute("open");
      }
    }

    function editorItem() {
      const name = fields.name.value.trim();
      const id = (fields.id.value.trim() || sanitizeId(name)).trim();
      const noColor = fields.noColor.checked;
      const color = noColor ? null : fields.color.value.trim() || null;
      const fill =
        fields.range.checked && fields.rangeTarget.value
          ? {
              target: fields.rangeTarget.value,
              color: fields.rangeColor.value.trim() || null,
              opacity:
                fields.rangeOpacity.value === ""
                  ? 0.25
                  : Number(fields.rangeOpacity.value),
            }
          : null;
      return {
        id,
        name: name || id,
        expression: fields.expression.value.trim(),
        unit: fields.unit.value,
        color,
        show: fields.show.checked,
        listed: fields.listed.checked,
        fill_to: fill,
        scope: originalScope,
        audience: options.canRelease
          ? fields.release.checked
            ? "released"
            : "own_picks"
          : originalAudience,
      };
    }

    async function save() {
      // Any preview still in flight must not overwrite the result of this
      // save, success or failure.
      previewGeneration++;
      if (previewTimer) {
        clearTimeout(previewTimer);
        previewTimer = null;
      }
      const item = editorItem();
      if (!item.expression) {
        fields.status.textContent = "The expression is empty.";
        fields.status.classList.add("editor-error");
        return;
      }
      const next = options.items.map((existing) => ({
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
      const index = next.findIndex((existing) => existing.id === editingId);
      if (index >= 0) next[index] = { ...next[index], ...item };
      else next.push(item);
      try {
        await RIDAL.fetchJson("/api/v1/derived", {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            schema: "ridal-derived",
            schema_version: "1",
            items: next,
          }),
        });
      } catch (error) {
        // The server's message, verbatim -- including a 403 on releasing,
        // which must not be folded into a generic failure.
        fields.status.textContent = error.message;
        fields.status.classList.add("editor-error");
        return;
      }
      close();
      await options.onSaved();
    }

    // `highlight` is exposed so the /layers derived list can render the same
    // token colours as the editor, rather than a second tokenizer drifting
    // from it. It returns sanitised HTML (every token escaped).
    return { open, close, highlight };
  })(),
});

/* Dismiss any menu on Escape or a click outside it.
 *
 * `<details>` gives the disclosure, the keyboard behaviour and the open
 * state for free, but it stays open until its own summary is clicked again,
 * which is wrong for a menu: tapping the page elsewhere should close it.
 * That is the only reason this file knows menus exist.
 *
 * Applies to every `.site-menu` rather than one by id, so the header menu
 * and the viewer's download menu behave the same and a third would too.
 *
 * The outside-click listener runs in the capture phase, which no handler
 * can opt out of. That is insurance rather than a fix for an observed
 * failure: several handlers in the viewer call `stopPropagation`, and the
 * obvious worry is that one of them hides a click from the document. In
 * practice the one most likely to -- the picker's, on every picked line --
 * does not, measured by counting document listeners in both phases. Capture
 * costs nothing and removes the question. */
(function setupMenus() {
  const menus = [...document.querySelectorAll("details.site-menu")];
  if (menus.length === 0) return;

  document.addEventListener(
    "click",
    (event) => {
      for (const menu of menus) {
        if (menu.open && !menu.contains(event.target)) menu.open = false;
      }
    },
    true,
  );

  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    for (const menu of menus) {
      if (!menu.open) continue;
      menu.open = false;
      // Return focus to the control that opened it, or the close is
      // invisible to a keyboard user.
      menu.querySelector("summary").focus();
    }
  });
})();
