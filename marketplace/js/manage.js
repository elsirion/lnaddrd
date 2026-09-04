// Renders the "Manage" tab: one aggregated table of every Lightning Address
// the connected Nostr identity owns, fetched concurrently from every
// verified operator that supports nostr-auth.
//
// Refresh policy: renderManage() rebuilds the whole section from scratch, so
// app.js calls it only on two events — activating the Manage tab, and a
// successful Nostr connect (see app.js's tab-switch handler and doConnect())
// — rather than on every discovery update. Within a render, row actions
// (update/delete) re-fetch only the one operator they touched via
// refreshOperator(), not the whole aggregate.
//
// This tab is Nostr-identity-only: there is no operator picker and no
// manual domain/username/token fallback form. Every request against
// `/api/v1/addresses`, `/lnaddress/update`, and `/lnaddress/remove` carries a
// NIP-98 signature; there is no bearer-token path left in this file.
import { nip98Header } from "./nostr-auth.js";
import { listAddresses, listAddressesUrl, updateAddress, updateAddressUrl, removeAddress, removeAddressUrl } from "./api.js";
import { classifyOperator } from "./visibility.js";

const INPUT_CLASS =
  "block w-full p-2.5 border border-gray-300 rounded-lg bg-gray-50 text-gray-900 focus:ring-blue-500 focus:border-blue-500";
const SECONDARY_BUTTON_CLASS =
  "text-white bg-blue-700 hover:bg-blue-800 font-medium rounded-lg text-xs px-3 py-1.5 disabled:opacity-50 disabled:cursor-not-allowed";
const DANGER_BUTTON_CLASS =
  "text-white bg-red-700 hover:bg-red-800 font-medium rounded-lg text-xs px-3 py-1.5 disabled:opacity-50 disabled:cursor-not-allowed";

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

/** A status/alert paragraph — used for per-row update/delete feedback. */
function statusParagraph(text, variant) {
  const p = document.createElement("p");
  p.className = `text-sm ${variant === "error" ? "text-red-700" : "text-green-700"}`;
  p.setAttribute("role", variant === "error" ? "alert" : "status");
  p.textContent = text;
  return p;
}

/** A muted informational line — connect prompts, loading/empty states, the
 * per-operator unreachable note. Never an alert: nothing here is a failure
 * the user needs to act on beyond what the surrounding UI already shows. */
function note(text) {
  const p = document.createElement("p");
  p.className = "text-sm text-gray-500";
  p.textContent = text;
  return p;
}

function truncate(text, max = 40) {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

/**
 * Returns the operators eligible for address management: ≥1 verified domain
 * (Task 3's classifyOperator, reusing the same live domain-status lookup the
 * Browse tab renders from) AND the `nostr-auth` capability. Each item is
 * `{ origin, name }`.
 */
function verifiedNostrAuthOperators(operators, getDomainStatus) {
  const targets = [];
  for (const entry of operators.values()) {
    const { validated, event } = entry;
    const capabilities = Array.isArray(validated.announcement.capabilities) ? validated.announcement.capabilities : [];
    if (!capabilities.includes("nostr-auth")) continue;
    const domains = validated.announcement.domains;
    const { category } = classifyOperator(domains, domain => getDomainStatus(event.pubkey, domain));
    if (category !== "visible") continue;
    targets.push({ origin: validated.origin, name: validated.announcement.name || validated.origin });
  }
  return targets.sort((a, b) => a.origin.localeCompare(b.origin));
}

async function fetchOperatorAddresses(target) {
  const url = listAddressesUrl(target.origin);
  const authHeader = await nip98Header(url, "GET");
  const { addresses } = await listAddresses(target.origin, authHeader);
  return addresses;
}

function rowSort(a, b) {
  return `${a.username}@${a.domain}`.localeCompare(`${b.username}@${b.domain}`) || a.operator.origin.localeCompare(b.operator.origin);
}

/**
 * Renders the whole Manage tab into `container`.
 * `operators`: the shared discovery Map (`${pubkey}:${dtag}` -> {validated, event}).
 * `connectedPubkey`: hex pubkey from a NIP-07 connect, or null.
 * `getDomainStatus(pubkey, domain)`: Task 3's per-domain well-known status
 *   lookup, passed through so this tab's operator eligibility matches
 *   exactly what the Browse tab currently shows as verified.
 */
export function renderManage(container, { operators, connectedPubkey, getDomainStatus }) {
  container.replaceChildren();

  if (!connectedPubkey) {
    container.append(note("Connect Nostr to manage your addresses."));
    return;
  }

  const targets = verifiedNostrAuthOperators(operators, getDomainStatus);

  const statusEl = document.createElement("div");
  statusEl.className = "space-y-1";
  const tableWrap = document.createElement("div");
  tableWrap.className = "overflow-x-auto";
  tableWrap.append(note("Loading your addresses…"));
  container.append(statusEl, tableWrap);

  // In-memory rows for the aggregated table, one entry per address, each
  // tagged with the operator it came from. Kept here (rather than re-derived
  // from the DOM) so refreshOperator() can replace just one operator's slice
  // without re-fetching every other operator.
  let rows = [];

  function renderRows() {
    tableWrap.replaceChildren();
    if (rows.length === 0) {
      tableWrap.append(note("No addresses found for this identity."));
      return;
    }
    tableWrap.append(buildTable(rows, refreshOperator));
  }

  async function refreshOperator(target) {
    try {
      const addresses = await fetchOperatorAddresses(target);
      rows = rows.filter(r => r.operator.origin !== target.origin);
      for (const addr of addresses) rows.push({ ...addr, operator: target });
      rows.sort(rowSort);
      renderRows();
    } catch (err) {
      // The action itself already succeeded or failed and reported that
      // separately; this is only the follow-up re-fetch. Drop the
      // operator's rows rather than show them possibly stale, and say why.
      rows = rows.filter(r => r.operator.origin !== target.origin);
      renderRows();
      statusEl.append(note(`Could not refresh ${target.origin}: ${err.message}`));
    }
  }

  (async () => {
    const settled = await Promise.allSettled(targets.map(fetchOperatorAddresses));
    const next = [];
    let failures = 0;
    settled.forEach((result, i) => {
      if (result.status === "fulfilled") {
        for (const addr of result.value) next.push({ ...addr, operator: targets[i] });
      } else {
        failures++;
      }
    });
    next.sort(rowSort);
    rows = next;
    renderRows();
    statusEl.replaceChildren();
    if (failures > 0) {
      statusEl.append(note(`Could not reach ${failures} operator${failures === 1 ? "" : "s"}.`));
    }
  })();
}

function buildTable(rows, refreshOperator) {
  const table = document.createElement("table");
  table.className = "min-w-full text-sm divide-y divide-gray-200";

  const thead = document.createElement("thead");
  const headRow = document.createElement("tr");
  for (const label of ["Address", "Destination", "Operator", "Actions"]) {
    const th = document.createElement("th");
    th.className = "px-2 py-1 text-left text-xs font-medium text-gray-500";
    th.textContent = label;
    headRow.append(th);
  }
  thead.append(headRow);
  table.append(thead);

  const tbody = document.createElement("tbody");
  tbody.className = "divide-y divide-gray-100";
  for (const row of rows) {
    tbody.append(addressRow(row, refreshOperator));
  }
  table.append(tbody);

  return table;
}

function addressRow({ domain, username, destination, operator }, refreshOperator) {
  const row = document.createElement("tr");

  const addressCell = document.createElement("td");
  addressCell.className = "px-2 py-1 font-mono align-top";
  addressCell.textContent = `${username}@${domain}`;
  row.append(addressCell);

  const destCell = document.createElement("td");
  destCell.className = "px-2 py-1 font-mono text-gray-600 align-top";
  destCell.textContent = truncate(destination);
  destCell.title = destination;
  row.append(destCell);

  const operatorCell = document.createElement("td");
  operatorCell.className = "px-2 py-1 text-gray-500 align-top";
  operatorCell.textContent = operator.origin;
  row.append(operatorCell);

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
    const newDestination = editInput.value.trim();
    if (!newDestination) {
      status.replaceChildren(statusParagraph("Enter a new destination.", "error"));
      return;
    }
    confirmBtn.disabled = true;
    deleteBtn.disabled = true;
    try {
      // Build the JSON body string ONCE and reuse the identical string for
      // both the NIP-98 payload hash and the actual fetch body (see
      // modal.js's registration flow for the same discipline) — signing a
      // freshly re-serialized object could hash a different string than the
      // one sent (key order, whitespace), breaking NIP-98 verification.
      const bodyStr = JSON.stringify({ domain, username, destination: newDestination });
      const url = updateAddressUrl(operator.origin);
      const authHeader = await nip98Header(url, "PUT", bodyStr);
      await updateAddress(operator.origin, bodyStr, authHeader);
      status.replaceChildren(statusParagraph("Updated.", "status"));
      await refreshOperator(operator);
    } catch (err) {
      status.replaceChildren(statusParagraph(describeLegacyError(err.message, UPDATE_ERRORS), "error"));
      confirmBtn.disabled = false;
      deleteBtn.disabled = false;
    }
  });

  deleteBtn.addEventListener("click", async () => {
    status.replaceChildren();
    if (!confirm(`Delete ${username}@${domain}? This cannot be undone.`)) return;
    confirmBtn.disabled = true;
    deleteBtn.disabled = true;
    try {
      const bodyStr = JSON.stringify({ domain, username });
      const url = removeAddressUrl(operator.origin);
      const authHeader = await nip98Header(url, "DELETE", bodyStr);
      await removeAddress(operator.origin, bodyStr, authHeader);
      await refreshOperator(operator);
    } catch (err) {
      status.replaceChildren(statusParagraph(describeLegacyError(err.message, REMOVE_ERRORS), "error"));
      confirmBtn.disabled = false;
      deleteBtn.disabled = false;
    }
  });

  return row;
}
