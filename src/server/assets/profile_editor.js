/* The render profile editor (#183): tune a profile on this radargram, then
 * save it as one of your own or, for an operator, as the project's.
 *
 * Loaded after viewer.js and figure.js on the viewer page, so the map
 * (`window.RIDAL_MAP`), the shared geometry (`window.RIDAL_GEOMETRY`) and
 * the page config (`window.RIDAL_VIEWER`) all exist. Everything is inside
 * one function, as classic scripts on one page share their top-level names.
 *
 * The preview is not the viewer's tiles. Every change would otherwise be a
 * new set of tiles at every zoom level, rendered and cached for a profile
 * about to change again; instead `POST .../render-profile-preview` draws one
 * image of the extent that was on screen, with the profile as the form has
 * it, and nothing is kept. It renders when asked (Update preview), or after
 * each change once "Auto update" is ticked, which it is not by default:
 * each preview reads the whole extent, and a room full of people dragging a
 * slider should not cost the server that many reads. Only Save makes the
 * viewer render tiles with the profile, once, at its final settings.
 *
 * Resampling and the tile format are not in the form. Resampling only acts
 * where a tile pixel covers several samples, and a preview, like a figure,
 * averages colours instead, so it could never show what the control did;
 * it follows the transform as the built-ins pair them (`resamplingFor`).
 * The format is the server's concern rather than the picture's, and is
 * kept as the starting profile has it. Both can still be set through a
 * profile file or the API.
 *
 * Saving writes the whole profile under the name and place below the form:
 * "My profiles" (`personal-<name>`, the caller's own) or "Project" (an
 * operator's). That one control covers saving over the profile being
 * edited, saving a copy under a new name, duplicating a built-in as one's
 * own, and sharing one's own with the project.
 */
(function setupProfileEditor() {
  const dialog = document.getElementById('profile-dialog');
  const opener = document.getElementById('profile-edit');
  if (!dialog || !opener) return;

  const config = window.RIDAL_VIEWER;
  const radargramId = config.radargramId;
  const $ = (id) => document.getElementById(id);
  const viewerSelect = $('profile-select');

  const PERSONAL = 'personal-';
  const PREVIEW_DEBOUNCE_MS = 400;
  const MAX_PREVIEW_WIDTH = 2000;
  const editPersonal = dialog.dataset.editPersonal === 'true';
  const editProject = dialog.dataset.editProject === 'true';
  const SIGLOG_DEFAULT = Number(dialog.dataset.siglogDefault);
  const OFFSET_DEFAULT = Number(dialog.dataset.offsetDefault);
  const MAX_STOPS = Number(dialog.dataset.maxStops) || 32;

  /* Colour scales to start from, beside those of the profiles there are.
   * Positions are evenly spaced; each is a few stops of the matplotlib map
   * of the same name, which is close enough to read the same. */
  const PRESETS = [
    { name: 'bwr', stops: ['#0000ff', '#ffffff', '#ff0000'] },
    { name: 'grey-reversed', stops: ['#ffffff', '#000000'] },
    { name: 'viridis', stops: ['#440154', '#3b528b', '#21918c', '#5ec962', '#fde725'] },
    { name: 'magma', stops: ['#000004', '#3b0f70', '#8c2981', '#de4968', '#fe9f6d', '#fcfdbf'] },
    { name: 'cividis', stops: ['#00224e', '#35456c', '#666970', '#948e77', '#c8b866', '#fee838'] },
  ].map((preset) => ({
    name: preset.name,
    stops: preset.stops.map((hex, i, all) => ({
      position: all.length > 1 ? i / (all.length - 1) : 0,
      color: rgbOf(hex),
    })),
  }));

  const state = {
    profiles: [],
    defaultName: 'default',
    // The profile the form was loaded from, as the list gave it.
    base: null,
    // Its ETag, when the caller may save over it.
    version: null,
    stops: [],
    colormapName: null,
    // [first, last] trace and [top, bottom] row, in raster coordinates.
    traces: null,
    rows: null,
    previewUrl: null,
    previousUrl: null,
    comparing: false,
    controller: null,
    timer: null,
  };

  // --- Colours ---------------------------------------------------------------

  function rgbOf(hex) {
    const value = parseInt(hex.slice(1), 16);
    return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
  }

  function hexOf([r, g, b]) {
    return `#${[r, g, b].map((c) => c.toString(16).padStart(2, '0')).join('')}`;
  }

  /* Every colour scale there is to choose from: the presets, then those of
   * the profiles, once per name. */
  function colourScales() {
    const scales = new Map(PRESETS.map((p) => [p.name, p.stops]));
    for (const profile of state.profiles) {
      if (profile.colormap && !scales.has(profile.colormap.name)) {
        scales.set(profile.colormap.name, profile.colormap.stops);
      }
    }
    return scales;
  }

  function fillPresets() {
    const select = $('pe-preset');
    const options = [new Option('Grey (no colour scale)', '')];
    for (const name of colourScales().keys()) options.push(new Option(name, name));
    if (state.colormapName && !colourScales().has(state.colormapName)) {
      options.push(new Option(state.colormapName, state.colormapName));
    }
    select.replaceChildren(...options);
    select.value = state.colormapName || '';
  }

  function renderStops() {
    const box = $('pe-stops');
    box.hidden = state.colormapName === null;
    $('pe-add-stop').hidden = box.hidden;
    $('pe-reverse').hidden = box.hidden;
    $('pe-add-stop').disabled = state.stops.length >= MAX_STOPS;
    box.replaceChildren(
      ...state.stops.map((stop, index) => {
        const row = document.createElement('div');
        row.className = 'profile-stop';
        const position = document.createElement('input');
        position.type = 'number';
        position.min = '0';
        position.max = '1';
        position.step = '0.01';
        position.value = String(Math.round(stop.position * 1000) / 1000);
        position.setAttribute('aria-label', `Stop ${index + 1} position, 0 to 1`);
        position.addEventListener('change', () => {
          const value = Number(position.value);
          stop.position = Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : stop.position;
          stopsEdited();
        });
        const colour = document.createElement('input');
        colour.type = 'color';
        colour.value = hexOf(stop.color);
        colour.setAttribute('aria-label', `Stop ${index + 1} colour`);
        colour.addEventListener('change', () => {
          stop.color = rgbOf(colour.value);
          stopsEdited();
        });
        const remove = document.createElement('button');
        remove.type = 'button';
        remove.textContent = 'Remove';
        remove.disabled = state.stops.length <= 1;
        remove.addEventListener('click', () => {
          state.stops.splice(index, 1);
          stopsEdited();
        });
        row.append(position, colour, remove);
        return row;
      }),
    );
  }

  /* A colour scale that no longer matches its preset is the profile's own,
   * and named so: a scale keeps the name it was saved under. */
  function stopsEdited() {
    state.stops.sort((a, b) => a.position - b.position);
    if (colourScales().has(state.colormapName)) state.colormapName = 'custom';
    fillPresets();
    renderStops();
    changed();
  }

  // --- The form --------------------------------------------------------------

  const numberOr = (id, fallback) => {
    const value = Number($(id).value);
    return $(id).value !== '' && Number.isFinite(value) ? value : fallback;
  };

  function showValues() {
    $('pe-contrast-value').textContent = Number($('pe-contrast').value).toFixed(2);
    $('pe-black-value').textContent = Number($('pe-black').value).toFixed(2);
  }

  /* Show the controls that mean something with the others as they are. */
  function syncVisibility() {
    const source = $('pe-source').value;
    $('pe-siglog-row').hidden = source !== 'SigLog';
    $('pe-offset-row').hidden = source !== 'AdaptiveSigLog';
    const percentile = $('pe-limits').value === 'Percentile';
    dialog.querySelectorAll('.pe-percentile').forEach((el) => {
      el.hidden = !percentile;
    });
    dialog.querySelectorAll('.pe-explicit').forEach((el) => {
      el.hidden = percentile;
    });
    $('pe-nodata').disabled = !$('pe-nodata-on').checked;
    $('pe-thumbnail-row').hidden = $('pe-scope').value !== 'personal';
  }

  function fillForm(profile) {
    $('pe-source').value = profile.source_transform;
    $('pe-siglog').value = profile.siglog_minval_log10;
    $('pe-offset').value = profile.siglog_noise_offset;
    $('pe-transform').value = profile.transform;
    if (profile.limits.Percentile) {
      $('pe-limits').value = 'Percentile';
      $('pe-low').value = round(profile.limits.Percentile.low * 100);
      $('pe-high').value = round(profile.limits.Percentile.high * 100);
      $('pe-min').value = '';
      $('pe-max').value = '';
    } else {
      $('pe-limits').value = 'Explicit';
      $('pe-min').value = profile.limits.Explicit.min;
      $('pe-max').value = profile.limits.Explicit.max;
      $('pe-low').value = 1;
      $('pe-high').value = 99;
    }
    $('pe-symmetric').checked = Boolean(profile.symmetric_limits);
    $('pe-skip').value = profile.stats_skip_first_samples;
    $('pe-contrast').value = profile.contrast;
    $('pe-black').value = profile.black_level;
    $('pe-nodata-on').checked = Array.isArray(profile.nodata_color);
    $('pe-nodata').value = profile.nodata_color ? hexOf(profile.nodata_color) : '#808080';
    state.colormapName = profile.colormap ? profile.colormap.name : null;
    state.stops = profile.colormap
      ? profile.colormap.stops.map((s) => ({ position: s.position, color: [...s.color] }))
      : [];
    fillPresets();
    renderStops();
    showValues();
    syncVisibility();
  }

  function round(value) {
    return Math.round(value * 1000) / 1000;
  }

  /* The profile as the form has it, named `name`. Each siglog field is sent
   * at its default unless its own transform is chosen: the server refuses a
   * strength that would do nothing, rather than keep it silently. */
  function readForm(name) {
    const source = $('pe-source').value;
    const limits = $('pe-limits').value === 'Percentile'
      ? { Percentile: { low: numberOr('pe-low', 1) / 100, high: numberOr('pe-high', 99) / 100 } }
      : { Explicit: { min: numberOr('pe-min', NaN), max: numberOr('pe-max', NaN) } };
    return {
      name,
      view: state.base ? state.base.view : 'Standard',
      source_transform: source,
      siglog_minval_log10: source === 'SigLog' ? numberOr('pe-siglog', SIGLOG_DEFAULT) : SIGLOG_DEFAULT,
      siglog_noise_offset: source === 'AdaptiveSigLog'
        ? numberOr('pe-offset', OFFSET_DEFAULT)
        : OFFSET_DEFAULT,
      transform: $('pe-transform').value,
      limits,
      resampling: resamplingFor($('pe-transform').value),
      format: state.base ? state.base.format : { Jpeg: { quality: 85 } },
      contrast: numberOr('pe-contrast', 1),
      black_level: numberOr('pe-black', 0),
      colormap: state.colormapName === null
        ? null
        : { name: state.colormapName, stops: state.stops },
      symmetric_limits: $('pe-symmetric').checked,
      stats_skip_first_samples: Math.max(0, Math.round(numberOr('pe-skip', 0))),
      nodata_color: $('pe-nodata-on').checked ? rgbOf($('pe-nodata').value) : null,
    };
  }

  /* How tiles combine the samples under one pixel, for `transform`: as the
   * built-ins pair them. A transform that shows magnitude filters
   * magnitudes, or signed traces average out to nothing when zoomed out; a
   * signed one averages, so a colour scale keeps the sign. The starting
   * profile's own choice is kept while its transform is. */
  function resamplingFor(transform) {
    if (state.base && state.base.transform === transform) return state.base.resampling;
    return transform === 'Linear' ? 'Mean' : 'LanczosRectified';
  }

  /* Combinations that render, but rarely as meant. Said, not refused: each
   * has a use the profile docs do not rule out. */
  function warnings(profile) {
    const out = [];
    if (profile.colormap && profile.transform !== 'Linear') {
      out.push(`The ${profile.transform === 'AbsLog' ? 'log of magnitude' : 'positive'} transform folds the sign away, so a colour scale for signed amplitude shows only part of its range.`);
    }
    if (profile.black_level !== 0 && (profile.symmetric_limits || profile.colormap)) {
      out.push('A black level moves the middle of the scale off zero amplitude, which a diverging colour scale is centred on.');
    }
    return out;
  }

  function showWarnings() {
    const box = $('pe-warnings');
    box.replaceChildren(
      ...warnings(readForm('')).map((text) => {
        const line = document.createElement('p');
        line.className = 'hint';
        line.textContent = text;
        return line;
      }),
    );
  }

  // --- Profiles --------------------------------------------------------------

  async function loadProfiles() {
    const list = await RIDAL.fetchJson(RIDAL.apiPath('render-profiles'));
    state.profiles = list.profiles || [];
    state.defaultName = list.default || 'default';
  }

  const SCOPE_LABEL = { built_in: 'Built-in', project: 'Project', personal: 'Mine' };

  function labelOf(profile) {
    return profile.scope === 'personal' ? profile.name.slice(PERSONAL.length) : profile.name;
  }

  /* `select`'s options, grouped by scope as the viewer's are. Hidden
   * built-ins are left out, except `keep`. */
  function fillGrouped(select, keep, filter) {
    const groups = new Map();
    for (const profile of state.profiles) {
      if (profile.hidden && profile.name !== keep) continue;
      if (filter && !filter(profile)) continue;
      if (!groups.has(profile.scope)) {
        const group = document.createElement('optgroup');
        group.label = SCOPE_LABEL[profile.scope] || profile.scope;
        groups.set(profile.scope, group);
      }
      const option = new Option(labelOf(profile), profile.name);
      if (profile.colormap) option.dataset.colormap = '';
      groups.get(profile.scope).append(option);
    }
    select.replaceChildren(...groups.values());
  }

  /* Whether the caller may save over `profile` where it is. */
  function mayOverwrite(profile) {
    if (profile.scope === 'personal') return editPersonal;
    if (profile.scope === 'project') return editProject;
    return false;
  }

  function scopeOf(profile) {
    return profile.scope === 'personal' ? 'personal' : 'project';
  }

  async function loadBase(name) {
    const profile = state.profiles.find((p) => p.name === name);
    if (!profile) return;
    state.base = profile;
    state.version = null;
    fillForm(profile);
    $('pe-start').value = name;

    // Where Save goes to start with: back over the profile where it may,
    // and otherwise a copy among the caller's own (or, for an operator who
    // may keep none, the project's).
    const scope = mayOverwrite(profile) ? scopeOf(profile) : (editPersonal ? 'personal' : 'project');
    $('pe-scope').value = scope;
    let saveName = labelOf(profile);
    if (!mayOverwrite(profile) && scope === 'project') saveName = `${saveName}-custom`;
    $('pe-name').value = saveName;
    $('pe-thumbnail').value = profile.thumbnail_profile || '';
    const kind = { built_in: 'a built-in', project: 'a project profile', personal: 'one of yours' }[profile.scope];
    $('pe-origin').textContent = mayOverwrite(profile)
      ? `Editing ${labelOf(profile)}, ${kind}.`
      : `Starting from ${labelOf(profile)}, ${kind}. Saving keeps a copy; the original is unchanged.`;
    syncSaveControls();
    showWarnings();

    if (mayOverwrite(profile)) {
      // The ETag to save back with, so an edit made elsewhere meanwhile is
      // not silently replaced.
      const url = profile.scope === 'personal'
        ? RIDAL.apiPath('preferences', 'render-profiles', name)
        : RIDAL.apiPath('render-profiles', name);
      try {
        const response = await fetch(url);
        if (response.ok && state.base === profile) state.version = response.headers.get('etag');
      } catch {
        // Saved without a condition then; the save still asks before
        // replacing a profile it did not start from.
      }
    }
  }

  function targetName() {
    const name = $('pe-name').value.trim();
    return $('pe-scope').value === 'personal' ? `${PERSONAL}${name}` : name;
  }

  /* Whether Save writes over the profile the form was loaded from. */
  function savesOverBase() {
    return Boolean(state.base) && mayOverwrite(state.base)
      && scopeOf(state.base) === $('pe-scope').value && state.base.name === targetName();
  }

  function syncSaveControls() {
    syncVisibility();
    const remove = $('pe-delete');
    remove.hidden = !savesOverBase();
    remove.disabled = state.base && state.base.name === state.defaultName;
    remove.title = remove.disabled ? 'The project default cannot be deleted.' : '';
    $('pe-save').textContent = savesOverBase() ? 'Save' : 'Save as new';
  }

  function fillThumbnails() {
    const select = $('pe-thumbnail');
    const value = select.value;
    fillGrouped(select, value, (p) => p.scope !== 'personal');
    select.prepend(new Option('Project default', ''));
    select.value = value;
  }

  // --- Extent ----------------------------------------------------------------

  function rasterHeight() {
    return window.RIDAL_GEOMETRY.rasterHeight;
  }

  /* What the viewer shows now, in raster trace and row, clipped to the
   * radargram: zoomed out, the map's bounds reach well past it. */
  function extentFromView() {
    const g = window.RIDAL_GEOMETRY;
    const scale = window.RIDAL_XSCALE || 1;
    const bounds = window.RIDAL_MAP.getBounds();
    const toRaster = (latlng) => [latlng.lng / scale / g.rasterScale, -latlng.lat / g.verticalRasterScale];
    const [t0, r0] = toRaster(bounds.getNorthWest());
    const [t1, r1] = toRaster(bounds.getSouthEast());
    const clip = (a, b, n) => {
      let lo = Math.max(0, Math.min(Math.min(a, b), n));
      let hi = Math.max(0, Math.min(Math.max(a, b), n));
      if (hi - lo < 1) [lo, hi] = [Math.max(0, lo - 0.5), Math.min(n, lo + 0.5)];
      return [lo, hi];
    };
    state.traces = clip(t0, t1, config.sourceWidth);
    state.rows = clip(r0, r1, rasterHeight());
    $('pe-extent-note').textContent =
      `Traces ${Math.round(state.traces[0])}–${Math.round(state.traces[1])}, ` +
      `rows ${Math.round(state.rows[0])}–${Math.round(state.rows[1])}, as the viewer showed them.`;
  }

  // --- Preview ---------------------------------------------------------------

  async function errorText(response) {
    try {
      const body = await response.json();
      if (body && body.error && body.error.message) return body.error.message;
    } catch {
      // Not an envelope: something in front of the server answered.
    }
    return RIDAL.upstreamMessage(response.status);
  }

  function previewWidth() {
    const pane = $('pe-preview');
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    const width = Math.round(Math.max(200, (pane.clientWidth || 700) - 2) * ratio);
    return Math.min(MAX_PREVIEW_WIDTH, width);
  }

  function showImage(url) {
    const image = $('pe-image');
    image.src = url;
    image.hidden = false;
  }

  async function preview() {
    clearTimeout(state.timer);
    if (!state.traces) return;
    if (state.controller) state.controller.abort();
    const controller = new AbortController();
    state.controller = controller;
    const pane = $('pe-preview');
    const status = $('pe-status');
    const done = () => {
      if (controller === state.controller) pane.classList.remove('is-busy');
    };
    pane.classList.add('is-busy');
    status.hidden = false;
    status.textContent = 'Rendering…';
    $('pe-update').classList.remove('is-stale');
    let response;
    try {
      response = await fetch(RIDAL.apiPath('datasets', radargramId, 'render-profile-preview'), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          profile: readForm(targetName() || 'draft'),
          view: window.RIDAL_GEOMETRY.view || 'standard',
          traces: state.traces,
          rows: state.rows,
          x_scale: window.RIDAL_XSCALE || 1,
          width: previewWidth(),
        }),
        signal: controller.signal,
      });
    } catch (error) {
      if (error.name === 'AbortError') return;
      status.textContent = `Could not reach the server: ${error.message}`;
      done();
      return;
    }
    if (controller !== state.controller) return;
    if (!response.ok) {
      status.textContent = await errorText(response);
      done();
      return;
    }
    const blob = await response.blob();
    if (controller !== state.controller) return;
    // The one before is kept for "Show previous"; the one before that goes.
    if (state.previousUrl) URL.revokeObjectURL(state.previousUrl);
    state.previousUrl = state.previewUrl;
    state.previewUrl = URL.createObjectURL(blob);
    setComparing(false);
    $('pe-compare').disabled = !state.previousUrl;
    status.hidden = true;
    done();
  }

  function setComparing(on) {
    state.comparing = on && Boolean(state.previousUrl);
    const button = $('pe-compare');
    button.setAttribute('aria-pressed', String(state.comparing));
    button.textContent = state.comparing ? 'Show latest' : 'Show previous';
    const url = state.comparing ? state.previousUrl : state.previewUrl;
    if (url) showImage(url);
  }

  /* After a change: re-render if "Auto update" is on, after the controls
   * have been still for a moment; otherwise mark the preview stale. */
  function changed() {
    setUnsaved(true);
    showWarnings();
    clearTimeout(state.timer);
    if ($('pe-auto').checked) {
      state.timer = setTimeout(preview, PREVIEW_DEBOUNCE_MS);
      return;
    }
    // A preview already on its way is of the profile before this change:
    // it must not arrive and hide that the picture is stale.
    if (state.controller) state.controller.abort();
    state.controller = null;
    $('pe-preview').classList.remove('is-busy');
    $('pe-update').classList.add('is-stale');
    if (state.previewUrl) {
      $('pe-status').hidden = false;
      $('pe-status').textContent = 'Update the preview to see the changes.';
    }
  }

  // --- Saving ----------------------------------------------------------------

  /* The button that closes the editor says what closing does: with edits
   * not yet saved it throws them away, so it is "Cancel". */
  function setUnsaved(unsaved) {
    $('pe-close').textContent = unsaved ? 'Cancel' : 'Close';
  }

  function setSaveStatus(text) {
    $('pe-save-status').textContent = text;
  }

  async function put(url, body, headers) {
    return fetch(url, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', ...headers },
      body: JSON.stringify(body),
    });
  }

  async function save() {
    const shortName = $('pe-name').value.trim();
    if (!shortName) {
      setSaveStatus('Give the profile a name.');
      $('pe-name').focus();
      return;
    }
    const personal = $('pe-scope').value === 'personal';
    const name = targetName();
    const body = readForm(name);
    let url;
    if (personal) {
      body.thumbnail_profile = $('pe-thumbnail').value || null;
      url = RIDAL.apiPath('preferences', 'render-profiles', name);
    } else {
      url = RIDAL.apiPath('render-profiles', name);
    }
    // Over the profile it was loaded from, only as it was loaded; anywhere
    // else, only where there is nothing yet, unless the person says so.
    const over = savesOverBase();
    const condition = over
      ? (state.version ? { 'If-Match': state.version } : {})
      : { 'If-None-Match': '*' };
    $('pe-save').disabled = true;
    setSaveStatus('Saving…');
    try {
      let response = await put(url, body, condition);
      if (response.status === 412) {
        const question = over
          ? `${shortName} was changed since you opened it. Replace it with this?`
          : `There is already a profile called ${shortName}${personal ? ' among yours' : ' in the project'}. Replace it?`;
        if (!window.confirm(question)) {
          setSaveStatus('Not saved.');
          return;
        }
        response = await put(url, body, {});
      }
      if (!response.ok) {
        setSaveStatus(await errorText(response));
        return;
      }
      await afterSave(name, response.headers.get('etag'));
      setUnsaved(false);
      setSaveStatus('Saved; the viewer now uses it.');
    } catch (error) {
      setSaveStatus(`Could not reach the server: ${error.message}`);
    } finally {
      $('pe-save').disabled = false;
    }
  }

  /* The viewer's select, rebuilt from the list, on `name`. Its change event
   * makes the viewer load chunks with it, and the cache-busting entry makes
   * those new requests even when the name is one the page already drew. */
  function useInViewer(name) {
    fillGrouped(viewerSelect, name);
    const edits = window.RIDAL_PROFILE_EDITS || (window.RIDAL_PROFILE_EDITS = {});
    edits[name] = Date.now().toString(36);
    viewerSelect.value = name;
    viewerSelect.dispatchEvent(new Event('change'));
  }

  async function afterSave(name, version) {
    await loadProfiles();
    fillStart(name);
    fillThumbnails();
    const saved = state.profiles.find((p) => p.name === name);
    if (saved) {
      state.base = saved;
      state.version = version;
      $('pe-start').value = name;
      $('pe-origin').textContent = `Editing ${labelOf(saved)}, ${saved.scope === 'personal' ? 'one of yours' : 'a project profile'}.`;
    }
    syncSaveControls();
    useInViewer(name);
  }

  async function remove() {
    const profile = state.base;
    if (!profile || !savesOverBase()) return;
    if (!window.confirm(`Delete ${labelOf(profile)}? Links and settings that name it fall back to the next profile they would have used.`)) {
      return;
    }
    const url = profile.scope === 'personal'
      ? RIDAL.apiPath('preferences', 'render-profiles', profile.name)
      : RIDAL.apiPath('render-profiles', profile.name);
    try {
      const response = await fetch(url, { method: 'DELETE' });
      if (!response.ok) {
        setSaveStatus(await errorText(response));
        return;
      }
    } catch (error) {
      setSaveStatus(`Could not reach the server: ${error.message}`);
      return;
    }
    await loadProfiles();
    const next = viewerSelect.value === profile.name ? state.defaultName : viewerSelect.value;
    fillStart(next);
    fillThumbnails();
    useInViewer(next);
    await loadBase(next);
    setUnsaved(false);
    setSaveStatus(`Deleted ${labelOf(profile)}.`);
  }

  function fillStart(keep) {
    fillGrouped($('pe-start'), keep);
  }

  // --- Opening ---------------------------------------------------------------

  async function open() {
    setSaveStatus('');
    setUnsaved(true);
    try {
      await loadProfiles();
    } catch (error) {
      $('pe-origin').textContent = `Could not load the render profiles: ${error.message}`;
      dialog.showModal();
      return;
    }
    const current = viewerSelect.value;
    fillStart(current);
    fillThumbnails();
    extentFromView();
    dialog.showModal();
    await loadBase(current);
    preview();
  }

  opener.addEventListener('click', () => {
    const menu = opener.closest('details');
    if (menu) menu.open = false;
    open();
  });
  $('pe-form').addEventListener('submit', (event) => event.preventDefault());
  // `change` rather than `input`: a slider reports once, when released.
  $('pe-form').addEventListener('change', (event) => {
    const id = event.target.id;
    if (id === 'pe-auto') return;
    if (id === 'pe-start') {
      loadBase(event.target.value).then(changed);
      return;
    }
    if (id === 'pe-preset') {
      const name = event.target.value;
      state.colormapName = name || null;
      state.stops = name
        ? colourScales().get(name).map((s) => ({ position: s.position, color: [...s.color] }))
        : [];
      renderStops();
    }
    // A stop's own controls handled it already.
    if (event.target.closest('#pe-stops')) return;
    syncVisibility();
    changed();
  });
  $('pe-form').addEventListener('input', showValues);
  $('pe-add-stop').addEventListener('click', () => {
    // Halfway along the widest gap, in the colour already there.
    const stops = state.stops;
    let at = 1;
    let color = [128, 128, 128];
    if (stops.length === 1) {
      at = stops[0].position < 0.5 ? 1 : 0;
      color = [...stops[0].color];
    } else {
      let widest = -1;
      for (let i = 0; i + 1 < stops.length; i++) {
        const gap = stops[i + 1].position - stops[i].position;
        if (gap > widest) {
          widest = gap;
          at = (stops[i].position + stops[i + 1].position) / 2;
          color = stops[i].color.map((c, k) => Math.round((c + stops[i + 1].color[k]) / 2));
        }
      }
    }
    stops.push({ position: at, color });
    stopsEdited();
  });
  $('pe-reverse').addEventListener('click', () => {
    state.stops = state.stops.map((s) => ({ position: 1 - s.position, color: s.color }));
    stopsEdited();
  });
  $('pe-update').addEventListener('click', preview);
  $('pe-auto').addEventListener('change', () => {
    if ($('pe-auto').checked && $('pe-update').classList.contains('is-stale')) preview();
  });
  $('pe-compare').addEventListener('click', () => setComparing(!state.comparing));
  $('pe-extent').addEventListener('click', () => {
    extentFromView();
    preview();
  });
  $('pe-name').addEventListener('input', syncSaveControls);
  $('pe-scope').addEventListener('change', syncSaveControls);
  $('pe-save').addEventListener('click', save);
  $('pe-delete').addEventListener('click', remove);
  $('pe-close').addEventListener('click', () => dialog.close());
  dialog.addEventListener('close', () => {
    if (state.controller) state.controller.abort();
    state.controller = null;
    clearTimeout(state.timer);
  });
})();
