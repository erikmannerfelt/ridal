/* --- The processing page (#382) --------------------------------------------
 *
 * Every processing profile, the radargrams in it and where each stands:
 * current, stale (and why), processed by an older Ridal, not processed yet,
 * or without raw data. An operator can make and change profiles, put
 * radargrams that have raw data in them, reprocess a profile's stale
 * radargrams as a batch, and process any one of them through the same
 * dialog as the catalog (process.js). Raw data that is not processed yet,
 * in a profile or not, is listed on its own, so it is never out of sight.
 */
(function setupProcessingPage() {
  const host = document.getElementById('processing-profiles');
  if (!host) return;
  const writable = host.dataset.writable === 'true';
  const errorBox = document.getElementById('processing-error');
  const empty = document.getElementById('processing-empty');
  const historyList = document.getElementById('processing-history-list');

  const fail = (message) => {
    RIDAL.setMessage(errorBox, message);
    errorBox.hidden = false;
  };

  const STATE_WORDS = {
    current: 'current',
    stale: 'stale',
    older_processing: 'older processing',
    not_processed: 'not processed yet',
    no_raw_data: 'no raw data',
  };

  const OUTCOME_WORDS = {
    waiting: 'waiting',
    processing: 'processing',
    committed: 'applied',
    already_current: 'already up to date',
    skipped: 'skipped',
    failed: 'failed',
    interrupted: 'interrupted',
  };

  const el = (tag, props = {}, ...children) => {
    const node = document.createElement(tag);
    Object.assign(node, props);
    node.append(...children);
    return node;
  };
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

  const linesOf = (text) =>
    text
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line !== '' && !line.startsWith('#'));

  /** `etag` makes the write conditional on what was read; `'new'` on
   * there being nothing there yet, so creating a profile cannot replace
   * one that has the same id. */
  async function send(url, method, body, etag) {
    const headers = { 'Content-Type': 'application/json' };
    if (etag === 'new') headers['If-None-Match'] = '*';
    else if (etag) headers['If-Match'] = etag;
    return RIDAL.fetchJson(url, { method, headers, body: JSON.stringify(body) });
  }

  /** A profile's form, for changing it. Opened with the profile's ETag, so
   * that someone else's change in between is refused, not overwritten.
   * `close` takes it away again, for Cancel. */
  async function editForm(profile, close) {
    const response = await fetch(RIDAL.apiPath('processing', 'profiles', profile.id));
    const etag = response.headers.get('ETag');
    const current = profile.versions[profile.versions.length - 1];
    const steps = el('textarea', { rows: 8, spellcheck: false });
    steps.value = current.steps.join('\n');
    const name = el('input', { value: profile.name, required: true });
    const velocity = el('input', {
      type: 'number',
      step: '0.001',
      min: '0.01',
      value: current.medium_velocity,
    });
    const save = el('button', { type: 'submit', textContent: 'Save as a new version' });
    const cancel = el('button', { type: 'button', textContent: 'Cancel' });
    cancel.addEventListener('click', close);
    const form = el(
      'form',
      { className: 'processing-edit' },
      el('label', {}, 'Name ', name),
      el('label', { className: 'process-steps' }, 'Steps, one per line', steps),
      el('label', {}, 'Medium velocity (m/ns) ', velocity),
      el('p', {}, save, ' ', cancel),
    );
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      save.disabled = true;
      try {
        await send(
          RIDAL.apiPath('processing', 'profiles', profile.id),
          'PUT',
          {
            name: name.value,
            description: profile.description || '',
            steps: linesOf(steps.value),
            medium_velocity: Number(velocity.value),
          },
          etag,
        );
        load();
      } catch (error) {
        save.disabled = false;
        fail(
          error.status === 412
            ? 'Someone changed this profile since you opened it. Reload to see their change.'
            : error.message,
        );
      }
    });
    return form;
  }

  function memberRow(profile, member, status) {
    const state = status ? status.state : 'not_processed';
    const badge = el('span', {
      className: `processing-state processing-state-${state}`,
      textContent: STATE_WORDS[state] || state,
    });
    const stateCell = el('td', {});
    // Why, folded under the badge: open it to read the reasons.
    if (status && status.reasons && status.reasons.length) {
      stateCell.append(
        el(
          'details',
          { className: 'processing-why' },
          el('summary', {}, badge),
          el(
            'ul',
            { className: 'processing-reasons' },
            ...status.reasons.map((reason) => el('li', { textContent: reason })),
          ),
        ),
      );
    } else {
      stateCell.append(badge);
    }
    // Folded too: most radargrams have none. The same steps the process
    // dialog shows in its first box, and editable here as there.
    const preCell = el('td', {});
    const preText = el('pre', {
      className: 'processing-steps',
      textContent: member.pre_steps.join('\n') || 'none',
    });
    const pre = el(
      'details',
      { className: 'processing-pre' },
      el('summary', {
        textContent: member.pre_steps.length
          ? plural(member.pre_steps.length, 'step', 'steps')
          : 'none',
      }),
    );
    if (writable) {
      const box = el('textarea', { rows: 2, spellcheck: false, placeholder: 'subset(0 2000)' });
      box.value = member.pre_steps.join('\n');
      const save = el('button', { type: 'button', textContent: 'Save' });
      save.addEventListener('click', async () => {
        save.disabled = true;
        try {
          await send(RIDAL.apiPath('processing', 'assignments', member.id), 'PUT', {
            profile: profile.id,
            pre_steps: linesOf(box.value),
          });
          load();
        } catch (error) {
          save.disabled = false;
          fail(error.message);
        }
      });
      pre.append(
        box,
        el('p', {
          className: 'hint',
          textContent:
            "Run before the profile's steps. Only subset and remove_traces, one per line.",
        }),
        el('p', {}, save),
      );
    } else {
      pre.append(preText);
    }
    preCell.append(pre);
    const cells = [
      el(
        'td',
        {},
        el('a', {
          className: 'processing-id',
          href: RIDAL.pagePath('view', member.id),
          textContent: member.id,
        }),
      ),
      stateCell,
      preCell,
    ];
    if (writable) {
      // The process dialog (process.js) picks this up by its data attribute.
      const process = el('button', {
        type: 'button',
        textContent: state === 'no_raw_data' ? 'Attach raw data…' : 'Process…',
      });
      process.dataset.processRadargram = member.id;
      process.dataset.served = String(member.served);
      const remove = el('button', { type: 'button', textContent: 'Take out' });
      remove.addEventListener('click', async () => {
        remove.disabled = true;
        try {
          await RIDAL.fetchJson(RIDAL.apiPath('processing', 'assignments', member.id), {
            method: 'DELETE',
          });
          load();
        } catch (error) {
          remove.disabled = false;
          fail(error.message);
        }
      });
      cells.push(el('td', { className: 'processing-row-actions' }, process, ' ', remove));
    }
    return el('tr', {}, ...cells);
  }

  /** Put one of `addable` (radargrams with raw data in no profile) in
   * `profile`. In a box of its own, so it reads as one action. */
  function addForm(profile, addable) {
    const box = el(
      'fieldset',
      { className: 'processing-add' },
      el('legend', { textContent: 'Add a radargram' }),
    );
    if (addable.length === 0) {
      box.append(
        el('p', {
          className: 'hint',
          textContent:
            'Every radargram with raw data is in a profile already. Give one raw data ' +
            'from the catalog ("From raw data…", or "Attach raw data…" in its Edit menu).',
        }),
      );
      return box;
    }
    const choose = el('select', {});
    for (const id of addable) choose.append(el('option', { value: id, textContent: id }));
    const pre = el('textarea', { rows: 2, spellcheck: false, placeholder: 'subset(0 2000)' });
    const add = el('button', { type: 'submit', textContent: 'Add' });
    const form = el(
      'form',
      {},
      el('label', {}, 'Radargram ', choose),
      el(
        'label',
        {},
        "Steps before the profile's, one per line (optional)",
        pre,
        el('span', {
          className: 'hint',
          textContent: 'This radargram\'s own, run first. Only subset and remove_traces.',
        }),
      ),
      el('p', {}, add),
    );
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (!choose.value) return;
      add.disabled = true;
      try {
        await send(RIDAL.apiPath('processing', 'assignments', choose.value), 'PUT', {
          profile: profile.id,
          pre_steps: linesOf(pre.value),
        });
        load();
      } catch (error) {
        add.disabled = false;
        fail(error.message);
      }
    });
    box.append(form);
    return box;
  }

  /** How a batch is getting on, in one line. */
  function batchSummary(batch) {
    const counts = {};
    for (const item of batch.items) counts[item.outcome] = (counts[item.outcome] || 0) + 1;
    const finished = batch.items.filter(
      (i) => i.outcome !== 'waiting' && i.outcome !== 'processing',
    ).length;
    const parts = Object.entries(counts)
      .filter(([outcome]) => outcome !== 'waiting' && outcome !== 'processing')
      .map(([outcome, n]) => `${n} ${OUTCOME_WORDS[outcome] || outcome}`);
    const moved = batch.items.filter((i) => (i.picks_not_carried || []).length).length;
    if (moved) parts.push(`${moved} with picks that did not carry`);
    const progress = batch.finished_at
      ? 'Finished'
      : `${finished} of ${batch.items.length} done`;
    return `${progress}${parts.length ? `: ${parts.join(', ')}` : ''}.`;
  }

  /** Whose picks an applied item did not carry whole, as a phrase. */
  const CARRY_WORDS = { approximate: 'moved', partial: 'partly left out', refused: 'not carried' };
  const notCarried = (picks) =>
    picks
      .map((p) => {
        const how = p.dropped ? `${p.dropped} left out` : CARRY_WORDS[p.severity] || p.severity;
        return `${p.user}'s (${how})`;
      })
      .join(', ');

  function batchItems(batch) {
    return el(
      'ul',
      { className: 'processing-batch-items' },
      ...batch.items.map((item) => {
        const why = item.reason || item.message;
        const picks = item.picks_not_carried || [];
        const said = picks.length
          ? `applied, but some picks did not carry: ${notCarried(picks)}. Check them in the viewer.`
          : `${OUTCOME_WORDS[item.outcome] || item.outcome}${why ? `: ${why}` : ''}`;
        return el(
          'li',
          { className: picks.length ? 'processing-batch-picks' : '' },
          el(
            'a',
            { href: RIDAL.pagePath('view', item.radargram_id) },
            el('code', { textContent: item.radargram_id }),
          ),
          ` ${said}`,
        );
      }),
    );
  }

  /** A running batch, under its profile, followed until it ends. */
  function followBatch(area, batch) {
    const line = el('p', {});
    const items = el('details', {}, el('summary', { textContent: 'Each radargram' }));
    area.replaceChildren(line, items);
    area.hidden = false;
    const draw = (b) => {
      line.textContent = `Processing with version ${b.version}. ${batchSummary(b)}`;
      items.replaceChildren(items.firstChild, batchItems(b));
    };
    draw(batch);
    const tick = async () => {
      let b;
      try {
        b = await RIDAL.fetchJson(RIDAL.apiPath('processing', 'batches', batch.id));
      } catch {
        setTimeout(tick, 4000);
        return;
      }
      draw(b);
      if (b.finished_at) load();
      else setTimeout(tick, 2000);
    };
    setTimeout(tick, 1000);
  }

  /** Reprocess, asked first, inline: it applies every result with no one
   * looking at it. */
  function reprocessControls(profile, counts, area) {
    const count = counts.stale + counts.not_processed;
    const which = [
      counts.stale ? `${counts.stale} stale` : '',
      counts.not_processed ? `${counts.not_processed} not processed yet` : '',
    ]
      .filter(Boolean)
      .join(', ');
    const start = el('button', {
      type: 'button',
      textContent: `Process ${plural(count, 'out-of-date radargram', 'out-of-date radargrams')}…`,
    });
    const confirm = el('button', { type: 'button', textContent: 'Process and apply' });
    const cancel = el('button', { type: 'button', textContent: 'Cancel' });
    const ask = el(
      'p',
      { className: 'processing-confirm warning', hidden: true },
      `Process ${plural(count, 'radargram', 'radargrams')} (${which}) with version ` +
        `${profile.current_version} of ${profile.name}, and apply each without review? ` +
        'Picks are carried over as in a replace. A radargram someone else changes in ' +
        'the meantime is skipped. ',
      confirm,
      ' ',
      cancel,
    );
    start.addEventListener('click', () => {
      ask.hidden = false;
      start.disabled = true;
    });
    cancel.addEventListener('click', () => {
      ask.hidden = true;
      start.disabled = false;
    });
    confirm.addEventListener('click', async () => {
      confirm.disabled = true;
      try {
        const batch = await send(
          RIDAL.apiPath('processing', 'profiles', profile.id, 'reprocess'),
          'POST',
          {},
        );
        ask.hidden = true;
        followBatch(area, batch);
      } catch (error) {
        confirm.disabled = false;
        fail(error.message);
      }
    });
    return { start, ask };
  }

  function renderProfile(profile, statuses, assignments, addable, served, running) {
    const current = profile.versions[profile.versions.length - 1];
    const members = profile.members.map((id) => ({
      id,
      pre_steps: (assignments[id] && assignments[id].pre_steps) || [],
      served: served.has(id),
    }));
    const counts = {};
    for (const m of members) {
      const s = statuses[m.id];
      const state = s ? s.state : 'not_processed';
      counts[state] = (counts[state] || 0) + 1;
    }
    const summary = Object.entries(counts)
      .map(([state, n]) => `${n} ${STATE_WORDS[state] || state}`)
      .join(', ');

    const section = el(
      'section',
      // Where a catalog card's processing badge lands.
      { className: 'processing-profile', id: `profile-${profile.id}` },
      el(
        'h2',
        {},
        profile.name,
        el('span', {
          className: 'hint',
          textContent: ` ${profile.id}, version ${profile.current_version}`,
        }),
      ),
      el('pre', { className: 'processing-steps', textContent: current.steps.join('\n') }),
      el('p', {
        className: 'hint',
        textContent:
          `Medium velocity ${current.medium_velocity} m/ns. ` +
          `Version ${current.version} by ${current.by}, ${current.at.slice(0, 10)}.` +
          (members.length
            ? ` ${plural(members.length, 'radargram', 'radargrams')}: ${summary}.`
            : ' No radargrams yet.'),
      }),
    );
    const batchArea = el('div', { className: 'processing-batch', hidden: true });
    if (writable) {
      const actions = el('p', { className: 'processing-actions' });
      const outOfDate = { stale: counts.stale || 0, not_processed: counts.not_processed || 0 };
      let ask = null;
      if (outOfDate.stale + outOfDate.not_processed > 0 && !running) {
        const controls = reprocessControls(profile, outOfDate, batchArea);
        actions.append(controls.start, ' ');
        ask = controls.ask;
      }
      const edit = el('button', { type: 'button', textContent: 'Change…' });
      actions.append(edit);
      if (members.length === 0) {
        const remove = el('button', { type: 'button', textContent: 'Delete', className: 'danger' });
        remove.addEventListener('click', async () => {
          remove.disabled = true;
          try {
            await RIDAL.fetchJson(RIDAL.apiPath('processing', 'profiles', profile.id), {
              method: 'DELETE',
            });
            load();
          } catch (error) {
            remove.disabled = false;
            fail(error.message);
          }
        });
        actions.append(' ', remove);
      }
      section.append(actions);
      if (ask) section.append(ask);
      // The form opens right under the button that opened it.
      const editSlot = el('div', {});
      section.append(editSlot);
      edit.addEventListener('click', async () => {
        edit.disabled = true;
        try {
          editSlot.replaceChildren(
            await editForm(profile, () => {
              editSlot.replaceChildren();
              edit.disabled = false;
            }),
          );
        } catch (error) {
          edit.disabled = false;
          fail(error.message);
        }
      });
    }
    section.append(batchArea);
    if (running) followBatch(batchArea, running);
    if (members.length) {
      const head = ['Radargram', 'State', 'Steps before the profile'];
      if (writable) head.push('');
      section.append(
        el(
          'div',
          { className: 'table-scroll' },
          el(
            'table',
            // The layers page's table: columns keep a readable width and
            // the wrapper scrolls sideways on a narrow screen, rather than
            // squeezing the reasons into a column a word wide.
            { className: 'processing-members layers-table' },
            el('thead', {}, el('tr', {}, ...head.map((h) => el('th', { scope: 'col', textContent: h })))),
            el('tbody', {}, ...members.map((m) => memberRow(profile, m, statuses[m.id]))),
          ),
        ),
      );
    }
    if (writable) section.append(addForm(profile, addable));
    return section;
  }

  /** The radargrams with raw data and no processed file, with the profile
   * each is in, if any. Processing one opens the dialog at its steps, as
   * for a member that is not processed yet. */
  function renderUnprocessed(ids, assignments, profiles) {
    const section = document.getElementById('raw-unprocessed');
    if (!section) return;
    section.hidden = ids.length === 0;
    const names = Object.fromEntries(profiles.map((p) => [p.id, p.name]));
    const rows = ids.map((id) => {
      const profile = assignments[id]?.profile;
      const cells = [
        el('td', {}, el('span', { className: 'processing-id', textContent: id })),
        el(
          'td',
          {},
          profile
            ? el('a', { href: `#profile-${profile}`, textContent: names[profile] || profile })
            : el('span', { className: 'hint', textContent: 'none' }),
        ),
      ];
      if (writable) {
        const process = el('button', { type: 'button', textContent: 'Process…' });
        process.dataset.processRadargram = id;
        process.dataset.served = 'false';
        cells.push(el('td', { className: 'processing-row-actions' }, process));
      }
      return el('tr', {}, ...cells);
    });
    const head = ['Radargram', 'Profile'];
    if (writable) head.push('');
    document.getElementById('raw-unprocessed-list').replaceChildren(
      el(
        'div',
        { className: 'table-scroll' },
        el(
          'table',
          { className: 'processing-members layers-table' },
          el('thead', {}, el('tr', {}, ...head.map((h) => el('th', { scope: 'col', textContent: h })))),
          el('tbody', {}, ...rows),
        ),
      ),
    );
  }

  function renderHistory(batches, profiles) {
    if (!historyList) return;
    const names = Object.fromEntries(profiles.map((p) => [p.id, p.name]));
    if (batches.length === 0) {
      historyList.replaceChildren(
        el('p', { className: 'hint', textContent: 'No profile has been reprocessed yet.' }),
      );
      return;
    }
    historyList.replaceChildren(
      ...batches.map((b) =>
        el(
          'details',
          // The banner for a finished batch links here.
          { className: 'processing-history-batch', id: `batch-${b.id}` },
          el('summary', {
            textContent:
              `${b.started_at.slice(0, 16).replace('T', ' ')} UTC: ` +
              `${names[b.profile] || b.profile}, version ${b.version}, by ${b.started_by}. ` +
              batchSummary(b),
          }),
          batchItems(b),
        ),
      ),
    );
    const linked = window.location.hash.startsWith('#batch-')
      ? document.getElementById(window.location.hash.slice(1))
      : null;
    if (linked && !historyShown) {
      historyShown = true;
      linked.open = true;
      document.getElementById('processing-history').open = true;
      linked.scrollIntoView();
    }
  }
  let historyShown = false;

  /* --- DEMs ------------------------------------------------------------ */

  const demsList = document.getElementById('dems-list');
  const demsEmpty = document.getElementById('dems-empty');
  const usersDialog = document.getElementById('dems-users');

  /** Who links to `dem`, each a link to its viewer. */
  function showUsers(dem) {
    document.getElementById('dems-users-title').textContent =
      `Radargrams using ${dem.name}`;
    document.getElementById('dems-users-list').replaceChildren(
      ...dem.radargram_ids.map((id) =>
        el('li', {}, el('a', { href: RIDAL.pagePath('view', id), textContent: id })),
      ),
    );
    usersDialog.showModal();
  }
  if (usersDialog) {
    document
      .getElementById('dems-users-close')
      .addEventListener('click', () => usersDialog.close());
  }

  /** Rename `dem` in place: the name cell becomes a box until saved. */
  function renameIn(cell, dem) {
    const box = el('input', { type: 'text', value: dem.name, required: true });
    const save = el('button', { type: 'button', textContent: 'Save' });
    const cancel = el('button', { type: 'button', textContent: 'Cancel' });
    cancel.addEventListener('click', loadDems);
    save.addEventListener('click', async () => {
      save.disabled = true;
      try {
        await send(RIDAL.apiPath('dems', dem.id), 'PUT', { name: box.value });
        loadDems();
      } catch (error) {
        save.disabled = false;
        fail(error.message);
      }
    });
    cell.replaceChildren(box, ' ', save, ' ', cancel);
    box.focus();
  }

  function demRow(dem) {
    const used = dem.radargram_ids.length;
    const usedCell = el('td', {});
    if (used === 0) {
      usedCell.textContent = 'none';
    } else {
      const open = el('button', {
        type: 'button',
        textContent: plural(used, 'radargram', 'radargrams'),
      });
      open.addEventListener('click', () => showUsers(dem));
      usedCell.append(open);
    }
    const nameCell = el('td', { textContent: dem.name });
    const at = new Date(dem.at);
    const cells = [
      nameCell,
      el('td', {}, el('code', { textContent: dem.id })),
      el('td', { textContent: RIDAL.formatBytes(dem.bytes) }),
      el('td', {
        textContent: `${dem.by}, ${Number.isNaN(at.getTime()) ? dem.at : at.toLocaleDateString()}`,
        title: `${dem.file.name} (${dem.file.blake3})`,
      }),
      usedCell,
    ];
    if (writable) {
      const rename = el('button', { type: 'button', textContent: 'Rename' });
      rename.addEventListener('click', () => renameIn(nameCell, dem));
      const actions = el('td', { className: 'processing-row-actions' }, rename);
      if (used === 0) {
        const remove = el('button', { type: 'button', textContent: 'Delete', className: 'danger' });
        remove.addEventListener('click', async () => {
          remove.disabled = true;
          try {
            await RIDAL.fetchJson(RIDAL.apiPath('dems', dem.id), { method: 'DELETE' });
            loadDems();
          } catch (error) {
            remove.disabled = false;
            fail(error.message);
          }
        });
        actions.append(' ', remove);
      }
      cells.push(actions);
    }
    return el('tr', {}, ...cells);
  }

  async function loadDems() {
    if (!demsList) return;
    let dems;
    try {
      dems = (await RIDAL.fetchJson(RIDAL.apiPath('dems'))).dems;
    } catch (error) {
      fail(`Could not load the DEMs (${error.message}).`);
      return;
    }
    demsEmpty.hidden = dems.length > 0;
    if (dems.length === 0) {
      demsList.replaceChildren();
      return;
    }
    const heads = ['Name', 'ID', 'Size', 'Uploaded', 'Used by'];
    if (writable) heads.push('');
    demsList.replaceChildren(
      el(
        'table',
        { className: 'processing-members layers-table' },
        el('thead', {}, el('tr', {}, ...heads.map((h) => el('th', { textContent: h })))),
        el('tbody', {}, ...dems.map(demRow)),
      ),
    );
  }

  const demsForm = document.getElementById('dems-form');
  if (demsForm) {
    const progressLine = document.getElementById('dems-progress');
    const add = document.getElementById('dems-add');
    const cancel = document.getElementById('dems-cancel');
    let uploading = null;
    const derive = () => {
      demsForm.elements.id.placeholder =
        RIDAL.slugify(demsForm.elements.name.value) || 'svalbard-dem-2008';
    };
    demsForm.elements.name.addEventListener('input', derive);
    cancel.addEventListener('click', () => uploading?.abort());
    demsForm.addEventListener('submit', async (event) => {
      event.preventDefault();
      errorBox.hidden = true;
      const name = demsForm.elements.name.value.trim();
      const id = demsForm.elements.id.value.trim() || RIDAL.slugify(name);
      const file = demsForm.elements.file.files[0];
      if (!id) {
        fail('Give the DEM a name with at least one letter or digit, or an id.');
        return;
      }
      add.disabled = true;
      cancel.hidden = false;
      try {
        let lastPercent = -1;
        uploading = RIDAL.postFile(
          `${RIDAL.apiPath('raw')}?filename=${encodeURIComponent(file.name)}`,
          file,
          (loaded, size) => {
            const percent = Math.round((loaded / size) * 100);
            if (percent === lastPercent) return;
            lastPercent = percent;
            progressLine.textContent = `Uploading ${file.name}… ${percent}%`;
          },
        );
        const response = await uploading.promise;
        uploading = null;
        cancel.hidden = true;
        if (!response.ok) {
          throw new Error(response.json?.error?.message || RIDAL.upstreamMessage(response.status));
        }
        progressLine.textContent = 'Checking that it is a DEM…';
        const body = {
          name,
          file: { name: response.json.name, blake3: response.json.blake3 },
        };
        try {
          await send(RIDAL.apiPath('dems', id), 'PUT', body, 'new');
        } catch (error) {
          if (error.status !== 412) throw error;
          // The id is taken. Its file may be replaced while nothing uses
          // it; the server refuses, naming them, when something does.
          const ok = window.confirm(
            `There is already a DEM with the id '${id}'. Replace its file with ${file.name}?`,
          );
          if (!ok) {
            progressLine.textContent = '';
            return;
          }
          await send(RIDAL.apiPath('dems', id), 'PUT', body);
        }
        demsForm.reset();
        derive();
        progressLine.textContent = '';
        loadDems();
      } catch (error) {
        progressLine.textContent = '';
        fail(error.message === 'upload cancelled' ? 'Upload cancelled.' : error.message);
      } finally {
        add.disabled = false;
        cancel.hidden = true;
        uploading = null;
      }
    });
  }

  async function load() {
    errorBox.hidden = true;
    const demsLoaded = loadDems();
    let profiles, status, assignments, datasets, sources, batches;
    try {
      [profiles, status, assignments, datasets, sources, batches] = await Promise.all([
        RIDAL.fetchJson(RIDAL.apiPath('processing', 'profiles')),
        RIDAL.fetchJson(RIDAL.apiPath('processing', 'status')),
        RIDAL.fetchJson(RIDAL.apiPath('processing', 'assignments')),
        RIDAL.fetchJson(RIDAL.apiPath('datasets')),
        RIDAL.fetchJson(RIDAL.apiPath('sources')),
        // Only an operator sees the batches.
        writable
          ? RIDAL.fetchJson(RIDAL.apiPath('processing', 'batches'))
          : Promise.resolve({ batches: [] }),
      ]);
    } catch (error) {
      fail(`Could not load the processing profiles (${error.message}).`);
      return;
    }
    const statuses = {};
    for (const r of status.radargrams) statuses[r.radargram_id] = r;
    const assigned = assignments.radargrams || {};
    const served = new Set((datasets.entries || []).map((d) => d.radargram_id));
    // Only what has raw data can be processed, so only that can join.
    const addable = (sources.radargram_ids || []).filter((id) => !assigned[id]);
    const running = {};
    for (const b of batches.batches) {
      if (!b.finished_at && !running[b.profile]) running[b.profile] = b;
    }
    empty.hidden = profiles.profiles.length > 0;
    host.replaceChildren(
      ...profiles.profiles.map((p) =>
        renderProfile(p, statuses, assigned, addable, served, running[p.id]),
      ),
    );
    renderUnprocessed(
      (sources.radargram_ids || []).filter((id) => !served.has(id)),
      assigned,
      profiles.profiles,
    );
    renderHistory(batches.batches, profiles.profiles);
    // Links land on a profile (a catalog card's badge) or on `#dems` (the
    // process dialog's "Add a DEM"), drawn or moved since the browser
    // looked for them.
    await demsLoaded;
    if (!landed && window.location.hash.length > 1) {
      document.getElementById(decodeURIComponent(window.location.hash.slice(1)))?.scrollIntoView();
    }
    landed = true;
  }
  let landed = false;

  const newForm = document.getElementById('processing-new-form');
  if (newForm) {
    // The id is the name's unless one is typed, as on the settings page.
    const derive = () => {
      newForm.elements.id.placeholder =
        RIDAL.slugify(newForm.elements.name.value) || 'svalbard-100-mhz';
    };
    newForm.elements.name.addEventListener('input', derive);
    newForm.addEventListener('submit', async (event) => {
      event.preventDefault();
      const data = new FormData(newForm);
      const id = String(data.get('id')).trim() || RIDAL.slugify(String(data.get('name')));
      if (!id) {
        fail('Give the profile a name with at least one letter or digit, or an id.');
        return;
      }
      try {
        await send(
          RIDAL.apiPath('processing', 'profiles', id),
          'PUT',
          {
            name: String(data.get('name')),
            steps: linesOf(String(data.get('steps'))),
            medium_velocity: Number(data.get('medium_velocity')),
          },
          'new',
        );
        newForm.reset();
        derive();
        document.getElementById('processing-new').open = false;
        load();
      } catch (error) {
        fail(error.status === 412 ? `There is already a profile with the id '${id}'.` : error.message);
      }
    });
  }

  load();
})();
