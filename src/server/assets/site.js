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

  for (const form of document.querySelectorAll("form.rename-project")) {
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      const card = projectOf(form);
      const key = card.dataset.projectKey;
      const status = card.querySelector(".project-status");
      status.textContent = "Saving…";
      try {
        await send("PATCH", RIDAL.siteApiPath("projects", key), {
          name: form.elements.name.value.trim(),
        });
        window.location.reload();
      } catch (error) {
        showError(error.message);
        status.textContent = "";
      }
    });
  }

  for (const button of document.querySelectorAll("button.archive-project")) {
    button.addEventListener("click", async () => {
      clearError();
      const card = projectOf(button);
      const key = card.dataset.projectKey;
      const archived = button.dataset.archived === "true";
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
      const confirmed = window.confirm(
        `Delete the project "${name}" and everything in it?\n\nThis removes ` +
          `the project directory, including its interpretations and cache. ` +
          `It cannot be undone. Archive it instead to keep the data while ` +
          `making it read-only.`,
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
    admin.addEventListener("change", async () => {
      const previous = !admin.checked;
      adminStatus.textContent = "Saving…";
      try {
        await send("PUT", RIDAL.siteApiPath("accounts", account.name), {
          server_admin: admin.checked,
        });
        adminStatus.textContent = "Saved";
      } catch (error) {
        showError(error.message);
        admin.checked = previous;
        adminStatus.textContent = "";
      }
    });
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
    try {
      const result = await send(
        "POST",
        RIDAL.siteApiPath("accounts", name, "invite"),
        {},
      );
      showInvite(name, result);
      await loadAccounts();
    } catch (error) {
      showError(error.message);
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
    try {
      await send("DELETE", RIDAL.siteApiPath("accounts", name), {});
      await loadAccounts();
    } catch (error) {
      showError(error.message);
    }
  }

  if (addAccountForm) {
    addAccountForm.addEventListener("submit", async (event) => {
      event.preventDefault();
      clearError();
      const name = addAccountForm.elements.name.value.trim();
      try {
        const result = await send("POST", RIDAL.siteApiPath("accounts"), {
          name,
          server_admin: addAccountForm.elements.server_admin.checked,
        });
        addAccountForm.reset();
        showInvite(name, result);
        await loadAccounts();
      } catch (error) {
        showError(error.message);
      }
    });
  }

  loadAccounts();
})();
