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

// Each group's "Hide all" / "Show all" (#314). Which way the page opens
// is a setting the server already applied; this changes it for this page
// only, the same way the viewer's "Show picks" does. Unhiding is also what
// lets the hidden cards' lazy thumbnails start loading.
//
// "This page" includes the reloads the page does to itself after an edit
// (`reloadKeepingGroups`): without carrying the state across, removing one
// radargram from a group the operator had just shown hid it again.
const GROUPS_SHOWN = 'ridal.groups-shown';

function setGroupShown(button, shown) {
  const target = document.getElementById(button.getAttribute('aria-controls'));
  target.hidden = !shown;
  button.setAttribute('aria-expanded', String(shown));
  button.textContent = shown ? 'Hide all' : 'Show all';
}

/** Reload the page, keeping each group shown or hidden as it is now.
 *
 * Keyed by group id rather than the toggle's own id, which is a loop index
 * and shifts when the edit adds or removes a group. Read back, and cleared,
 * once on the next load; storage that is unavailable only costs the state. */
function reloadKeepingGroups() {
  const shown = {};
  document.querySelectorAll('.group-toggle').forEach((button) => {
    shown[button.closest('section.group').id] = button.getAttribute('aria-expanded') === 'true';
  });
  try {
    sessionStorage.setItem(GROUPS_SHOWN, JSON.stringify(shown));
  } catch (_) {
    // Nothing to do: the page opens the way the setting says.
  }
  window.location.reload();
}

// The properties and replace dialogs (radargram_edit.js) reload the page
// after an edit; here, keeping the groups as they are.
document.addEventListener('ridal:radargram-edited', (event) => {
  event.preventDefault();
  reloadKeepingGroups();
});

(function restoreGroupsShown() {
  let shown = null;
  try {
    shown = JSON.parse(sessionStorage.getItem(GROUPS_SHOWN));
    sessionStorage.removeItem(GROUPS_SHOWN);
  } catch (_) {
    return;
  }
  if (!shown || typeof shown !== 'object') return;
  document.querySelectorAll('.group-toggle').forEach((button) => {
    const groupId = button.closest('section.group').id;
    if (groupId in shown) setGroupShown(button, shown[groupId]);
  });
})();

document.querySelectorAll('.group-toggle').forEach((button) => {
  button.addEventListener('click', () => {
    setGroupShown(button, button.getAttribute('aria-expanded') !== 'true');
  });
});

// One map per group (#121), each showing every member's track. The
// catalog's target scale (~100 files, #122/#123) keeps this cheap
// enough to load eagerly rather than needing an IntersectionObserver
// lazy-init trick for per-card maps.
document.querySelectorAll('.group-map').forEach((el) => {
  // The element id is passed so a failing overlay can explain itself over
  // this map rather than over whichever one Leaflet asked last.
  const map = RIDAL.basemap(L.map(el.id), el.id);

  RIDAL.fetchJson(RIDAL.apiPath("groups", el.dataset.group, "tracks"))
    .then((members) => {
      // Unlisted radargrams are shown to operators and admins but do not
      // define the view: an unlisted outlier should not stretch the map
      // away from the tracks that are in focus (#192).
      const listedPoints = [];
      const unlistedPoints = [];
      for (const [radargramId, info] of Object.entries(members)) {
        const unlisted = info.unlisted === true;
        const points = unlisted ? unlistedPoints : listedPoints;
        const style = unlisted
          ? {
              color: RIDAL.unlistedColor,
              weight: RIDAL.unlistedWeight,
              opacity: RIDAL.unlistedOpacity,
              dashArray: RIDAL.unlistedDashArray,
            }
          : { color: RIDAL.trackColor, weight: RIDAL.trackWeight };
        const pairs = RIDAL.trackToLatLngs(info.track).map((latlngs) => {
          points.push(...latlngs);
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
            ...style,
            interactive: false,
          }).addTo(map);
          return { visible, hit };
        });
        // Two-way highlight with the matching catalog card (#121
        // planning round item 7): hovering either one highlights both.
        const card = document.getElementById(`card-${radargramId}`);
        RIDAL.bindTrackHighlight(
          pairs,
          card,
          style.weight,
          unlisted ? style.weight : RIDAL.trackFocusWeight,
        );
      }
      const boundsPoints = listedPoints.length > 0 ? listedPoints : unlistedPoints;
      if (boundsPoints.length > 0) {
        map.fitBounds(boundsPoints);
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
  const everyUserRow = document.getElementById('group-every-user-row');
  const everyUser = document.getElementById('group-every-user');
  const includeUnlistedRow = document.getElementById('group-include-unlisted-row');
  const includeUnlisted = document.getElementById('group-include-unlisted');
  let base = null;
  // Which product the open dialog is for: the two share one dialog and the
  // one extra checkbox each needs.
  let product = 'level2';

  /* Fetched rather than navigated to, so a refusal -- "nothing in this
   * group has been interpreted yet" being the ordinary one -- is shown on
   * this page instead of replacing it with the error envelope. */
  const go = (url) => RIDAL.download(url, 'download-error');

  for (const menu of menus) {
    const menuBase = menu.dataset.downloadBase;
    const label = menu.dataset.downloadLabel || '';
    for (const button of menu.querySelectorAll('button[data-download]')) {
      button.addEventListener('click', () => {
        menu.open = false;
        const wanted = button.dataset.download;
        // The two point products share this dialog; everything else is a
        // plain link with no options to ask for.
        if (wanted !== 'level2' && wanted !== 'derived') {
          go(`${menuBase}/${wanted}`);
          return;
        }
        base = menuBase;
        product = wanted;
        const derived = product === 'derived';
        title.textContent =
          (derived ? 'Download derived layer points' : 'Download picked layer points') +
          (label ? ` - ${label}` : '');
        if (everyUserRow) everyUserRow.hidden = derived;
        if (includeUnlistedRow) includeUnlistedRow.hidden = !derived;
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
    const params = new URLSearchParams();
    params.set('spacing', spacing.value);
    params.set('format', fileFormat);
    if (choice === 'geojson-native') params.set('crs', 'native');
    if (product === 'derived') {
      params.set('derived', 'true');
      if (includeUnlisted && includeUnlisted.checked) {
        params.set('include_unlisted', 'true');
      }
    } else if (everyUser && everyUser.checked) {
      params.set('every_user', 'true');
    }
    dialog.close();
    go(`${base}/level2?${params.toString()}`);
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
    RIDAL.setMessage(errorBox, message);
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
        showError(envelope?.error?.message || RIDAL.upstreamMessage(response.status));
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
    reloadKeepingGroups();
  });
})();

/* --- The "Add data" menu (#409) ------------------------------------------
 *
 * Choosing an item closes it, as the Edit menus do: left open behind the
 * dialog or file picker the item opens, it is a second thing to dismiss.
 * Each item's own behaviour is set up where it always was (below, and in
 * process.js); this only knows they are in a menu.
 */
(function setupAddDataMenu() {
  const menu = document.getElementById('add-data-menu');
  if (!menu) return;
  menu.querySelector('ul').addEventListener('click', (event) => {
    if (event.target.closest('button, a')) menu.open = false;
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
  const cancel = document.getElementById('add-radargram-cancel');
  if (!button || !picker) return;

  // The in-flight upload, so the cancel button can abort it. `RIDAL.postFile`
  // returns the handle with the promise precisely for this.
  let upload = null;
  if (cancel) {
    cancel.addEventListener('click', () => {
      if (upload) upload.abort();
    });
  }

  const say = (message, tone) => {
    status.replaceChildren();
    if (!message) return;
    // A <div> rather than the <p> this used to build: `.warning` draws one
    // box, and a multi-paragraph message has to nest its paragraphs inside
    // that box rather than produce three boxes that read as three separate
    // failures. A `.warning` holding block children is the established
    // shape -- see the no-accounts notice in login.html.jinja.
    const box = document.createElement('div');
    box.className = tone === 'problem' ? 'warning' : 'hint';
    RIDAL.setMessage(box, message);
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
    if (cancel) cancel.hidden = false;
    say(`Uploading ${file.name}…`);
    // Repaint only when the whole percent changes: an XHR progress event
    // fires many times a second and `say` rebuilds the box each call (#175).
    let lastPercent = -1;
    const onProgress = (loaded, total) => {
      const percent = Math.round((loaded / total) * 100);
      if (percent === lastPercent) return;
      lastPercent = percent;
      say(
        `Uploading ${file.name}… ${percent}% ` +
          `(${RIDAL.formatBytes(loaded)} of ${RIDAL.formatBytes(total)})`,
      );
    };
    try {
      upload = RIDAL.postFile(
        `${RIDAL.apiPath('datasets')}?filename=${encodeURIComponent(file.name)}`,
        file,
        onProgress,
      );
      const response = await upload.promise;
      if (!response.ok) {
        say(
          response.json?.error?.message || RIDAL.upstreamMessage(response.status),
          'problem',
        );
        return;
      }
      const added = response.json;
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
      // An abort is a choice, not a failure; say so without the warning tone.
      if (error.message === 'upload cancelled') {
        say('Upload cancelled.');
      } else {
        say(`Could not add it (${error.message}).`, 'problem');
      }
      return;
    } finally {
      button.disabled = false;
      upload = null;
      if (cancel) cancel.hidden = true;
    }
    // Reloaded rather than patched: a new radargram may create a group
    // section, which is most of the page.
    reloadKeepingGroups();
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
        RIDAL.setMessage(
          errorBox,
          envelope?.error?.message || RIDAL.upstreamMessage(response.status),
        );
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
    reloadKeepingGroups();
  });
})();

/* --- Where each radargram stands against its processing profile (#382) ----
 *
 * One request for the whole catalog, not one per card: the status route
 * already answers for every radargram in a profile. Radargrams in none get
 * no badge. For an operator, what is being processed now and what failed
 * come from the batches and their own drafts (drafts are private, so only
 * their own), and each group's heading offers to process its out-of-date
 * radargrams: one batch per profile involved.
 */
(function setupProcessingBadges() {
  const operator = Boolean(document.getElementById('process-new'));
  const WORDS = {
    current: 'Current',
    stale: 'Stale',
    custom: 'Custom steps',
    older_processing: 'Older processing',
    not_processed: 'Not processed',
    no_raw_data: 'No raw data',
    hidden: 'Hidden',
    processing: 'Processing',
    failed: 'Failed',
  };
  const outOfDate = (status) => status.state === 'stale' || status.state === 'not_processed';

  /** What each radargram's badge says, and why. */
  async function standings() {
    let status;
    try {
      status = await RIDAL.fetchJson(RIDAL.apiPath('processing', 'status'));
    } catch {
      // Not a project, or nothing to say.
      return null;
    }
    const by = new Map();
    for (const s of status.radargrams) {
      const word = s.state === 'stale' && s.custom ? 'custom' : s.state;
      by.set(s.radargram_id, { status: s, word, why: (s.reasons || []).join('; ') });
    }
    // Who is processing what (#382), for everyone: someone's draft says
    // only whose it is. A radargram in no profile gets a badge for it too.
    const activity = await RIDAL.fetchJson(RIDAL.apiPath('processing', 'activity')).catch(
      () => ({ activity: [] }),
    );
    for (const a of activity.activity) {
      if (!by.has(a.radargram_id)) by.set(a.radargram_id, { status: null });
      const known = by.get(a.radargram_id);
      known.word = 'processing';
      known.why = RIDAL.describeActivity(a);
    }
    if (!operator) return by;
    // What the last batch to reach a radargram made of it, newest first.
    const batches = await RIDAL.fetchJson(RIDAL.apiPath('processing', 'batches')).catch(() => ({
      batches: [],
    }));
    const said = new Set();
    for (const batch of batches.batches) {
      for (const item of batch.items) {
        const known = by.get(item.radargram_id);
        if (!known?.status || said.has(item.radargram_id)) continue;
        // Says nothing new about the radargram; an older batch might.
        if (item.outcome === 'skipped' && item.reason) continue;
        if (item.outcome === 'failed' && outOfDate(known.status) && known.word !== 'processing') {
          known.word = 'failed';
          known.why = item.message || 'The last batch could not process it';
        }
        said.add(item.radargram_id);
      }
    }
    return by;
  }

  function badgeFor(standing) {
    const link = document.createElement('a');
    link.className = `processing-state processing-state-${standing.word}`;
    link.textContent = WORDS[standing.word] || standing.word;
    const profile = standing.status?.profile;
    link.href = `${RIDAL.pagePath('processing')}${profile ? `#profile-${profile}` : ''}`;
    link.title = standing.why || `Processing profile ${profile}`;
    link.setAttribute('aria-label', `${link.textContent}: ${link.title}`);
    return link;
  }

  /** Each group's out-of-date radargrams, by processing profile. */
  function groupOutOfDate(section, by) {
    const byProfile = new Map();
    let count = 0;
    for (const card of section.querySelectorAll('li.card')) {
      const standing = by.get(card.id.replace(/^card-/, ''));
      if (!standing?.status || !outOfDate(standing.status) || standing.word === 'processing') {
        continue;
      }
      const profile = standing.status.profile;
      if (!byProfile.has(profile)) byProfile.set(profile, []);
      byProfile.get(profile).push(standing.status.radargram_id);
      count += 1;
    }
    return { count, byProfile };
  }

  async function processGroup(section, label, byProfile, button, note) {
    const count = [...byProfile.values()].reduce((n, ids) => n + ids.length, 0);
    const profiles = byProfile.size;
    const ok = window.confirm(
      `Process ${count} out-of-date radargram${count === 1 ? '' : 's'} in ${label}` +
        (profiles > 1 ? `, as ${profiles} batches, one for each processing profile,` : '') +
        ' and apply each without review?',
    );
    if (!ok) return;
    button.disabled = true;
    const failures = [];
    let started = 0;
    for (const [profile, ids] of byProfile) {
      try {
        await RIDAL.fetchJson(RIDAL.apiPath('processing', 'profiles', profile, 'reprocess'), {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ radargram_ids: ids }),
        });
        started += 1;
      } catch (error) {
        failures.push(`${profile}: ${error.message}`);
      }
    }
    note.replaceChildren();
    if (started) {
      const link = document.createElement('a');
      link.href = RIDAL.pagePath('processing');
      link.textContent = 'the Processing page';
      note.append(
        `Started ${started} batch${started === 1 ? '' : 'es'}; follow ${started === 1 ? 'it' : 'them'} on `,
        link,
        '.',
      );
    }
    if (failures.length) note.append(` Not started: ${failures.join('; ')}`);
    note.hidden = false;
    refresh();
  }

  let polling = null;
  async function refresh() {
    if (polling) clearTimeout(polling);
    polling = null;
    const by = await standings();
    if (!by) return;
    for (const card of document.querySelectorAll('li.card')) {
      card.querySelector('.card-processing')?.remove();
      const standing = by.get(card.id.replace(/^card-/, ''));
      if (!standing) continue;
      const line = document.createElement('p');
      line.className = 'card-processing';
      line.append(badgeFor(standing));
      card.querySelector('.card-heading')?.after(line);
    }
    for (const section of document.querySelectorAll('section.group')) {
      const actions = section.querySelector('.group-actions');
      if (!actions) continue;
      actions.querySelector('.group-process')?.remove();
      const { count, byProfile } = groupOutOfDate(section, by);
      if (count === 0) continue;
      const label = section.querySelector('h2')?.textContent.trim() || 'this catalog';
      if (operator) {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'group-toggle group-process';
        button.textContent = `Process ${count} out-of-date…`;
        let note = section.querySelector('.group-process-note');
        if (!note) {
          note = document.createElement('p');
          note.className = 'group-process-note hint';
          note.hidden = true;
          section.querySelector('.group-heading').append(note);
        }
        button.addEventListener('click', () =>
          processGroup(section, label, byProfile, button, note),
        );
        actions.append(button);
      } else {
        const text = document.createElement('span');
        text.className = 'group-process group-outdated';
        text.textContent = `${count} out of date`;
        actions.append(text);
      }
    }
    // While anything is being processed, look again now and then.
    if ([...by.values()].some((s) => s.word === 'processing')) {
      polling = setTimeout(refresh, 10000);
    }
  }
  refresh();
})();
