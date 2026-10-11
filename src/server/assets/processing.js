/* --- The processing page (#382) --------------------------------------------
 *
 * Every processing profile, the radargrams in it and where each stands:
 * current, stale (and why), processed by an older Ridal, not processed yet,
 * or without raw data. An operator can make and change profiles and put
 * radargrams in them.
 */
(function setupProcessingPage() {
  const host = document.getElementById('processing-profiles');
  if (!host) return;
  const writable = host.dataset.writable === 'true';
  const errorBox = document.getElementById('processing-error');
  const empty = document.getElementById('processing-empty');

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

  const el = (tag, props = {}, ...children) => {
    const node = document.createElement(tag);
    Object.assign(node, props);
    node.append(...children);
    return node;
  };

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

  /** A profile's form, for editing it. Opened with the profile's ETag, so
   * that someone else's change in between is refused, not overwritten. */
  async function editForm(profile) {
    const response = await fetch(RIDAL.apiPath('processing', 'profiles', profile.id));
    const etag = response.headers.get('ETag');
    const steps = el('textarea', { rows: 8, spellcheck: false });
    steps.value = profile.versions[profile.versions.length - 1].steps.join('\n');
    const name = el('input', { value: profile.name, required: true });
    const velocity = el('input', {
      type: 'number',
      step: '0.001',
      min: '0.01',
      value: profile.versions[profile.versions.length - 1].medium_velocity,
    });
    const save = el('button', { type: 'submit', textContent: 'Save' });
    const form = el(
      'form',
      { className: 'processing-edit' },
      el('label', {}, 'Name ', name),
      el('label', { className: 'process-steps' }, 'Steps, one per line', steps),
      el('label', {}, 'Medium velocity (m/ns) ', velocity),
      el('p', {}, save),
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

  function memberRow(member, status) {
    const state = status ? status.state : 'not_processed';
    const badge = el('span', {
      className: `processing-state processing-state-${state}`,
      textContent: STATE_WORDS[state] || state,
    });
    const why = el('span', { className: 'hint' });
    if (status && status.reasons) why.textContent = ` ${status.reasons.join('; ')}`;
    const cells = [
      el(
        'td',
        {},
        el('a', {
          href: RIDAL.pagePath('view', member.id),
          textContent: member.id,
        }),
      ),
      el('td', {}, badge, why),
      el('td', {}, el('code', { textContent: member.pre_steps.join(', ') })),
    ];
    if (writable) {
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
      cells.push(el('td', {}, remove));
    }
    return el('tr', {}, ...cells);
  }

  function addForm(profile, unassigned) {
    const choose = el('select', {});
    for (const id of unassigned) choose.append(el('option', { value: id, textContent: id }));
    const pre = el('input', { placeholder: 'subset(0 2000)' });
    const add = el('button', { type: 'submit', textContent: 'Add' });
    const form = el(
      'form',
      { className: 'processing-add' },
      el('label', {}, 'Add a radargram ', choose),
      el('label', {}, ' its own steps (optional) ', pre),
      add,
    );
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (!choose.value) return;
      add.disabled = true;
      try {
        await send(RIDAL.apiPath('processing', 'assignments', choose.value), 'PUT', {
          profile: profile.id,
          pre_steps: pre.value
            .split(/[;\n]/)
            .map((s) => s.trim())
            .filter((s) => s !== ''),
        });
        load();
      } catch (error) {
        add.disabled = false;
        fail(error.message);
      }
    });
    return form;
  }

  function renderProfile(profile, statuses, assignments, unassigned) {
    const current = profile.versions[profile.versions.length - 1];
    const members = profile.members.map((id) => ({
      id,
      pre_steps: (assignments[id] && assignments[id].pre_steps) || [],
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
      { className: 'processing-profile' },
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
          (members.length ? ` ${members.length} radargram(s): ${summary}.` : ' No radargrams yet.'),
      }),
    );
    if (members.length) {
      const head = ['Radargram', 'State', 'Its own steps'];
      if (writable) head.push('');
      section.append(
        el(
          'div',
          { className: 'table-scroll' },
          el(
            'table',
            { className: 'processing-members' },
            el('thead', {}, el('tr', {}, ...head.map((h) => el('th', { scope: 'col', textContent: h })))),
            el('tbody', {}, ...members.map((m) => memberRow(m, statuses[m.id]))),
          ),
        ),
      );
    }
    if (writable) {
      const actions = el('p', {});
      const edit = el('button', { type: 'button', textContent: 'Change…' });
      edit.addEventListener('click', async () => {
        edit.disabled = true;
        try {
          section.append(await editForm(profile));
        } catch (error) {
          fail(error.message);
        }
      });
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
      if (unassigned.length) section.append(addForm(profile, unassigned));
    }
    return section;
  }

  async function load() {
    errorBox.hidden = true;
    let profiles, status, assignments, datasets;
    try {
      [profiles, status, assignments, datasets] = await Promise.all([
        RIDAL.fetchJson(RIDAL.apiPath('processing', 'profiles')),
        RIDAL.fetchJson(RIDAL.apiPath('processing', 'status')),
        RIDAL.fetchJson(RIDAL.apiPath('processing', 'assignments')),
        RIDAL.fetchJson(RIDAL.apiPath('datasets')),
      ]);
    } catch (error) {
      fail(`Could not load the processing profiles (${error.message}).`);
      return;
    }
    const statuses = {};
    for (const r of status.radargrams) statuses[r.radargram_id] = r;
    const assigned = assignments.radargrams || {};
    const unassigned = (datasets.entries || [])
      .map((d) => d.radargram_id)
      .filter((id) => id && !assigned[id]);
    empty.hidden = profiles.profiles.length > 0;
    host.replaceChildren(
      ...profiles.profiles.map((p) => renderProfile(p, statuses, assigned, unassigned)),
    );
  }

  const newForm = document.getElementById('processing-new-form');
  if (newForm) {
    newForm.addEventListener('submit', async (event) => {
      event.preventDefault();
      const data = new FormData(newForm);
      try {
        await send(
          RIDAL.apiPath('processing', 'profiles', String(data.get('id'))),
          'PUT',
          {
            name: String(data.get('name')),
            steps: linesOf(String(data.get('steps'))),
            medium_velocity: Number(data.get('medium_velocity')),
          },
          'new',
        );
        newForm.reset();
        document.getElementById('processing-new').open = false;
        load();
      } catch (error) {
        fail(
          error.status === 412
            ? `There is already a profile called '${data.get('id')}'.`
            : error.message,
        );
      }
    });
  }

  load();
})();
