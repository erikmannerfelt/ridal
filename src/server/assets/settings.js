/* The settings page: my preferences, the project defaults, and access.
 *
 * First-party, embedded via assets.rs, loaded after app.js. Deliberately
 * NOT under assets/vendor/ -- scripts/vendor_leaflet.sh does `rm -rf` on
 * that directory.
 *
 * Three sections in one file because they share one load: the settings
 * endpoint already answers what every one of them needs to render, and
 * splitting them would mean three requests to draw one page.
 *
 * Wrapped in an IIFE so it declares nothing globally; `assets.rs` has a
 * test that fails if any two scripts on a page collide.
 */
(function () {
  "use strict";

  const byId = (id) => document.getElementById(id);

  const projectForm = byId("settings-form");
  const myForm = byId("my-settings-form");
  const accessSection = byId("access-section");
  if (!projectForm && !myForm && !accessSection) return; // Not a project.

  const errorBox = byId("settings-error");

  let canEditProject = false;
  let canEditAccess = false;
  let xscales = [];
  let profiles = [];
  /* Offered by the server so a stored value always has an option to select,
   * and so these lists cannot drift from the download dialogs' (#166). */
  let spacings = [];
  let formats = [];
  let themes = [];

  const showError = (message) => {
    errorBox.textContent = message;
    errorBox.hidden = false;
  };
  const clearError = () => {
    errorBox.hidden = true;
    errorBox.textContent = "";
  };
  const setStatus = (id, message) => {
    const element = byId(id);
    if (element) element.textContent = message;
  };

  /* Send JSON and surface the server's own message on failure. */
  async function send(method, url, body) {
    const response = await fetch(url, {
      method,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (response.status === 204) return null;
    const parsed = await response.json().catch(() => null);
    if (!response.ok) {
      throw new Error(
        parsed?.error?.message || `Request failed (${response.status}).`,
      );
    }
    return parsed;
  }

  /* Fill a <select> with profile names. `emptyLabel` names the "unset"
   * choice, which is a real option rather than a placeholder: it clears
   * the setting instead of storing the name of the fallback, which is what
   * lets a later change to the layer below reach whoever never chose. */
  function fillProfiles(select, emptyLabel, selected) {
    if (!select) return;
    const options = [new Option(emptyLabel, "")];
    for (const name of profiles) options.push(new Option(name, name));
    select.replaceChildren(...options);
    select.value = selected || "";
  }

  /* Offered scales come from the server, so a stored value always has an
   * entry to select. No empty option: unlike a profile name, 1x *is* the
   * neutral value, so "no preference" and "1x" are the same choice and
   * offering both would be a distinction without a difference. */
  function fillScales(select, selected) {
    if (!select) return;
    select.replaceChildren(...xscales.map((s) => new Option(s.label, s.text)));
    select.value = String(selected || 1);
  }

  /* Fill a <select> from the server's `{value, label}` list. `emptyLabel`,
   * when given, is the "no choice" option -- a real entry rather than a
   * placeholder, since choosing it clears the setting instead of storing
   * the name of the fallback. */
  function fillOptions(select, options, selected, emptyLabel) {
    if (!select) return;
    const entries = emptyLabel === undefined ? [] : [new Option(emptyLabel, "")];
    for (const option of options) {
      entries.push(new Option(option.label, option.value));
    }
    select.replaceChildren(...entries);
    select.value = selected || "";
  }

  function fillNames(select, names, selected) {
    if (!select) return;
    select.replaceChildren(...names.map((name) => new Option(name, name)));
    if (selected) select.value = selected;
  }

  async function load() {
    clearError();
    let settings;
    try {
      settings = await RIDAL.fetchJson("/api/v1/project/settings");
    } catch (error) {
      showError(`Could not load settings: ${error.message}`);
      return;
    }
    canEditProject = Boolean(settings.can_edit_project);
    canEditAccess = Boolean(settings.can_edit_access);
    profiles = settings.profiles || [];
    xscales = settings.xscales || [];
    spacings = settings.spacings || [];
    formats = settings.formats || [];
    themes = (settings.themes || []).map((name) => ({
      value: name,
      // Capitalised here rather than server-side: these are two words shown
      // in one dropdown, not a vocabulary anything else reads.
      label: name.charAt(0).toUpperCase() + name.slice(1),
    }));

    fillProfiles(byId("my-profile"), "Project default", settings.my_profile);
    fillScales(byId("my-xscale"), settings.my_xscale);
    fillOptions(byId("my-theme"), themes, settings.my_theme, "Follow this device");
    // Absent means shown, which is what the viewer did before the toggle
    // existed -- so only an explicit `false` unticks it.
    const showPicks = byId("my-show-picks");
    if (showPicks) showPicks.checked = settings.my_show_picks !== false;
    fillOptions(byId("my-spacing"), spacings, settings.my_spacing, "Project default");
    fillOptions(byId("my-format"), formats, settings.my_format, "Project default");

    fillProfiles(
      byId("default-profile"),
      "Ridal default",
      settings.default_profile,
    );
    fillScales(byId("default-xscale"), settings.default_xscale);
    fillOptions(byId("default-spacing"), spacings, settings.default_spacing, "Ridal default");
    fillOptions(byId("default-format"), formats, settings.default_format, "Ridal default");
    for (const id of [
      "default-profile",
      "default-xscale",
      "default-spacing",
      "default-format",
    ]) {
      const select = byId(id);
      if (select) select.disabled = !canEditProject;
    }

    setStatus("settings-status", "");
    setStatus("my-settings-status", "");

    if (canEditAccess) await loadAccess();
  }

  if (myForm) {
    myForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      setStatus("my-settings-status", "Saving…");
      try {
        const saved = await send("PUT", "/api/v1/preferences", {
          render_profile: byId("my-profile").value || null,
          x_scale: Number(byId("my-xscale").value) || null,
          theme: byId("my-theme").value || null,
          show_picks: byId("my-show-picks").checked,
          level2_spacing: byId("my-spacing").value || null,
          level2_format: byId("my-format").value || null,
        });
        byId("my-profile").value = saved.render_profile || "";
        // 1x is stored as absent, so read it back the way it was sent.
        byId("my-xscale").value = String(saved.x_scale || 1);
        byId("my-theme").value = saved.theme || "";
        byId("my-show-picks").checked = saved.show_picks !== false;
        byId("my-spacing").value = saved.level2_spacing || "";
        byId("my-format").value = saved.level2_format || "";
        // Applied to the page being looked at, not only stored: a theme
        // that took effect on the next page load would read as a setting
        // that did not work. The server writes the same attribute into
        // every page it renders from here on.
        if (saved.theme) {
          document.documentElement.dataset.theme = saved.theme;
        } else {
          delete document.documentElement.dataset.theme;
        }
        setStatus("my-settings-status", "Saved");
      } catch (error) {
        showError(error.message);
        setStatus("my-settings-status", "");
      }
    });
  }

  if (projectForm) {
    projectForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (!canEditProject) return;
      clearError();
      setStatus("settings-status", "Saving…");
      try {
        const saved = await send("PUT", "/api/v1/project/settings", {
          default_profile: byId("default-profile").value || null,
          default_xscale: Number(byId("default-xscale").value) || null,
          default_spacing: byId("default-spacing").value || null,
          default_format: byId("default-format").value || null,
        });
        byId("default-profile").value = saved.default_profile || "";
        byId("default-xscale").value = String(saved.default_xscale || 1);
        // Both are stored as absent when they are the neutral answer, so
        // they read back as the "Ridal default" option rather than as the
        // value that was sent.
        byId("default-spacing").value = saved.default_spacing || "";
        byId("default-format").value = saved.default_format || "";
        // Naming the file is the point: the change lands somewhere the user
        // can go and look at, which is not obvious from a dropdown.
        setStatus("settings-status", "Saved to ridal.toml");
      } catch (error) {
        showError(error.message);
        setStatus("settings-status", "");
      }
    });
  }

  /* ---- Access ------------------------------------------------------- */

  let access = null;

  async function loadAccess() {
    try {
      access = await RIDAL.fetchJson("/api/v1/users");
    } catch (error) {
      showError(`Could not load accounts: ${error.message}`);
      return;
    }

    const addForm = byId("add-user");
    if (addForm) {
      fillNames(addForm.elements.role, access.roles || [], "picker");
      // `all` to match what the API does when the field is omitted, and
      // what the README says a new account gets. The form always submits
      // its selection, so a different preselection here would silently
      // be the real default and the documented one would be fiction.
      fillNames(addForm.elements.download, access.download_scopes || [], "all");
    }
    fillNames(
      byId("anonymous-download"),
      access.download_scopes || [],
      access.anonymous_download,
    );
    byId("require-auth").checked = Boolean(access.require_auth_to_read);

    renderUsers();
  }

  function renderUsers() {
    const body = byId("users-table").querySelector("tbody");
    body.replaceChildren();
    for (const user of access.users || []) {
      body.appendChild(userRow(user));
    }
  }

  function userRow(user) {
    const row = document.createElement("tr");

    const name = document.createElement("td");
    name.textContent = user.name;
    row.appendChild(name);

    /* Both selects apply on change rather than waiting for a Save, so
     * each carries its own confirmation: there is no button to go quiet
     * afterwards, and the only Save button on this page belongs to a
     * different setting entirely. */
    const cellWithSelect = (options, selected, change) => {
      const cell = document.createElement("td");
      const select = document.createElement("select");
      fillNames(select, options, selected);
      const status = document.createElement("span");
      status.className = "row-status";
      select.addEventListener("change", () =>
        updateUser(user.name, change(select.value), select, selected, status),
      );
      cell.append(select, status);
      return cell;
    };

    row.appendChild(
      cellWithSelect(access.roles || [], user.role, (value) => ({ role: value })),
    );
    row.appendChild(
      cellWithSelect(access.download_scopes || [], user.download, (value) => ({
        download: value,
      })),
    );

    const status = document.createElement("td");
    // Three states worth telling apart: never activated, active, and active
    // with a reset outstanding.
    if (!user.activated) {
      status.textContent = user.invite_pending
        ? "invited, not yet activated"
        : "no password and no invite";
    } else {
      status.textContent = user.invite_pending ? "reset pending" : "active";
    }
    row.appendChild(status);

    /* One shape for both, differing only in colour: they are the same
     * kind of control and one of them is more serious, which is what the
     * colour is for. */
    const actions = document.createElement("td");
    const group = document.createElement("div");
    group.className = "row-actions";

    const reset = document.createElement("button");
    reset.type = "button";
    reset.textContent = user.activated ? "Reset password" : "New invite link";
    reset.addEventListener("click", () => reissue(user.name));
    group.appendChild(reset);

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.textContent = "Remove";
    remove.addEventListener("click", () => removeUser(user.name));
    group.appendChild(remove);

    actions.appendChild(group);
    row.appendChild(actions);

    return row;
  }

  async function updateUser(name, change, select, previous, status) {
    clearError();
    if (status) status.textContent = "Saving…";
    try {
      await send("PUT", `/api/v1/users/${encodeURIComponent(name)}`, change);
      // Redrawn from the server's answer, which also replaces this row --
      // so the "Saved" below is set on a row that is about to go. It is
      // still worth setting: a refusal leaves the old row in place, and
      // the difference between the two outcomes is the point.
      if (status) status.textContent = "Saved";
      await loadAccess();
    } catch (error) {
      showError(error.message);
      if (status) status.textContent = "";
      // Put the control back to what the server still believes, so the page
      // never shows a role that was refused.
      select.value = previous;
    }
  }

  function showInvite(name, result) {
    const box = byId("invite-result");
    byId("invite-who").textContent = name;
    byId("invite-days").textContent = String(result.invite_ttl_days);
    // Built from this page's own origin rather than from anything the
    // server guessed: Ridal is normally behind a reverse proxy and has no
    // reliable idea what address the browser reached it on.
    byId("invite-link").textContent =
      window.location.origin + result.invite_path;
    box.hidden = false;
  }

  async function reissue(name) {
    clearError();
    try {
      const result = await send(
        "POST",
        `/api/v1/users/${encodeURIComponent(name)}/invite`,
        {},
      );
      showInvite(name, result);
      await loadAccess();
    } catch (error) {
      showError(error.message);
    }
  }

  async function removeUser(name) {
    // Worth a confirmation, and worth saying what it does not do: the picks
    // are attributed data and stay.
    const confirmed = window.confirm(
      `Remove the account "${name}"?\n\nTheir interpretations are kept — an ` +
        `account going away does not unmake the picks. Only the account and ` +
        `their personal settings are removed.`,
    );
    if (!confirmed) return;
    clearError();
    try {
      await send("DELETE", `/api/v1/users/${encodeURIComponent(name)}`, {});
      await loadAccess();
    } catch (error) {
      showError(error.message);
    }
  }

  const addForm = byId("add-user");
  if (addForm) {
    addForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      const name = addForm.elements.name.value.trim();
      try {
        const result = await send("POST", "/api/v1/users", {
          name,
          role: addForm.elements.role.value,
          download: addForm.elements.download.value,
        });
        addForm.reset();
        showInvite(name, result);
        await loadAccess();
      } catch (error) {
        showError(error.message);
      }
    });
  }

  const accessForm = byId("access-form");
  if (accessForm) {
    accessForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      setStatus("access-status", "Saving…");
      try {
        await send("PUT", "/api/v1/access", {
          require_auth_to_read: byId("require-auth").checked,
          anonymous_download: byId("anonymous-download").value,
        });
        setStatus("access-status", "Saved");
      } catch (error) {
        showError(error.message);
        setStatus("access-status", "");
      }
    });
  }

  load();
})();
