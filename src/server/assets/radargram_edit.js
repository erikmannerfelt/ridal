/* A radargram's own edits: its properties, and replacing it with a new
 * version (#431). Shared by the catalog, where each card's Edit menu has
 * the buttons, and the viewer, whose Edit menu has them for the radargram
 * on screen. The dialogs are radargram_dialogs.html.jinja; the buttons are
 * found by `data-edit-properties` and `data-replace-radargram`, so either
 * page offers them just by rendering them.
 *
 * Loaded by both pages after app.js. Deliberately NOT under assets/vendor/
 * -- scripts/vendor_leaflet.sh does `rm -rf` on that directory.
 */

/* After a save or a commit, the page reloads: what changed can be anything
 * the page was rendered from. The event lets a page reload its own way --
 * the catalog keeps its groups shown or hidden -- by cancelling it; the
 * viewer has nothing to keep and lets the plain reload happen. */
function reloadAfterEdit() {
  const event = new CustomEvent('ridal:radargram-edited', { cancelable: true });
  if (document.dispatchEvent(event)) location.reload();
}

/* --- Edit properties -----------------------------------------------------
 *
 * One dialog for every card, filled from the API when it opens rather than
 * from the card's own markup. The card knows only the resolved values; the
 * dialog also has to show what each field would be *without* its override,
 * so that reverting can be labelled with the value it goes back to.
 * Rendering that into every card would put the whole override document on
 * the page for the sake of the one card somebody edits.
 *
 * A save reloads the page. The change moves cards between groups, renames
 * headings, adds and removes group sections and can make a card disappear
 * into a disclosure -- so patching the DOM would mean reimplementing the
 * index template in JavaScript to stay honest about it.
 */
(function setupPropertiesDialog() {
  const dialog = document.getElementById('properties-dialog');
  const buttons = [...document.querySelectorAll('button[data-edit-properties]')];
  if (!dialog || buttons.length === 0) return;

  const title = document.getElementById('properties-title');
  const errorBox = document.getElementById('properties-error');
  const nameInput = document.getElementById('properties-display-name');
  const nameFromFile = document.getElementById('properties-display-name-file');
  const groupChoice = document.getElementById('properties-group-choice');
  const groupRow = document.getElementById('properties-group-row');
  const groupInput = document.getElementById('properties-group-name');
  const groupFromFile = document.getElementById('properties-group-file');
  const unlisted = document.getElementById('properties-unlisted');
  const elevationMin = document.getElementById('properties-elevation-min');
  const elevationMax = document.getElementById('properties-elevation-max');
  const xScale = document.getElementById('properties-x-scale');
  const save = document.getElementById('properties-save');

  /* The three fixed choices. Anything else in the list is a group id, which
   * is why they are words rather than something a slug could collide with:
   * `GroupId` rejects them, so no real group can be spelled this way. */
  const INHERIT = 'inherit';
  const UNGROUPED = 'ungrouped';
  const NEW = 'new';

  let radargramId = null;

  const showError = (message) => {
    RIDAL.setMessage(errorBox, message);
    errorBox.hidden = !message;
  };

  const syncGroupRow = () => {
    groupRow.hidden = groupChoice.value !== NEW;
  };
  groupChoice.addEventListener('change', syncGroupRow);

  /* Rebuild the list: the three fixed choices, then one option per group
   * the catalog knows about. Named by name and valued by id, so nobody has
   * to know slugs exist and picking a group cannot mistype its id. */
  const fillGroups = (groups, selected) => {
    const fixed = [...groupChoice.options].filter((o) =>
      [INHERIT, UNGROUPED, NEW].includes(o.value),
    );
    groupChoice.replaceChildren(...fixed);
    const newOption = groupChoice.querySelector(`option[value="${NEW}"]`);
    for (const group of groups) {
      const option = document.createElement('option');
      option.value = group.id;
      option.textContent = group.name;
      groupChoice.insertBefore(option, newOption);
    }
    groupChoice.value = selected;
    // A group that no longer exists cannot be preselected; fall back to
    // inherit rather than leaving the select showing nothing.
    if (!groupChoice.value) groupChoice.value = INHERIT;
  };

  const open = async (id, label) => {
    radargramId = id;
    title.textContent = `Edit properties - ${label}`;
    showError('');
    groupInput.value = '';
    let properties;
    try {
      properties = await RIDAL.fetchJson(RIDAL.apiPath('datasets', id, 'properties'));
    } catch (error) {
      RIDAL.reportProblem('download-error', `Could not read properties: ${error.message}`);
      return;
    }

    // Only the overridden fields are prefilled with the project's values.
    // An inherited field shows empty with the file's value named beneath
    // it, so "this is inherited" and "this happens to match" look
    // different -- which is the distinction the whole document is about.
    nameInput.value = properties.overridden.display_name
      ? properties.effective.display_name || ''
      : '';
    nameFromFile.textContent = properties.from_file.display_name
      ? `Without this, it would be called "${properties.from_file.display_name}".`
      : `Without this, it would be called "${id}" — the file gives no name.`;

    let selected = INHERIT;
    if (properties.overridden.group) {
      selected = properties.effective.group_id || UNGROUPED;
    }
    fillGroups(properties.groups, selected);
    groupFromFile.textContent = properties.from_file.group_name
      ? `Without this, it would be in "${properties.from_file.group_name}".`
      : 'Without this, it would be in no group.';

    unlisted.checked = properties.effective.unlisted;

    // Unlike name/group, elevation has no file-side counterpart to fall
    // back to: `null` on either side just means "no bound", which is the
    // same value whether it is inherited or explicitly cleared, so the
    // field always shows the effective value rather than only-when-set.
    elevationMin.value = properties.effective.elevation_min ?? '';
    elevationMax.value = properties.effective.elevation_max ?? '';
    // The options' values are the server's canonical text for each scale,
    // which is what `String` gives for the number too (`2`, not `2.0`).
    xScale.value = properties.overridden.x_scale ? String(properties.effective.x_scale) : '';

    syncGroupRow();
    dialog.showModal();
  };

  for (const button of buttons) {
    button.addEventListener('click', () => {
      // Close the menu it came out of, the way the download menus do:
      // leaving it open behind a modal is a second thing to dismiss.
      const menu = button.closest('details.site-menu');
      if (menu) menu.open = false;
      open(button.dataset.editProperties, button.dataset.editLabel || '');
    });
  }

  document
    .getElementById('properties-close')
    .addEventListener('click', () => dialog.close());

  save.addEventListener('click', async () => {
    if (!radargramId) return;
    const body = {
      display_name: nameInput.value.trim() || null,
      unlisted: unlisted.checked,
    };
    const choice = groupChoice.value;
    if (choice === INHERIT) {
      body.grouping = 'inherit';
    } else if (choice === UNGROUPED) {
      body.grouping = 'ungrouped';
    } else if (choice === NEW) {
      const name = groupInput.value.trim();
      if (!name) {
        showError('Give the new group a name, or pick an existing one.');
        return;
      }
      // No id: the server derives the slug exactly as processing does, so
      // a name matching an existing group joins it rather than forking.
      body.grouping = 'group';
      body.group_name = name;
    } else {
      // An existing group, named by id. Deliberately no `group_name`:
      // joining a group must not be able to rename it, which is what the
      // group's own dialog is for.
      body.grouping = 'group';
      body.group_id = choice;
    }

    const parseElevation = (input) => {
      const trimmed = input.value.trim();
      return trimmed === '' ? null : Number(trimmed);
    };
    const minValue = parseElevation(elevationMin);
    const maxValue = parseElevation(elevationMax);
    if (minValue !== null && Number.isNaN(minValue)) {
      showError('The topographic correction floor is not a number.');
      return;
    }
    if (maxValue !== null && Number.isNaN(maxValue)) {
      showError('The topographic correction surface cap is not a number.');
      return;
    }
    if (minValue !== null && maxValue !== null && !(minValue < maxValue)) {
      showError('The floor must be below the surface cap.');
      return;
    }
    body.elevation_min = minValue;
    body.elevation_max = maxValue;
    body.x_scale = xScale.value === '' ? null : Number(xScale.value);

    save.disabled = true;
    try {
      const response = await fetch(
        RIDAL.apiPath('datasets', radargramId, 'properties'),
        {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        },
      );
      if (!response.ok) {
        const envelope = await response.json().catch(() => null);
        showError(envelope?.error?.message || RIDAL.upstreamMessage(response.status));
        return;
      }
    } catch (error) {
      showError(`Could not save properties (${error.message}).`);
      return;
    } finally {
      save.disabled = false;
    }
    dialog.close();
    reloadAfterEdit();
  });
})();

/* --- Replace a radargram with a new version (#148) ------------------------
 *
 * Two requests, because what a replace costs cannot be known until the new
 * file has been read. The upload stages it and comes back with a report;
 * this dialog shows the report and the operator commits or discards it.
 *
 * Committing is the destructive step -- the old file goes -- so the button
 * stays disabled until there is a staged replacement to commit, and
 * closing the dialog any other way discards what was staged rather than
 * leaving it in the project.
 */
(function setupReplaceRadargram() {
  const dialog = document.getElementById('replace-dialog');
  const picker = document.getElementById('replace-radargram-file');
  const buttons = [...document.querySelectorAll('button[data-replace-radargram]')];
  if (!dialog || !picker || buttons.length === 0) return;

  const title = document.getElementById('replace-title');
  const status = document.getElementById('replace-status');
  const headline = document.getElementById('replace-headline');
  const detail = document.getElementById('replace-detail');
  const errorBox = document.getElementById('replace-error');
  const confirm = document.getElementById('replace-confirm');
  const choose = document.getElementById('replace-choose');

  let radargramId = null;
  let token = null;
  let busy = false;
  let committing = false;

  const TIER_WORDS = {
    current: 'unchanged',
    carried: 'carries cleanly',
    approximate: 'moves',
    partial: 'partly outside the new version',
    refused: 'cannot be shown',
  };

  const fail = (message) => {
    RIDAL.setMessage(errorBox, message);
    errorBox.hidden = false;
  };

  function reset() {
    token = null;
    confirm.disabled = true;
    errorBox.hidden = true;
    headline.textContent = '';
    detail.replaceChildren();
    status.textContent = '';
  }

  /** Nothing may be started while something is in flight.
   *
   * Both the upload and the commit take as long as a radargram takes to
   * move, and during that time every control here would start a second
   * one. The confirm button was already disabled while uploading, but
   * nothing said so on screen — see the `:disabled` rule in app.css. */
  function setBusy(value) {
    busy = value;
    choose.disabled = value;
    if (value) confirm.disabled = true;
  }

  /** Give back a staged file nobody is going to commit.
   *
   * Best effort: the sweep catches it either way, and an operator who has
   * closed the dialog should not be shown a failure about cleanup. */
  function discard() {
    if (!token || !radargramId) return;
    const url = `${RIDAL.apiPath('datasets', radargramId, 'replace')}/${token}`;
    // `keepalive` so it still goes if the page is on its way out.
    fetch(url, { method: 'DELETE', keepalive: true }).catch(() => {});
    token = null;
  }

  function render(report) {
    headline.textContent = report.headline || '';

    const rows = [];
    if (report.shape) {
      const s = report.shape;
      rows.push([
        'Size',
        s.changed
          ? `${s.from_traces} × ${s.from_samples} → ${s.to_traces} × ${s.to_samples}`
          : `${s.to_traces} × ${s.to_samples} (unchanged)`,
      ]);
    }
    rows.push(['New version', report.to_revision]);
    if (!report.outgoing_axes_kept) {
      rows.push([
        'Current version',
        'does not describe its axes, so its mapping cannot be kept',
      ]);
    }
    if (report.documents.length === 0) {
      rows.push(['Picks', 'none']);
    }
    for (const d of report.documents) {
      const moved = d.moved
        ? ` (up to ${d.moved.worst_traces.toFixed(1)} traces, ` +
          `${d.moved.worst_samples.toFixed(1)} samples)`
        : '';
      const left = d.dropped && d.dropped.length > 0
        ? ` — ${d.dropped.length} left out`
        : '';
      const warned = d.warnings && d.warnings.length > 0
        ? ` — ${d.warnings.join(' ')}`
        : '';
      rows.push([
        `Picks by ${d.user}`,
        `${TIER_WORDS[d.severity] || d.severity}${moved}${left}${warned}`,
      ]);
    }

    const dl = document.createElement('dl');
    dl.className = 'replace-detail';
    for (const [term, value] of rows) {
      const dt = document.createElement('dt');
      dt.textContent = term;
      const dd = document.createElement('dd');
      dd.textContent = value;
      dl.append(dt, dd);
    }
    detail.replaceChildren(dl);

    // The one case that is not a caveat but a refusal. Committing would
    // leave every pick drawn on the current version impossible to place on
    // anything, ever, because the mapping that relates them is about to be
    // deleted along with the file.
    const stopped =
      report.revision_id_collision ||
      (!report.outgoing_axes_kept && report.documents.length > 0);
    confirm.disabled = stopped;
    if (stopped) {
      fail(
        report.revision_id_collision
          ? 'Reprocess the new file so it gets its own processing date, then try again.'
          : 'This replace is refused because the current version has no mapping to keep.',
      );
    }
  }

  for (const button of buttons) {
    button.addEventListener('click', () => {
      const menu = button.closest('details.site-menu');
      if (menu) menu.open = false;
      discard();
      reset();
      radargramId = button.dataset.replaceRadargram;
      title.textContent = `Replace ${button.dataset.replaceLabel || radargramId}?`;
      status.textContent =
        'Choose the processed .nc file to put behind this radargram. Nothing ' +
        'changes until you confirm.';
      setBusy(false);
      dialog.showModal();
      picker.click();
    });
  }

  picker.addEventListener('change', async () => {
    const file = picker.files && picker.files[0];
    picker.value = '';
    if (!file || !radargramId) return;

    // Hand back whatever is already staged before forgetting its token.
    // `reset()` cleared the token without sending a DELETE, so choosing a
    // second file left the first upload -- a whole radargram -- sitting in
    // `.staging` against the project's quota until the six-hour sweep.
    discard();
    reset();
    setBusy(true);
    // "Uploading and checking" rather than "Checking": the radargram has
    // to cross the network before anything can be read from it, and on a
    // file of this size that is nearly all of the wait. Saying only
    // "Checking" made a transfer look like a hang.
    status.textContent = `Uploading and checking ${file.name}…`;
    let lastPercent = -1;
    const onProgress = (loaded, total) => {
      const percent = Math.round((loaded / total) * 100);
      if (percent === lastPercent) return;
      lastPercent = percent;
      status.textContent =
        `Uploading and checking ${file.name}… ${percent}% ` +
        `(${RIDAL.formatBytes(loaded)} of ${RIDAL.formatBytes(total)})`;
    };
    try {
      const response = await RIDAL.postFile(
        `${RIDAL.apiPath('datasets', radargramId, 'replace')}` +
          `?filename=${encodeURIComponent(file.name)}`,
        file,
        onProgress,
      ).promise;
      const body = response.json;
      if (!response.ok) {
        status.textContent = 'Choose another file, or cancel.';
        fail(body?.error?.message || RIDAL.upstreamMessage(response.status));
        return;
      }
      token = body.token;
      status.textContent = `${file.name} is ready. This is what replacing would do:`;
      render(body.report);
    } catch (error) {
      status.textContent = 'Choose another file, or cancel.';
      fail(`Could not upload it (${error.message}).`);
    } finally {
      setBusy(false);
    }
  });

  choose.addEventListener('click', () => {
    if (busy) return;
    picker.click();
  });

  // Cancelling the operating system's file chooser fires this rather than
  // `change`, so without it the dialog said "Uploading and checking…" for
  // a file that was never chosen — or, before that message existed, sat
  // silently offering Replace and Cancel with nothing to replace.
  picker.addEventListener('cancel', () => {
    if (token) return;
    status.textContent =
      'No file chosen. Choose one to see what replacing would do, or cancel ' +
      'to leave this radargram as it is.';
  });

  confirm.addEventListener('click', async () => {
    if (!token || !radargramId || busy) return;
    setBusy(true);
    committing = true;
    status.textContent = 'Replacing…';
    try {
      const response = await fetch(
        `${RIDAL.apiPath('datasets', radargramId, 'replace')}/${token}`,
        { method: 'POST' },
      );
      if (!response.ok) {
        const body = await response.json().catch(() => null);
        status.textContent = '';
        fail(body?.error?.message || RIDAL.upstreamMessage(response.status));
        committing = false;
        setBusy(false);
        confirm.disabled = false;
        return;
      }
    } catch (error) {
      status.textContent = '';
      fail(`Could not replace it (${error.message}).`);
      committing = false;
      setBusy(false);
      confirm.disabled = false;
      return;
    }
    // Committed, so there is nothing staged left to give back.
    token = null;
    reloadAfterEdit();
  });

  document.getElementById('replace-close').addEventListener('click', () => {
    // Cancelling during the *upload* is fine, and has to be: a radargram
    // takes as long as it takes, and nobody should be held in a modal
    // waiting for one. Cancelling during the commit is not — discarding
    // then would delete the staged file out from under the request that
    // is installing it.
    if (committing) return;
    discard();
    dialog.close();
  });
  // Escape, or any other way out. A staged file that nobody committed is
  // occupying the project's disk for no reason -- unless a commit is
  // using it, which is the one case where letting go would break the
  // thing it is trying to finish.
  dialog.addEventListener('close', () => {
    if (committing) return;
    discard();
  });
  window.addEventListener('pagehide', discard);
})();
