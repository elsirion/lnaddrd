// Renders the "Manage" tab into a container element.
//
// Refresh policy: renderManage() rebuilds the whole section from scratch, so
// app.js calls it only on two events — activating the Manage tab, and a
// successful Nostr connect — rather than on every discovery update. This
// keeps the operator picker fresh whenever a user actually looks at the tab
// without wiping in-progress form input (typed destination, loaded address
// table) every time a background relay event adds an operator.
import { nip98Header } from "./nostr-auth.js";
import { listAddresses, updateAddress, removeAddress } from "./api.js";

const INPUT_CLASS =
  "block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500";
const LABEL_CLASS = "block mb-2 text-sm font-medium text-gray-900";
const PRIMARY_BUTTON_CLASS =
  "text-white bg-blue-700 hover:bg-blue-800 focus:ring-4 focus:ring-blue-300 font-medium rounded-lg text-sm px-4 py-2 disabled:opacity-50 disabled:cursor-not-allowed";
const SECONDARY_BUTTON_CLASS =
  "text-white bg-blue-700 hover:bg-blue-800 font-medium rounded-lg text-xs px-3 py-1.5 disabled:opacity-50 disabled:cursor-not-allowed";
const DANGER_BUTTON_CLASS =
  "text-white bg-red-700 hover:bg-red-800 font-medium rounded-lg text-xs px-3 py-1.5 disabled:opacity-50 disabled:cursor-not-allowed";
const DANGER_BUTTON_CLASS_WIDE =
  "text-white bg-red-700 hover:bg-red-800 font-medium rounded-lg text-sm px-4 py-2 disabled:opacity-50 disabled:cursor-not-allowed";

// The legacy `/lnaddress/*` endpoints return bare HTTP status codes with no
// JSON body at all (docs/protocol/03-registration-api.md, "Management"), so
// apiFetch's generic `HTTP <status>` fallback message is what a caller sees.
// These tables translate that into the endpoint-specific meaning the doc
// assigns each status; a status not covered here (or a network failure)
// falls through to a generic message that still shows the raw text.
const UPDATE_ERRORS = {
  "HTTP 401": "Not authorized: no valid credentials were supplied.",
  "HTTP 400":
    "Update rejected: invalid destination, the address doesn't exist or isn't active, or the credentials don't match this address's owner.",
};
const REMOVE_ERRORS = {
  "HTTP 400": "Malformed request.",
  "HTTP 401": "Not authorized: missing or invalid credentials, or the address doesn't exist.",
};

function describeLegacyError(message, table) {
  return table[message] ?? `Something went wrong: ${message}`;
}

/** A status/alert paragraph — the only way manage-tab feedback is surfaced. */
function statusParagraph(text, variant) {
  const p = document.createElement("p");
  p.className = `text-sm ${variant === "error" ? "text-red-700" : "text-green-700"}`;
  p.setAttribute("role", variant === "error" ? "alert" : "status");
  p.textContent = text;
  return p;
}

function truncate(text, max = 40) {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

/**
 * Renders the whole Manage tab into `container`.
 * `operators`: the shared discovery Map (`${pubkey}:${dtag}` -> {validated, event}).
 * `connectedPubkey`: hex pubkey from a NIP-07 connect, or null.
 */
export function renderManage(container, { operators, connectedPubkey }) {
  container.replaceChildren();

  const entries = [...operators.values()].sort((a, b) => a.validated.origin.localeCompare(b.validated.origin));

  const pickerSection = document.createElement("div");
  const pickerLabel = document.createElement("label");
  pickerLabel.className = LABEL_CLASS;
  pickerLabel.htmlFor = "manage-operator";
  pickerLabel.textContent = "Operator";
  const picker = document.createElement("select");
  picker.id = "manage-operator";
  picker.className = INPUT_CLASS;
  for (const entry of entries) {
    const opt = document.createElement("option");
    opt.value = entry.validated.origin;
    opt.textContent = entry.validated.announcement.name || entry.validated.origin;
    picker.append(opt);
  }
  pickerSection.append(pickerLabel, picker);
  container.append(pickerSection);

  if (entries.length === 0) {
    const note = document.createElement("p");
    note.className = "text-sm text-gray-500";
    note.textContent = "No operators discovered yet.";
    container.append(note);
  }

  const currentOrigin = () => picker.value || null;

  if (connectedPubkey) {
    container.append(renderConnectedSection(currentOrigin, connectedPubkey));
  }

  container.append(renderTokenSection(currentOrigin));
}

// --- connected (NIP-07 / NIP-98) section --------------------------------

function renderConnectedSection(currentOrigin, connectedPubkey) {
  const section = document.createElement("section");
  section.className = "rounded-lg border border-gray-200 bg-white p-4 shadow-sm space-y-3";

  const heading = document.createElement("h3");
  heading.className = "text-sm font-semibold text-gray-700";
  heading.textContent = "My addresses";
  section.append(heading);

  const who = document.createElement("p");
  who.className = "text-xs text-gray-500";
  who.textContent = `Connected as ${window.NostrTools.nip19.npubEncode(connectedPubkey).slice(0, 12)}…`;
  section.append(who);

  const loadBtn = document.createElement("button");
  loadBtn.type = "button";
  loadBtn.className = PRIMARY_BUTTON_CLASS;
  loadBtn.textContent = "Load my addresses";
  section.append(loadBtn);

  const status = document.createElement("div");
  section.append(status);

  const tableWrap = document.createElement("div");
  tableWrap.className = "overflow-x-auto";
  section.append(tableWrap);

  async function load() {
    status.replaceChildren();
    const origin = currentOrigin();
    if (!origin) {
      status.replaceChildren(statusParagraph("Select an operator first.", "error"));
      return;
    }
    loadBtn.disabled = true;
    try {
      const url = `${origin}/api/v1/addresses`;
      const authHeader = await nip98Header(url, "GET");
      const { addresses } = await listAddresses(origin, authHeader);
      renderAddressTable(tableWrap, origin, addresses, load);
    } catch (err) {
      tableWrap.replaceChildren();
      status.replaceChildren(statusParagraph(`Could not load addresses: ${err.message}`, "error"));
    } finally {
      loadBtn.disabled = false;
    }
  }

  loadBtn.addEventListener("click", load);

  return section;
}

function renderAddressTable(container, origin, addresses, refresh) {
  container.replaceChildren();
  if (!addresses.length) {
    const p = document.createElement("p");
    p.className = "text-sm text-gray-500";
    p.textContent = "No addresses found for this key.";
    container.append(p);
    return;
  }

  const table = document.createElement("table");
  table.className = "min-w-full text-sm divide-y divide-gray-200";

  const thead = document.createElement("thead");
  const headRow = document.createElement("tr");
  for (const label of ["Address", "Destination", "Actions"]) {
    const th = document.createElement("th");
    th.className = "px-2 py-1 text-left text-xs font-medium text-gray-500";
    th.textContent = label;
    headRow.append(th);
  }
  thead.append(headRow);
  table.append(thead);

  const tbody = document.createElement("tbody");
  tbody.className = "divide-y divide-gray-100";
  for (const addr of addresses) {
    tbody.append(addressRow(origin, addr, refresh));
  }
  table.append(tbody);

  container.append(table);
}

function addressRow(origin, addr, refresh) {
  const row = document.createElement("tr");

  const addressCell = document.createElement("td");
  addressCell.className = "px-2 py-1 font-mono align-top";
  addressCell.textContent = `${addr.username}@${addr.domain}`;
  row.append(addressCell);

  const destCell = document.createElement("td");
  destCell.className = "px-2 py-1 font-mono text-gray-600 align-top";
  destCell.textContent = truncate(addr.destination);
  destCell.title = addr.destination;
  row.append(destCell);

  const actionsCell = document.createElement("td");
  actionsCell.className = "px-2 py-1 space-y-1 align-top";

  const editRow = document.createElement("div");
  editRow.className = "flex flex-wrap items-center gap-1";
  const editInput = document.createElement("input");
  editInput.type = "text";
  editInput.placeholder = "New destination";
  editInput.className = `${INPUT_CLASS} w-40 text-xs py-1`;
  const confirmBtn = document.createElement("button");
  confirmBtn.type = "button";
  confirmBtn.className = SECONDARY_BUTTON_CLASS;
  confirmBtn.textContent = "Update";
  const deleteBtn = document.createElement("button");
  deleteBtn.type = "button";
  deleteBtn.className = DANGER_BUTTON_CLASS;
  deleteBtn.textContent = "Delete";
  editRow.append(editInput, confirmBtn, deleteBtn);
  actionsCell.append(editRow);

  const status = document.createElement("div");
  actionsCell.append(status);
  row.append(actionsCell);

  confirmBtn.addEventListener("click", async () => {
    status.replaceChildren();
    const destination = editInput.value.trim();
    if (!destination) {
      status.replaceChildren(statusParagraph("Enter a new destination.", "error"));
      return;
    }
    confirmBtn.disabled = true;
    try {
      // `bodyObj` is passed unmutated to updateAddress(), which JSON.stringifies
      // it itself for the fetch body — that call produces the exact same
      // string as `bodyStr` below (same object, same key order), which is
      // what the NIP-98 payload tag must hash.
      const bodyObj = { domain: addr.domain, username: addr.username, destination };
      const bodyStr = JSON.stringify(bodyObj);
      const url = `${origin}/lnaddress/update`;
      const authHeader = await nip98Header(url, "PUT", bodyStr);
      await updateAddress(origin, bodyObj, authHeader);
      status.replaceChildren(statusParagraph("Updated.", "status"));
      await refresh();
    } catch (err) {
      status.replaceChildren(statusParagraph(describeLegacyError(err.message, UPDATE_ERRORS), "error"));
      confirmBtn.disabled = false;
    }
  });

  deleteBtn.addEventListener("click", async () => {
    status.replaceChildren();
    if (!confirm(`Delete ${addr.username}@${addr.domain}? This cannot be undone.`)) return;
    deleteBtn.disabled = true;
    try {
      const bodyObj = { domain: addr.domain, username: addr.username };
      const bodyStr = JSON.stringify(bodyObj);
      const url = `${origin}/lnaddress/remove`;
      const authHeader = await nip98Header(url, "DELETE", bodyStr);
      await removeAddress(origin, bodyObj, authHeader);
      await refresh();
    } catch (err) {
      status.replaceChildren(statusParagraph(describeLegacyError(err.message, REMOVE_ERRORS), "error"));
      deleteBtn.disabled = false;
    }
  });

  return row;
}

// --- token fallback section (always visible) ----------------------------

function renderTokenSection(currentOrigin) {
  const section = document.createElement("section");
  section.className = "rounded-lg border border-gray-200 bg-white p-4 shadow-sm space-y-3";

  const heading = document.createElement("h3");
  heading.className = "text-sm font-semibold text-gray-700";
  heading.textContent = "Manage with a token";
  section.append(heading);

  const note = document.createElement("p");
  note.className = "text-xs text-gray-500";
  note.textContent = "Use the management token you received at registration, against the operator selected above.";
  section.append(note);

  const fields = document.createElement("div");
  fields.className = "grid gap-3 sm:grid-cols-2";
  section.append(fields);

  function field(id, label, type = "text") {
    const wrap = document.createElement("div");
    const l = document.createElement("label");
    l.className = LABEL_CLASS;
    l.htmlFor = id;
    l.textContent = label;
    const input = document.createElement("input");
    input.id = id;
    input.type = type;
    input.autocomplete = "off";
    input.className = INPUT_CLASS;
    wrap.append(l, input);
    fields.append(wrap);
    return input;
  }

  const domainInput = field("manage-token-domain", "Domain");
  const usernameInput = field("manage-token-username", "Username");
  const tokenInput = field("manage-token-token", "Management token", "password");
  const destInput = field("manage-token-destination", "New destination");

  const buttonRow = document.createElement("div");
  buttonRow.className = "flex flex-wrap gap-2";
  const updateBtn = document.createElement("button");
  updateBtn.type = "button";
  updateBtn.className = PRIMARY_BUTTON_CLASS;
  updateBtn.textContent = "Update";
  const deleteBtn = document.createElement("button");
  deleteBtn.type = "button";
  deleteBtn.className = DANGER_BUTTON_CLASS_WIDE;
  deleteBtn.textContent = "Delete";
  buttonRow.append(updateBtn, deleteBtn);
  section.append(buttonRow);

  const status = document.createElement("div");
  section.append(status);

  function readCommon() {
    return {
      origin: currentOrigin(),
      domain: domainInput.value.trim(),
      username: usernameInput.value.trim(),
      authentication_token: tokenInput.value.trim(),
    };
  }

  updateBtn.addEventListener("click", async () => {
    status.replaceChildren();
    const { origin, domain, username, authentication_token } = readCommon();
    const destination = destInput.value.trim();
    if (!origin) {
      status.replaceChildren(statusParagraph("Select an operator first.", "error"));
      return;
    }
    if (!domain || !username || !authentication_token || !destination) {
      status.replaceChildren(statusParagraph("Fill in domain, username, token, and new destination.", "error"));
      return;
    }
    updateBtn.disabled = true;
    try {
      await updateAddress(origin, { domain, username, destination, authentication_token });
      status.replaceChildren(statusParagraph("Updated.", "status"));
    } catch (err) {
      status.replaceChildren(statusParagraph(describeLegacyError(err.message, UPDATE_ERRORS), "error"));
    } finally {
      updateBtn.disabled = false;
    }
  });

  deleteBtn.addEventListener("click", async () => {
    status.replaceChildren();
    const { origin, domain, username, authentication_token } = readCommon();
    if (!origin) {
      status.replaceChildren(statusParagraph("Select an operator first.", "error"));
      return;
    }
    if (!domain || !username || !authentication_token) {
      status.replaceChildren(statusParagraph("Fill in domain, username, and token.", "error"));
      return;
    }
    if (!confirm(`Delete ${username}@${domain}? This cannot be undone.`)) return;
    deleteBtn.disabled = true;
    try {
      await removeAddress(origin, { domain, username, authentication_token });
      status.replaceChildren(statusParagraph("Deleted.", "status"));
    } catch (err) {
      status.replaceChildren(statusParagraph(describeLegacyError(err.message, REMOVE_ERRORS), "error"));
    } finally {
      deleteBtn.disabled = false;
    }
  });

  return section;
}
