/* Index (catalog) page behaviour (#115, #121).
 *
 * Loaded by index.html.jinja after leaflet.js and app.js. Unlike the
 * viewer, this page needs no server-side values interpolated into JS --
 * everything it needs is already in the DOM as `data-` attributes -- so
 * there is no inline block at all here.
 *
 * Deliberately NOT under assets/vendor/ -- scripts/vendor_leaflet.sh does
 * `rm -rf` on that directory.
 */

// A cataloged entry can still fail to produce a preview: AppState::build
// skips files SourceReader::open rejects, and any render failure is a
// 500. Either way the browser would show a broken-image glyph, so
// degrade to an explicit "no preview" instead (#121: rendering failures
// should be reported to the user).
document.querySelectorAll('.card-thumb img').forEach((img) => {
  img.addEventListener('error', () => img.parentElement.classList.add('is-missing'));
});

// Reload with the chosen profile as a URL query param -- shareable and
// bookmarkable, and reused as-is by every thumbnail/card link the
// template already rendered with it (see entry_card's `profile` arg).
document.getElementById('index-profile-select').addEventListener('change', (event) => {
  const params = new URLSearchParams(location.search);
  params.set('profile', event.target.value);
  location.search = params.toString();
});

// One map per group (#121), each showing every member's track. The
// catalog's target scale (~100 files, #122/#123) keeps this cheap
// enough to load eagerly rather than needing an IntersectionObserver
// lazy-init trick for per-card maps.
document.querySelectorAll('.group-map').forEach((el) => {
  const map = RIDAL.basemap(L.map(el.id));

  RIDAL.fetchJson(RIDAL.apiPath("groups", el.dataset.group, "tracks"))
    .then((members) => {
      const allPoints = [];
      for (const [radargramId, info] of Object.entries(members)) {
        const pairs = RIDAL.trackToLatLngs(info.track).map((latlngs) => {
          allPoints.push(...latlngs);
          // The wide companion goes down first and carries the popup, so a
          // track is as easy to hit as it is to see.
          const hit = RIDAL.hitLine(latlngs)
            // A function, not a string: evaluated when the popup opens, so
            // the thumbnail and the link use whatever profile is selected
            // then.
            .bindPopup(() =>
              RIDAL.popupContent(
                radargramId,
                info.effective_label,
                document.getElementById('index-profile-select').value,
              ),
            )
            .addTo(map);
          const visible = L.polyline(latlngs, {
            color: RIDAL.trackColor,
            weight: RIDAL.trackWeight,
            interactive: false,
          }).addTo(map);
          return { visible, hit };
        });
        // Two-way highlight with the matching catalog card (#121
        // planning round item 7): hovering either one highlights both.
        const card = document.getElementById(`card-${radargramId}`);
        RIDAL.bindTrackHighlight(pairs, card, RIDAL.trackWeight, RIDAL.trackFocusWeight);
      }
      if (allPoints.length > 0) {
        map.fitBounds(allPoints);
      } else {
        map.setView([0, 0], 2);
      }
    })
    .catch((error) => {
      // Previously this left a blank map with no explanation -- the
      // group's cards are still listed below it, so a silent empty map
      // reads as "this group has no tracks" rather than "the request
      // failed".
      RIDAL.reportError(el.id, `Could not load tracks for this group: ${error.message}`);
      map.setView([0, 0], 2);
    });
});

/* --- Merged downloads ----------------------------------------------------
 *
 * One handler for every scope on the page: the catalog-wide menu and one
 * per group. Each menu carries the API prefix its items hang off
 * (`data-download-base`), so a scope is a prefix and nothing else -- adding
 * a saved-selection scope later needs no change here, and a new merged
 * product needs one template line rather than one per scope.
 *
 * The points dialog is shared, with the base it was opened for remembered
 * while it is up: only one can be open at a time, so per-scope copies of
 * the markup would be dead weight.
 *
 * Dismissal (outside click, Escape) comes from app.js, which handles every
 * `.site-menu` on the page.
 */
(function setupMergedDownloads() {
  const dialog = document.getElementById('group-download-dialog');
  const menus = [...document.querySelectorAll('.download-menu[data-download-base]')];
  if (!dialog || menus.length === 0) return;

  const title = document.getElementById('group-download-title');
  const spacing = document.getElementById('group-spacing');
  const format = document.getElementById('group-format');
  let base = null;

  /* Fetched rather than navigated to, so a refusal -- "nothing in this
   * group has been interpreted yet" being the ordinary one -- is shown on
   * this page instead of replacing it with the error envelope. */
  const go = (url) => RIDAL.download(url, 'download-error');

  for (const menu of menus) {
    const menuBase = menu.dataset.downloadBase;
    const label = menu.dataset.downloadLabel || '';
    for (const button of menu.querySelectorAll('button[data-download]')) {
      const product = button.dataset.download;
      button.addEventListener('click', () => {
        menu.open = false;
        // Everything except level 2 is a plain link: no options to ask for.
        if (product !== 'level2') {
          go(`${menuBase}/${product}`);
          return;
        }
        base = menuBase;
        title.textContent = label
          ? `Download layer points - ${label}`
          : 'Download layer points';
        dialog.showModal();
      });
    }
  }

  document
    .getElementById('group-download-close')
    .addEventListener('click', () => dialog.close());

  document.getElementById('group-download-go').addEventListener('click', () => {
    if (!base) return;
    // Same two-parameters-from-one-choice shape as the viewer's dialog:
    // "GeoJSON in native coordinates" is one decision to a user.
    const choice = format.value;
    const fileFormat = choice === 'csv' ? 'csv' : 'geojson';
    const crs = choice === 'geojson-native' ? '&crs=native' : '';
    dialog.close();
    go(
      `${base}/level2` +
        `?spacing=${encodeURIComponent(spacing.value)}` +
        `&format=${encodeURIComponent(fileFormat)}${crs}`,
    );
  });
})();

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
  const save = document.getElementById('properties-save');

  /* The three fixed choices. Anything else in the list is a group id, which
   * is why they are words rather than something a slug could collide with:
   * `GroupId` rejects them, so no real group can be spelled this way. */
  const INHERIT = 'inherit';
  const UNGROUPED = 'ungrouped';
  const NEW = 'new';

  let radargramId = null;

  const showError = (message) => {
    errorBox.textContent = message;
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
        showError(
          envelope?.error?.message || `Could not save properties (${response.status}).`,
        );
        return;
      }
    } catch (error) {
      showError(`Could not save properties (${error.message}).`);
      return;
    } finally {
      save.disabled = false;
    }
    dialog.close();
    window.location.reload();
  });
})();

/* --- Edit a group --------------------------------------------------------
 *
 * The same shape as the radargram dialog above, one field shorter. Kept
 * separate rather than generalised: they share a pattern, not a form, and
 * the moment either grows a second field the shared version would be a
 * switch on which one it is.
 *
 * A group's name lives with the group rather than with its members (see
 * `project::overrides`), which is what makes this one save rather than one
 * per radargram.
 */
(function setupGroupPropertiesDialog() {
  const dialog = document.getElementById('group-properties-dialog');
  const buttons = [...document.querySelectorAll('button[data-edit-group]')];
  if (!dialog || buttons.length === 0) return;

  const title = document.getElementById('group-properties-title');
  const errorBox = document.getElementById('group-properties-error');
  const nameInput = document.getElementById('group-properties-name');
  const fromFile = document.getElementById('group-properties-file');
  const save = document.getElementById('group-properties-save');

  let groupId = null;

  const showError = (message) => {
    errorBox.textContent = message;
    errorBox.hidden = !message;
  };

  const open = async (id, label) => {
    groupId = id;
    title.textContent = `Edit group - ${label}`;
    showError('');
    let properties;
    try {
      properties = await RIDAL.fetchJson(RIDAL.apiPath('groups', id, 'properties'));
    } catch (error) {
      RIDAL.reportProblem('download-error', `Could not read the group: ${error.message}`);
      return;
    }

    // Empty when inherited, so "this is the project's name" and "this
    // happens to be what the files say" do not look alike.
    nameInput.value = properties.overridden ? properties.name || '' : '';
    const members =
      properties.member_count === 1 ? '1 radargram' : `${properties.member_count} radargrams`;
    fromFile.textContent = properties.from_file
      ? `Without this, it would be called "${properties.from_file}" — the name its ${members} carry. `
      : `Without this, it would be called "${id}" — its ${members} give no name of their own. `;
    dialog.showModal();
  };

  for (const button of buttons) {
    button.addEventListener('click', () => {
      const menu = button.closest('details.site-menu');
      if (menu) menu.open = false;
      open(button.dataset.editGroup, button.dataset.editGroupLabel || '');
    });
  }

  document
    .getElementById('group-properties-close')
    .addEventListener('click', () => dialog.close());

  save.addEventListener('click', async () => {
    if (!groupId) return;
    save.disabled = true;
    try {
      const response = await fetch(RIDAL.apiPath('groups', groupId, 'properties'), {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ display_name: nameInput.value.trim() || null }),
      });
      if (!response.ok) {
        const envelope = await response.json().catch(() => null);
        showError(envelope?.error?.message || `Could not save the group (${response.status}).`);
        return;
      }
    } catch (error) {
      showError(`Could not save the group (${error.message}).`);
      return;
    } finally {
      save.disabled = false;
    }
    dialog.close();
    // Same reason the radargram dialog reloads: a rename changes a heading,
    // every card's group and the download menu labels under it.
    window.location.reload();
  });
})();

/* --- Add a radargram -----------------------------------------------------
 *
 * The raw file as the request body rather than a multipart form. A NetCDF
 * upload is one file and no fields, so multipart would wrap it in a
 * boundary for nothing and cost the server a parser; this streams straight
 * through on both ends.
 *
 * The filename travels as a query parameter and is a display hint only --
 * the server names the installed file from the radargram id inside it, so
 * nothing a client sends is ever used to build a path.
 */
(function setupAddRadargram() {
  const button = document.getElementById('add-radargram');
  const picker = document.getElementById('add-radargram-file');
  const status = document.getElementById('add-radargram-status');
  if (!button || !picker) return;

  const say = (message, tone) => {
    status.replaceChildren();
    if (!message) return;
    const box = document.createElement('p');
    box.className = tone === 'problem' ? 'warning' : 'hint';
    box.textContent = message;
    status.appendChild(box);
  };

  // A notice that has to outlive the reload the add itself triggers. The
  // only thing worth carrying across is the archived-picks warning, which
  // is precisely the case where the reload would otherwise swallow it.
  const CARRIED = 'ridal.add-notice';
  const carried = sessionStorage.getItem(CARRIED);
  if (carried) {
    sessionStorage.removeItem(CARRIED);
    say(carried, 'problem');
  }

  button.addEventListener('click', () => picker.click());

  picker.addEventListener('change', async () => {
    const file = picker.files && picker.files[0];
    // Reset immediately: without this, picking the same file twice in a row
    // fires no `change` event the second time, and a failed upload cannot
    // be retried without choosing something else first.
    picker.value = '';
    if (!file) return;

    button.disabled = true;
    say(`Uploading ${file.name}…`);
    try {
      const response = await fetch(
        `${RIDAL.apiPath('datasets')}?filename=${encodeURIComponent(file.name)}`,
        { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: file },
      );
      if (!response.ok) {
        const envelope = await response.json().catch(() => null);
        say(envelope?.error?.message || `Could not add it (${response.status}).`, 'problem');
        return;
      }
      const added = await response.json().catch(() => null);
      const archived = added?.archived_interpretations || 0;
      if (archived > 0) {
        // Said, not asked. The picks are in the archive and nothing attaches
        // them to this file; the add is legitimate either way. What the
        // operator cannot know without being told is that the id carries a
        // history, and whether this is the same line returning is a question
        // only they can answer.
        sessionStorage.setItem(
          CARRIED,
          `Added ${added.radargram_id}. Note that ${archived} interpretation(s) ` +
            'were archived under this id when it was last removed. They are not ' +
            'attached to this file, and stay in the archive until someone ' +
            'restores them deliberately.',
        );
      }
    } catch (error) {
      say(`Could not add it (${error.message}).`, 'problem');
      return;
    } finally {
      button.disabled = false;
    }
    // Reloaded rather than patched: a new radargram may create a group
    // section, which is most of the page.
    window.location.reload();
  });
})();

/* --- Remove a radargram --------------------------------------------------
 *
 * Asks first, and says what it will actually do. The two cases genuinely
 * differ -- a file in the project is deleted, one in an external root is
 * only dropped from the catalog -- and a confirmation that did not
 * distinguish them would be worse than none, because it would imply the
 * archive had been changed.
 */
(function setupRemoveRadargram() {
  const dialog = document.getElementById('remove-dialog');
  const buttons = [...document.querySelectorAll('button[data-remove-radargram]')];
  if (!dialog || buttons.length === 0) return;

  const title = document.getElementById('remove-title');
  const explain = document.getElementById('remove-explain');
  const errorBox = document.getElementById('remove-error');
  const confirm = document.getElementById('remove-confirm');

  let radargramId = null;

  for (const button of buttons) {
    button.addEventListener('click', () => {
      const menu = button.closest('details.site-menu');
      if (menu) menu.open = false;
      radargramId = button.dataset.removeRadargram;
      const label = button.dataset.removeLabel || radargramId;
      const inProject = button.dataset.removeInProject === '1';

      title.textContent = inProject ? `Remove ${label}?` : `Stop serving ${label}?`;
      explain.textContent = inProject
        ? 'The file is deleted from the project. Any picks made on it are kept, ' +
          'archived under the interpretations directory, so nothing anyone drew is lost.'
        : 'This radargram lives outside the project, which Ridal never writes to. ' +
          'The file stays exactly where it is; it is only left out of the catalog, ' +
          'and you can put it back later.';
      confirm.textContent = inProject ? 'Remove' : 'Stop serving';
      errorBox.hidden = true;
      dialog.showModal();
    });
  }

  document.getElementById('remove-close').addEventListener('click', () => dialog.close());

  confirm.addEventListener('click', async () => {
    if (!radargramId) return;
    confirm.disabled = true;
    try {
      const response = await fetch(RIDAL.apiPath('datasets', radargramId), {
        method: 'DELETE',
      });
      if (!response.ok) {
        const envelope = await response.json().catch(() => null);
        errorBox.textContent =
          envelope?.error?.message || `Could not remove it (${response.status}).`;
        errorBox.hidden = false;
        return;
      }
    } catch (error) {
      errorBox.textContent = `Could not remove it (${error.message}).`;
      errorBox.hidden = false;
      return;
    } finally {
      confirm.disabled = false;
    }
    dialog.close();
    window.location.reload();
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
    errorBox.textContent = message;
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
      rows.push([
        `Picks by ${d.user}`,
        `${TIER_WORDS[d.severity] || d.severity}${moved}${left}`,
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

    reset();
    setBusy(true);
    // "Uploading and checking" rather than "Checking": the radargram has
    // to cross the network before anything can be read from it, and on a
    // file of this size that is nearly all of the wait. Saying only
    // "Checking" made a transfer look like a hang.
    status.textContent = `Uploading and checking ${file.name}…`;
    try {
      const response = await fetch(
        `${RIDAL.apiPath('datasets', radargramId, 'replace')}` +
          `?filename=${encodeURIComponent(file.name)}`,
        { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: file },
      );
      const body = await response.json().catch(() => null);
      if (!response.ok) {
        status.textContent = 'Choose another file, or cancel.';
        fail(body?.error?.message || `Could not read it (${response.status}).`);
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
        fail(body?.error?.message || `Could not replace it (${response.status}).`);
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
    window.location.reload();
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
