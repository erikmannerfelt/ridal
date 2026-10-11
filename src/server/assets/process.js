/* --- Process a radargram on the server (#382) -----------------------------
 *
 * For an operator. Steps in, a draft out, a look at it beside what is
 * served now, and a commit that goes through the same consequence report
 * as a replace.
 *
 * Raw data comes first where the project has none: on the catalog, for a
 * radargram that does not exist yet ("From raw data…"), and in the viewer,
 * for a served one nobody has given raw data ("Attach raw data…"). The
 * files are uploaded one by one, grouped into recordings by their name
 * without its extension, which is how every supported format ties its
 * header, data and positions together, and merged in name order.
 *
 * The draft is the server's: this dialog only asks for it, waits for it,
 * and shows it. Closing the dialog throws away a draft that was not
 * committed, since it is private to this person and nobody else can reach
 * it from here. Uploaded raw data stays: it is the radargram's now.
 */
(function setupProcessing() {
  const dialog = document.getElementById('process-dialog');
  if (!dialog) return;
  const viewerOpen = document.getElementById('process-open');
  const catalogOpen = document.getElementById('process-new');
  if (!viewerOpen && !catalogOpen) return;

  const title = document.getElementById('process-title');
  const status = document.getElementById('process-status');
  const rawStage = document.getElementById('process-raw');
  const draftStage = document.getElementById('process-draft');
  const idLabel = document.getElementById('process-id-label');
  const idBox = document.getElementById('process-id');
  const choose = document.getElementById('process-choose');
  const picker = document.getElementById('process-files');
  const fileList = document.getElementById('process-file-list');
  const crsBox = document.getElementById('process-crs');
  const profileBox = document.getElementById('process-profile');
  const upload = document.getElementById('process-upload');
  const cancelUpload = document.getElementById('process-cancel-upload');
  const stepsBox = document.getElementById('process-steps');
  const velocityBox = document.getElementById('process-velocity');
  const errorBox = document.getElementById('process-error');
  const result = document.getElementById('process-result');
  const image = document.getElementById('process-image');
  const caption = document.getElementById('process-caption');
  const headline = document.getElementById('process-headline');
  const detail = document.getElementById('process-detail');
  const run = document.getElementById('process-run');
  const toggle = document.getElementById('process-toggle');
  const commit = document.getElementById('process-commit');
  const confirm = document.getElementById('process-confirm');

  // The radargram being processed. Fixed in the viewer; typed on the catalog.
  let radargramId = window.RIDAL_VIEWER ? window.RIDAL_VIEWER.radargramId : null;
  // Whether it is served, so a draft has something to be compared with and
  // a commit is a replacement rather than an addition.
  let served = Boolean(window.RIDAL_VIEWER);
  let files = [];
  let uploading = null;
  let job = null;
  let token = null;
  let showingDraft = true;
  let polling = null;

  const fail = (message) => {
    RIDAL.setMessage(errorBox, message);
    errorBox.hidden = false;
  };

  function reset() {
    errorBox.hidden = true;
    result.hidden = true;
    toggle.hidden = true;
    commit.hidden = true;
    confirm.hidden = true;
    headline.textContent = '';
    detail.replaceChildren();
    token = null;
  }

  /** Show the raw-data stage or the steps stage, and only their buttons. */
  function stage(raw) {
    rawStage.hidden = !raw;
    draftStage.hidden = raw;
    upload.hidden = !raw;
    run.hidden = raw;
  }

  /** In the viewer, the button says what it will do: process, when the
   * project has this radargram's raw data, or attach some when it has
   * none. Asked once, on load. Anything else -- no project, no right --
   * offers nothing. */
  let hasSource = false;
  if (viewerOpen) {
    (async function offer() {
      try {
        await RIDAL.fetchJson(RIDAL.apiPath('sources', radargramId));
        hasSource = true;
        viewerOpen.textContent = 'Process…';
      } catch (error) {
        if (error.status !== 404) return;
        viewerOpen.textContent = 'Attach raw data…';
      }
      viewerOpen.hidden = false;
    })();
  }

  /** Start from the steps the served file was processed with, one per
   * line, so a change is an edit rather than a retyping. */
  async function prefill() {
    if (stepsBox.value.trim() !== '' || !served) return;
    try {
      const attributes = await RIDAL.fetchJson(
        RIDAL.apiPath('datasets', radargramId, 'attributes'),
      );
      stepsBox.value = (attributes.processing_steps || []).join('\n');
    } catch {
      // Unreadable: start empty.
    }
  }

  /** The processing profiles, to put a new radargram in one and start from
   * its steps. Read each time the dialog opens: they change on another
   * page. */
  let profiles = [];
  async function loadProfiles() {
    try {
      profiles = (await RIDAL.fetchJson(RIDAL.apiPath('processing', 'profiles'))).profiles;
    } catch {
      profiles = [];
    }
    const chosen = profileBox.value;
    profileBox.replaceChildren(new Option('None', ''));
    for (const profile of profiles) {
      profileBox.append(new Option(`${profile.name} (v${profile.current_version})`, profile.id));
    }
    profileBox.value = profiles.some((p) => p.id === chosen) ? chosen : '';
  }

  /** The chosen files as recordings: grouped by name without extension,
   * in name order. */
  function recordings() {
    const groups = new Map();
    for (const file of files) {
      const dot = file.name.lastIndexOf('.');
      const stem = dot > 0 ? file.name.slice(0, dot) : file.name;
      if (!groups.has(stem)) groups.set(stem, []);
      groups.get(stem).push(file);
    }
    return [...groups.entries()]
      .sort(([a], [b]) => a.localeCompare(b, undefined, { numeric: true }))
      .map(([stem, members]) => ({ stem, files: members }));
  }

  function listFiles() {
    fileList.replaceChildren();
    for (const recording of recordings()) {
      const item = document.createElement('li');
      const size = recording.files.reduce((sum, f) => sum + f.size, 0);
      item.textContent =
        `${recording.stem}: ${recording.files.map((f) => f.name).join(', ')} ` +
        `(${RIDAL.formatBytes(size)})`;
      fileList.appendChild(item);
    }
    upload.disabled = files.length === 0;
  }

  choose.addEventListener('click', () => picker.click());
  picker.addEventListener('change', () => {
    files = [...(picker.files || [])];
    // Reset, so choosing the same files again still fires `change`.
    picker.value = '';
    listFiles();
    // On the catalog, the first recording's name is the likeliest id, as
    // an id is written: `DAT_0066_A1` is `dat_0066_a1`.
    if (!idBox.readOnly && idBox.value.trim() === '' && files.length > 0) {
      idBox.value = RIDAL.slugify(recordings()[0].stem);
    }
  });

  /** One line per recording, from what the server read in its header.
   * Kept above the steps for as long as the dialog is open. */
  const recordingList = document.getElementById('process-recordings');
  function describe(records) {
    recordingList.replaceChildren();
    for (const record of records) {
      const m = record.metadata;
      const l = record.location;
      const item = document.createElement('li');
      // RFC 3339 to the minute: `2025-03-27 12:10 UTC`.
      const start = `${l.start_time.slice(0, 16).replace('T', ' ')} UTC`;
      item.textContent =
        `${record.input.split(/[\\/]/).pop()}: ${m.antenna_name || `${m.antenna_mhz} MHz`}, ` +
        `${m.last_trace + 1} traces × ${m.samples} samples, ${start}, ` +
        `${Math.round(l.track_length_m)} m (${l.crs})`;
      recordingList.appendChild(item);
    }
  }

  /** Is `id` free for a new radargram? Says why not, or null. A source
   * without a served radargram is a raw upload that was never processed,
   * and is taken over. */
  async function idProblem(id) {
    if (id === '') return 'Give the radargram an id.';
    try {
      await RIDAL.fetchJson(RIDAL.apiPath('datasets', id, 'attributes'));
      return `'${id}' is a radargram already. Open it and attach raw data there.`;
    } catch (error) {
      if (error.status !== 404) return error.message;
    }
    try {
      await RIDAL.fetchJson(RIDAL.apiPath('sources', id));
    } catch (error) {
      // An id the server will not take is refused here, before the upload.
      if (error.status !== 404) return error.message;
    }
    return null;
  }

  upload.addEventListener('click', async () => {
    reset();
    const id = served ? radargramId : idBox.value.trim();
    upload.disabled = true;
    choose.disabled = true;
    try {
      if (!served) {
        const problem = await idProblem(id);
        if (problem) {
          fail(problem);
          return;
        }
      }
      const named = [];
      const groups = recordings();
      const total = files.length;
      let done = 0;
      cancelUpload.hidden = false;
      for (const recording of groups) {
        const bundle = [];
        for (const file of recording.files) {
          done += 1;
          let lastPercent = -1;
          const progress = (loaded, size) => {
            const percent = Math.round((loaded / size) * 100);
            if (percent === lastPercent) return;
            lastPercent = percent;
            status.textContent = `Uploading ${done} of ${total}: ${file.name}… ${percent}%`;
          };
          status.textContent = `Uploading ${done} of ${total}: ${file.name}…`;
          uploading = RIDAL.postFile(
            `${RIDAL.apiPath('raw')}?filename=${encodeURIComponent(file.name)}`,
            file,
            progress,
          );
          const response = await uploading.promise;
          if (!response.ok) {
            status.textContent = '';
            fail(response.json?.error?.message || RIDAL.upstreamMessage(response.status));
            return;
          }
          bundle.push({ name: response.json.name, blake3: response.json.blake3 });
        }
        named.push(bundle);
      }
      cancelUpload.hidden = true;
      uploading = null;

      status.textContent = 'Reading the recordings…';
      const crs = crsBox.value.trim();
      const source = await RIDAL.fetchJson(RIDAL.apiPath('sources', id), {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ recordings: named, crs: crs === '' ? null : crs }),
      });
      radargramId = id;
      hasSource = true;

      const profileId = profileBox.value;
      if (profileId !== '') {
        await RIDAL.fetchJson(RIDAL.apiPath('processing', 'assignments', id), {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ profile: profileId }),
        });
        const profile = profiles.find((p) => p.id === profileId);
        const current = profile?.versions.find((v) => v.version === profile.current_version);
        if (current) {
          stepsBox.value = current.steps.join('\n');
          velocityBox.value = String(current.medium_velocity);
        }
      } else {
        await prefill();
      }

      if (viewerOpen) viewerOpen.textContent = 'Process…';
      title.textContent = `Process ${id}`;
      stage(false);
      describe(source.recordings);
      status.textContent =
        'The raw data is stored. Choose the steps and process; nothing is ' +
        (served ? 'replaced' : 'added to the catalog') +
        ' until you make the draft the radargram.';
    } catch (error) {
      status.textContent = '';
      fail(
        error.message === 'upload cancelled'
          ? 'Upload cancelled.'
          : `Could not store the raw data (${error.message}).`,
      );
    } finally {
      upload.disabled = files.length === 0;
      choose.disabled = false;
      cancelUpload.hidden = true;
      uploading = null;
    }
  });

  cancelUpload.addEventListener('click', () => {
    if (uploading) uploading.abort();
  });

  function discard() {
    if (polling) clearTimeout(polling);
    polling = null;
    if (job === null) return;
    fetch(RIDAL.apiPath('drafts', String(job)), {
      method: 'DELETE',
      keepalive: true,
    }).catch(() => {});
    job = null;
  }

  function overviewUrl(draft) {
    return draft
      ? `${RIDAL.apiPath('drafts', String(job), 'views', 'standard', 'overview')}`
      : `${RIDAL.apiPath('datasets', radargramId, 'views', 'standard', 'overview')}`;
  }

  function show(draft) {
    showingDraft = draft;
    image.src = overviewUrl(draft);
    image.alt = draft ? 'The draft' : 'The radargram as it is served now';
    caption.textContent = draft
      ? 'The draft.'
      : 'As it is served now.';
    toggle.textContent = draft ? 'Show current' : 'Show draft';
  }

  async function poll() {
    let draft;
    try {
      draft = await RIDAL.fetchJson(RIDAL.apiPath('drafts', String(job)));
    } catch (error) {
      fail(`Could not follow the draft (${error.message}).`);
      run.disabled = false;
      return;
    }
    if (draft.state === 'queued') {
      status.textContent = 'Waiting for the server to start it…';
    } else if (draft.state === 'running') {
      status.textContent = draft.step
        ? `Step ${draft.step.index + 1} of ${draft.step.of}: ${draft.step.step}`
        : 'Starting…';
    } else if (draft.state === 'done') {
      run.disabled = false;
      status.textContent = 'Done. Nothing has changed until you make it the radargram.';
      result.hidden = false;
      commit.hidden = false;
      // A radargram that is not served yet has nothing to compare with.
      toggle.hidden = draft.base_revision === null;
      show(true);
      return;
    } else {
      run.disabled = false;
      status.textContent = '';
      fail(draft.message || `The draft ended without a result (${draft.state}).`);
      return;
    }
    polling = setTimeout(poll, 1000);
  }

  /** Open at the stage this radargram needs. */
  async function begin(raw) {
    reset();
    stage(raw);
    recordingList.replaceChildren();
    if (raw) {
      files = [];
      listFiles();
      idBox.readOnly = served;
      idLabel.hidden = served;
      if (!served) idBox.value = '';
      title.textContent = served ? `Raw data for ${radargramId}` : 'New radargram from raw data';
      status.textContent = served
        ? 'Choose the raw files this radargram was processed from. It stays as ' +
          'it is until you process them and make the draft the radargram.'
        : 'Choose the raw files of one radargram: several recordings are merged ' +
          'in name order.';
    } else {
      title.textContent = `Process ${radargramId}`;
      status.textContent =
        'Change the steps and process. Nothing changes until you make the draft the radargram.';
    }
    dialog.showModal();
    if (raw) await loadProfiles();
    else await prefill();
  }

  if (viewerOpen) viewerOpen.addEventListener('click', () => begin(!hasSource));
  if (catalogOpen) {
    catalogOpen.addEventListener('click', () => {
      radargramId = null;
      served = false;
      hasSource = false;
      stepsBox.value = '';
      begin(true);
    });
  }

  run.addEventListener('click', async () => {
    discard();
    reset();
    const steps = stepsBox.value
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line !== '' && !line.startsWith('#'));
    const velocity = Number(velocityBox.value);
    run.disabled = true;
    status.textContent = 'Sending…';
    try {
      const draft = await RIDAL.fetchJson(
        RIDAL.apiPath('sources', radargramId, 'drafts'),
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            steps,
            medium_velocity: Number.isFinite(velocity) && velocity > 0 ? velocity : null,
          }),
        },
      );
      job = draft.job;
      poll();
    } catch (error) {
      run.disabled = false;
      status.textContent = '';
      fail(error.message);
    }
  });

  toggle.addEventListener('click', () => show(!showingDraft));

  const TIER_WORDS = {
    current: 'unchanged',
    carried: 'carries cleanly',
    approximate: 'moves',
    partial: 'partly outside the new version',
    refused: 'cannot be shown',
  };

  /** The consequence report, as the catalog's replace dialog shows it. */
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
    if (report.documents.length === 0) rows.push(['Picks', 'none']);
    for (const d of report.documents) {
      const left = d.dropped && d.dropped.length > 0 ? ` — ${d.dropped.length} left out` : '';
      rows.push([`Picks by ${d.user}`, `${TIER_WORDS[d.severity] || d.severity}${left}`]);
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
    const stopped =
      report.revision_id_collision ||
      (!report.outgoing_axes_kept && report.documents.length > 0);
    confirm.hidden = stopped;
    if (stopped) fail(report.headline);
  }

  commit.addEventListener('click', async () => {
    if (job === null) return;
    commit.disabled = true;
    errorBox.hidden = true;
    status.textContent = 'Checking what this would do…';
    let response;
    try {
      response = await fetch(RIDAL.apiPath('drafts', String(job), 'commit'), {
        method: 'POST',
      });
    } catch (error) {
      commit.disabled = false;
      fail(`Could not commit it (${error.message}).`);
      return;
    }
    const body = await response.json().catch(() => null);
    commit.disabled = false;
    if (!response.ok) {
      status.textContent = '';
      fail(body?.error?.message || RIDAL.upstreamMessage(response.status));
      return;
    }
    if (response.status === 201) {
      // A new radargram: it is in the catalog now, so go and look at it.
      job = null;
      if (window.RIDAL_VIEWER) window.location.reload();
      else window.location.href = RIDAL.pagePath('view', radargramId);
      return;
    }
    token = body.token;
    commit.hidden = true;
    status.textContent = 'This is what making the draft the radargram would do:';
    render(body.report);
  });

  confirm.addEventListener('click', async () => {
    if (!token) return;
    confirm.disabled = true;
    status.textContent = 'Replacing…';
    try {
      const response = await fetch(
        `${RIDAL.apiPath('datasets', radargramId, 'replace')}/${token}`,
        { method: 'POST' },
      );
      if (!response.ok) {
        const body = await response.json().catch(() => null);
        confirm.disabled = false;
        status.textContent = '';
        fail(body?.error?.message || RIDAL.upstreamMessage(response.status));
        return;
      }
    } catch (error) {
      confirm.disabled = false;
      fail(`Could not replace it (${error.message}).`);
      return;
    }
    token = null;
    job = null;
    window.location.reload();
  });

  document.getElementById('process-close').addEventListener('click', () => dialog.close());
  dialog.addEventListener('close', () => {
    if (uploading) uploading.abort();
    // A staged replacement nobody committed goes too.
    if (token) {
      fetch(`${RIDAL.apiPath('datasets', radargramId, 'replace')}/${token}`, {
        method: 'DELETE',
        keepalive: true,
      }).catch(() => {});
      token = null;
    }
    discard();
  });
})();
