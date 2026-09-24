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
    const response = await fetch("/api/v1/layers", {
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
      showError(body?.error?.message || RIDAL.upstreamMessage(response.status));
      // Re-read so the page shows what is actually stored rather than the
      // rejected edit.
      await load();
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
    usage = await RIDAL.fetchJson("/api/v1/layers/usage");
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
    const response = await fetch("/api/v1/layers");
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
    if (!/^[a-z0-9_-]+$/.test(id)) {
      const bad = [...id].find((c) => !/[a-z0-9_-]/.test(c));
      return [
        `The id cannot contain "${bad}". Use lowercase letters, digits, "-" ` +
          "and \"_\" only -- it ends up in exported columns and URLs. Put " +
          "capitals, spaces and punctuation in the name instead.",
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
    await RIDAL.fetchJson("/api/v1/derived", {
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
    const body = await RIDAL.fetchJson("/api/v1/derived");
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

load();
