/* The settings page: my preferences, the project defaults, and access.
 *
 * First-party, embedded via assets.rs, loaded after app.js. Deliberately
 * NOT under assets/vendor/ -- scripts/vendor_leaflet.sh does `rm -rf` on
 * that directory.
 *
 * Three sections in one file because they share one load: the settings
 * endpoint already answers what every one of them needs to render, and
 * splitting them would mean three requests to draw one page.
 *
 * Wrapped in an IIFE so it declares nothing globally; `assets.rs` has a
 * test that fails if any two scripts on a page collide.
 */
(function () {
  "use strict";

  const byId = (id) => document.getElementById(id);

  const projectForm = byId("settings-form");
  const myForm = byId("my-settings-form");
  const accessSection = byId("access-section");
  if (!projectForm && !myForm && !accessSection) return; // Not a project.

  const errorBox = byId("settings-error");

  let canEditProject = false;
  let canEditAccess = false;
  let xscales = [];
  let profiles = [];
  /* Offered by the server so a stored value always has an option to select,
   * and so these lists cannot drift from the download dialogs' (#166). */
  let spacings = [];
  let formats = [];
  let themes = [];
  /* Two lists, as the settings endpoint answers them: `offeredBasemaps` is
   * what can be chosen (the project's, plus the built-in), and `basemaps` is
   * the project's own editable entries. Only the second is ever sent back. */
  let offeredBasemaps = [];
  let basemaps = [];
  /* Which list-shaped sections hold edits the server has not seen (#177
   * review). Adding a basemap puts a card on the page, which looks like it
   * happened -- so the section says it has not, and leaving the page asks
   * first. Keyed by section id so the two lists are tracked apart: saving
   * the basemaps must not quietly claim the overlays are safe. */
  const unsaved = new Set();
  /* The project's vector overlays (#177). One list, unlike the basemaps:
   * there is no built-in to offer alongside them. */
  let overlays = [];

  /* Mark (or clear) a section as holding unsaved edits.
   *
   * The class drives the badge and the rule beside the heading in app.css;
   * the `Set` drives the question asked on the way out. */
  const setUnsaved = (sectionId, isUnsaved) => {
    const section = byId(sectionId);
    if (!section) return;
    section.classList.toggle("is-unsaved", isUnsaved);
    if (isUnsaved) unsaved.add(sectionId);
    else unsaved.delete(sectionId);
  };

  /* A page with pending edits does not leave without asking. The same guard
   * picker.js puts on unsaved picks, for the same reason: the work is in
   * the page and nowhere else until Save. */
  window.addEventListener("beforeunload", (event) => {
    if (unsaved.size > 0) event.preventDefault();
  });

  const showError = (message) => {
    errorBox.textContent = message;
    errorBox.hidden = false;
  };
  const clearError = () => {
    errorBox.hidden = true;
    errorBox.textContent = "";
  };
  const setStatus = (id, message) => {
    const element = byId(id);
    if (element) element.textContent = message;
  };

  /* Send JSON and surface the server's own message on failure. */
  async function send(method, url, body) {
    const response = await fetch(url, {
      method,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (response.status === 204) return null;
    const parsed = await response.json().catch(() => null);
    if (!response.ok) {
      throw new Error(
        parsed?.error?.message || `Request failed (${response.status}).`,
      );
    }
    return parsed;
  }

  /* Fill a <select> with profile names. `emptyLabel` names the "unset"
   * choice, which is a real option rather than a placeholder: it clears
   * the setting instead of storing the name of the fallback, which is what
   * lets a later change to the layer below reach whoever never chose. */
  function fillProfiles(select, emptyLabel, selected) {
    if (!select) return;
    const options = [new Option(emptyLabel, "")];
    for (const name of profiles) options.push(new Option(name, name));
    select.replaceChildren(...options);
    select.value = selected || "";
  }

  /* Offered scales come from the server, so a stored value always has an
   * entry to select.
   *
   * `emptyLabel` is what makes this a choice rather than a value (#176):
   * every dropdown in "My settings" offers the layer below it -- "Project
   * default" -- because that is how a person stops having an opinion, and
   * it is what lets a later project change reach them. 1x used to be
   * treated as the same thing as no preference, which made "I want 1x"
   * unsayable in a project whose default was 2x.
   *
   * The project's own select passes no `emptyLabel`: under it is Ridal's
   * built-in 1x, and an unset project default already shows as 1x. */
  function fillScales(select, selected, emptyLabel) {
    if (!select) return;
    const options = emptyLabel === undefined ? [] : [new Option(emptyLabel, "")];
    options.push(...xscales.map((s) => new Option(s.label, s.text)));
    select.replaceChildren(...options);
    select.value =
      selected === null || selected === undefined
        ? emptyLabel === undefined
          ? "1"
          : ""
        : String(selected);
  }

  /* Fill a <select> from the server's `{value, label}` list. `emptyLabel`,
   * when given, is the "no choice" option -- a real entry rather than a
   * placeholder, since choosing it clears the setting instead of storing
   * the name of the fallback. */
  function fillOptions(select, options, selected, emptyLabel) {
    if (!select) return;
    const entries = emptyLabel === undefined ? [] : [new Option(emptyLabel, "")];
    for (const option of options) {
      entries.push(new Option(option.label, option.value));
    }
    select.replaceChildren(...entries);
    select.value = selected || "";
  }

  /* Fill a <select> with the basemaps that can be chosen. Named by `name`
   * but valued by `id`, which is what a preference and the project default
   * both store -- a rename must not silently repoint either. */
  function fillBasemaps(select, emptyLabel, selected) {
    if (!select) return;
    const options = [new Option(emptyLabel, "")];
    for (const map of offeredBasemaps) options.push(new Option(map.name, map.id));
    select.replaceChildren(...options);
    // A stored id the project no longer offers has no option to select, so
    // the control would silently show the first one. Say so instead: the
    // server is already falling back, and this is the page to learn it on.
    if (selected && !offeredBasemaps.some((map) => map.id === selected)) {
      select.appendChild(new Option(`${selected} (no longer offered)`, selected));
    }
    select.value = selected || "";
  }

  function fillNames(select, names, selected) {
    if (!select) return;
    select.replaceChildren(...names.map((name) => new Option(name, name)));
    if (selected) select.value = selected;
  }

  /* Read everything the page shows and redraw it.
   *
   * `keep` names a section whose in-memory list and unsaved marker must
   * survive -- a save in one list-shaped section must not quietly replace
   * the other section's pending edits with the server's older copy, and
   * then clear the warning that said they were pending. The saved section
   * always takes the server's answer, because that is what was just
   * normalised and stored. */
  async function load(keep) {
    clearError();
    let settings;
    try {
      settings = await RIDAL.fetchJson("/api/v1/project/settings");
    } catch (error) {
      showError(`Could not load settings: ${error.message}`);
      return;
    }
    canEditProject = Boolean(settings.can_edit_project);
    canEditAccess = Boolean(settings.can_edit_access);
    profiles = settings.profiles || [];
    xscales = settings.xscales || [];
    // The *offered* list always refreshes: both dropdowns are built from
    // it, and a basemap just saved has to be selectable.
    offeredBasemaps = settings.basemaps || [];
    if (keep !== "basemaps-section") {
      basemaps = settings.project_basemaps || [];
      showBasemapProblems(settings.basemap_problems || []);
    }

    fillBasemaps(byId("my-basemap"), "Project default", settings.my_basemap);

    fillBasemaps(byId("default-basemap"), "First in the list", settings.default_basemap);
    const builtIn = byId("built-in-basemap");
    if (builtIn) builtIn.checked = settings.built_in_basemap !== false;
    renderBasemaps();

    // A section showing what the server just answered with has nothing
    // pending; one being kept keeps whatever state it had.
    if (keep !== "basemaps-section") setUnsaved("basemaps-section", false);
    if (keep !== "overlays-section") setUnsaved("overlays-section", false);

    if (keep !== "overlays-section") {
      overlays = settings.overlays || [];
      showOverlayProblems(settings.overlay_problems || []);
    }
    renderOverlays();
    spacings = settings.spacings || [];
    formats = settings.formats || [];
    themes = (settings.themes || []).map((name) => ({
      value: name,
      // Capitalised here rather than server-side: these are two words shown
      // in one dropdown, not a vocabulary anything else reads.
      label: name.charAt(0).toUpperCase() + name.slice(1),
    }));

    fillProfiles(byId("my-profile"), "Project default", settings.my_profile);
    fillScales(byId("my-xscale"), settings.my_xscale, "Project default");
    fillOptions(byId("my-theme"), themes, settings.my_theme, "Follow this device");
    // Absent means shown, which is what the viewer did before the toggle
    // existed -- so only an explicit `false` unticks it.
    const showPicks = byId("my-show-picks");
    if (showPicks) showPicks.checked = settings.my_show_picks !== false;
    fillOptions(byId("my-spacing"), spacings, settings.my_spacing, "Project default");
    fillOptions(byId("my-format"), formats, settings.my_format, "Project default");

    fillProfiles(
      byId("default-profile"),
      "Ridal default",
      settings.default_profile,
    );
    fillScales(byId("default-xscale"), settings.default_xscale);
    fillOptions(byId("default-spacing"), spacings, settings.default_spacing, "Ridal default");
    fillOptions(byId("default-format"), formats, settings.default_format, "Ridal default");
    for (const id of [
      "default-profile",
      "default-xscale",
      "default-spacing",
      "default-format",
    ]) {
      const select = byId(id);
      if (select) select.disabled = !canEditProject;
    }

    setStatus("settings-status", "");
    setStatus("my-settings-status", "");

    if (canEditAccess) await loadAccess();
  }

  if (myForm) {
    myForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      setStatus("my-settings-status", "Saving…");
      try {
        const saved = await send("PUT", "/api/v1/preferences", {
          render_profile: byId("my-profile").value || null,
          x_scale: Number(byId("my-xscale").value) || null,
          theme: byId("my-theme").value || null,
          show_picks: byId("my-show-picks").checked,
          level2_spacing: byId("my-spacing").value || null,
          level2_format: byId("my-format").value || null,
          basemap: byId("my-basemap").value || null,
        });
        byId("my-profile").value = saved.render_profile || "";
        // Read back as sent, including an explicit 1x -- and as "Project
        // default" when it was cleared.
        byId("my-xscale").value = saved.x_scale ? String(saved.x_scale) : "";
        byId("my-theme").value = saved.theme || "";
        byId("my-show-picks").checked = saved.show_picks !== false;
        byId("my-spacing").value = saved.level2_spacing || "";
        byId("my-format").value = saved.level2_format || "";
        // Applied to the page being looked at, not only stored: a theme
        // that took effect on the next page load would read as a setting
        // that did not work. The server writes the same attribute into
        // every page it renders from here on.
        if (saved.theme) {
          document.documentElement.dataset.theme = saved.theme;
        } else {
          delete document.documentElement.dataset.theme;
        }
        byId("my-basemap").value = saved.basemap || "";
        setStatus("my-settings-status", "Saved");
      } catch (error) {
        showError(error.message);
        setStatus("my-settings-status", "");
      }
    });
  }

  if (projectForm) {
    projectForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (!canEditProject) return;
      clearError();
      setStatus("settings-status", "Saving…");
      try {
        const saved = await send("PUT", "/api/v1/project/settings", {
          default_profile: byId("default-profile").value || null,
          default_xscale: Number(byId("default-xscale").value) || null,
          default_spacing: byId("default-spacing").value || null,
          default_format: byId("default-format").value || null,
        });
        byId("default-profile").value = saved.default_profile || "";
        byId("default-xscale").value = String(saved.default_xscale || 1);
        // Both are stored as absent when they are the neutral answer, so
        // they read back as the "Ridal default" option rather than as the
        // value that was sent.
        byId("default-spacing").value = saved.default_spacing || "";
        byId("default-format").value = saved.default_format || "";
        // Naming the file is the point: the change lands somewhere the user
        // can go and look at, which is not obvious from a dropdown.
        setStatus("settings-status", "Saved to ridal.toml");
      } catch (error) {
        showError(error.message);
        setStatus("settings-status", "");
      }
    });
  }

  /* Show, in the id box's placeholder, the id a name would produce.
   *
   * The id is how a stored preference and the project default refer to an
   * entry, so it has to exist -- but almost nobody wants to think about it,
   * and "OpenStreetMap" answers it already. The box is therefore optional
   * and greyed with what will be used, rather than required and empty.
   *
   * `RIDAL.slugify` is the twin of the rule Ridal already derives ids by,
   * so what is shown here is what gets stored. */
  function updateDerivedId(form) {
    const derived = RIDAL.slugify(form.elements.name.value);
    form.elements.id.placeholder = derived || "id";
  }

  /* The id to store for a newly added entry: what was typed, or what the
   * name derives to. */
  function chosenId(form) {
    return form.elements.id.value.trim() || RIDAL.slugify(form.elements.name.value);
  }

  /* ---- Basemaps (#177) ----------------------------------------------- *
   *
   * Edited as a list held here and sent whole, rather than saved field by
   * field the way the layers page does: a half-typed tile URL saved on blur
   * would be refused by the server on every keystroke that left a box, and
   * the default below can name a basemap that is only being added now.
   */

  function showBasemapProblems(problems) {
    const box = byId("basemap-problems");
    if (!box) return;
    if (problems.length === 0) {
      box.hidden = true;
      box.replaceChildren();
      return;
    }
    const intro = document.createElement("p");
    intro.textContent =
      problems.length === 1
        ? "One basemap in ridal.toml cannot be used, so it is not offered:"
        : `${problems.length} basemaps in ridal.toml cannot be used, so they are not offered:`;
    const list = document.createElement("ul");
    for (const problem of problems) {
      const item = document.createElement("li");
      item.textContent = problem;
      list.appendChild(item);
    }
    box.replaceChildren(intro, list);
    box.hidden = false;
  }

  /* One labelled input, bound to `field` on `map`. `parse` turns the typed
   * text into what the API takes; an empty box always means "unset", which
   * is what lets a basemap fall back to Ridal's defaults rather than
   * storing them. */
  function basemapField(map, field, label, options) {
    const { parse = (value) => value || undefined, type = "text", size } = options || {};
    const wrapper = document.createElement("label");
    wrapper.appendChild(document.createTextNode(label));
    const input = document.createElement("input");
    input.type = type;
    input.value = map[field] === undefined || map[field] === null ? "" : String(map[field]);
    if (size) input.size = size;
    if (type === "number") input.className = "narrow-number";
    input.disabled = !canEditProject;
    input.setAttribute("aria-label", `${label} for ${map.id}`);
    input.addEventListener("change", () => {
      map[field] = parse(input.value.trim());
      setUnsaved("basemaps-section", true);
    });
    wrapper.appendChild(input);
    return wrapper;
  }

  const wholeNumber = (value) => (value === "" ? undefined : Number(value));

  function basemapCard(map, index) {
    const card = document.createElement("fieldset");
    card.className = "basemap-card";

    const legend = document.createElement("legend");
    const id = document.createElement("code");
    // Immutable, for the same reason a layer id is: this string is what a
    // person's saved preference and the project default point at.
    id.textContent = map.id;
    legend.appendChild(id);
    card.appendChild(legend);

    const fields = document.createElement("div");
    fields.className = "add-layer-fields";
    fields.append(
      basemapField(map, "name", "Name"),
      basemapField(map, "url", "Address", { size: 42 }),
      basemapField(map, "attribution", "Attribution"),
      basemapField(map, "attribution_url", "Attribution link", { size: 28 }),
      basemapField(map, "tile_size", "Tile size", { type: "number", parse: wholeNumber }),
      basemapField(map, "max_zoom", "Max zoom", { type: "number", parse: wholeNumber }),
      basemapField(map, "zoom_offset", "Zoom offset", { type: "number", parse: wholeNumber }),
      basemapField(map, "subdomains", "Subdomains"),
    );
    card.appendChild(fields);

    if (canEditProject) {
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "danger";
      remove.textContent = "Remove";
      remove.addEventListener("click", () => {
        basemaps.splice(index, 1);
        renderBasemaps();
        setUnsaved("basemaps-section", true);
        setStatus("basemap-status", "Removed — not saved yet");
      });
      card.appendChild(remove);
    }

    return card;
  }

  function renderBasemaps() {
    const list = byId("basemaps-list");
    if (!list) return;
    if (basemaps.length === 0) {
      const note = document.createElement("p");
      note.className = "hint";
      note.textContent =
        "This project defines no basemaps of its own, so the maps draw on " +
        "Ridal's built-in ESRI World Imagery.";
      list.replaceChildren(note);
      return;
    }
    list.replaceChildren(...basemaps.map(basemapCard));
  }

  const addBasemapForm = byId("add-basemap");
  if (addBasemapForm) {
    addBasemapForm.elements.name.addEventListener("input", () =>
      updateDerivedId(addBasemapForm),
    );
    addBasemapForm.addEventListener("submit", (event) => {
      event.preventDefault();
      clearError();
      const name = addBasemapForm.elements.name.value.trim();
      const url = addBasemapForm.elements.url.value.trim();
      const id = chosenId(addBasemapForm);
      if (!name || !url) {
        showError("A basemap needs a name and an address.");
        return;
      }
      if (!id) {
        // Only reachable from a name with no letters or digits in it at
        // all, where there is nothing to derive an id from.
        showError(
          `No id could be derived from "${name}". Type one in the Id box.`,
        );
        return;
      }
      if (basemaps.some((map) => map.id === id)) {
        showError(`This project already has a basemap called '${id}'.`);
        return;
      }
      basemaps.push({ id, name, url });
      addBasemapForm.reset();
      updateDerivedId(addBasemapForm);
      renderBasemaps();
      // Added to the list, not to the project: the server has not seen it
      // yet, and saying so is the difference between a pending edit and a
      // save that silently did not happen.
      setUnsaved("basemaps-section", true);
      setStatus("basemap-status", "Added — press Save basemaps to keep it");
    });
  }

  const basemapForm = byId("basemap-form");
  if (basemapForm) {
    basemapForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (!canEditProject) return;
      clearError();
      setStatus("basemap-status", "Saving…");
      try {
        const saved = await send("PUT", "/api/v1/project/settings", {
          // Only the basemap half is sent; the render defaults above have
          // their own form, and the API leaves alone what a request does
          // not mention.
          basemaps,
          default_basemap: byId("default-basemap").value || null,
          built_in_basemap: byId("built-in-basemap").checked,
        });
        // Reloaded rather than patched: saving normalises what was typed,
        // and the offered list -- which both dropdowns are built from --
        // has just changed. The overlays are left as they are on screen,
        // pending edits included.
        await load("overlays-section");
        setStatus(
          "basemap-status",
          saved.project_basemaps.length === 0
            ? "Saved to ridal.toml"
            : `Saved ${saved.project_basemaps.length} to ridal.toml`,
        );
      } catch (error) {
        showError(error.message);
        // Named rather than blanked, unlike the forms above: this one sits
        // at the bottom of a long page, its error box is at the top, and a
        // status that simply goes quiet reads as "nothing happened". The
        // refusal is often about a *different* basemap than the one just
        // edited -- one already in `ridal.toml` that cannot be drawn -- so
        // the pointer to where the explanation is matters.
        setStatus("basemap-status", "Not saved — see the message at the top of the page");
      }
    });
  }

  /* ---- Overlays (#177) ------------------------------------------------ *
   *
   * The same shape as the basemaps above -- a list held here, edited in
   * place and sent whole -- because it is the same kind of editing, and two
   * different interactions for two lists on one page would be a distinction
   * without a reason.
   */

  function showOverlayProblems(problems) {
    const box = byId("overlay-problems");
    if (!box) return;
    if (problems.length === 0) {
      box.hidden = true;
      box.replaceChildren();
      return;
    }
    const intro = document.createElement("p");
    intro.textContent =
      problems.length === 1
        ? "One overlay in ridal.toml cannot be used, so it is not drawn:"
        : `${problems.length} overlays in ridal.toml cannot be used, so they are not drawn:`;
    const list = document.createElement("ul");
    for (const problem of problems) {
      const item = document.createElement("li");
      item.textContent = problem;
      list.appendChild(item);
    }
    box.replaceChildren(intro, list);
    box.hidden = false;
  }

  function overlayField(overlay, field, label, options) {
    const { size, placeholder, type = "text" } = options || {};
    const wrapper = document.createElement("label");
    wrapper.appendChild(document.createTextNode(label));
    const input = document.createElement("input");
    input.type = type;
    input.value = overlay[field] === undefined || overlay[field] === null
      ? ""
      : String(overlay[field]);
    if (size) input.size = size;
    if (placeholder) input.placeholder = placeholder;
    input.disabled = !canEditProject;
    input.setAttribute("aria-label", `${label} for ${overlay.id}`);
    input.addEventListener("change", () => {
      const value = input.value.trim();
      // An emptied box means "no field" rather than "a property called
      // nothing", which the server refuses -- so it is cleared here.
      overlay[field] = value === "" ? undefined : value;
      setUnsaved("overlays-section", true);
    });
    wrapper.appendChild(input);
    return wrapper;
  }

  function overlayCard(overlay, index) {
    const card = document.createElement("fieldset");
    card.className = "basemap-card";

    const legend = document.createElement("legend");
    const id = document.createElement("code");
    id.textContent = overlay.id;
    legend.appendChild(id);
    card.appendChild(legend);

    const fields = document.createElement("div");
    fields.className = "add-layer-fields";
    fields.append(
      overlayField(overlay, "name", "Layer name"),
      overlayField(overlay, "url", "Address", { size: 42 }),
      // The two popup dials. Named after what they hold rather than what
      // they are: someone filling these in is reading their own GeoJSON's
      // property names, not Ridal's vocabulary.
      overlayField(overlay, "name_field", "Name property", {
        placeholder: "name",
      }),
      overlayField(overlay, "description_field", "Description property", {
        placeholder: "description",
      }),
      overlayField(overlay, "color", "Colour", { size: 9, placeholder: "#3aa3e3" }),
    );
    card.appendChild(fields);

    const hint = document.createElement("span");
    hint.className = "hint";
    hint.textContent =
      "The name property titles each popup; the description property is " +
      "shown below it as HTML.";
    card.appendChild(hint);

    if (canEditProject) {
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "danger";
      remove.textContent = "Remove";
      remove.addEventListener("click", () => {
        overlays.splice(index, 1);
        renderOverlays();
        setUnsaved("overlays-section", true);
        setStatus("overlay-status", "Removed — not saved yet");
      });
      card.appendChild(remove);
    }

    return card;
  }

  function renderOverlays() {
    const list = byId("overlays-list");
    if (!list) return;
    if (overlays.length === 0) {
      const note = document.createElement("p");
      note.className = "hint";
      note.textContent =
        "This project has no overlays, so the maps show the radargram " +
        "tracks alone.";
      list.replaceChildren(note);
      return;
    }
    list.replaceChildren(...overlays.map(overlayCard));
  }

  const addOverlayForm = byId("add-overlay");
  if (addOverlayForm) {
    addOverlayForm.elements.name.addEventListener("input", () =>
      updateDerivedId(addOverlayForm),
    );
    addOverlayForm.addEventListener("submit", (event) => {
      event.preventDefault();
      clearError();
      const name = addOverlayForm.elements.name.value.trim();
      const url = addOverlayForm.elements.url.value.trim();
      const id = chosenId(addOverlayForm);
      if (!name || !url) {
        showError("An overlay needs a layer name and an address.");
        return;
      }
      if (!id) {
        showError(
          `No id could be derived from "${name}". Type one in the Id box.`,
        );
        return;
      }
      if (overlays.some((overlay) => overlay.id === id)) {
        showError(`This project already has an overlay called '${id}'.`);
        return;
      }
      overlays.push({ id, name, url });
      addOverlayForm.reset();
      updateDerivedId(addOverlayForm);
      renderOverlays();
      setUnsaved("overlays-section", true);
      setStatus("overlay-status", "Added — press Save overlays to keep it");
    });
  }

  const overlayForm = byId("overlay-form");
  if (overlayForm) {
    overlayForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (!canEditProject) return;
      clearError();
      setStatus("overlay-status", "Saving…");
      try {
        const saved = await send("PUT", "/api/v1/project/settings", { overlays });
        await load("basemaps-section");
        setStatus(
          "overlay-status",
          `Saved ${saved.overlays.length} to ridal.toml`,
        );
      } catch (error) {
        showError(error.message);
        setStatus(
          "overlay-status",
          "Not saved — see the message at the top of the page",
        );
      }
    });
  }

  /* ---- Access ------------------------------------------------------- */

  let access = null;

  async function loadAccess() {
    try {
      access = await RIDAL.fetchJson("/api/v1/users");
    } catch (error) {
      showError(`Could not load accounts: ${error.message}`);
      return;
    }

    const addForm = byId("add-user");
    if (addForm) {
      fillNames(addForm.elements.role, access.roles || [], "picker");
      // `all` to match what the API does when the field is omitted, and
      // what the README says a new account gets. The form always submits
      // its selection, so a different preselection here would silently
      // be the real default and the documented one would be fiction.
      fillNames(addForm.elements.download, access.download_scopes || [], "all");
    }
    const bulkForm = byId("bulk-user-form");
    if (bulkForm) {
      fillNames(bulkForm.elements.role, access.roles || [], "picker");
      fillNames(bulkForm.elements.download, access.download_scopes || [], "all");
      const max = access.bulk?.max_accounts || 100;
      bulkForm.elements.count.max = String(max);
    }
    fillNames(
      byId("anonymous-download"),
      access.download_scopes || [],
      access.anonymous_download,
    );
    byId("require-auth").checked = Boolean(access.require_auth_to_read);

    renderUsers();
  }

  function renderUsers() {
    const body = byId("users-table").querySelector("tbody");
    body.replaceChildren();
    for (const user of access.users || []) {
      body.appendChild(userRow(user));
    }
  }

  function userRow(user) {
    const row = document.createElement("tr");

    const name = document.createElement("td");
    name.textContent = user.name;
    row.appendChild(name);

    /* Both selects apply on change rather than waiting for a Save, so
     * each carries its own confirmation: there is no button to go quiet
     * afterwards, and the only Save button on this page belongs to a
     * different setting entirely. */
    const cellWithSelect = (options, selected, change) => {
      const cell = document.createElement("td");
      const select = document.createElement("select");
      fillNames(select, options, selected);
      const status = document.createElement("span");
      status.className = "row-status";
      select.addEventListener("change", () =>
        updateUser(user.name, change(select.value), select, selected, status),
      );
      cell.append(select, status);
      return cell;
    };

    row.appendChild(
      cellWithSelect(access.roles || [], user.role, (value) => ({ role: value })),
    );
    row.appendChild(
      cellWithSelect(access.download_scopes || [], user.download, (value) => ({
        download: value,
      })),
    );

    const status = document.createElement("td");
    // Three states worth telling apart: never activated, active, and active
    // with a reset outstanding.
    if (!user.activated) {
      status.textContent = user.invite_pending
        ? "invited, not yet activated"
        : "no password and no invite";
    } else {
      status.textContent = user.invite_pending ? "reset pending" : "active";
    }
    row.appendChild(status);

    /* One shape for both, differing only in colour: they are the same
     * kind of control and one of them is more serious, which is what the
     * colour is for. */
    const actions = document.createElement("td");
    const group = document.createElement("div");
    group.className = "row-actions";

    const reset = document.createElement("button");
    reset.type = "button";
    reset.textContent = user.activated ? "Reset password" : "New invite link";
    reset.addEventListener("click", () => reissue(user.name));
    group.appendChild(reset);

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.textContent = "Remove";
    remove.addEventListener("click", () => removeUser(user.name));
    group.appendChild(remove);

    actions.appendChild(group);
    row.appendChild(actions);

    return row;
  }

  async function updateUser(name, change, select, previous, status) {
    clearError();
    if (status) status.textContent = "Saving…";
    try {
      await send("PUT", `/api/v1/users/${encodeURIComponent(name)}`, change);
      // Redrawn from the server's answer, which also replaces this row --
      // so the "Saved" below is set on a row that is about to go. It is
      // still worth setting: a refusal leaves the old row in place, and
      // the difference between the two outcomes is the point.
      if (status) status.textContent = "Saved";
      await loadAccess();
    } catch (error) {
      showError(error.message);
      if (status) status.textContent = "";
      // Put the control back to what the server still believes, so the page
      // never shows a role that was refused.
      select.value = previous;
    }
  }

  function showInvite(name, result) {
    const box = byId("invite-result");
    byId("invite-who").textContent = name;
    byId("invite-days").textContent = String(result.invite_ttl_days);
    // Built from this page's own origin rather than from anything the
    // server guessed: Ridal is normally behind a reverse proxy and has no
    // reliable idea what address the browser reached it on.
    byId("invite-link").textContent =
      window.location.origin + result.invite_path;
    box.hidden = false;
  }

  async function reissue(name) {
    clearError();
    try {
      const result = await send(
        "POST",
        `/api/v1/users/${encodeURIComponent(name)}/invite`,
        {},
      );
      showInvite(name, result);
      await loadAccess();
    } catch (error) {
      showError(error.message);
    }
  }

  async function removeUser(name) {
    // Worth a confirmation, and worth saying what it does not do: the picks
    // are attributed data and stay.
    const confirmed = window.confirm(
      `Remove the account "${name}"?\n\nTheir interpretations are kept — an ` +
        `account going away does not unmake the picks. Only the account and ` +
        `their personal settings are removed.`,
    );
    if (!confirmed) return;
    clearError();
    try {
      await send("DELETE", `/api/v1/users/${encodeURIComponent(name)}`, {});
      await loadAccess();
    } catch (error) {
      showError(error.message);
    }
  }

  const addForm = byId("add-user");
  if (addForm) {
    addForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      const name = addForm.elements.name.value.trim();
      try {
        const result = await send("POST", "/api/v1/users", {
          name,
          role: addForm.elements.role.value,
          download: addForm.elements.download.value,
        });
        addForm.reset();
        showInvite(name, result);
        await loadAccess();
      } catch (error) {
        showError(error.message);
      }
    });
  }

  function downloadCsv(filename, rows) {
    const csv = rows
      .map((row) => row.map((value) => `"${String(value).replaceAll('"', '""')}"`).join(","))
      .join("\n");
    const link = document.createElement("a");
    link.href = URL.createObjectURL(new Blob([csv], { type: "text/csv" }));
    link.download = filename;
    link.click();
    URL.revokeObjectURL(link.href);
  }

  function renderBulkResult(result, mode) {
    const box = byId("bulk-result");
    box.replaceChildren();
    const heading = document.createElement("h3");
    heading.textContent = mode === "passwords" ? "Generated passwords" : "Invite links";
    box.appendChild(heading);
    const note = document.createElement("p");
    note.className = "hint";
    note.textContent = mode === "passwords"
      ? `${result.advisory} These passwords are shown once and are not stored.`
      : "Each link works once. Send each person only their own link.";
    box.appendChild(note);

    const table = document.createElement("table");
    table.className = "layers-table bulk-table";
    const header = document.createElement("tr");
    for (const label of mode === "passwords" ? ["Name", "Password"] : ["Name", "Invite link"]) {
      const cell = document.createElement("th");
      cell.textContent = label;
      header.appendChild(cell);
    }
    table.appendChild(header);
    const rows = [["name", mode === "passwords" ? "password" : "invite link"]];
    for (const user of result.users || []) {
      const row = document.createElement("tr");
      const name = document.createElement("td");
      name.textContent = user.name;
      const value = document.createElement("td");
      const text = mode === "passwords"
        ? user.password
        : window.location.origin + user.invite_path;
      value.textContent = text;
      row.append(name, value);
      table.appendChild(row);
      rows.push([user.name, text]);
    }
    box.appendChild(table);
    const actions = document.createElement("div");
    actions.className = "bulk-actions";
    const print = document.createElement("button");
    print.type = "button";
    print.textContent = "Print";
    print.addEventListener("click", () => window.print());
    const csv = document.createElement("button");
    csv.type = "button";
    csv.textContent = "Download CSV";
    csv.addEventListener("click", () => downloadCsv(`ridal-${mode}.csv`, rows));
    actions.append(print, csv);
    box.appendChild(actions);
    box.hidden = false;
  }

  const bulkForm = byId("bulk-user-form");
  if (bulkForm) {
    /* Bulk warnings belong beside "Add several people", not at the top of
     * the page: an error about a checkbox here read as an unrelated page
     * fault when it was shown up there. */
    const bulkWarning = byId("bulk-warning");
    const showBulkWarning = (message) => {
      bulkWarning.textContent = message;
      bulkWarning.hidden = false;
    };
    const clearBulkWarning = () => {
      bulkWarning.hidden = true;
      bulkWarning.textContent = "";
    };

    const bulkBody = () => ({
      prefix: bulkForm.elements.prefix.value.trim(),
      count: Number(bulkForm.elements.count.value),
      role: bulkForm.elements.role.value,
      download: bulkForm.elements.download.value,
    });

    bulkForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearBulkWarning();
      try {
        const result = await send("POST", "/api/v1/users/bulk/invites", bulkBody());
        renderBulkResult(result, "invites");
        await loadAccess();
      } catch (error) {
        showBulkWarning(error.message);
      }
    });

    /* Acknowledging the risk is the fix the warning asks for, so ticking
     * the box takes the warning away rather than leaving it to be
     * dismissed some other way. */
    byId("bulk-risk").addEventListener("change", clearBulkWarning);

    byId("bulk-passwords").addEventListener("click", async () => {
      if (bulkForm.elements.role.value === "admin") {
        showBulkWarning("Administrator accounts must use one-time invite links.");
        return;
      }
      if (!byId("bulk-risk").checked) {
        showBulkWarning(
          "Generated passwords are shared secrets and less safe than invite links. " +
            "Check the acknowledgement above, then press the button again.",
        );
        return;
      }
      clearBulkWarning();
      try {
        const result = await send("POST", "/api/v1/users/bulk/passwords", {
          ...bulkBody(),
          acknowledge_risk: true,
        });
        renderBulkResult(result, "passwords");
        await loadAccess();
      } catch (error) {
        showBulkWarning(error.message);
      }
    });
  }

  const accessForm = byId("access-form");
  if (accessForm) {
    accessForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      setStatus("access-status", "Saving…");
      try {
        await send("PUT", "/api/v1/access", {
          require_auth_to_read: byId("require-auth").checked,
          anonymous_download: byId("anonymous-download").value,
        });
        setStatus("access-status", "Saved");
      } catch (error) {
        showError(error.message);
        setStatus("access-status", "");
      }
    });
  }

  load();
})();
