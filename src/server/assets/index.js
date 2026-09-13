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
 * from the card's own markup. The card only knows the *resolved* values;
 * the dialog also has to show what each field would be without its
 * override, so that reverting can be labelled with the value it goes back
 * to. Rendering that into every card would put the whole override document
 * on the page for the sake of the one card somebody edits.
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
  const grouping = document.getElementById('properties-grouping');
  const groupRow = document.getElementById('properties-group-row');
  const groupInput = document.getElementById('properties-group-name');
  const groupList = document.getElementById('properties-group-list');
  const groupFromFile = document.getElementById('properties-group-file');
  const unlisted = document.getElementById('properties-unlisted');
  const save = document.getElementById('properties-save');

  let radargramId = null;
  /* The id of the group the name box was last known to mean. Sent back
   * alongside the name so that editing only the *name* of an existing
   * group renames it in place instead of forking a new slug off the
   * changed text. */
  let chosenGroupId = null;

  const showError = (message) => {
    errorBox.textContent = message;
    errorBox.hidden = !message;
  };

  /* "Choose or name one" is the only mode with a group to name. */
  const syncGroupRow = () => {
    groupRow.hidden = grouping.value !== 'group';
  };
  grouping.addEventListener('change', () => {
    // Typing a different name means a different group unless the user
    // picked one from the list, which `input` below re-establishes.
    if (grouping.value !== 'group') chosenGroupId = null;
    syncGroupRow();
  });
  groupInput.addEventListener('input', () => {
    const match = [...groupList.options].find((o) => o.value === groupInput.value);
    chosenGroupId = match ? match.dataset.groupId : null;
  });

  const open = async (id, label) => {
    radargramId = id;
    title.textContent = `Edit properties - ${label}`;
    showError('');
    let properties;
    try {
      properties = await RIDAL.fetchJson(
        RIDAL.apiPath('datasets', id, 'properties'),
      );
    } catch (error) {
      RIDAL.reportProblem('download-error', `Could not read properties: ${error.message}`);
      return;
    }

    groupList.replaceChildren(
      ...properties.groups.map((group) => {
        const option = document.createElement('option');
        option.value = group.name;
        option.dataset.groupId = group.id;
        return option;
      }),
    );

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

    if (!properties.overridden.group) {
      grouping.value = 'inherit';
      groupInput.value = '';
      chosenGroupId = null;
    } else if (properties.effective.group_id) {
      grouping.value = 'group';
      groupInput.value = properties.effective.group_name || '';
      chosenGroupId = properties.effective.group_id;
    } else {
      grouping.value = 'ungrouped';
      groupInput.value = '';
      chosenGroupId = null;
    }
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
      grouping: grouping.value,
      unlisted: unlisted.checked,
    };
    if (grouping.value === 'group') {
      const name = groupInput.value.trim();
      if (!name) {
        showError('Give the group a name, or choose "No group".');
        return;
      }
      body.group_name = name;
      // Present only when the name still refers to the group it was read
      // as: otherwise the server derives a fresh id from the text, which
      // is what makes "type a new name" create a new group.
      if (chosenGroupId) body.group_id = chosenGroupId;
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
