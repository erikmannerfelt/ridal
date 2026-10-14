/* Layer definition management (the /layers page).
 *
 * First-party, embedded via assets.rs and loaded after app.js, so `RIDAL`
 * exists. Deliberately NOT under assets/vendor/ -- scripts/vendor_leaflet.sh
 * does `rm -rf` on that directory.
 *
 * The whole vocabulary is one document, so every edit is a full PUT of the
 * layer list carrying the ETag the page last read. That is why `etag` is
 * tracked here rather than being recomputed: it is what stops this page from
 * silently discarding a change made in another tab between load and save.
 */

const WRITABLE = window.RIDAL_LAYERS.writable;

const table = document.querySelector("#layers-table tbody");
const emptyNote = document.getElementById("layers-empty");
const errorBox = document.getElementById("layers-error");

const groupsList = document.getElementById("groups-list");
const groupsEmpty = document.getElementById("groups-empty");
const groupsActions = document.getElementById("groups-actions");

let layers = [];
// Mutually exclusive layer groups (#208). Membership is the union of a
// group's own `members` and every layer that lists the group in `layer.groups`
// -- a hand-edited file can express it either way, and the UI must show both.
let groups = [];
// The in-progress new group, if the user clicked "Add group". Kept out of
// `groups` until it has a name, because its id is generated from that name
// and is immutable afterwards.
let draftGroup = null;
let etag = null;
let usage = { counts: {}, undefined: {} };

function showError(message) {
  RIDAL.setMessage(errorBox, message);
  errorBox.hidden = false;
}

function clearError() {
  errorBox.hidden = true;
  errorBox.textContent = "";
}

function layerById(id) {
  return layers.find((layer) => layer.id === id);
}

/** Every layer in a group, from both representations (#208).
 *
 * `group.members` and `layer.groups` are two views of one relation, and a
 * hand-edited file may use either. Reading the union is what stops a layer
 * set from the layer side looking absent from a group it is in. */
function membersOf(group) {
  const members = [...(group.members || [])];
  for (const layer of layers) {
    if ((layer.groups || []).includes(group.id) && !members.includes(layer.id)) {
      members.push(layer.id);
    }
  }
  return members;
}

/** How many groups a layer is in, counting both representations. */
function groupCount(layer) {
  const ids = new Set(layer.groups || []);
  for (const group of groups) {
    if (membersOf(group).includes(layer.id)) ids.add(group.id);
  }
  return ids.size;
}

function addMember(group, layerId) {
  const members = membersOf(group);
  if (!members.includes(layerId)) {
    group.members = [...members, layerId];
  }
  render();
  save();
}

/** Remove a member from a group, clearing both representations.
 *
 * Clearing only `group.members` would leave `layer.groups` behind, and the
 * union read would put the chip straight back -- the `×` would silently do
 * nothing on a hand-edited file. */
function removeMember(group, layerId) {
  group.members = membersOf(group).filter((id) => id !== layerId);
  for (const layer of layers) {
    if (layer.groups) {
      layer.groups = layer.groups.filter((id) => id !== group.id);
      if (layer.groups.length === 0) delete layer.groups;
    }
  }
  render();
  save();
}

function addGroup() {
  draftGroup = { name: "", id: "", idEdited: false };
  renderGroups();
  const input = groupsList.querySelector(".group-draft input");
  if (input) input.focus();
}

function commitDraft() {
  const name = draftGroup.name.trim();
  if (name === "") {
    cancelDraft();
    return;
  }
  const taken = groups.map((group) => group.id);
  // The id autofills from the name but is hand-editable; sanitise it either
  // way so a stray capital or space cannot produce an awkward identifier.
  const typed = draftGroup.id.trim();
  const id = RIDAL.sanitizeIdentifier(typed === "" ? name : typed, taken);
  groups.push({ id, name, members: [] });
  draftGroup = null;
  render();
  save();
}

function cancelDraft() {
  draftGroup = null;
  renderGroups();
}

function confirmDeleteGroup(group, actionCell) {
  actionCell.replaceChildren();
  const prompt = document.createElement("span");
  prompt.className = "layer-panel-confirm";
  prompt.textContent = `Delete '${group.name || group.id}'?`;
  const yes = document.createElement("button");
  yes.type = "button";
  yes.className = "danger";
  yes.textContent = "Delete";
  yes.addEventListener("click", () => deleteGroup(group));
  const no = document.createElement("button");
  no.type = "button";
  no.textContent = "Cancel";
  no.addEventListener("click", renderGroups);
  prompt.append(yes, no);
  actionCell.appendChild(prompt);
}

function deleteGroup(group) {
  groups = groups.filter((existing) => existing.id !== group.id);
  // Drop the id from every layer that referenced it, or the group would live
  // on as a dangling `layer.groups` entry that still creates conflicts.
  for (const layer of layers) {
    if (layer.groups) {
      layer.groups = layer.groups.filter((id) => id !== group.id);
      if (layer.groups.length === 0) delete layer.groups;
    }
  }
  render();
  save();
}

function renderChip(group, memberId) {
  const chip = document.createElement("span");
  chip.className = "group-chip";
  const layer = layerById(memberId);
  if (layer) {
    const dot = document.createElement("span");
    dot.className = "layer-swatch";
    dot.style.background = layer.color || "#888888";
    chip.appendChild(dot);
    const label = document.createElement("span");
    label.textContent = layer.name || layer.id;
    chip.appendChild(label);
  } else {
    // A group is a statement about layers that may be added later, so a
    // member with no definition is reported, not dropped.
    const unknown = document.createElement("span");
    unknown.className = "group-chip-unknown";
    unknown.textContent = `⚠ ${memberId}`;
    chip.appendChild(unknown);
  }
  if (WRITABLE) {
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "group-chip-remove";
    remove.setAttribute(
      "aria-label",
      `Remove ${layer ? layer.name : memberId} from ${group.name || group.id}`,
    );
    remove.textContent = "×";
    remove.addEventListener("click", () => removeMember(group, memberId));
    chip.appendChild(remove);
  }
  return chip;
}

function addLayerControl(group) {
  const select = document.createElement("select");
  select.className = "group-add";
  select.setAttribute("aria-label", `Add a layer to ${group.name || group.id}`);
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = "Add layer";
  placeholder.disabled = true;
  placeholder.selected = true;
  select.appendChild(placeholder);
  const members = membersOf(group);
  const available = layers.filter((layer) => !members.includes(layer.id));
  for (const layer of available) {
    const option = document.createElement("option");
    option.value = layer.id;
    option.textContent = layer.name || layer.id;
    select.appendChild(option);
  }
  select.disabled = !WRITABLE || available.length === 0;
  select.addEventListener("change", () => {
    if (select.value) addMember(group, select.value);
  });
  return select;
}

/** One labelled field in a group card, matching the add-layer form's row. */
function groupField(labelText, input) {
  const label = document.createElement("label");
  label.append(labelText);
  label.appendChild(input);
  return label;
}

function renderGroup(group) {
  const card = document.createElement("div");
  card.className = "group-card";

  const fields = document.createElement("div");
  fields.className = "add-layer-fields";

  const nameInput = document.createElement("input");
  nameInput.type = "text";
  nameInput.value = group.name || "";
  nameInput.setAttribute("aria-label", `Name for ${group.id}`);
  nameInput.addEventListener("change", () => {
    const value = nameInput.value.trim();
    if (value !== "" && value !== group.name) {
      group.name = value;
      save();
    } else {
      nameInput.value = group.name || "";
    }
  });
  fields.appendChild(groupField("Name", nameInput));

  // Read-only, not disabled: it is shown and copyable, but the id is what
  // `layer.groups` refers to, so it does not move once the group exists.
  const idInput = document.createElement("input");
  idInput.type = "text";
  idInput.value = group.id;
  idInput.readOnly = true;
  idInput.setAttribute("aria-label", `ID for ${group.id}`);
  fields.appendChild(groupField("ID", idInput));

  card.appendChild(fields);

  const members = membersOf(group);
  const chips = document.createElement("div");
  chips.className = "group-chips";
  for (const memberId of members) chips.appendChild(renderChip(group, memberId));
  card.appendChild(chips);

  const definedCount = members.filter((id) => layerById(id)).length;
  if (definedCount !== members.length) {
    const warning = document.createElement("p");
    warning.className = "group-warning";
    warning.textContent = "⚠ This group names a layer that is not defined.";
    card.appendChild(warning);
  }
  if (definedCount < 2) {
    const note = document.createElement("p");
    note.className = "group-note";
    note.textContent = "A group needs at least two layers to mean anything.";
    card.appendChild(note);
  }

  const footer = document.createElement("div");
  footer.className = "group-footer";
  footer.appendChild(addLayerControl(group));
  if (WRITABLE) {
    const actions = document.createElement("span");
    actions.className = "row-actions";
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.textContent = "Delete";
    remove.addEventListener("click", () => confirmDeleteGroup(group, actions));
    actions.appendChild(remove);
    footer.appendChild(actions);
  }
  card.appendChild(footer);
  return card;
}

function renderDraft() {
  const card = document.createElement("div");
  card.className = "group-card group-draft";

  const fields = document.createElement("div");
  fields.className = "add-layer-fields";

  const nameInput = document.createElement("input");
  nameInput.type = "text";
  nameInput.placeholder = "Group name";
  nameInput.value = draftGroup.name;
  nameInput.setAttribute("aria-label", "Name for the new exclusivity group");

  const idInput = document.createElement("input");
  idInput.type = "text";
  idInput.placeholder = "group_id";
  idInput.value = draftGroup.id;
  idInput.setAttribute("aria-label", "ID for the new exclusivity group");

  // The id follows the name until it is edited by hand, the same way a new
  // derived item's id does.
  nameInput.addEventListener("input", () => {
    draftGroup.name = nameInput.value;
    if (!draftGroup.idEdited) {
      draftGroup.id = RIDAL.sanitizeIdentifier(
        draftGroup.name,
        groups.map((group) => group.id),
      );
      idInput.value = draftGroup.id;
    }
  });
  idInput.addEventListener("input", () => {
    draftGroup.idEdited = true;
    draftGroup.id = idInput.value;
  });
  const onKey = (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      commitDraft();
    } else if (event.key === "Escape") {
      event.preventDefault();
      cancelDraft();
    }
  };
  nameInput.addEventListener("keydown", onKey);
  idInput.addEventListener("keydown", onKey);

  fields.append(groupField("Name", nameInput), groupField("ID", idInput));
  card.appendChild(fields);

  const footer = document.createElement("div");
  footer.className = "group-footer";
  const create = document.createElement("button");
  create.type = "button";
  create.id = "group-draft-create";
  create.textContent = "Add group";
  create.addEventListener("click", commitDraft);
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.id = "group-draft-cancel";
  cancel.textContent = "Cancel";
  cancel.addEventListener("click", cancelDraft);
  footer.append(create, cancel);
  card.appendChild(footer);
  return card;
}

function renderGroups() {
  if (!groupsList) return;
  const cards = groups.map(renderGroup);
  if (draftGroup) cards.push(renderDraft());
  groupsList.replaceChildren(...cards);
  groupsEmpty.hidden = groups.length > 0 || draftGroup !== null;
  groupsActions.replaceChildren();
  if (WRITABLE) {
    const add = document.createElement("button");
    add.type = "button";
    add.id = "group-new";
    add.textContent = "Add group";
    add.addEventListener("click", addGroup);
    groupsActions.appendChild(add);
  }
}

/** Membership moved onto `group.members`, which is the canonical form.
 *
 * A reference to a group id with no group object is kept rather than dropped:
 * it is a relation the file expressed deliberately, and erasing it would
 * change which layers conflict. */
function normalizedLayers() {
  const defined = new Set(groups.map((group) => group.id));
  return layers.map((layer) => {
    const copy = { ...layer };
    const dangling = (layer.groups || []).filter((id) => !defined.has(id));
    if (dangling.length > 0) {
      copy.groups = dangling;
    } else {
      delete copy.groups;
    }
    return copy;
  });
}

function normalizedGroups() {
  return groups.map((group) => ({ ...group, members: membersOf(group) }));
}

/** Build one row. Text goes in via textContent, never innerHTML: layer
 * names and descriptions are free-text fields a user typed. */
function renderRow(layer, index) {
  const row = document.createElement("tr");

  const swatchCell = document.createElement("td");
  if (WRITABLE) {
    const picker = document.createElement("input");
    picker.type = "color";
    picker.value = layer.color || "#888888";
    picker.setAttribute("aria-label", `Colour for ${layer.id}`);
    picker.addEventListener("change", () => {
      layers[index].color = picker.value;
      save();
    });
    swatchCell.appendChild(picker);
  } else {
    const swatch = document.createElement("span");
    swatch.className = "layer-swatch";
    swatch.style.background = layer.color || "#888888";
    swatchCell.appendChild(swatch);
  }
  row.appendChild(swatchCell);

  const idCell = document.createElement("td");
  const code = document.createElement("code");
  // Immutable by design: every stored pick refers to a layer by this
  // string, so changing it here would orphan them all silently.
  code.textContent = layer.id;
  idCell.appendChild(code);
  row.appendChild(idCell);

  row.appendChild(editableCell(layer, index, "name", "Name"));
  row.appendChild(editableCell(layer, index, "description", "Description"));

  const overhangCell = document.createElement("td");
  const toggle = document.createElement("input");
  toggle.type = "checkbox";
  toggle.checked = Boolean(layer.allow_overhangs);
  toggle.disabled = !WRITABLE;
  toggle.setAttribute("aria-label", `Allow overhangs in ${layer.id}`);
  toggle.addEventListener("change", () => {
    layers[index].allow_overhangs = toggle.checked;
    save();
  });
  overhangCell.appendChild(toggle);
  row.appendChild(overhangCell);

  // Static text, deliberately: naming the groups here would push the column
  // past the section directly below it, which already lists every member.
  const groupsCell = document.createElement("td");
  const groupTotal = groupCount(layer);
  groupsCell.textContent =
    groupTotal === 0 ? "—" : groupTotal === 1 ? "1 group" : `${groupTotal} groups`;
  row.appendChild(groupsCell);

  const usageCell = document.createElement("td");
  const count = usage.counts[layer.id] || 0;
  usageCell.textContent = count === 1 ? "1 feature" : `${count} features`;
  row.appendChild(usageCell);

  const actionCell = document.createElement("td");
  if (WRITABLE) {
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.textContent = "Delete";
    remove.addEventListener("click", () => deleteLayer(index));
    actionCell.appendChild(remove);
  }
  row.appendChild(actionCell);

  return row;
}

/** A cell that becomes the field's value on blur. Name and description are
 * cosmetic, so editing them in place needs no confirmation. */
function editableCell(layer, index, field, label) {
  const cell = document.createElement("td");
  if (!WRITABLE) {
    cell.textContent = layer[field] || "";
    return cell;
  }
  const input = document.createElement("input");
  input.type = "text";
  input.value = layer[field] || "";
  input.setAttribute("aria-label", `${label} for ${layer.id}`);
  input.addEventListener("change", () => {
    const value = input.value.trim();
    layers[index][field] = value === "" ? undefined : value;
    save();
  });
  cell.appendChild(input);
  return cell;
}

function render() {
  table.replaceChildren(...layers.map(renderRow));
  emptyNote.hidden = layers.length > 0;
  renderGroups();

  const undefinedNames = Object.keys(usage.undefined);
  const section = document.getElementById("undefined-labels");
  const list = document.getElementById("undefined-list");
  section.hidden = undefinedNames.length === 0;
  list.replaceChildren(
    ...undefinedNames.map((name) => {
      const item = document.createElement("li");
      const code = document.createElement("code");
      code.textContent = name;
      item.appendChild(code);
      const count = usage.undefined[name];
      item.appendChild(
        document.createTextNode(
          count === 1 ? " - 1 feature" : ` - ${count} features`,
        ),
      );
      return item;
    }),
  );
}

/** Deleting a layer definition leaves its picks alone; they simply lose
 * their colour. Saying so, with the count, is the difference between an
 * informed decision and a scary one. */
function deleteLayer(index) {
  const layer = layers[index];
  const count = usage.counts[layer.id] || 0;
  const consequence =
    count === 0
      ? "No picks use it."
      : `${count} picked feature(s) use it. They will be kept, but will lose their colour until a layer with this id exists again.`;
  if (!window.confirm(`Delete the layer "${layer.id}"?\n\n${consequence}`)) {
    return;
  }
  const removedId = layer.id;
  layers.splice(index, 1);
  // A deleted layer must not linger as a dangling group member: the warning
  // exists for hand-edited files, not for something the UI just did.
  for (const group of groups) {
    if (group.members) {
      group.members = group.members.filter((id) => id !== removedId);
    }
  }
  render();
  save();
}

async function save() {
  clearError();
  const headers = { "Content-Type": "application/json" };
  if (etag) {
    // Refuses rather than clobbers if another tab saved in between.
    headers["If-Match"] = etag;
  } else {
    // An empty vocabulary has no ETag, so the first save had no condition
    // at all: two pages both starting from nothing would both succeed and
    // the later one would discard the other's layers.
    headers["If-None-Match"] = "*";
  }
  try {
    const response = await fetch(RIDAL.apiPath("layers"), {
      method: "PUT",
      headers,
      // A partial overlay, not a bare array: the server merges it onto the
      // stored document, so `default_reducer` and any field this page does
      // not know survive an edit.
      body: JSON.stringify({
        layers: normalizedLayers(),
        groups: normalizedGroups(),
      }),
    });
    if (response.status === 412) {
      showError(
        "These layers were changed somewhere else while this page was open. " +
          "Reload to see the current definitions, then reapply your change.",
      );
      return;
    }
    if (!response.ok) {
      const body = await response.json().catch(() => null);
      // Re-read so the page shows what is actually stored rather than the
      // rejected edit. Before the error, not after: `load` clears the error
      // box, which erased the message before it was ever drawn (#348).
      await load();
      showError(body?.error?.message || RIDAL.upstreamMessage(response.status));
      return;
    }
    etag = response.headers.get("ETag");
    const body = await response.json();
    layers = body.layers;
    groups = body.groups || [];
    await loadUsage();
    render();
  } catch (error) {
    showError(`Could not save layers: ${error.message}`);
  }
}

async function loadUsage() {
  try {
    usage = await RIDAL.fetchJson(RIDAL.apiPath("layers", "usage"));
  } catch (error) {
    // Usage is advisory. Losing it must not stop the page working, so it
    // degrades to zero counts with a console note rather than an error box.
    console.warn(`Could not load layer usage: ${error.message}`);
    usage = { counts: {}, undefined: {} };
  }
}

async function load() {
  clearError();
  try {
    const response = await fetch(RIDAL.apiPath("layers"));
    if (!response.ok) {
      const body = await response.json().catch(() => null);
      showError(body?.error?.message || RIDAL.upstreamMessage(response.status));
      return;
    }
    etag = response.headers.get("ETag") || null;
    const body = await response.json();
    layers = body.layers || [];
    groups = body.groups || [];
    await loadUsage();
    render();
  } catch (error) {
    showError(`Could not load layers: ${error.message}`);
  }
  // A separate document with a separate save; a failure of one must not stop
  // the other from working.
  await loadDerived();
  // After the layers, which give the chips their colours.
  await loadPicks();
}

const form = document.getElementById("add-layer");
if (form) {
  /** Why this layer cannot be added, or null if it can.
   *
   * Specific rather than generic: "the id must be lowercase" is actionable,
   * "please match the requested format" is not, and an id is not something
   * the user can guess the rules for. */
  function rejectionReason(id, name) {
    if (id === "") {
      return [
        "A layer needs an id. It is the short name written into every pick " +
          'and exported as the "layer" column, for example "bed".',
        "id",
      ];
    }
    // The server's rule for a new id (`is_valid_identifier`): it must work
    // as a variable name in a derived-item expression, so no "-" (#348).
    if (!/^[a-z][a-z0-9_]*$/.test(id)) {
      const suggestion = RIDAL.sanitizeIdentifier(id, takenIds());
      const bad = [...id].find((c) => !/[a-z0-9_]/.test(c));
      const problem = bad === undefined
        ? "The id must start with a lowercase letter."
        : `The id cannot contain "${bad}".`;
      return [
        `${problem} Use lowercase letters, digits and "_" only, starting ` +
          "with a letter -- the id is a variable name in derived-item " +
          "expressions. Put capitals, spaces and punctuation in the name " +
          `instead. Try "${suggestion}"?`,
        "id",
      ];
    }
    if (layers.some((layer) => layer.id === id)) {
      return [`A layer with the id "${id}" already exists.`, "id"];
    }
    if (name === "") {
      return ["A layer needs a name. This is the label shown in the viewer.", "name"];
    }
    return null;
  }

  function takenIds() {
    return layers.map((layer) => layer.id);
  }

  /* Derive the ID from the name while the name is being typed (#242), the
   * same way the derived-item and basemap editors do. A layer ID is an
   * expression identifier (`RIDAL.sanitizeIdentifier`), not a slug: the
   * server refuses a new id with "-" in it (#348). Once the ID is edited by
   * hand it is left alone, so a deliberate choice is never clobbered by a
   * later tweak to the name. */
  let idEdited = false;
  form.elements.id.addEventListener("input", () => {
    idEdited = true;
  });
  form.elements.name.addEventListener("input", () => {
    if (idEdited) return;
    const name = form.elements.name.value;
    form.elements.id.value = name.trim() === "" ? "" : RIDAL.sanitizeIdentifier(name, takenIds());
  });

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const data = new FormData(form);
    const id = String(data.get("id") || "").trim();
    const name = String(data.get("name") || "").trim();

    const rejection = rejectionReason(id, name);
    if (rejection) {
      const [message, field] = rejection;
      showError(message);
      form.elements[field].focus();
      return;
    }

    clearError();
    const description = String(data.get("description") || "").trim();
    layers.push({
      id,
      name,
      color: String(data.get("color") || ""),
      description: description === "" ? undefined : description,
      allow_overhangs: data.get("allow_overhangs") === "on",
    });
    form.reset();
    idEdited = false;
    save();
  });
}


// --- Derived items (#209) -------------------------------------------------
//
// The vocabulary and the derived items are two documents with two saves. This
// section is deliberately independent of the layer table above: a rejected
// layer write reloads the layers, and a rejected derived write reloads the
// derived items, and neither touches the other.

const derivedTable = document.querySelector("#derived-table tbody");
const derivedEmpty = document.getElementById("derived-empty");
const derivedError = document.getElementById("derived-error");
const derivedActions = document.getElementById("derived-actions");

let derivedItems = [];
let derivedUnusable = [];
let derivedCanAuthor = false;
let derivedCanRelease = false;

function showDerivedError(message) {
  if (!derivedError) return;
  derivedError.textContent = message;
  derivedError.hidden = false;
}

function clearDerivedError() {
  if (!derivedError) return;
  derivedError.hidden = true;
  derivedError.textContent = "";
}

function derivedCell(text) {
  const cell = document.createElement("td");
  cell.textContent = text == null ? "" : String(text);
  return cell;
}

function renderDerivedRow(item) {
  const row = document.createElement("tr");

  const swatchCell = document.createElement("td");
  const swatch = document.createElement("span");
  swatch.className = "layer-swatch";
  swatch.style.background = item.color || "#888888";
  swatchCell.appendChild(swatch);
  row.appendChild(swatchCell);

  const idCell = document.createElement("td");
  const code = document.createElement("code");
  code.textContent = item.id;
  idCell.appendChild(code);
  row.appendChild(idCell);

  row.appendChild(derivedCell(item.name));
  row.appendChild(derivedCell(item.kind));
  row.appendChild(derivedCell(item.unit));
  const expressionCell = document.createElement("td");
  const expression = document.createElement("code");
  // Same token colours as the editor; `highlight` escapes every token.
  expression.innerHTML = RIDAL.derivedEditor.highlight(item.expression);
  expressionCell.appendChild(expression);
  row.appendChild(expressionCell);
  row.appendChild(derivedCell(item.show ? "yes" : "no"));
  row.appendChild(derivedCell(item.listed === false ? "no" : "yes"));
  // A counter, so an intermediate layer is visibly one before someone tries
  // to delete it; the server refuses that delete and names the dependent.
  row.appendChild(
    derivedCell(item.used_by ? `${item.used_by}` : "—"),
  );
  row.appendChild(
    derivedCell(item.audience === "released" ? "everyone" : "own picks"),
  );

  const actionCell = document.createElement("td");
  if (derivedCanAuthor) {
    // `.row-actions` is the shared table-row button style, so Edit and Delete
    // read as one set rather than as controls borrowed from two pages.
    const actions = document.createElement("span");
    actions.className = "row-actions";
    const edit = document.createElement("button");
    edit.type = "button";
    edit.textContent = "Edit";
    edit.addEventListener("click", () => editDerived(item));
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.textContent = "Delete";
    remove.addEventListener("click", () => confirmDeleteDerived(item, actions));
    actions.append(edit, remove);
    actionCell.appendChild(actions);
  }
  row.appendChild(actionCell);
  return row;
}

function renderDerived() {
  if (!derivedTable) return;
  derivedTable.replaceChildren(...derivedItems.map(renderDerivedRow));
  if (derivedEmpty) derivedEmpty.hidden = derivedItems.length > 0;
  if (derivedActions) {
    derivedActions.replaceChildren();
    if (derivedCanAuthor) {
      const add = document.createElement("button");
      add.type = "button";
      add.id = "derived-new";
      add.textContent = "Add expression";
      add.addEventListener("click", () => editDerived(null));
      derivedActions.appendChild(add);
    }
  }
}

function editDerived(item) {
  RIDAL.derivedEditor.open({
    item,
    items: derivedItems,
    layerIds: layers.map((layer) => layer.id),
    unusable: derivedUnusable,
    canRelease: derivedCanRelease,
    // No radargram on this page, so no live preview; the editor says so.
    preview: null,
    onSaved: loadDerived,
    onClose: () => {},
  });
}

function confirmDeleteDerived(item, actionCell) {
  actionCell.replaceChildren();
  const prompt = document.createElement("span");
  prompt.className = "layer-panel-confirm";
  prompt.textContent = `Delete '${item.name || item.id}'?`;
  const yes = document.createElement("button");
  yes.type = "button";
  yes.className = "danger";
  yes.textContent = "Delete";
  yes.addEventListener("click", () => deleteDerived(item));
  const no = document.createElement("button");
  no.type = "button";
  no.textContent = "Cancel";
  no.addEventListener("click", renderDerived);
  prompt.append(yes, no);
  actionCell.appendChild(prompt);
}

async function deleteDerived(item) {
  clearDerivedError();
  const next = derivedItems
    .filter((existing) => existing.id !== item.id)
    .map((existing) => ({
      id: existing.id,
      name: existing.name,
      expression: existing.expression,
      unit: existing.unit,
      color: existing.color,
      show: existing.show,
      listed: existing.listed,
      fill_to: existing.fill_to,
      scope: existing.scope,
      audience: existing.audience,
    }));
  try {
    await RIDAL.fetchJson(RIDAL.apiPath("derived"), {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        schema: "ridal-derived",
        schema_version: "1",
        items: next,
      }),
    });
  } catch (error) {
    // Includes the server's refusal to delete an item another depends on,
    // which names the dependent.
    showDerivedError(error.message);
    return;
  }
  await loadDerived();
}

async function loadDerived() {
  clearDerivedError();
  try {
    const body = await RIDAL.fetchJson(RIDAL.apiPath("derived"));
    derivedItems = body.items || [];
    derivedUnusable = body.layers_unusable_in_expressions || [];
    derivedCanAuthor = Boolean(body.can_author);
    derivedCanRelease = Boolean(body.can_release);
  } catch (error) {
    showDerivedError(`Could not load derived items: ${error.message}`);
    derivedItems = [];
  }
  renderDerived();
}

// ---------------------------------------------------------------------------
// Pick manager (#178)
//
// Every stored interpretation the caller may know about, as statistics. The
// server decides whose rows these are (an operator's are everyone's) and
// never sends a coordinate; the actions offered on a row are the existing
// routes', so what one may open or download is decided where it always was.

const picksSection = document.getElementById("picks-section");
const picksTable = document.querySelector("#picks-table tbody");
const picksEmpty = document.getElementById("picks-empty");
const picksError = document.getElementById("picks-error");
const picksSummary = document.getElementById("picks-summary");
const picksContributors = document.getElementById("picks-contributors");
const picksFilterUser = document.getElementById("picks-filter-user");
const picksFilterLayer = document.getElementById("picks-filter-layer");
const picksFilterState = document.getElementById("picks-filter-state");
const picksFilterText = document.getElementById("picks-filter-text");

const PICK_STATES = {
  live: { label: "Current", title: "Drawn on the radargram as it is now." },
  stale: {
    label: "Older revision",
    title:
      "Drawn on an earlier revision of the radargram. The viewer shows it carried onto the current one.",
  },
  ignored: {
    label: "Ignored",
    title: "The radargram is ignored, so it is not served. Lifting the ignore brings these back.",
  },
  orphaned: {
    label: "No radargram",
    title:
      "No radargram with this id is served, and these were not archived by a removal: the file was moved or deleted on the server.",
  },
  archived: {
    label: "Archived",
    title: "Kept when the radargram was removed.",
  },
};

let picks = { interpretations: [], everyone: false, may_download: false, unreadable: [] };
let picksSort = { key: "radargram", descending: false };

function showPicksError(message) {
  picksError.textContent = message;
  picksError.hidden = false;
}

function clearPicksError() {
  picksError.hidden = true;
  picksError.textContent = "";
}

/** A removal's directory name, `2026-10-14T10-00-00Z` with perhaps a `-2`,
 * read back as the time it names. */
function removalTime(removal) {
  const match = /^(\d{4}-\d{2}-\d{2})T(\d{2})-(\d{2})/.exec(removal || "");
  return match ? `${match[1]} ${match[2]}:${match[3]}` : removal || "";
}

function radargramLabel(row) {
  return row.display_name || row.radargram_id;
}

function rowKey(row) {
  return `${row.radargram_id}/${row.removal || ""}/${row.user}`;
}

/** Whether `row` is something the caller drew. Without accounts everyone is
 * the local `default` user, which the server reports as the current user. */
function isOwn(row) {
  const current = window.RIDAL_LAYERS.currentUser;
  return current !== null && row.user === current;
}

/** Whether the radargram behind `row` can be opened and its picks fetched
 * through the ordinary routes. */
function isServed(row) {
  return row.state === "live" || row.state === "stale";
}

function formatCoverage(value) {
  if (value == null) return "—";
  // One decimal below 10 %, where a whole number would round a short line
  // to nothing; whole percent above.
  return value < 10 && value > 0 ? `${value.toFixed(1)} %` : `${Math.round(value)} %`;
}

function sortValue(row, key) {
  switch (key) {
    case "user":
      return row.user;
    case "features":
      return row.features;
    case "coverage":
      return row.coverage == null ? -1 : row.coverage;
    case "modified":
      return row.date_modified || "";
    case "state":
      return Object.keys(PICK_STATES).indexOf(row.state);
    default:
      return radargramLabel(row).toLowerCase();
  }
}

function filteredPicks() {
  const user = picksFilterUser.value;
  const layer = picksFilterLayer.value;
  const state = picksFilterState.value;
  const text = picksFilterText.value.trim().toLowerCase();
  const rows = picks.interpretations.filter(
    (row) =>
      (!user || row.user === user) &&
      (!layer || row.layers.some((l) => (l.layer || "") === layer)) &&
      (!state || row.state === state) &&
      (!text ||
        row.radargram_id.toLowerCase().includes(text) ||
        (row.display_name || "").toLowerCase().includes(text)),
  );
  const direction = picksSort.descending ? -1 : 1;
  return rows.sort((a, b) => {
    const left = sortValue(a, picksSort.key);
    const right = sortValue(b, picksSort.key);
    if (left < right) return -direction;
    if (left > right) return direction;
    return rowKey(a).localeCompare(rowKey(b));
  });
}

function layerChip(entry) {
  const chip = document.createElement("span");
  chip.className = "group-chip";
  const defined = entry.layer ? layerById(entry.layer) : null;
  const swatch = document.createElement("span");
  swatch.className = "layer-swatch";
  swatch.style.background = defined?.color || "#888888";
  const name = entry.layer ? defined?.name || entry.layer : "unlabelled";
  if (!defined) chip.classList.add("group-chip-unknown");
  chip.append(swatch, document.createTextNode(`${name} ${entry.features}`));
  const lines = entry.features === 1 ? "line" : "lines";
  chip.title =
    `${entry.layer || "No layer"}: ${entry.features} ${lines} (${entry.vertices} vertices)` +
    (entry.coverage == null ? "" : `, ${formatCoverage(entry.coverage)} of traces`) +
    (entry.layer && !defined ? ". This layer is not defined." : "");
  return chip;
}

function coverageCell(row) {
  const cell = document.createElement("td");
  cell.className = "picks-nowrap";
  if (row.coverage == null) {
    cell.textContent = "—";
    cell.title = isServed(row)
      ? "The revision these were drawn on was not kept, so there is nothing to measure against."
      : "There is no radargram to measure against.";
    return cell;
  }
  const bar = document.createElement("span");
  bar.className = "coverage-bar";
  const fill = document.createElement("span");
  fill.style.width = `${Math.min(100, row.coverage)}%`;
  bar.appendChild(fill);
  const text = document.createElement("span");
  text.textContent = formatCoverage(row.coverage);
  cell.append(bar, text);
  cell.title = `${formatCoverage(row.coverage)} of ${row.traces} traces have a picked line over them.`;
  // Picks drawn on an earlier revision are measured as the viewer draws
  // them, carried onto the current one. When they cannot be carried, the
  // number is of the picks as stored, and the cell says it was not carried.
  const basis = {
    carried: [
      "carried",
      "Measured after carrying these picks onto the current revision, as the viewer shows them.",
    ],
    drawn: [
      "not carried",
      "These picks cannot be carried onto the current revision, so this is measured against the earlier revision they were drawn on.",
    ],
    uncarried: [
      "not carried",
      "These picks cannot be carried onto the current revision, and the revision they were drawn on was not kept, so this is measured with the picks where they are stored. It is only right if reprocessing did not change which trace is which.",
    ],
  }[row.coverage_basis];
  if (basis) {
    const note = document.createElement("span");
    note.className = "picks-coverage-basis";
    note.textContent = ` (${basis[0]})`;
    cell.appendChild(note);
    cell.title += ` ${basis[1]}`;
  }
  return cell;
}

function renderPickRow(row) {
  const tr = document.createElement("tr");

  const radargramCell = document.createElement("td");
  if (isServed(row)) {
    const link = document.createElement("a");
    link.href = RIDAL.pagePath("view", row.radargram_id);
    link.textContent = radargramLabel(row);
    radargramCell.appendChild(link);
  } else {
    radargramCell.textContent = radargramLabel(row);
  }
  if (row.display_name) radargramCell.title = row.radargram_id;
  tr.appendChild(radargramCell);

  if (picks.everyone) {
    const userCell = document.createElement("td");
    userCell.textContent = row.user;
    tr.appendChild(userCell);
  }

  const layersCell = document.createElement("td");
  const chips = document.createElement("span");
  chips.className = "picks-layer-chips";
  chips.append(...row.layers.map(layerChip));
  layersCell.appendChild(chips);
  tr.appendChild(layersCell);

  const linesCell = document.createElement("td");
  linesCell.className = "picks-nowrap";
  linesCell.textContent = `${row.features} (${row.vertices})`;
  tr.appendChild(linesCell);

  tr.appendChild(coverageCell(row));

  const modifiedCell = document.createElement("td");
  modifiedCell.className = "picks-nowrap";
  modifiedCell.textContent = row.date_modified ? row.date_modified.slice(0, 10) : "—";
  if (row.date_modified) modifiedCell.title = row.date_modified;
  tr.appendChild(modifiedCell);

  const stateCell = document.createElement("td");
  const badge = document.createElement("span");
  badge.className = `picks-state picks-state-${row.state}`;
  const state = PICK_STATES[row.state] || { label: row.state, title: "" };
  badge.textContent =
    row.state === "archived" ? `${state.label} ${removalTime(row.removal)}` : state.label;
  badge.title = state.title;
  stateCell.appendChild(badge);
  tr.appendChild(stateCell);

  const actionCell = document.createElement("td");
  const actions = document.createElement("span");
  actions.className = "row-actions";
  if (isServed(row) && picks.may_download) {
    const download = document.createElement("a");
    download.className = "button";
    download.href = RIDAL.apiPath("datasets", row.radargram_id, "interpretations", row.user, "raw");
    download.download = `${row.radargram_id}.${row.user}.gprinterp.json`;
    download.textContent = "Download";
    actions.appendChild(download);
  }
  if (isServed(row) && isOwn(row) && row.version) {
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.textContent = "Delete";
    remove.addEventListener("click", () => confirmDeletePicks(row, actions));
    actions.appendChild(remove);
  }
  actionCell.appendChild(actions);
  tr.appendChild(actionCell);
  return tr;
}

function confirmDeletePicks(row, actionCell) {
  actionCell.replaceChildren();
  const prompt = document.createElement("span");
  prompt.className = "layer-panel-confirm";
  const lines = row.features === 1 ? "line" : "lines";
  prompt.textContent = `Delete your ${row.features} ${lines} on '${radargramLabel(row)}'? This cannot be undone.`;
  const yes = document.createElement("button");
  yes.type = "button";
  yes.className = "danger";
  yes.textContent = "Delete";
  yes.addEventListener("click", () => deletePicks(row));
  const no = document.createElement("button");
  no.type = "button";
  no.textContent = "Cancel";
  no.addEventListener("click", renderPicks);
  prompt.append(yes, no);
  actionCell.appendChild(prompt);
}

async function deletePicks(row) {
  clearPicksError();
  try {
    // Conditional on the version this page listed, so picks saved in
    // another tab since then are not deleted unseen.
    const response = await fetch(
      RIDAL.apiPath("datasets", row.radargram_id, "interpretations", row.user),
      { method: "DELETE", headers: { "If-Match": `"${row.version}"` } },
    );
    if (!response.ok) {
      const body = await response.json().catch(() => null);
      const message = body?.error?.message || RIDAL.upstreamMessage(response.status);
      showPicksError(
        response.status === 412
          ? "These picks were changed after this page loaded, so nothing was deleted. The list has been reloaded."
          : `Could not delete: ${message}`,
      );
    }
  } catch (error) {
    showPicksError(`Could not delete: ${error.message}`);
  }
  await loadPicks();
}

function renderPicksSummary(rows) {
  const all = picks.interpretations;
  const radargrams = new Set(all.filter(isServed).map((r) => r.radargram_id));
  const lines = all.filter(isServed).reduce((sum, r) => sum + r.features, 0);
  if (all.length === 0) {
    picksSummary.textContent = "";
  } else {
    const shown = rows.length === all.length ? "" : ` Showing ${rows.length} of ${all.length}.`;
    picksSummary.textContent =
      `${lines} ${lines === 1 ? "line" : "lines"} on ${radargrams.size} ` +
      `${radargrams.size === 1 ? "radargram" : "radargrams"} in the catalog.${shown}`;
  }

  // How many radargrams each contributor has picked: the contribution count
  // #178 asks for, at a glance. Clicking one filters to them.
  picksContributors.replaceChildren();
  picksContributors.hidden = !picks.everyone || all.length === 0;
  if (picksContributors.hidden) return;
  const perUser = new Map();
  for (const row of all.filter(isServed)) {
    if (!perUser.has(row.user)) perUser.set(row.user, new Set());
    perUser.get(row.user).add(row.radargram_id);
  }
  for (const [user, ids] of [...perUser].sort((a, b) => b[1].size - a[1].size)) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "group-chip";
    if (picksFilterUser.value === user) chip.classList.add("group-chip-active");
    chip.textContent = `${user}: ${ids.size} ${ids.size === 1 ? "radargram" : "radargrams"}`;
    chip.title = `${user} has picks on ${ids.size} ${ids.size === 1 ? "radargram" : "radargrams"} in the catalog`;
    chip.addEventListener("click", () => {
      picksFilterUser.value = picksFilterUser.value === user ? "" : user;
      renderPicks();
    });
    picksContributors.appendChild(chip);
  }
}

function fillSelect(select, values, labelOf) {
  const current = select.value;
  const first = select.options[0];
  select.replaceChildren(first);
  for (const value of values) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = labelOf(value);
    select.appendChild(option);
  }
  // Keep the choice if it is still there.
  select.value = values.includes(current) ? current : "";
}

function renderPicksFilters() {
  const rows = picks.interpretations;
  document.getElementById("picks-filter-user-label").hidden = !picks.everyone;
  fillSelect(picksFilterUser, [...new Set(rows.map((r) => r.user))].sort(), (u) => u);
  const layerIds = [...new Set(rows.flatMap((r) => r.layers.map((l) => l.layer || "")))].sort();
  fillSelect(picksFilterLayer, layerIds, (id) =>
    id === "" ? "unlabelled" : layerById(id)?.name || id,
  );
  const states = Object.keys(PICK_STATES).filter((s) => rows.some((r) => r.state === s));
  fillSelect(picksFilterState, states, (s) => PICK_STATES[s].label);
}

function renderPicks() {
  const rows = filteredPicks();
  for (const column of document.querySelectorAll(".picks-user-column")) {
    column.hidden = !picks.everyone;
  }
  for (const th of document.querySelectorAll("#picks-table th[data-sort]")) {
    const sorted = th.dataset.sort === picksSort.key;
    th.setAttribute(
      "aria-sort",
      sorted ? (picksSort.descending ? "descending" : "ascending") : "none",
    );
  }
  picksTable.replaceChildren(...rows.map(renderPickRow));
  picksEmpty.hidden = rows.length > 0;
  picksEmpty.textContent =
    picks.interpretations.length === 0
      ? picks.everyone
        ? "No one has picked anything yet."
        : "You have not picked anything yet."
      : "Nothing matches these filters.";
  renderPicksSummary(rows);

  const unreadable = document.getElementById("picks-unreadable");
  unreadable.hidden = picks.unreadable.length === 0;
  document.getElementById("picks-unreadable-list").replaceChildren(
    ...picks.unreadable.map((item) => {
      const li = document.createElement("li");
      const code = document.createElement("code");
      code.textContent = item.path;
      li.append(code, document.createTextNode(`: ${item.reason}`));
      return li;
    }),
  );
}

async function loadPicks() {
  if (!picksSection) return;
  clearPicksError();
  try {
    picks = await RIDAL.fetchJson(RIDAL.apiPath("interpretations"));
  } catch (error) {
    showPicksError(`Could not load picks: ${error.message}`);
    picks = { interpretations: [], everyone: false, may_download: false, unreadable: [] };
  }
  renderPicksFilters();
  renderPicks();
}

if (picksSection) {
  for (const control of [picksFilterUser, picksFilterLayer, picksFilterState]) {
    control.addEventListener("change", renderPicks);
  }
  picksFilterText.addEventListener("input", renderPicks);
  for (const th of document.querySelectorAll("#picks-table th[data-sort]")) {
    th.querySelector("button").addEventListener("click", () => {
      const key = th.dataset.sort;
      picksSort =
        picksSort.key === key
          ? { key, descending: !picksSort.descending }
          : // Numbers read best largest first; names alphabetically.
            { key, descending: ["features", "coverage", "modified"].includes(key) };
      renderPicks();
    });
  }
}

load();
