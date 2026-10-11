/* --- Process a radargram on the server (#382) -----------------------------
 *
 * For an operator, on a radargram whose raw data the project has: steps in,
 * a draft out, a look at it beside what is served now, and a commit that
 * goes through the same consequence report as a replace.
 *
 * The draft is the server's: this dialog only asks for it, waits for it,
 * and shows it. Closing the dialog throws away a draft that was not
 * committed, since it is private to this person and nobody else can reach
 * it from here.
 */
(function setupProcessing() {
  const open = document.getElementById('process-open');
  const dialog = document.getElementById('process-dialog');
  if (!open || !dialog || !window.RIDAL_VIEWER) return;
  const radargramId = window.RIDAL_VIEWER.radargramId;

  const status = document.getElementById('process-status');
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

  /** The button only appears where processing can work: the project has
   * this radargram's raw data. Asked once, on load. */
  (async function offer() {
    try {
      await RIDAL.fetchJson(RIDAL.apiPath('sources', radargramId));
      open.hidden = false;
    } catch {
      // No raw data, or no project: nothing to offer.
    }
  })();

  /** Start from the steps the served file was processed with, one per
   * line, so a change is an edit rather than a retyping. */
  async function prefill() {
    if (stepsBox.value.trim() !== '') return;
    try {
      const attributes = await RIDAL.fetchJson(
        RIDAL.apiPath('datasets', radargramId, 'attributes'),
      );
      stepsBox.value = (attributes.processing_steps || []).join('\n');
    } catch {
      // Not served yet, or unreadable: start empty.
    }
  }

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

  open.addEventListener('click', async () => {
    reset();
    status.textContent =
      'Change the steps and process. Nothing changes until you make the draft the radargram.';
    dialog.showModal();
    await prefill();
  });

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
      // A new radargram: it is in the catalog now.
      job = null;
      window.location.reload();
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
