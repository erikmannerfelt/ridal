/* --- Process a radargram on the server (#382) -----------------------------
 *
 * For an operator. Steps in, a draft out, a look at it beside what is
 * served now, and a commit that goes through the same consequence report
 * as a replace.
 *
 * Raw data comes first where the project has none: on the catalog, for a
 * radargram that does not exist yet ("New radargram from raw data…"), and in the viewer,
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
  const demField = document.getElementById('process-dem-field');
  const demBox = document.getElementById('process-dem');
  const demSlot = document.getElementById('process-dem-slot');
  const antennaField = document.getElementById('process-antenna-field');
  const mhzBox = document.getElementById('process-antenna-mhz');
  const separationBox = document.getElementById('process-antenna-separation');
  const antennaSlot = document.getElementById('process-antenna-slot');
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
    offer.hidden = true;
    document.getElementById('process-close').textContent = 'Cancel';
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
    // The DEM likewise: with the upload, or above the recordings.
    (raw ? rawStage : demSlot).appendChild(demField);
    // And the antenna overrides, after it.
    (raw ? rawStage : antennaSlot).appendChild(antennaField);
  }

  /** An antenna override as the source takes it: null when empty, and
   * refused here when it is not a positive number. */
  function antennaValue(box, what) {
    const text = box.value.trim();
    if (text === '') return null;
    const value = Number(text);
    if (!(Number.isFinite(value) && value > 0)) {
      throw new Error(`The ${what} must be a positive number, or empty to read it from the files.`);
    }
    return value;
  }
  const antennaOverrides = () => ({
    antenna_mhz: antennaValue(mhzBox, 'antenna frequency'),
    antenna_separation: antennaValue(separationBox, 'antenna separation'),
  });
  /** What the source has, as the boxes show it. */
  let savedAntenna = { antenna_mhz: null, antenna_separation: null };
  function showAntenna(source) {
    savedAntenna = {
      antenna_mhz: source?.antenna_mhz ?? null,
      antenna_separation: source?.antenna_separation ?? null,
    };
    mhzBox.value = savedAntenna.antenna_mhz ?? '';
    separationBox.value = savedAntenna.antenna_separation ?? '';
  }
  /** A header whose antenna string is not a frequency (mala.rs) is read
   * once the source gives one: say where. */
  const antennaHint = (message) =>
    /antenna field/.test(message)
      ? `${message}\n\nGive the antenna frequency (MHz) below, and try again.`
      : message;

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

  /** The project's DEMs, and the one this radargram's source links to.
   * Read each time the dialog opens: they are added on another page. */
  let linkedDem = '';
  async function loadDems() {
    let dems = [];
    try {
      dems = (await RIDAL.fetchJson(RIDAL.apiPath('dems'))).dems;
    } catch {
      dems = [];
    }
    linkedDem = '';
    showAntenna(null);
    if (hasSource && radargramId !== null) {
      try {
        const source = await RIDAL.fetchJson(RIDAL.apiPath('sources', radargramId));
        linkedDem = source.source.dem?.id || '';
        showAntenna(source.source);
      } catch {
        // No source to read: no DEM, and no overrides.
      }
    }
    demBox.replaceChildren(new Option('None', ''));
    for (const dem of dems) demBox.append(new Option(`${dem.name} (${dem.id})`, dem.id));
    // A link to a DEM that has since gone is still the source's: shown
    // as its id rather than as None, which it is not.
    if (linkedDem && !dems.some((d) => d.id === linkedDem)) {
      demBox.append(new Option(linkedDem, linkedDem));
    }
    demBox.value = linkedDem;
  }

  /** Before there is raw data the choice goes with the upload; after, it
   * is linked at once, which makes what is served stale. */
  demBox.addEventListener('change', async () => {
    if (!hasSource || !rawStage.hidden) return;
    const dem = demBox.value;
    errorBox.hidden = true;
    demBox.disabled = true;
    progress.textContent = dem ? 'Reading the recordings with the DEM…' : '';
    try {
      const answer = await RIDAL.fetchJson(RIDAL.apiPath('sources', radargramId, 'dem'), {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ dem: dem === '' ? null : dem }),
      });
      linkedDem = answer.source.dem?.id || '';
      if (answer.recordings.length) describe(answer.recordings);
      const name = demBox.selectedOptions[0]?.textContent;
      status.textContent =
        (linkedDem ? `${radargramId} takes its elevations from ${name} now.` :
          `${radargramId} has no DEM now.`) +
        (served ? ' What is served was made otherwise, so it needs processing again.' : '');
      await showStanding();
    } catch (error) {
      demBox.value = linkedDem;
      fail(`Could not change the DEM (${error.message}).`);
    } finally {
      demBox.disabled = false;
      progress.textContent = '';
      updateRun();
    }
  });

  /** Like the DEM: before there is raw data the overrides go with the
   * upload; after, a change is saved to the source at once, which reads
   * the recordings again with it and makes what is served stale. */
  async function saveAntenna() {
    if (!hasSource || !rawStage.hidden) return;
    errorBox.hidden = true;
    let wanted;
    try {
      wanted = antennaOverrides();
    } catch (error) {
      fail(error.message);
      return;
    }
    if (
      wanted.antenna_mhz === savedAntenna.antenna_mhz &&
      wanted.antenna_separation === savedAntenna.antenna_separation
    ) {
      return;
    }
    mhzBox.disabled = true;
    separationBox.disabled = true;
    progress.textContent = 'Reading the recordings with the antenna settings…';
    try {
      // The whole source goes back with only these changed: the route
      // sets what a radargram is made from, and keeps the names unless told.
      const { source } = await RIDAL.fetchJson(RIDAL.apiPath('sources', radargramId));
      const answer = await RIDAL.fetchJson(RIDAL.apiPath('sources', radargramId), {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          recordings: source.bundles.map((b) =>
            b.files.map((f) => ({ name: f.name, blake3: f.blake3 })),
          ),
          crs: source.crs,
          dem: source.dem?.id ?? null,
          ...wanted,
        }),
      });
      showAntenna(answer.source);
      if (answer.recordings.length) describe(answer.recordings);
      status.textContent =
        `The antenna settings of ${radargramId} are saved.` +
        (served ? ' What is served was made otherwise, so it needs processing again.' : '');
      await showStanding();
    } catch (error) {
      showAntenna(savedAntenna);
      fail(`Could not change the antenna settings (${antennaHint(error.message)}).`);
    } finally {
      mhzBox.disabled = false;
      separationBox.disabled = false;
      progress.textContent = '';
      updateRun();
    }
  }
  mhzBox.addEventListener('change', saveAntenna);
  separationBox.addEventListener('change', saveAntenna);

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
    // Checked before anything is sent, not after the upload it would waste.
    let overrides;
    try {
      overrides = antennaOverrides();
    } catch (error) {
      fail(error.message);
      return;
    }
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
        body: JSON.stringify({
          recordings: named,
          crs: crs === '' ? null : crs,
          dem: demBox.value === '' ? null : demBox.value,
          ...overrides,
          // A new radargram takes over an upload nobody processed, and
          // not the names that upload was to have (#382 phase 7).
          ...(served ? {} : { names: null }),
        }),
      });
      linkedDem = source.source.dem?.id || '';
      showAntenna(source.source);
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
      else if (error.status) fail(antennaHint(error.message));
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
  // The DEM too: linking another makes another draft.
  const runKey = () => JSON.stringify([stepLines(), Number(velocityBox.value), linkedDem]);
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
          'The steps or the DEM have changed since this draft was made. Process again ' +
          'to apply them.';
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
      try {
        await RIDAL.fetchJson(RIDAL.apiPath('processing', 'assignments', radargramId), {
          method: 'DELETE',
        });
      } catch (error) {
        // Already out of its profile is what was asked for.
        if (error.status !== 404) throw error;
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
      hidden: `In ${name}, but hidden in this project, so it is left alone.`,
    };
    standingText.textContent = says[status.state] || status.state;
    standing.classList.toggle('warning', status.state === 'stale');
    useProfile.hidden = ['current', 'no_raw_data', 'hidden'].includes(status.state);
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

  /** Whether someone else is processing this radargram: a draft of
   * theirs, or a batch. The caller's own drafts are this dialog's business
   * already. Advice, so nothing waits on it. */
  const activityLine = document.getElementById('process-activity');
  async function showActivity() {
    activityLine.hidden = true;
    if (radargramId === null) return;
    let activity = [];
    try {
      activity = (
        await RIDAL.fetchJson(
          `${RIDAL.apiPath('processing', 'activity')}?radargram_id=${encodeURIComponent(radargramId)}`,
        )
      ).activity;
    } catch {
      return;
    }
    const others = activity.filter((a) => !(a.mine && a.kind === 'draft'));
    if (!others.length) return;
    activityLine.textContent =
      others.map((a) => `${RIDAL.describeActivity(a)}.`).join(' ') +
      ' Applying a draft here may replace what that makes, or be replaced by it.';
    activityLine.hidden = false;
  }

  /** What a source that names no CRS is processed in: the project's, when
   * it sets one, else the zone that suits the track. Said in the CRS box,
   * so leaving it empty is a choice someone can see. */
  const automaticCrs = crsBox.placeholder;
  async function loadProjectCrs() {
    try {
      const settings = await RIDAL.fetchJson(RIDAL.apiPath('settings'));
      crsBox.placeholder = settings.processing_crs
        ? `The project's: ${settings.processing_crs}`
        : automaticCrs;
    } catch {
      crsBox.placeholder = automaticCrs;
    }
  }

  /** The profiles, and which one this radargram is in, if any. */
  async function loadAssignment() {
    await Promise.all([loadProfiles(), loadDems(), loadProjectCrs()]);
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
    showActivity();
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
      afterApply(true);
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
    afterApply(false);
  }

  /** Where to go once applied: the new radargram, or this page again to
   * show what replaced it. Deferred while the offer below is open. */
  let leaveTo = null;
  function leave() {
    const to = leaveTo;
    leaveTo = null;
    if (to === 'view' && !window.RIDAL_VIEWER) window.location.href = RIDAL.pagePath('view', radargramId);
    else window.location.reload();
  }

  /** The profile change applying this draft would make the profile's:
   * its steps or velocity, when they are not the profile's current ones. */
  function profileChange() {
    if (!assignment) return null;
    const profile = profiles.find((p) => p.id === assignment.profile);
    const current = profile?.versions.find((v) => v.version === profile.current_version);
    if (!current) return null;
    const steps = linesIn(stepsBox);
    const velocity = Number(velocityBox.value);
    if (JSON.stringify(steps) === JSON.stringify(current.steps) && velocity === current.medium_velocity) {
      return null;
    }
    const others = profile.members.filter((id) => id !== radargramId);
    return { profile, next: current.version + 1, steps, velocity, others };
  }

  const offer = document.getElementById('process-offer');
  const offerText = document.getElementById('process-offer-text');
  const offerYes = document.getElementById('process-offer-yes');
  const offerNo = document.getElementById('process-offer-no');
  let offered = null;

  /** Applied. With steps of its own, offer to make them the profile's next
   * version, and to process the profile's other radargrams with it; the
   * one just applied is then current, so it is not among them. */
  function afterApply(isNew) {
    leaveTo = isNew ? 'view' : 'reload';
    offered = profileChange();
    if (!offered) {
      leave();
      return;
    }
    const { profile, next, others } = offered;
    const n = others.length;
    // What was applied is done with: the question is all that is left.
    progress.textContent = '';
    status.textContent = '';
    draftStage.hidden = true;
    result.hidden = true;
    headline.textContent = `Applied to ${radargramId}.`;
    detail.replaceChildren();
    for (const button of [run, toggle, commit, confirm]) button.hidden = true;
    offerText.textContent =
      `${radargramId} was processed with steps or a velocity other than ${profile.name}'s. ` +
      `Make them version ${next} of ${profile.name}` +
      (n === 0 ? '?' : n === 1
        ? ', and process its other radargram with them, applying it without review?'
        : `, and process its ${n} other radargrams with them, applying each without review?`);
    offerYes.textContent = n
      ? `Save as version ${next} and process ${n} more`
      : `Save as version ${next}`;
    offerYes.disabled = false;
    offerNo.textContent = 'No, only this radargram';
    offer.hidden = false;
    document.getElementById('process-close').textContent = 'Close';
  }

  offerNo.addEventListener('click', () => dialog.close());
  offerYes.addEventListener('click', async () => {
    if (!offered) return;
    const { profile, next, steps, velocity, others } = offered;
    offerYes.disabled = true;
    errorBox.hidden = true;
    progress.textContent = `Saving version ${next} of ${profile.name}…`;
    try {
      // Against the profile as it is now, so a change someone made since
      // the dialog opened is refused rather than overwritten.
      const read = await fetch(RIDAL.apiPath('processing', 'profiles', profile.id));
      if (!read.ok) throw new Error(RIDAL.upstreamMessage(read.status));
      const now = await read.json();
      if (now.current_version !== next - 1) {
        throw new Error(
          `${profile.name} has changed since this dialog opened (it is at version ` +
            `${now.current_version} now). Change it on the Processing page instead.`,
        );
      }
      const saved = await RIDAL.fetchJson(RIDAL.apiPath('processing', 'profiles', profile.id), {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json', 'If-Match': read.headers.get('ETag') },
        body: JSON.stringify({
          name: now.name,
          description: now.description || '',
          steps,
          medium_velocity: velocity,
        }),
      });
      // Written differently but the same steps once read: no new version,
      // and nothing for the others to catch up with.
      if (saved.current_version !== next) {
        offerText.textContent =
          `These are ${profile.name}'s steps already, written differently; nothing ` +
          'was changed.';
        offerYes.hidden = true;
        offerNo.textContent = 'Close';
        progress.textContent = '';
        return;
      }
      let said = `Saved as version ${next} of ${profile.name}.`;
      let follow = false;
      if (others.length) {
        progress.textContent = 'Starting a batch for the others…';
        try {
          const batch = await RIDAL.fetchJson(
            RIDAL.apiPath('processing', 'profiles', profile.id, 'reprocess'),
            {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({}),
            },
          );
          const waiting = batch.items.filter((i) => i.outcome === 'waiting').length;
          said += ` Processing ${waiting} other radargram${waiting === 1 ? '' : 's'}; follow ` +
            `${waiting === 1 ? 'it' : 'them'} on the`;
          follow = true;
        } catch (error) {
          // Nothing else could be processed (no raw data, say): the
          // version is saved all the same.
          said += ` The others were not processed: ${error.message}`;
        }
      }
      offerText.replaceChildren(said);
      if (follow) {
        const link = document.createElement('a');
        link.href = RIDAL.pagePath('processing');
        link.textContent = 'Processing page';
        offerText.append(' ', link, '.');
      }
      offerYes.hidden = true;
      offerNo.textContent = 'Close';
      progress.textContent = '';
    } catch (error) {
      progress.textContent = '';
      offerYes.disabled = false;
      fail(`Could not save the profile (${error.message}).`);
    }
  });

  document.getElementById('process-close').addEventListener('click', () => dialog.close());
  dialog.addEventListener('close', () => {
    if (uploading) uploading.abort();
    // Applied, with the offer open: show what was applied now.
    if (leaveTo) {
      offer.hidden = true;
      leave();
      return;
    }
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
