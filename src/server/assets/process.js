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

  const title = document.getElementById('process-title');
  const status = document.getElementById('process-status');
  const progress = document.getElementById('process-progress');
  const renderBox = document.getElementById('process-render');
  // The page's own render profile select: the viewer's or the catalog's.
  const pageRender =
    document.getElementById('profile-select') ||
    document.getElementById('index-profile-select');
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
  // What has been uploaded in this page, so taking a file out and trying
  // again does not send the others twice. The server would keep one copy
  // anyway; this saves the transfer.
  const uploaded = new Map();
  const fileKey = (file) => `${file.name}:${file.size}:${file.lastModified}`;
  let uploading = null;
  // The radargram's place in a processing profile, as the server has it.
  let assignment = null;
  // What the last draft that finished was made from, so the same thing is
  // not processed again for nothing; and whether one is being made.
  let lastRun = null;
  let running = false;
  // A finished draft is waiting to be applied.
  let draftReady = false;
  let job = null;
  let token = null;
  let showingDraft = true;
  let polling = null;

  const fail = (message) => {
    RIDAL.setMessage(errorBox, message);
    errorBox.hidden = false;
  };

  function reset() {
    draftReady = false;
    errorBox.hidden = true;
    progress.textContent = '';
    result.hidden = true;
    toggle.hidden = true;
    commit.hidden = true;
    confirm.hidden = true;
    headline.textContent = '';
    detail.replaceChildren();
    token = null;
  }

  /** Show the raw-data stage or the steps stage, and only their buttons. */
  const profileLabel = document.getElementById('process-profile-label');
  const profileSlot = document.getElementById('process-profile-slot');
  function stage(raw) {
    rawStage.hidden = !raw;
    draftStage.hidden = raw;
    upload.hidden = !raw;
    run.hidden = raw;
    // With the upload's other choices, or just above the steps it picks.
    (raw ? rawStage : profileSlot).appendChild(profileLabel);
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
      let steps = attributes.processing_steps || [];
      // In a profile, the radargram's own steps are in their own box: the
      // file's steps go below them without the ones they begin with. A
      // file that does not begin with them was not made through the
      // profile, so the profile's steps are the better start.
      if (assignment) {
        const own = assignment.pre_steps || [];
        if (own.length && own.every((step, i) => steps[i] === step)) {
          steps = steps.slice(own.length);
        } else {
          // Not made through the profile as it stands. Still, a file whose
          // steps are a selection followed by a version of the profile's
          // was most likely made that way with own steps that were since
          // lost: offer them again rather than leave them among the rest.
          const profile = profiles.find((p) => p.id === assignment.profile);
          const selects = (step) => /^(subset|remove_traces)\(/.test(step);
          for (const version of [...(profile?.versions || [])].reverse()) {
            const rest = steps.length - version.steps.length;
            const tail = steps.slice(rest);
            if (
              rest >= 0 &&
              tail.every((step, i) => step === version.steps[i]) &&
              steps.slice(0, rest).every(selects)
            ) {
              if (own.length === 0 && rest > 0) preSteps.value = steps.slice(0, rest).join('\n');
              break;
            }
          }
          fillFromProfile(assignment.profile);
          return;
        }
      }
      stepsBox.value = steps.join('\n');
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

  const stemOf = (file) => {
    const dot = file.name.lastIndexOf('.');
    return dot > 0 ? file.name.slice(0, dot) : file.name;
  };

  /** The chosen files as recordings: grouped by name without extension,
   * in name order. */
  function recordings() {
    const groups = new Map();
    for (const file of files) {
      const stem = stemOf(file);
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
      const text = document.createElement('span');
      text.textContent =
        `${recording.stem}: ${recording.files.map((f) => f.name).join(', ')} ` +
        `(${RIDAL.formatBytes(size)})`;
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'process-remove';
      remove.textContent = '×';
      remove.title = `Leave out ${recording.stem}`;
      remove.setAttribute('aria-label', `Leave out ${recording.stem}`);
      remove.addEventListener('click', () => {
        files = files.filter((f) => stemOf(f) !== recording.stem);
        listFiles();
      });
      item.append(text, remove);
      fileList.appendChild(item);
    }
    upload.disabled = files.length === 0;
  }

  choose.addEventListener('click', () => picker.click());
  picker.addEventListener('change', () => {
    // Added to what is chosen already, so a recording in another folder can
    // join; a file chosen twice is there once.
    for (const file of picker.files || []) {
      files = files.filter((f) => f.name !== file.name);
      files.push(file);
    }
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
      return (
        `The id '${id}' is taken by a radargram already. Choose another id.\n\n` +
        'If these data belong to that radargram, open it and attach raw data there.'
      );
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
          const known = uploaded.get(fileKey(file));
          if (known) {
            bundle.push(known);
            continue;
          }
          let lastPercent = -1;
          const onProgress = (loaded, size) => {
            const percent = Math.round((loaded / size) * 100);
            if (percent === lastPercent) return;
            lastPercent = percent;
            progress.textContent = `Uploading ${done} of ${total}: ${file.name}… ${percent}%`;
          };
          progress.textContent = `Uploading ${done} of ${total}: ${file.name}…`;
          uploading = RIDAL.postFile(
            `${RIDAL.apiPath('raw')}?filename=${encodeURIComponent(file.name)}`,
            file,
            onProgress,
          );
          const response = await uploading.promise;
          if (!response.ok) {
            progress.textContent = '';
            fail(response.json?.error?.message || RIDAL.upstreamMessage(response.status));
            return;
          }
          const stored = { name: response.json.name, blake3: response.json.blake3 };
          uploaded.set(fileKey(file), stored);
          bundle.push(stored);
        }
        named.push(bundle);
      }
      cancelUpload.hidden = true;
      uploading = null;

      progress.textContent = 'Reading the recordings…';
      const crs = crsBox.value.trim();
      const source = await RIDAL.fetchJson(RIDAL.apiPath('sources', id), {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ recordings: named, crs: crs === '' ? null : crs }),
      });
      radargramId = id;
      hasSource = true;
      sourceKnown.set(id, true);

      const profileId = profileBox.value;
      if (profileId !== (assignment?.profile || '')) {
        await assign(profileId);
      } else if (profileId !== '') {
        fillFromProfile(profileId);
      } else {
        await prefill();
      }

      if (viewerOpen) viewerOpen.textContent = 'Process…';
      title.textContent = `Process ${id}`;
      stage(false);
      describe(source.recordings);
      progress.textContent = '';
      await showStanding();
      status.textContent =
        'The raw data is stored. Choose the steps and process; nothing is ' +
        (served ? 'replaced' : 'added to the catalog') +
        ' until you apply a draft.';
    } catch (error) {
      progress.textContent = '';
      // A refusal from the server is a sentence of its own: shown as it is.
      if (error.message === 'upload cancelled') fail('Upload cancelled.');
      else if (error.status) fail(error.message);
      else fail(`Could not store the raw data (${error.message}).`);
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
    const path = draft
      ? RIDAL.apiPath('drafts', String(job), 'views', 'standard', 'overview')
      : RIDAL.apiPath('datasets', radargramId, 'views', 'standard', 'overview');
    return `${path}?profile=${encodeURIComponent(renderBox.value || 'default')}`;
  }

  /** The render profiles the page offers, starting at the one it shows,
   * which is the person's own choice unless they changed it here. */
  function copyRenderProfiles() {
    if (!pageRender) return;
    renderBox.replaceChildren(
      ...[...pageRender.options].map((o) => new Option(o.text, o.value)),
    );
    renderBox.value = pageRender.value;
  }
  renderBox.addEventListener('change', () => show(showingDraft));

  function show(draft) {
    showingDraft = draft;
    image.src = overviewUrl(draft);
    image.alt = draft ? 'The draft' : 'The radargram as it is served now';
    caption.textContent = draft
      ? 'Shown: the draft.'
      : 'Shown: as it is served now.';
    toggle.textContent = draft ? 'Show current' : 'Show draft';
  }

  async function poll() {
    let draft;
    try {
      draft = await RIDAL.fetchJson(RIDAL.apiPath('drafts', String(job)));
    } catch (error) {
      fail(`Could not follow the draft (${error.message}).`);
      finishRun(false);
      return;
    }
    if (draft.state === 'queued') {
      progress.textContent = 'Waiting for the server to start it…';
    } else if (draft.state === 'running') {
      if (draft.writing) progress.textContent = 'Writing the processed file…';
      else if (draft.step) {
        progress.textContent = `Step ${draft.step.index + 1} of ${draft.step.of}: ${draft.step.step}`;
      } else progress.textContent = 'Reading the raw data…';
    } else if (draft.state === 'done') {
      draftReady = true;
      finishRun(true);
      progress.textContent = 'Done. Nothing changes until you apply it.';
      result.hidden = false;
      commit.hidden = false;
      // A radargram that is not served yet has nothing to compare with.
      toggle.hidden = draft.base_revision === null;
      show(true);
      return;
    } else {
      finishRun(false);
      progress.textContent = '';
      fail(draft.message || `The draft ended without a result (${draft.state}).`);
      return;
    }
    polling = setTimeout(poll, 1000);
  }

  const linesIn = (box) =>
    box.value
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line !== '' && !line.startsWith('#'));

  /** The radargram's own steps, run before the profile's: a box of their
   * own while it is in a profile, so which steps are whose is plain. */
  const preBox = document.getElementById('process-pre');
  const preSteps = document.getElementById('process-pre-steps');
  const mainBox = document.getElementById('process-main');
  const mainSummary = document.getElementById('process-main-summary');
  const mainNote = document.getElementById('process-main-note');
  const preLines = () => (preBox.hidden ? [] : linesIn(preSteps));
  // Kept when the radargram leaves its profile, so that coming back (a
  // mistaken click on None, say) does not lose them.
  let keptPre = [];
  function showPre() {
    preBox.hidden = !assignment;
    showMain(true);
  }

  /** In a profile, its steps and velocity fold away under a line that
   * says whose they are, and open, saying so, once they differ from the
   * profile's. Outside one they are just the steps, always open. `fold`
   * closes them again when they are the profile's. */
  function showMain(fold = false) {
    const profile = assignment && profiles.find((p) => p.id === assignment.profile);
    const current = profile?.versions.find((v) => v.version === profile.current_version);
    mainBox.classList.toggle('process-main-free', !profile);
    mainNote.hidden = !profile;
    if (!profile) {
      mainBox.open = true;
      return;
    }
    const differs =
      !current ||
      JSON.stringify(linesIn(stepsBox)) !== JSON.stringify(current.steps) ||
      Number(velocityBox.value) !== current.medium_velocity;
    mainSummary.textContent = differs
      ? `Steps and velocity: changed from ${profile.name}'s for this radargram`
      : `Steps and velocity: ${profile.name}'s, version ${current.version}`;
    mainSummary.classList.toggle('process-differs', differs);
    if (differs) mainBox.open = true;
    else if (fold) mainBox.open = false;
  }

  /** The steps as they are sent: its own first, then the rest. */
  const stepLines = () => [...preLines(), ...linesIn(stepsBox)];
  const runKey = () => JSON.stringify([stepLines(), Number(velocityBox.value)]);
  // What the draft being made is made from, until it is done.
  let pendingKey = null;

  /** Process is offered while a draft is not being made and what is in the
   * boxes is not what the last finished draft was made from: the same
   * steps would make the same draft. */
  function updateRun() {
    const same = lastRun !== null && runKey() === lastRun;
    run.disabled = running || same;
    run.title = same ? 'This draft is made from these steps already. Change them to process again.' : '';
    // A draft is applied as it was made: once the boxes say something
    // else, applying it would not apply what they say.
    if (draftReady) {
      commit.hidden = !same;
      if (!same) {
        confirm.hidden = true;
        progress.textContent =
          'The steps have changed since this draft was made. Process again to apply them.';
      } else {
        progress.textContent = 'Done. Nothing changes until you apply it.';
      }
    }
    showMain();
  }
  stepsBox.addEventListener('input', updateRun);
  preSteps.addEventListener('input', updateRun);
  velocityBox.addEventListener('input', updateRun);

  function finishRun(succeeded) {
    running = false;
    lastRun = succeeded ? pendingKey : null;
    pendingKey = null;
    updateRun();
  }

  /** Fill the steps below the radargram's own with a profile's current
   * version, and its velocity, as a batch runs them. */
  function fillFromProfile(profileId) {
    const profile = profiles.find((p) => p.id === profileId);
    const current = profile?.versions.find((v) => v.version === profile.current_version);
    if (!current) return;
    stepsBox.value = current.steps.join('\n');
    velocityBox.value = String(current.medium_velocity);
    updateRun();
    showMain(true);
  }

  /** Save the radargram in `profileId` with `pre` as its own steps; the
   * server's answer is what is shown, in its canonical form. */
  async function saveAssignment(profileId, pre) {
    const saved = await RIDAL.fetchJson(
      RIDAL.apiPath('processing', 'assignments', radargramId),
      {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ profile: profileId, pre_steps: pre }),
      },
    );
    assignment = { profile: saved.profile, pre_steps: saved.pre_steps || [] };
    keptPre = assignment.pre_steps;
    preSteps.value = assignment.pre_steps.join('\n');
    showPre();
  }

  /** Put the radargram in `profileId`, or take it out of its profile for
   * `''`. Its own steps go with it: into the steps box when it leaves, so
   * what it is processed with does not change, and back into their own
   * box when it joins again. */
  async function assign(profileId) {
    if (profileId === '') {
      // `fetch` rather than `fetchJson`: a 204 has no body to read.
      const response = await fetch(RIDAL.apiPath('processing', 'assignments', radargramId), {
        method: 'DELETE',
      });
      if (!response.ok && response.status !== 404) {
        const body = await response.json().catch(() => null);
        throw new Error(body?.error?.message || RIDAL.upstreamMessage(response.status));
      }
      const own = preLines();
      keptPre = own;
      assignment = null;
      showPre();
      if (own.length) stepsBox.value = [...own, ...linesIn(stepsBox)].join('\n');
      updateRun();
      return;
    }
    const own = assignment ? preLines() : keptPre;
    await saveAssignment(profileId, own);
    fillFromProfile(profileId);
  }

  /** Before there is raw data the choice waits for the upload, which
   * assigns it; after, it is made at once and fills in the steps. */
  profileBox.addEventListener('change', async () => {
    if (!hasSource || !rawStage.hidden) return;
    const profileId = profileBox.value;
    errorBox.hidden = true;
    try {
      await assign(profileId);
      const name = profiles.find((p) => p.id === profileId)?.name;
      status.textContent = name
        ? `${radargramId} is in ${name} now: its own steps are in the first box, the ` +
          "profile's below."
        : `${radargramId} is in no processing profile now; its own steps, if it had ` +
          'any, are at the top of the steps.';
      await showStanding();
    } catch (error) {
      profileBox.value = assignment?.profile || '';
      fail(`Could not change the processing profile (${error.message}).`);
    }
  });

  /** Where the radargram stands against its profile, said above the
   * steps, with a way back to the profile's steps when it has left them. */
  const standing = document.getElementById('process-standing');
  const standingText = document.getElementById('process-standing-text');
  const useProfile = document.getElementById('process-use-profile');
  async function showStanding() {
    standing.hidden = true;
    if (!assignment || radargramId === null) return;
    const profile = profiles.find((p) => p.id === assignment.profile);
    let status = null;
    try {
      const answer = await RIDAL.fetchJson(
        `${RIDAL.apiPath('processing', 'status')}?radargram_id=${encodeURIComponent(radargramId)}`,
      );
      status = answer.radargrams[0] || null;
    } catch {
      return;
    }
    if (!status || !profile) return;
    const name = `${profile.name} (version ${profile.current_version})`;
    const why = (status.reasons || []).join('; ');
    const says = {
      current: `Up to date with ${name}.`,
      stale: `What is served now is stale against ${name}: ${why}. Process with the ` +
        "profile's steps and apply the draft to bring it up to date.",
      older_processing: `Processed as ${name} says, but by an older Ridal: ${why}.`,
      not_processed: `In ${name}, and not processed yet.`,
      no_raw_data: `In ${name}, but without raw data.`,
    };
    standingText.textContent = says[status.state] || status.state;
    standing.classList.toggle('warning', status.state === 'stale');
    useProfile.hidden = status.state === 'current' || status.state === 'no_raw_data';
    standing.hidden = false;
  }
  useProfile.addEventListener('click', () => {
    if (!assignment) return;
    preSteps.value = assignment.pre_steps.join('\n');
    fillFromProfile(assignment.profile);
    useProfile.hidden = true;
    standingText.textContent =
      "The profile's steps are filled in below. Process, and apply the draft, to bring " +
      'it up to date.';
  });

  /** The profiles, and which one this radargram is in, if any. */
  async function loadAssignment() {
    await loadProfiles();
    assignment = null;
    if (radargramId !== null) {
      try {
        const all = await RIDAL.fetchJson(RIDAL.apiPath('processing', 'assignments'));
        assignment = all.radargrams?.[radargramId] || null;
      } catch {
        // No assignments to read: in no profile.
      }
    }
    profileBox.value = assignment?.profile || '';
    keptPre = assignment?.pre_steps || [];
    preSteps.value = keptPre.join('\n');
    showPre();
    await showStanding();
  }

  /** Open at the stage this radargram needs. */
  async function begin(raw) {
    copyRenderProfiles();
    lastRun = null;
    updateRun();
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
          'it is until you process them and apply a draft.'
        : 'Choose the raw files of one radargram: several recordings are merged ' +
          'in name order.';
    } else {
      title.textContent = `Process ${radargramId}`;
      status.textContent =
        'Change the steps and process. Nothing changes until you apply a draft.';
    }
    dialog.showModal();
    await loadAssignment();
    if (!raw) await prefill();
    showMain(true);
  }

  if (viewerOpen) viewerOpen.addEventListener('click', () => begin(!hasSource));
  /** A card's Edit menu: says what it would do once the menu is opened,
   * rather than asking about every radargram when the page loads. */
  const sourceKnown = new Map();
  async function hasSourceFor(id) {
    if (!sourceKnown.has(id)) {
      try {
        await RIDAL.fetchJson(RIDAL.apiPath('sources', id));
        sourceKnown.set(id, true);
      } catch (error) {
        if (error.status !== 404) throw error;
        sourceKnown.set(id, false);
      }
    }
    return sourceKnown.get(id);
  }
  // Delegated, so a button drawn after load (the processing page's rows)
  // works as one in the catalog's cards does. `toggle` does not bubble, so
  // it is caught on the way down.
  document.addEventListener(
    'toggle',
    async (event) => {
      const menu = event.target;
      if (!(menu instanceof HTMLDetailsElement) || !menu.open) return;
      for (const button of menu.querySelectorAll('button[data-process-radargram]')) {
        try {
          const known = await hasSourceFor(button.dataset.processRadargram);
          button.textContent = known ? 'Process…' : 'Attach raw data…';
        } catch {
          // Asked again on the click.
        }
      }
    },
    true,
  );
  document.addEventListener('click', async (event) => {
    const button = event.target.closest?.('button[data-process-radargram]');
    if (!button) return;
    const menu = button.closest('details');
    if (menu) menu.open = false;
    radargramId = button.dataset.processRadargram;
    served = button.dataset.served !== 'false';
    stepsBox.value = '';
    try {
      hasSource = await hasSourceFor(radargramId);
    } catch {
      hasSource = false;
    }
    begin(!hasSource);
  });

  document.getElementById('process-default-steps').addEventListener('click', (event) => {
    stepsBox.value = event.currentTarget.dataset.steps;
    updateRun();
  });

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
    const velocity = Number(velocityBox.value);
    running = true;
    updateRun();
    progress.textContent = 'Sending…';
    try {
      // Its own steps belong to the radargram, not to this draft: saved
      // with it, so the profile and a batch run them too.
      const own = preLines();
      if (assignment && JSON.stringify(own) !== JSON.stringify(assignment.pre_steps)) {
        await saveAssignment(assignment.profile, own);
      }
      // After the save, which writes them back in their canonical form.
      const steps = stepLines();
      pendingKey = runKey();
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
      finishRun(false);
      progress.textContent = '';
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
    progress.textContent = 'Checking what this would do…';
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
      progress.textContent = '';
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
    const report = body.report;
    // Nothing anyone picked moves or is left out: nothing to look at.
    const harmless =
      !report.revision_id_collision &&
      report.documents.every(
        (d) => (d.severity === 'current' || d.severity === 'carried') && !(d.dropped || []).length,
      );
    if (harmless) {
      await applyReplacement();
      return;
    }
    commit.hidden = true;
    progress.textContent =
      "Applying it would change people's picks, as above. Nothing has changed yet.";
    render(report);
  });

  confirm.addEventListener('click', applyReplacement);

  /** Install the staged replacement and reload, to show it. */
  async function applyReplacement() {
    if (!token) return;
    confirm.disabled = true;
    commit.disabled = true;
    progress.textContent = 'Applying…';
    try {
      const response = await fetch(
        `${RIDAL.apiPath('datasets', radargramId, 'replace')}/${token}`,
        { method: 'POST' },
      );
      if (!response.ok) {
        const body = await response.json().catch(() => null);
        confirm.disabled = false;
        commit.disabled = false;
        progress.textContent = '';
        fail(body?.error?.message || RIDAL.upstreamMessage(response.status));
        return;
      }
    } catch (error) {
      confirm.disabled = false;
      commit.disabled = false;
      fail(`Could not replace it (${error.message}).`);
      return;
    }
    token = null;
    job = null;
    window.location.reload();
  }

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
