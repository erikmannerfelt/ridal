/* --- Adding raw data in bulk (#382 phase 7) -------------------------------
 *
 * Many recordings in, the radargrams they would make out. Files are grouped
 * into recordings by name and uploaded one at a time; every change (an
 * upload, a threshold, a join, a split, a pattern, a typed id) sends the
 * whole plan to `POST /ingest/plan`, which keeps nothing between calls.
 * Creating is the existing routes, one output at a time, so each refusal
 * lands on its output: `PUT /sources/{id}` with `If-None-Match: *`, then
 * the profile assignment, then one batch.
 *
 * Recordings and outputs are named by key, the blake3 of the header file,
 * which is what the plan's `excluded`, `joins`, `splits`, `ids` and
 * `display_names` take, so an edit sticks to its recording however the
 * plan around it changes.
 */
(function setupIngestPage() {
  const drop = document.getElementById('ingest-drop');
  if (!drop) return;
  const $ = (id) => document.getElementById(id);
  const errorBox = $('ingest-error');
  const input = $('ingest-input');
  const uploadLine = $('ingest-upload');
  const cancelUpload = $('ingest-cancel');
  const threshold = $('ingest-threshold');
  const never = $('ingest-never');
  const summary = $('ingest-summary');
  const timeline = $('ingest-timeline');
  const selectedBox = $('ingest-selected');
  const leftOut = $('ingest-left-out');
  const idPattern = $('ingest-id-pattern');
  const namePattern = $('ingest-name-pattern');
  const groupChoice = $('ingest-group');
  const groupName = $('ingest-group-name');
  const groupId = $('ingest-group-id');
  const outputRows = $('ingest-output-rows');
  const antennaRows = $('ingest-antenna-rows');
  const profileBox = $('ingest-profile');
  const newProfile = $('ingest-new-profile');
  const demBox = $('ingest-dem');
  const crsBox = $('ingest-crs');
  const createButton = $('ingest-create-button');
  const blockers = $('ingest-blockers');
  const progressList = $('ingest-progress');
  const doneBox = $('ingest-done');

  const fail = (message) => {
    RIDAL.setMessage(errorBox, message);
    errorBox.hidden = false;
  };
  const el = (tag, props = {}, ...children) => {
    const node = document.createElement(tag);
    Object.assign(node, props);
    node.append(...children);
    return node;
  };
  const SVG = 'http://www.w3.org/2000/svg';
  const svg = (tag, attrs = {}, ...children) => {
    const node = document.createElementNS(SVG, tag);
    for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
    node.append(...children);
    return node;
  };
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

  /** Readable on light and dark basemaps alike, and told apart from their
   * neighbours: an output's colour on the timeline and the map. */
  const COLOURS = ['#1f77b4', '#d95f02', '#1b9e77', '#7570b3', '#e7298a', '#66a61e', '#a6761d', '#e6ab02'];
  const colourOf = (output) => COLOURS[output % COLOURS.length];

  function duration(seconds) {
    const s = Math.round(Math.abs(seconds));
    if (s < 60) return `${s} s`;
    if (s < 3600) return `${Math.round(s / 60)} min`;
    if (s < 86400) {
      const h = Math.floor(s / 3600);
      const m = Math.round((s % 3600) / 60);
      return m ? `${h} h ${m} min` : `${h} h`;
    }
    const d = Math.floor(s / 86400);
    const h = Math.round((s % 86400) / 3600);
    return h ? `${d} d ${h} h` : `${d} d`;
  }
  const when = (rfc3339) => (rfc3339 ? `${rfc3339.slice(0, 16).replace('T', ' ')} UTC` : '');
  const seconds = (rfc3339) => Date.parse(rfc3339) / 1000;

  // --- Files and uploads ----------------------------------------------------

  /** Recordings by stem, in the order they were first seen: the files the
   * person gave, and what the upload answered once every one is stored. */
  const byStem = new Map();
  /** A file already stored, by what the browser knows of it, so adding
   * the `.cor` a recording lacked does not upload its `.rd3` again. */
  const stored = new Map();
  const fileKey = (f) => `${f.name}\u0000${f.size}\u0000${f.lastModified}`;
  const stemOf = (name) => {
    const dot = name.lastIndexOf('.');
    return dot > 0 ? name.slice(0, dot) : name;
  };
  let uploading = null;
  let stopped = false;
  let queue = Promise.resolve();
  let created = false;

  function addFiles(list) {
    if (created) return;
    errorBox.hidden = true;
    const touched = new Set();
    for (const file of list) {
      const stem = stemOf(file.name);
      if (!byStem.has(stem)) byStem.set(stem, { stem, files: [], stored: null });
      const entry = byStem.get(stem);
      // The same name again replaces it: a corrected `.cor`, say.
      entry.files = entry.files.filter((f) => f.name !== file.name);
      entry.files.push(file);
      entry.stored = null;
      touched.add(entry);
    }
    if (touched.size) {
      stopped = false;
      queue = queue.then(() => uploadAll([...touched]));
    }
  }

  async function uploadAll(entries) {
    const files = entries.flatMap((e) => e.files.filter((f) => !stored.has(fileKey(f))));
    let done = 0;
    cancelUpload.hidden = files.length === 0;
    for (const entry of entries) {
      if (stopped) break;
      const named = [];
      for (const file of entry.files) {
        const known = stored.get(fileKey(file));
        if (known) {
          named.push(known);
          continue;
        }
        done += 1;
        const prefix = `Uploading ${done} of ${files.length}: ${file.name}`;
        uploadLine.textContent = `${prefix}…`;
        let last = -1;
        uploading = RIDAL.uploadFile(
          `${RIDAL.apiPath('raw')}?filename=${encodeURIComponent(file.name)}`,
          file,
          (loaded, size) => {
            const percent = Math.round((loaded / size) * 100);
            if (percent === last) return;
            last = percent;
            uploadLine.textContent = `${prefix}… ${percent}%`;
          },
        );
        let response;
        try {
          response = await uploading.promise;
        } catch (error) {
          uploadLine.textContent = '';
          if (error.message !== 'upload cancelled') fail(`Could not upload ${file.name} (${error.message}).`);
          stopped = true;
          break;
        }
        if (!response.ok) {
          uploadLine.textContent = '';
          fail(response.json?.error?.message || RIDAL.upstreamMessage(response.status));
          stopped = true;
          break;
        }
        const answer = { name: response.json.name, blake3: response.json.blake3 };
        stored.set(fileKey(file), answer);
        named.push(answer);
      }
      if (named.length === entry.files.length) {
        entry.stored = named;
        // Planned as soon as it is in, so the plan grows with the upload.
        wantTracks = true;
        schedulePlan();
      }
    }
    uploading = null;
    cancelUpload.hidden = true;
    const waiting = [...byStem.values()].filter((e) => !e.stored).length;
    uploadLine.textContent = stopped && waiting
      ? `${plural(waiting, 'recording is', 'recordings are')} not uploaded. Add the files again to go on.`
      : `${plural([...byStem.values()].length, 'recording', 'recordings')} uploaded.`;
    updateCreate();
  }

  cancelUpload.addEventListener('click', () => {
    stopped = true;
    uploading?.abort();
  });
  $('ingest-choose').addEventListener('click', (event) => {
    event.stopPropagation();
    input.click();
  });
  drop.addEventListener('click', () => input.click());
  drop.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      input.click();
    }
  });
  input.addEventListener('change', () => {
    addFiles([...input.files]);
    input.value = '';
  });
  drop.addEventListener('dragover', (event) => {
    event.preventDefault();
    drop.classList.add('ingest-drop-over');
  });
  drop.addEventListener('dragleave', () => drop.classList.remove('ingest-drop-over'));
  drop.addEventListener('drop', (event) => {
    event.preventDefault();
    drop.classList.remove('ingest-drop-over');
    addFiles([...event.dataTransfer.files]);
  });
  window.addEventListener('beforeunload', (event) => {
    if (uploading || creating) event.preventDefault();
  });

  // --- The plan ---------------------------------------------------------------

  const edits = {
    excluded: new Set(),
    joins: new Set(),
    splits: new Set(),
    ids: {},
    displayNames: {},
    // By antenna as the headers name it: `{mhz, separation}`, the text
    // typed, so a box being typed in is not rewritten under the cursor.
    antennas: {},
  };
  /** Each recording's track, by key: asked for only when a recording is
   * new, and kept, since a track does not change with the plan. */
  const tracks = new Map();
  let wantTracks = true;
  let plan = null;
  let selected = null;
  let planning = false;
  let again = false;
  let timer = null;

  const uploaded = () => [...byStem.values()].filter((e) => e.stored);

  /** The catalog's groups and those planned raw data names, id to name. */
  const groups = new Map();
  const GROUP = 'group:';

  /** The group every output goes in, `{name, id}`, or `null` for none. A
   * new group's id is left to the server to derive when it is not typed. */
  function group() {
    const value = groupChoice.value;
    if (value.startsWith(GROUP)) {
      const id = value.slice(GROUP.length);
      return { name: groups.get(id), id };
    }
    const name = value === 'new' ? groupName.value.trim() : '';
    return name ? { name, id: groupId.value.trim() || null } : null;
  }

  function request() {
    const minutes = Number(threshold.value);
    const text = (box) => box.value.trim() || null;
    return {
      recordings: uploaded().map((e) => ({ files: e.stored })),
      merge_threshold_seconds: never.checked || !Number.isFinite(minutes) ? null : minutes * 60,
      excluded: [...edits.excluded],
      joins: [...edits.joins],
      splits: [...edits.splits],
      id_pattern: text(idPattern),
      display_name_pattern: text(namePattern),
      ids: edits.ids,
      display_names: edits.displayNames,
      group_name: group()?.name ?? null,
      group_id: group()?.id ?? null,
      crs: text(crsBox),
      dem: demBox.value || null,
      antennas: antennaOverrides(),
      include_tracks: wantTracks,
    };
  }

  /** A typed override as the plan takes it: a positive number, else none. */
  const overrideValue = (text) => {
    const value = Number(text);
    return text.trim() !== '' && Number.isFinite(value) && value > 0 ? value : null;
  };
  function antennaOverrides() {
    const out = {};
    for (const [antenna, typed] of Object.entries(edits.antennas)) {
      const mhz = overrideValue(typed.mhz || '');
      const separation = overrideValue(typed.separation || '');
      if (mhz !== null || separation !== null) out[antenna] = { mhz, separation };
    }
    return out;
  }

  function schedulePlan() {
    if (created) return;
    clearTimeout(timer);
    timer = setTimeout(runPlan, 250);
  }

  /** One plan at a time; edits made meanwhile are planned once it ends. */
  async function runPlan() {
    if (planning) {
      again = true;
      return;
    }
    if (uploaded().length === 0) return;
    planning = true;
    const asked = request();
    summary.textContent = 'Planning…';
    try {
      const answer = await RIDAL.fetchJson(RIDAL.apiPath('ingest', 'plan'), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(asked),
      });
      if (asked.include_tracks) {
        for (const r of answer.recordings) if (r.track) tracks.set(r.key, r.track);
        wantTracks = false;
      }
      if (!created) {
        plan = answer;
        errorBox.hidden = true;
        render();
      }
    } catch (error) {
      summary.textContent = '';
      fail(error.message);
    } finally {
      planning = false;
      if (again) {
        again = false;
        runPlan();
      }
    }
  }

  const problemsOf = (pick) => (plan ? plan.problems.filter(pick) : []);

  function render() {
    for (const id of ['ingest-plan', 'ingest-names', 'ingest-processing', 'ingest-create']) {
      $(id).hidden = false;
    }
    const outputs = plan.outputs.length;
    const used = plan.recordings.filter((r) => r.output !== null).length;
    summary.textContent =
      `${plural(outputs, 'radargram', 'radargrams')} from ` +
      `${plural(used, 'recording', 'recordings')}` +
      (plan.crs ? `, positions in ${plan.crs}.` : ', positions in the UTM zone of each track.');
    // Joins and splits outlast a change of threshold, never merge
    // included, so say they are there and offer to drop them.
    const made = edits.joins.size + edits.splits.size;
    if (made) {
      const undo = el('button', { type: 'button', textContent: 'Undo them' });
      undo.addEventListener('click', () => {
        edits.joins.clear();
        edits.splits.clear();
        schedulePlan();
      });
      const yours = [
        ...(edits.joins.size ? [plural(edits.joins.size, 'join', 'joins')] : []),
        ...(edits.splits.size ? [plural(edits.splits.size, 'split', 'splits')] : []),
      ].join(' and ');
      summary.append(` Your ${yours} ${made === 1 ? 'applies' : 'apply'} whatever the threshold. `, undo);
    }
    renderAntennas();
    renderTimeline();
    renderSelected();
    renderLeftOut();
    renderMap();
    renderPatternProblem(idPattern, $('ingest-id-pattern-problem'), 'id_pattern');
    renderPatternProblem(namePattern, $('ingest-name-pattern-problem'), 'display_name_pattern');
    renderNames();
    updateCreate();
  }

  // --- The antennas -------------------------------------------------------

  /** What the files say of each antenna, and the boxes to say otherwise.
   * Rebuilt from the plan, keeping a box being typed in, as the names are. */
  function renderAntennas() {
    $('ingest-antennas').hidden = plan.antennas.length === 0;
    const active = document.activeElement;
    const focus = antennaRows.contains(active)
      ? { antenna: active.dataset.antenna, field: active.dataset.field, value: active.value }
      : null;
    const metres = (v) => `${Number(v.toFixed(3))} m`;
    antennaRows.replaceChildren(
      ...plan.antennas.map((a) => {
        const separation =
          a.separation_min === null
            ? ''
            : a.separation_min === a.separation_max
              ? `, ${metres(a.separation_min)}`
              : `, ${metres(a.separation_min)} to ${metres(a.separation_max)}`;
        const readAs = a.antenna_mhz === null
          ? el('td', { className: 'ingest-problem', textContent: `no frequency${separation}` })
          : el('td', { textContent: `${a.antenna_mhz} MHz${separation}` });
        const box = (field, label) => {
          const typed = edits.antennas[a.antenna]?.[field];
          const given = field === 'mhz' ? a.mhz_override : a.separation_override;
          const input = el('input', {
            type: 'number',
            min: '0',
            step: 'any',
            value: typed ?? (given === null ? '' : String(given)),
            placeholder: 'From the files',
          });
          input.dataset.antenna = a.antenna;
          input.dataset.field = field;
          input.setAttribute('aria-label', `${label} for ${a.antenna}`);
          input.addEventListener('input', () => {
            edits.antennas[a.antenna] = { ...edits.antennas[a.antenna], [field]: input.value };
            input.setCustomValidity(
              input.value.trim() === '' || overrideValue(input.value) !== null
                ? ''
                : 'A positive number, or empty to read it from the files.',
            );
            wantTracks = true;
            schedulePlan();
          });
          return el('td', {}, input);
        };
        return el(
          'tr',
          {},
          el('td', {}, el('code', { textContent: a.antenna || '(none)' })),
          readAs,
          box('mhz', 'Frequency override (MHz)'),
          box('separation', 'Separation override (m)'),
          el('td', { textContent: String(a.recordings) }),
        );
      }),
    );
    if (focus) {
      const box = antennaRows.querySelector(
        `input[data-antenna="${CSS.escape(focus.antenna)}"][data-field="${focus.field}"]`,
      );
      if (box) {
        box.value = focus.value;
        box.focus();
      }
    }
  }

  // --- The timeline -------------------------------------------------------

  const ROW = 34;
  const LABEL = 170;
  const CHIP = 76;
  const recordingByKey = () => new Map(plan.recordings.map((r) => [r.key, r]));

  /** The split before an output, as a chip: why it is a radargram of its
   * own, and a click to join it to the one before. */
  function splitChip(x, y, output) {
    const split = output.split;
    const first = output.recordings[0];
    const kind = split.manual ? 'manual' : split.incompatible ? 'incompatible' : 'gap';
    const why = split.manual
      ? 'split here by you'
      : split.incompatible
        ? split.incompatible
        : `more than the threshold apart`;
    const label = `${duration(split.gap_seconds)} after the one before: ${why}. ` +
      (split.incompatible ? 'Cannot be joined.' : 'Click to join it to the radargram before.');
    const chip = svg(
      'g',
      { class: `ingest-chip ingest-chip-${kind}`, tabindex: split.incompatible ? -1 : 0, role: 'button' },
      svg('title', {}, label),
      svg('rect', { x, y: y + 6, width: CHIP - 6, height: ROW - 12, rx: 4 }),
      svg('text', { x: x + (CHIP - 6) / 2, y: y + ROW / 2 + 4, 'text-anchor': 'middle' },
        split.incompatible ? '≠' : duration(split.gap_seconds)),
    );
    chip.setAttribute('aria-label', label);
    if (!split.incompatible) onActivate(chip, () => join(first));
    return chip;
  }

  function onActivate(node, action) {
    node.addEventListener('click', action);
    node.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        action();
      }
    });
  }

  function renderTimeline() {
    const recordings = recordingByKey();
    const width = Math.max(timeline.parentElement.clientWidth, 480);
    // A shared scale, so lengths compare across rows; a gap is a chip of
    // its own rather than its length, which may be days.
    const spans = plan.outputs.map((o) =>
      o.recordings.reduce((sum, key) => {
        const r = recordings.get(key);
        return sum + Math.max(seconds(r.stop_time) - seconds(r.start_time), 1);
      }, 0),
    );
    // What a row takes besides its bars: its chips, and 2 px after each bar.
    const fixed = Math.max(
      0,
      ...plan.outputs.map((o) => (o.recordings.length - 1 + (o.split ? 1 : 0)) * CHIP + o.recordings.length * 2),
    );
    const room = Math.max(width - LABEL - fixed - 8, 120);
    const scale = room / Math.max(...spans, 1);
    const rows = [];
    let widest = width;
    plan.outputs.forEach((output, index) => {
      const y = index * ROW;
      const label = output.radargram_id || '(no valid id)';
      const row = [
        svg('text', { x: 4, y: y + ROW / 2 + 4, class: 'ingest-row-label' },
          svg('title', {}, label), `${index + 1}. ${label.length > 18 ? `${label.slice(0, 17)}…` : label}`),
      ];
      let x = LABEL;
      if (output.split) {
        row.push(splitChip(x, y, output));
        x += CHIP;
      }
      output.recordings.forEach((key, i) => {
        const r = recordings.get(key);
        if (i > 0) {
          const before = recordings.get(output.recordings[i - 1]);
          const gap = seconds(r.start_time) - seconds(before.stop_time);
          // A join of yours holds whatever the threshold, even with never
          // merge, so it must not look like a merge the threshold made.
          const joined = edits.joins.has(key);
          const text = joined
            ? `${duration(gap)} gap, joined by you. Click to undo the join.`
            : `${duration(gap)} gap, merged. Click to start a new radargram at ${r.name}.`;
          const chip = svg(
            'g',
            {
              class: `ingest-chip ingest-chip-merged${joined ? ' ingest-chip-joined' : ''}`,
              tabindex: 0,
              role: 'button',
              'aria-label': text,
            },
            svg('title', {}, text),
            svg('line', { x1: x, x2: x + CHIP - 6, y1: y + ROW / 2, y2: y + ROW / 2 }),
            svg('rect', { x, y: y + 8, width: CHIP - 6, height: ROW - 16, rx: 4, class: 'ingest-hit' }),
            svg('text', { x: x + (CHIP - 6) / 2, y: y + ROW / 2 - 3, 'text-anchor': 'middle' },
              duration(gap)),
          );
          onActivate(chip, () => split(key));
          row.push(chip);
          x += CHIP;
        }
        const w = Math.max((seconds(r.stop_time) - seconds(r.start_time)) * scale, 8);
        const bar = svg(
          'rect',
          {
            x,
            y: y + 7,
            width: w,
            height: ROW - 14,
            rx: 3,
            fill: colourOf(index),
            class: `ingest-bar${selected === key ? ' ingest-bar-selected' : ''}`,
            tabindex: 0,
            role: 'button',
            'data-key': key,
            'aria-label': `${r.name}, ${when(r.start_time)}, ${r.antenna_mhz} MHz`,
          },
          svg('title', {}, `${r.name}\n${when(r.start_time)} to ${when(r.stop_time)}\n${r.antenna_mhz} MHz, ${r.traces} traces`),
        );
        onActivate(bar, () => select(key));
        bar.addEventListener('mouseenter', () => highlight(key, true));
        bar.addEventListener('mouseleave', () => highlight(key, false));
        row.push(bar);
        x += w + 2;
      });
      widest = Math.max(widest, x + 8);
      rows.push(svg('g', {}, ...row));
    });
    timeline.setAttribute('width', widest);
    timeline.setAttribute('height', Math.max(plan.outputs.length * ROW, ROW));
    timeline.replaceChildren(...rows);
  }

  function select(key) {
    selected = selected === key ? null : key;
    renderTimeline();
    renderSelected();
    renderMap();
  }

  /** Start a new radargram at `key`, which is inside one now. */
  function split(key) {
    if (edits.joins.has(key)) edits.joins.delete(key);
    else edits.splits.add(key);
    schedulePlan();
  }

  /** Keep `key`, which starts a radargram now, with the one before. */
  function join(key) {
    if (edits.splits.has(key)) edits.splits.delete(key);
    else edits.joins.add(key);
    schedulePlan();
  }

  function exclude(key, out) {
    if (out) edits.excluded.add(key);
    else edits.excluded.delete(key);
    if (out && selected === key) selected = null;
    schedulePlan();
  }

  /** When a recording was made: one date if it fits in one. */
  function span(start, stop) {
    const from = when(start);
    const to = when(stop);
    const length = duration(seconds(stop) - seconds(start));
    if (from.slice(0, 10) === to.slice(0, 10)) {
      return `${from.slice(0, 10)}, ${from.slice(11, 16)} to ${to.slice(11)} (${length})`;
    }
    return `${from} to ${to} (${length})`;
  }

  function renderSelected() {
    const r = selected && recordingByKey().get(selected);
    if (!r || r.output === null) {
      selectedBox.hidden = true;
      return;
    }
    const output = plan.outputs[r.output];
    const next = plan.outputs[r.output + 1];
    const first = output.recordings[0] === r.key;
    const last = output.recordings.at(-1) === r.key;
    const actions = [];
    const button = (text, action, disabled = false, title = '') => {
      const b = el('button', { type: 'button', textContent: text, disabled, title });
      b.addEventListener('click', action);
      actions.push(b);
    };
    if (first && output.split) {
      const why = output.split.incompatible;
      button(
        edits.splits.has(r.key) ? 'Undo the split before it' : `Join with radargram ${r.output} before`,
        () => join(r.key),
        Boolean(why),
        why ? `Cannot be joined: ${why}` : '',
      );
    } else if (!first) {
      button(
        edits.joins.has(r.key) ? 'Undo the join' : 'Start a new radargram here',
        () => split(r.key),
      );
    }
    // Joining the next radargram to this one: the same edit as that
    // radargram's own "join with the one before".
    if (last && next) {
      const after = next.recordings[0];
      const why = next.split.incompatible;
      button(
        edits.splits.has(after) ? 'Undo the split after it' : `Join with radargram ${r.output + 2} after`,
        () => join(after),
        Boolean(why),
        why ? `Cannot be joined: ${why}` : '',
      );
    }
    button("Don't use", () => exclude(r.key, true));
    const problems = problemsOf((p) => p.recording === r.key);
    const fact = (term, ...value) => [el('dt', { textContent: term }), el('dd', {}, ...value)];
    selectedBox.style.setProperty('--ingest-output-colour', colourOf(r.output));
    selectedBox.replaceChildren(
      el('p', { className: 'ingest-selected-title' },
        el('strong', { textContent: r.name }),
        el('span', { className: 'hint', textContent: `in radargram ${r.output + 1}${output.radargram_id ? `, ${output.radargram_id}` : ''}` })),
      el('dl', { className: 'ingest-facts' },
        ...fact('Recorded', span(r.start_time, r.stop_time)),
        ...fact('Antenna', `${r.antenna_mhz} MHz (${r.antenna})`),
        ...fact('Traces', r.traces.toLocaleString('en')),
        ...fact('Files', ...r.files.map((f) => el('code', { textContent: f })))),
      ...problems.map((p) => el('p', { className: 'ingest-problem', textContent: p.message })),
      el('p', { className: 'ingest-actions' }, ...actions),
    );
    selectedBox.hidden = false;
  }

  /** What is not in any radargram: not used, or not readable. */
  function renderLeftOut() {
    const rows = plan.recordings
      .filter((r) => r.output === null)
      .map((r) => {
        const action = el('button', { type: 'button', textContent: r.excluded ? 'Use it' : "Don't use" });
        action.addEventListener('click', () => exclude(r.key, !r.excluded));
        // An antenna that gives no frequency is read once one is given
        // for it in the table above.
        const why = r.unreadable && r.antenna !== null
          ? `needs a frequency for its antenna, '${r.antenna}', above`
          : r.unreadable ? `cannot be read: ${r.unreadable}` : 'not used';
        return el('li', { className: r.unreadable && !r.excluded ? 'ingest-problem' : '' },
          el('code', { textContent: r.name }), ` ${why}${r.unreadable && r.excluded ? ' (not used)' : ''}. `, action);
      });
    leftOut.replaceChildren(
      ...(rows.length ? [el('p', { textContent: 'Not in any radargram:' }), el('ul', {}, ...rows)] : []),
    );
  }

  // --- The map --------------------------------------------------------------

  let map = null;
  const lines = new Map();
  let fitted = 0;

  function renderMap() {
    const host = $('ingest-map');
    if (!map) map = RIDAL.basemap(L.map(host.id), host.id);
    for (const line of lines.values()) line.remove();
    lines.clear();
    const bounds = [];
    for (const r of plan.recordings) {
      const track = tracks.get(r.key);
      if (!track || track.length < 2) continue;
      const latlngs = track.map(([lon, lat]) => [lat, lon]);
      bounds.push(...latlngs);
      const line = L.polyline(latlngs, {
        color: r.output === null ? '#888888' : colourOf(r.output),
        weight: r.key === selected ? 6 : 3,
        dashArray: r.output === null ? '4 4' : null,
        opacity: 0.9,
      }).addTo(map);
      line.bindTooltip(`${r.name}${r.output === null ? ' (not in any radargram)' : ` in radargram ${r.output + 1}`}`, { sticky: true });
      line.on('click', () => select(r.key));
      line.on('mouseover', () => highlight(r.key, true));
      line.on('mouseout', () => highlight(r.key, false));
      lines.set(r.key, line);
    }
    // Fitted again only when there is more to see, not on every edit.
    if (bounds.length && lines.size !== fitted) {
      map.fitBounds(bounds, { padding: [16, 16] });
      fitted = lines.size;
    }
  }

  function highlight(key, on) {
    lines.get(key)?.setStyle({ weight: on || key === selected ? 6 : 3 });
    timeline.querySelector(`rect[data-key="${CSS.escape(key)}"]`)?.classList.toggle('ingest-bar-hover', on);
  }

  // --- Names ----------------------------------------------------------------

  /** A pattern's problem, with the part it is about marked. */
  function renderPatternProblem(box, host, which) {
    const problems = problemsOf((p) => p.pattern === which);
    host.hidden = problems.length === 0;
    host.replaceChildren(
      ...problems.map((p) => {
        const out = el('p', { className: 'ingest-problem' }, p.message);
        if (p.span) {
          const chars = [...box.value];
          const [start, end] = p.span;
          out.append(
            el('br'),
            el('code', {},
              chars.slice(0, start).join(''),
              el('mark', { textContent: chars.slice(start, Math.max(end, start + 1)).join('') || ' ' }),
              chars.slice(Math.max(end, start + 1)).join('')),
          );
        }
        return out;
      }),
    );
    box.setAttribute('aria-invalid', String(problems.length > 0));
  }

  function nameInput(output, field, overrides, value, placeholder) {
    const box = el('input', { type: 'text', value: value || '', placeholder, spellcheck: false });
    box.dataset.key = output.key;
    box.dataset.field = field;
    box.addEventListener('input', () => {
      overrides[output.key] = box.value;
      schedulePlan();
    });
    const typed = Object.hasOwn(overrides, output.key);
    const cell = el('td', {}, box);
    if (typed) {
      const reset = el('button', {
        type: 'button',
        className: 'ingest-reset',
        textContent: '↺',
        title: 'Back to what the pattern gives',
      });
      reset.setAttribute('aria-label', 'Back to what the pattern gives');
      reset.addEventListener('click', () => {
        delete overrides[output.key];
        schedulePlan();
      });
      cell.append(' ', reset);
    }
    return cell;
  }

  function renderNames() {
    // Rebuilt from the plan, but a box being typed in keeps what it holds:
    // the plan answering may be one keystroke behind it.
    const active = document.activeElement;
    const focus = outputRows.contains(active)
      ? { key: active.dataset.key, field: active.dataset.field, value: active.value, at: active.selectionStart }
      : null;
    const recordings = recordingByKey();
    outputRows.replaceChildren(
      ...plan.outputs.map((output, index) => {
        const first = recordings.get(output.key);
        const problems = problemsOf((p) => p.output === index);
        const idCell = nameInput(output, 'id', edits.ids, output.radargram_id, 'An id');
        idCell.append(...problems.map((p) => el('p', { className: 'ingest-problem', textContent: p.message })));
        const row = el(
          'tr',
          { className: problems.length ? 'ingest-row-problem' : '' },
          el('td', {}, el('span', { className: 'ingest-swatch', style: `background: ${colourOf(index)}` }), ` ${index + 1}`),
          el('td', {}, `${first.name}${output.recordings.length > 1 ? ` and ${output.recordings.length - 1} more` : ''}`),
          el('td', { textContent: when(output.start_time) }),
          el('td', { textContent: `${Math.round(output.antenna_mhz)} MHz` }),
          idCell,
          nameInput(output, 'name', edits.displayNames, output.display_name, 'None'),
        );
        return row;
      }),
    );
    if (focus) {
      const box = outputRows.querySelector(
        `input[data-key="${CSS.escape(focus.key)}"][data-field="${focus.field}"]`,
      );
      if (box) {
        box.value = focus.value;
        box.focus();
        box.setSelectionRange(focus.at, focus.at);
      }
    }
  }

  for (const box of [idPattern, namePattern, groupName, groupId, crsBox]) {
    box.addEventListener('input', schedulePlan);
  }
  groupChoice.addEventListener('change', () => {
    $('ingest-group-new').hidden = groupChoice.value !== 'new';
    schedulePlan();
    updateCreate();
  });
  groupName.addEventListener('input', () => {
    groupId.placeholder = RIDAL.slugify(groupName.value) || 'dronbreen-2026';
  });
  threshold.addEventListener('input', schedulePlan);
  never.addEventListener('change', () => {
    threshold.disabled = never.checked;
    schedulePlan();
  });
  demBox.addEventListener('change', () => {
    wantTracks = true;
    schedulePlan();
  });

  // --- Processing -------------------------------------------------------------

  const choice = () => document.querySelector('input[name="ingest-when"]:checked').value;
  for (const radio of document.querySelectorAll('input[name="ingest-when"]')) {
    radio.addEventListener('change', () => {
      $('ingest-profile-field').hidden = choice() === 'none';
      updateCreate();
    });
  }
  profileBox.addEventListener('change', () => {
    newProfile.hidden = profileBox.value !== 'new';
    updateCreate();
  });
  $('ingest-profile-name').addEventListener('input', updateCreate);

  (async function loadChoices() {
    try {
      const [profiles, dems, settings, datasets, sources] = await Promise.all([
        RIDAL.fetchJson(RIDAL.apiPath('processing', 'profiles')),
        RIDAL.fetchJson(RIDAL.apiPath('dems')),
        RIDAL.fetchJson(RIDAL.apiPath('settings')),
        RIDAL.fetchJson(RIDAL.apiPath('datasets')),
        RIDAL.fetchJson(RIDAL.apiPath('sources')),
      ]);
      // The catalog's groups, and those of raw data not processed yet,
      // which are groups to be once it is.
      for (const entry of datasets.entries) {
        if (entry.group_id) groups.set(entry.group_id, entry.group_name || entry.group_id);
      }
      for (const names of Object.values(sources.names || {})) {
        if (names.group_id && !groups.has(names.group_id)) {
          groups.set(names.group_id, names.group_name || names.group_id);
        }
      }
      const newGroup = groupChoice.querySelector('option[value="new"]');
      for (const [id, name] of [...groups].sort((a, b) => a[1].localeCompare(b[1]))) {
        groupChoice.insertBefore(new Option(name === id ? id : `${name} (${id})`, `${GROUP}${id}`), newGroup);
      }
      const before = profileBox.querySelector('option[value="new"]');
      for (const p of profiles.profiles) {
        profileBox.insertBefore(new Option(`${p.name} (${p.id})`, p.id), before);
      }
      for (const dem of dems.dems) demBox.append(new Option(`${dem.name} (${dem.id})`, dem.id));
      if (settings.processing_crs) crsBox.placeholder = `The project's: ${settings.processing_crs}`;
    } catch (error) {
      fail(`Could not load the processing profiles and DEMs (${error.message}).`);
    }
  })();

  // --- Creating ---------------------------------------------------------------

  let creating = false;

  /** Why Create cannot be pressed yet, each a sentence. */
  function reasons() {
    const out = [];
    if (!plan || plan.outputs.length === 0) out.push('There is nothing to create yet.');
    const waiting = [...byStem.values()].filter((e) => !e.stored).length;
    if (waiting) out.push(`${plural(waiting, 'recording is', 'recordings are')} still uploading.`);
    if (plan) {
      for (const p of plan.problems) out.push(p.message);
    }
    if (groupChoice.value === 'new' && !groupName.value.trim()) {
      out.push('Give the new group a name, or choose "No group".');
    }
    if (choice() !== 'none') {
      if (!profileBox.value) out.push('Choose a processing profile, or "No profile".');
      if (profileBox.value === 'new' && !RIDAL.slugify($('ingest-profile-name').value)) {
        out.push('Give the new profile a name.');
      }
    }
    return out;
  }

  function updateCreate() {
    if (created) return;
    const why = reasons();
    createButton.disabled = creating || why.length > 0;
    const n = plan ? plan.outputs.length : 0;
    createButton.textContent =
      choice() === 'now'
        ? `Create ${plural(n, "radargram's raw data", "radargrams' raw data")} and process them`
        : `Create ${plural(n, "radargram's raw data", "radargrams' raw data")}`;
    blockers.replaceChildren(
      ...(why.length && plan
        ? [el('ul', {}, ...why.slice(0, 8).map((w) => el('li', { textContent: w })),
          ...(why.length > 8 ? [el('li', { textContent: `and ${why.length - 8} more` })] : []))]
        : []),
    );
  }

  const send = (url, method, body, headers = {}) =>
    RIDAL.fetchJson(url, {
      method,
      headers: { 'Content-Type': 'application/json', ...headers },
      body: JSON.stringify(body),
    });

  createButton.addEventListener('click', async () => {
    if (createButton.disabled) return;
    creating = true;
    created = true;
    updateCreate();
    createButton.disabled = true;
    for (const node of document.querySelectorAll('#ingest-files, #ingest-plan, #ingest-names, #ingest-processing')) {
      for (const control of node.querySelectorAll('input, select, textarea, button')) control.disabled = true;
    }
    drop.classList.add('ingest-drop-off');
    blockers.replaceChildren();
    const mode = choice();
    // Each recording's files, by its key, from the upload's answers.
    const byName = new Map();
    for (const entry of byStem.values()) for (const f of entry.stored || []) byName.set(f.name, entry.stored);
    const filesOf = new Map(plan.recordings.map((r) => [r.key, byName.get(r.name)]));
    const into = group();
    const crs = crsBox.value.trim() || null;
    const dem = demBox.value || null;
    const step = (text) => {
      const item = el('li', { textContent: text });
      progressList.append(item);
      return item;
    };
    const made = [];
    try {
      let profile = mode === 'none' ? null : profileBox.value;
      if (profile === 'new') {
        const name = $('ingest-profile-name').value.trim();
        profile = RIDAL.slugify(name);
        const item = step(`Profile ${name}…`);
        await send(
          RIDAL.apiPath('processing', 'profiles', profile),
          'PUT',
          {
            name,
            steps: $('ingest-profile-steps').value.split('\n').map((l) => l.trim()).filter((l) => l && !l.startsWith('#')),
            medium_velocity: Number($('ingest-profile-velocity').value),
          },
          { 'If-None-Match': '*' },
        ).catch((error) => {
          throw new Error(error.status === 412 ? `There is already a profile with the id '${profile}'.` : error.message);
        });
        item.textContent = `Profile ${name}: created.`;
      }
      for (const output of plan.outputs) {
        const id = output.radargram_id;
        const item = step(`${id}: storing its raw data…`);
        try {
          // Refused, not replaced, if the id got raw data since the plan.
          await send(
            RIDAL.apiPath('sources', id),
            'PUT',
            {
              recordings: output.recordings.map((key) => filesOf.get(key)),
              crs,
              dem,
              antenna_mhz: output.antenna_mhz_override,
              antenna_separation: output.antenna_separation_override,
              names: {
                display_name: output.display_name,
                group_name: into?.name ?? null,
                group_id: into?.id ?? null,
              },
            },
            { 'If-None-Match': '*' },
          );
          if (profile) {
            await send(RIDAL.apiPath('processing', 'assignments', id), 'PUT', { profile });
          }
        } catch (error) {
          item.className = 'ingest-problem';
          item.textContent =
            error.status === 412 ? `${id}: someone has added raw data under this id since the plan.` : `${id}: ${error.message}`;
          throw new Error('stopped');
        }
        item.textContent = profile ? `${id}: stored, in ${profile}.` : `${id}: stored.`;
        made.push(id);
      }
      if (mode === 'now' && made.length) {
        const item = step('Starting the batch…');
        const batch = await send(RIDAL.apiPath('processing', 'profiles', profile, 'reprocess'), 'POST', {
          radargram_ids: made,
        });
        follow(item, batch);
      } else {
        finish(made, mode);
      }
    } catch (error) {
      if (error.message !== 'stopped') step(error.message).className = 'ingest-problem';
      finish(made, mode, true);
    } finally {
      creating = false;
    }
  });

  function finish(made, mode, stoppedEarly = false) {
    const parts = [];
    if (stoppedEarly) {
      parts.push(
        made.length
          ? `Stopped after ${plural(made.length, 'radargram', 'radargrams')}: ${made.join(', ')}. ` +
            'What was stored stays, as raw data not processed yet.'
          : 'Nothing was stored.',
      );
    } else if (mode === 'none') {
      parts.push(`${plural(made.length, 'radargram has', 'radargrams have')} raw data and no processing.`);
    } else if (mode === 'later') {
      parts.push(`${plural(made.length, 'radargram is', 'radargrams are')} waiting in their profile.`);
    }
    const processing = el('a', { href: `${RIDAL.pagePath('processing')}#raw-unprocessed`, textContent: 'the Processing page' });
    doneBox.replaceChildren(
      el('p', {}, ...parts, ' See them on ', processing, '. ',
        el('a', { href: RIDAL.pagePath('ingest'), textContent: 'Add more raw data' }), '.'),
    );
    doneBox.hidden = false;
  }

  /** A batch, followed here as the Processing page follows one. */
  function follow(item, batch) {
    const draw = (b) => {
      const finished = b.items.filter((i) => i.outcome !== 'waiting' && i.outcome !== 'processing');
      const failed = b.items.filter((i) => i.outcome === 'failed' || i.outcome === 'skipped');
      item.textContent = b.finished_at
        ? `Processed: ${finished.length - failed.length} added to the catalog` +
          (failed.length ? `, ${failed.length} not (${failed.map((i) => `${i.radargram_id}: ${i.message || i.reason || i.outcome}`).join('; ')}).` : '.')
        : `Processing ${finished.length} of ${b.items.length}…`;
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
      if (b.finished_at) {
        doneBox.replaceChildren(
          el('p', {}, 'Done. ',
            el('a', { href: RIDAL.pagePath(), textContent: 'Open the catalog' }), ', or ',
            el('a', { href: `${RIDAL.pagePath('processing')}#batch-${batch.id}`, textContent: 'see the batch' }), '.'),
        );
        doneBox.hidden = false;
      } else {
        setTimeout(tick, 2000);
      }
    };
    setTimeout(tick, 1000);
    doneBox.replaceChildren(
      el('p', { className: 'hint' }, 'Processing goes on if you leave this page; ',
        el('a', { href: `${RIDAL.pagePath('processing')}#batch-${batch.id}`, textContent: 'the Processing page' }),
        ' follows it too.'),
    );
    doneBox.hidden = false;
  }

  window.addEventListener('resize', () => {
    if (plan) renderTimeline();
  });
})();
