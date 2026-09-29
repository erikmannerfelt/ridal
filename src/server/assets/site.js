/* The site home: projects and server-wide accounts (#214).
 *
 * First-party, embedded via assets.rs, loaded after app.js. Deliberately
 * NOT under assets/vendor/ -- scripts/vendor_leaflet.sh does `rm -rf` on
 * that directory.
 *
 * Loaded only on the landing page of a site, and every control it wires is
 * rendered only for a server administrator, so a non-admin visitor gets an
 * empty script rather than a broken page.
 *
 * Wrapped in an IIFE so it declares nothing globally; `assets.rs` has a
 * test that fails if any two scripts on a page collide.
 */
(function () {
  "use strict";

  const byId = (id) => document.getElementById(id);
  const errorBox = () => byId("site-error");

  const showError = (message) => {
    const box = errorBox();
    if (!box) return;
    RIDAL.setMessage(box, message);
    box.hidden = false;
  };
  const clearError = () => {
    const box = errorBox();
    if (box) box.hidden = true;
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
        parsed?.error?.message || RIDAL.upstreamMessage(response.status),
      );
    }
    return parsed;
  }

  /* ---- My site settings --------------------------------------------- */

  const siteSettingsForm = byId("my-site-settings-form");
  if (siteSettingsForm) {
    siteSettingsForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      const status = byId("site-settings-status");
      status.textContent = "Saving…";
      const theme = byId("site-theme").value || null;
      try {
        const saved = await send("PUT", RIDAL.siteApiPath("site", "preferences"), {
          theme,
        });
        // Applied to the page being looked at, not only stored: a theme
        // that took effect on the next page load would read as a setting
        // that did not work. The server writes the same attribute into
        // every page it renders from here on.
        if (saved.theme) {
          document.documentElement.dataset.theme = saved.theme;
        } else {
          delete document.documentElement.dataset.theme;
        }
        status.textContent = "Saved";
      } catch (error) {
        showError(error.message);
        status.textContent = "";
      }
    });
  }

  /* ---- New project -------------------------------------------------- */

  const newProjectForm = byId("new-project");
  if (newProjectForm) {
    const keyInput = newProjectForm.elements.key;
    const nameInput = newProjectForm.elements.name;
    const updateKeyPlaceholder = () => {
      keyInput.placeholder = RIDAL.slugify(nameInput.value) || "project-key";
    };
    nameInput.addEventListener("input", updateKeyPlaceholder);
    updateKeyPlaceholder();

    newProjectForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      const status = byId("new-project-status");
      const name = nameInput.value.trim();
      const key = keyInput.value.trim() || RIDAL.slugify(name);
      if (!key) {
        showError(
          `No key could be derived from "${name}". Type one in the key box.`,
        );
        return;
      }
      status.textContent = "Creating…";
      try {
        await send("POST", RIDAL.siteApiPath("projects"), { key, name });
        // Reloaded rather than appended: the list is rendered from the
        // server's own view of the registry, and a fresh project has a
        // directory behind it that only the server knows about.
        window.location.reload();
      } catch (error) {
        showError(error.message);
        status.textContent = "";
      }
    });
  }

  /* ---- Projects ----------------------------------------------------- */

  function projectOf(element) {
    return element.closest("li[data-project-key]");
  }

  /* Menu items close the menu they came from, the way a radargram's do:
   * app.js only closes a menu on an outside click or Escape. */
  function closeMenus() {
    for (const menu of document.querySelectorAll("details.site-menu[open]")) {
      menu.open = false;
    }
  }

  /* Rename, archive and delete sit in the card's Edit menu, mirroring a
   * radargram card. Rename opens a dialog rather than turning the title
   * into an input, so the shared menu stays a menu. */
  const renameDialog = byId("rename-project-dialog");
  const renameName = byId("rename-project-name");
  let renaming = null;

  for (const button of document.querySelectorAll("button.rename-project")) {
    button.addEventListener("click", () => {
      const card = projectOf(button);
      renaming = card;
      renameName.value = card.querySelector(".project-name").textContent.trim();
      byId("rename-project-key").textContent = card.dataset.projectKey;
      byId("rename-project-error").hidden = true;
      closeMenus();
      renameDialog.showModal();
      renameName.focus();
      renameName.select();
    });
  }

  if (byId("rename-project-save")) {
    byId("rename-project-save").addEventListener("click", async () => {
      if (!renaming) return;
      const key = renaming.dataset.projectKey;
      try {
        await send("PATCH", RIDAL.siteApiPath("projects", key), {
          name: renameName.value.trim(),
        });
        window.location.reload();
      } catch (error) {
        const box = byId("rename-project-error");
        RIDAL.setMessage(box, error.message);
        box.hidden = false;
      }
    });
  }

  if (byId("rename-project-close")) {
    byId("rename-project-close").addEventListener("click", () =>
      renameDialog.close(),
    );
  }

  for (const button of document.querySelectorAll("button.archive-project")) {
    button.addEventListener("click", async () => {
      clearError();
      const card = projectOf(button);
      const key = card.dataset.projectKey;
      const archived = button.dataset.archived === "true";
      closeMenus();
      const status = card.querySelector(".project-status");
      status.textContent = "Saving…";
      try {
        await send(
          "POST",
          RIDAL.siteApiPath("projects", key, archived ? "unarchive" : "archive"),
          {},
        );
        window.location.reload();
      } catch (error) {
        showError(error.message);
        status.textContent = "";
      }
    });
  }

  for (const button of document.querySelectorAll("button.delete-project")) {
    button.addEventListener("click", async () => {
      clearError();
      const card = projectOf(button);
      const key = card.dataset.projectKey;
      const name = card.querySelector(".project-name").textContent.trim();
      closeMenus();
      const confirmed = window.confirm(
        `Delete the project "${name}" and everything in it?\n\nThis removes ` +
          `the project directory, including its interpretations and cache. ` +
          `It cannot be undone. Unarchive it instead to keep working on it.`,
      );
      if (!confirmed) return;
      const status = card.querySelector(".project-status");
      status.textContent = "Deleting…";
      try {
        await send("DELETE", RIDAL.siteApiPath("projects", key), {});
        window.location.reload();
      } catch (error) {
        showError(error.message);
        status.textContent = "";
      }
    });
  }

  /* ---- Accounts ----------------------------------------------------- */

  const accountsTable = byId("accounts-table");
  const addAccountForm = byId("add-account");
  const accountError = byId("account-error");

  /* Account failures are shown beside the accounts section, not in the
   * top-of-page box a reader on this section would never look at. */
  const showAccountError = (message) => {
    if (!accountError) return;
    RIDAL.setMessage(accountError, message);
    accountError.hidden = false;
  };
  const clearAccountError = () => {
    if (!accountError) return;
    accountError.hidden = true;
    accountError.textContent = "";
  };
  if (addAccountForm) {
    addAccountForm.elements.name.addEventListener("input", clearAccountError);
  }

  function showInvite(name, result) {
    byId("invite-who").textContent = name;
    byId("invite-days").textContent = String(result.invite_ttl_days);
    // Built from this page's own origin rather than from anything the
    // server guessed: Ridal is normally behind a reverse proxy and has no
    // reliable idea what address the browser reached it on.
    byId("invite-link").textContent =
      window.location.origin + result.invite_path;
    byId("invite-result").hidden = false;
  }

  function accountRow(account) {
    const row = document.createElement("tr");

    const name = document.createElement("td");
    name.textContent = account.name;
    row.appendChild(name);

    /* The server-admin flag applies on change rather than waiting for a
     * Save, with its own confirmation, because there is no button to go
     * quiet afterwards. The last administrator cannot be demoted -- the
     * server refuses and the box is put back. */
    const adminCell = document.createElement("td");
    const admin = document.createElement("input");
    admin.type = "checkbox";
    admin.checked = Boolean(account.server_admin);
    const adminStatus = document.createElement("span");
    adminStatus.className = "row-status";
    /* The checkbox only asks: the change goes through a confirmation
     * dialog, because server administration reaches every project. */
    admin.addEventListener("change", () => {
      askAdminChange(account.name, admin.checked, admin, adminStatus);
    });
    // Granting needs an account that has already set a password; a revoke is
    // always allowed.
    if (!account.activated && !account.server_admin) {
      admin.disabled = true;
      admin.title =
        "They must set a password from the invite before they can be a " +
        "server administrator.";
    }
    adminCell.append(admin, adminStatus);
    row.appendChild(adminCell);

    const status = document.createElement("td");
    // Three states worth telling apart: never activated, active, and active
    // with a reset outstanding.
    if (!account.activated) {
      status.textContent = account.invite_pending
        ? "invited, not yet activated"
        : "no password and no invite";
    } else {
      status.textContent = account.invite_pending ? "reset pending" : "active";
    }
    row.appendChild(status);

    const actions = document.createElement("td");
    const group = document.createElement("div");
    group.className = "row-actions";
    const reset = document.createElement("button");
    reset.type = "button";
    reset.textContent = account.activated ? "Reset password" : "New invite link";
    reset.addEventListener("click", () => reissue(account.name));
    group.appendChild(reset);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.textContent = "Remove";
    remove.addEventListener("click", () => removeAccount(account.name));
    group.appendChild(remove);
    actions.appendChild(group);
    row.appendChild(actions);

    return row;
  }

  async function loadAccounts() {
    if (!accountsTable) return;
    try {
      const body = await RIDAL.fetchJson(RIDAL.siteApiPath("accounts"));
      const rows = (body.accounts || []).map(accountRow);
      const tbody = accountsTable.querySelector("tbody");
      tbody.replaceChildren(...rows);
    } catch (error) {
      showError(`Could not load accounts: ${error.message}`);
    }
  }

  async function reissue(name) {
    clearError();
    clearAccountError();
    try {
      const result = await send(
        "POST",
        RIDAL.siteApiPath("accounts", name, "invite"),
        {},
      );
      showInvite(name, result);
      await loadAccounts();
    } catch (error) {
      showAccountError(error.message);
    }
  }

  async function removeAccount(name) {
    const confirmed = window.confirm(
      `Remove the account "${name}"?\n\nTheir interpretations are kept -- an ` +
        `account going away does not unmake the picks. Their memberships are ` +
        `left in place, so recreating the same name reconnects them.`,
    );
    if (!confirmed) return;
    clearError();
    clearAccountError();
    try {
      await send("DELETE", RIDAL.siteApiPath("accounts", name), {});
      await loadAccounts();
    } catch (error) {
      showAccountError(error.message);
    }
  }

  if (addAccountForm) {
    const projectSelect = addAccountForm.elements.project;
    const memberControls = [
      addAccountForm.elements.role,
      addAccountForm.elements.download,
    ];
    /* A role and download scope only mean something inside a project, so
     * they are greyed out until one is chosen rather than looking like they
     * still apply to an account with no membership. */
    const syncProject = () => {
      const hasProject = Boolean(projectSelect.value);
      for (const control of memberControls) control.disabled = !hasProject;
    };
    projectSelect.addEventListener("change", syncProject);
    syncProject();

    addAccountForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      clearAccountError();
      const name = addAccountForm.elements.name.value.trim();
      const project = projectSelect.value || null;
      try {
        const result = await send("POST", RIDAL.siteApiPath("accounts"), {
          name,
          project,
          role: project ? addAccountForm.elements.role.value : null,
          download: project ? addAccountForm.elements.download.value : null,
        });
        addAccountForm.reset();
        syncProject();
        showInvite(name, result);
        await loadAccounts();
      } catch (error) {
        showAccountError(error.message);
      }
    });
  }

  /* ---- Bulk account creation ---------------------------------------- *
   *
   * The same shape the project accounts page had before the site took over
   * (#214): invite links are the safe default, generated passwords are
   * offered for a workshop with the risk stated plainly. Both may name a
   * project, granted on redemption for an invite and directly for a
   * password.
   */

  function downloadCsv(filename, rows) {
    const csv = rows
      .map((row) =>
        row.map((value) => `"${String(value).replaceAll('"', '""')}"`).join(","),
      )
      .join("\n");
    const link = document.createElement("a");
    link.href = URL.createObjectURL(new Blob([csv], { type: "text/csv" }));
    link.download = filename;
    link.click();
    URL.revokeObjectURL(link.href);
  }

  function renderBulkResult(result, mode) {
    const box = byId("bulk-result");
    box.replaceChildren();
    const heading = document.createElement("h3");
    heading.textContent =
      mode === "passwords" ? "Generated passwords" : "Invite links";
    box.appendChild(heading);
    const note = document.createElement("p");
    note.className = "hint";
    note.textContent =
      mode === "passwords"
        ? `${result.advisory} These passwords are shown once and are not stored.`
        : "Each link works once. Send each person only their own link.";
    box.appendChild(note);

    const table = document.createElement("table");
    table.className = "layers-table bulk-table";
    const header = document.createElement("tr");
    for (const label of mode === "passwords"
      ? ["Name", "Password"]
      : ["Name", "Invite link"]) {
      const cell = document.createElement("th");
      cell.textContent = label;
      header.appendChild(cell);
    }
    table.appendChild(header);
    const rows = [["name", mode === "passwords" ? "password" : "invite link"]];
    for (const user of result.users || []) {
      const row = document.createElement("tr");
      const name = document.createElement("td");
      name.textContent = user.name;
      const value = document.createElement("td");
      const text =
        mode === "passwords"
          ? user.password
          : window.location.origin + user.invite_path;
      value.textContent = text;
      row.append(name, value);
      table.appendChild(row);
      rows.push([user.name, text]);
    }
    box.appendChild(table);
    const actions = document.createElement("div");
    actions.className = "bulk-actions";
    const print = document.createElement("button");
    print.type = "button";
    print.textContent = "Print";
    print.addEventListener("click", () => window.print());
    const csv = document.createElement("button");
    csv.type = "button";
    csv.textContent = "Download CSV";
    csv.addEventListener("click", () => downloadCsv(`ridal-${mode}.csv`, rows));
    actions.append(print, csv);
    box.appendChild(actions);
    box.hidden = false;
  }

  const bulkForm = byId("bulk-account-form");
  if (bulkForm) {
    const bulkWarning = byId("bulk-warning");
    const showBulkWarning = (message) => {
      bulkWarning.textContent = message;
      bulkWarning.hidden = false;
    };
    const clearBulkWarning = () => {
      bulkWarning.hidden = true;
      bulkWarning.textContent = "";
    };

    const randomNames = byId("bulk-random-names");
    const prefixInput = bulkForm.elements.prefix;
    /* Random names make the prefix irrelevant, so the field greys out
     * rather than looking like it still contributes to the batch. */
    const syncPrefixState = () => {
      prefixInput.disabled = randomNames.checked;
    };
    randomNames.addEventListener("change", () => {
      syncPrefixState();
      clearBulkWarning();
    });
    syncPrefixState();

    const bulkBody = () => ({
      prefix: randomNames.checked ? "" : prefixInput.value.trim(),
      count: Number(bulkForm.elements.count.value),
      random_names: randomNames.checked,
      role: bulkForm.elements.role.value,
      download: bulkForm.elements.download.value,
      project: bulkForm.elements.project.value || null,
    });

    bulkForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearBulkWarning();
      try {
        const result = await send(
          "POST",
          RIDAL.siteApiPath("accounts", "bulk", "invites"),
          bulkBody(),
        );
        renderBulkResult(result, "invites");
        await loadAccounts();
      } catch (error) {
        showBulkWarning(error.message);
      }
    });

    /* Acknowledging the risk is the fix the warning asks for, so ticking
     * the box takes the warning away rather than leaving it to be
     * dismissed some other way. */
    byId("bulk-risk").addEventListener("change", clearBulkWarning);

    byId("bulk-passwords").addEventListener("click", async () => {
      if (bulkForm.elements.role.value === "admin") {
        showBulkWarning("Administrator accounts must use one-time invite links.");
        return;
      }
      if (!byId("bulk-risk").checked) {
        showBulkWarning(
          "Generated passwords are shared secrets and less safe than invite " +
            "links. Check the acknowledgement above, then press the button again.",
        );
        return;
      }
      clearBulkWarning();
      try {
        const result = await send(
          "POST",
          RIDAL.siteApiPath("accounts", "bulk", "passwords"),
          { ...bulkBody(), acknowledge_risk: true },
        );
        renderBulkResult(result, "passwords");
        await loadAccounts();
      } catch (error) {
        showBulkWarning(error.message);
      }
    });
  }

  /* ---- History ------------------------------------------------------ */

  /* Loaded the first time the disclosure is opened, so the settings page
   * does not fetch the whole ledger for someone who never looks. */
  const history = byId("history");
  if (history) {
    let loaded = false;
    history.addEventListener("toggle", async () => {
      if (!history.open || loaded) return;
      loaded = true;
      const box = byId("history-table");
      try {
        const body = await RIDAL.fetchJson(RIDAL.siteApiPath("site", "audit"));
        box.replaceChildren(RIDAL.historyTable(body.entries || []));
      } catch (error) {
        // Let the next open try again.
        loaded = false;
        box.replaceChildren();
        showError(`Could not load the history: ${error.message}`);
      }
    });
  }

  /* ---- Memberships overview ----------------------------------------- */

  /* Which projects each account belongs to, for a server administrator.
   * Read-only, and loaded when the disclosure is opened. */
  function membershipsOverview(accounts) {
    const list = document.createElement("ul");
    list.className = "membership-list";
    for (const account of accounts) {
      const item = document.createElement("li");
      const name = document.createElement("strong");
      name.textContent = account.name;
      item.appendChild(name);
      const memberships = account.memberships || [];
      const text =
        memberships.length === 0
          ? " — no project memberships"
          : ` — ${memberships
              .map((m) => `${m.project_name} (${m.role} · ${m.download})`)
              .join("; ")}`;
      item.appendChild(document.createTextNode(text));
      list.appendChild(item);
    }
    return list;
  }

  const memberships = byId("memberships");
  if (memberships) {
    let loaded = false;
    memberships.addEventListener("toggle", async () => {
      if (!memberships.open || loaded) return;
      loaded = true;
      const box = byId("memberships-overview");
      try {
        const body = await RIDAL.fetchJson(
          RIDAL.siteApiPath("site", "memberships"),
        );
        box.replaceChildren(membershipsOverview(body.accounts || []));
      } catch (error) {
        loaded = false;
        box.replaceChildren();
        showError(`Could not load memberships: ${error.message}`);
      }
    });
  }

  /* ---- Confirming a server-administrator change --------------------- */

  /* The checkbox asks; this dialog decides. Server administration reaches
   * every project, so it is never applied on a single click. */
  const adminDialog = byId("admin-confirm-dialog");
  let pendingAdmin = null;

  function askAdminChange(account, grant, checkbox, status) {
    pendingAdmin = { account, grant, checkbox, status };
    byId("admin-confirm-title").textContent = grant
      ? "Make a server administrator"
      : "Remove server administration";
    byId("admin-confirm-message").textContent = grant
      ? `Make ${account} a server administrator? They can create projects ` +
        `and accounts, and act as an administrator in every project.`
      : `Remove ${account}'s server administrator rights? They keep whatever ` +
        `membership and role they have in each project.`;
    byId("admin-confirm-error").hidden = true;
    adminDialog.showModal();
  }

  if (adminDialog) {
    byId("admin-confirm-cancel").addEventListener("click", () => {
      // Put the box back to what the server still believes.
      if (pendingAdmin) pendingAdmin.checkbox.checked = !pendingAdmin.grant;
      pendingAdmin = null;
      adminDialog.close();
    });

    byId("admin-confirm-ok").addEventListener("click", async () => {
      if (!pendingAdmin) return;
      const { account, grant, checkbox, status } = pendingAdmin;
      const errorBox = byId("admin-confirm-error");
      errorBox.hidden = true;
      status.textContent = "Saving…";
      try {
        await send("PUT", RIDAL.siteApiPath("accounts", account), {
          server_admin: grant,
        });
        pendingAdmin = null;
        adminDialog.close();
        await loadAccounts();
      } catch (error) {
        // Leave the dialog open with the refusal, and put the box back.
        RIDAL.setMessage(errorBox, error.message);
        errorBox.hidden = false;
        checkbox.checked = !grant;
        status.textContent = "";
      }
    });
  }

  loadAccounts();
})();
