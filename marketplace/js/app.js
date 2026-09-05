import { ANNOUNCEMENT_KIND, ANNOUNCEMENT_TAG } from "./config.js";
import { validateAnnouncement, upsertByCoordinate } from "./announcement.js";
import { currentRelays, renderRelayEditor } from "./relays.js";
import { domainRow } from "./render.js";
import { buildRows, filterRows, sortRows } from "./browse.js";
import { fetchBackupCounts } from "./counts.js";
import { classifyOperator, reconcileDomainStatuses } from "./visibility.js";
import { openRegisterModal } from "./modal.js";
import { connect } from "./nostr-auth.js";
import { renderManage } from "./manage.js";

// Fallback name length used for the "price" sort when the name-check input
// is empty — per the plan, a representative-but-arbitrary length so
// short-name-favoring tier structures don't just collapse to "cheapest
// possible" when nobody has typed a name yet.
const DEFAULT_SORT_LENGTH = 8;

// The Manage tab button itself starts `hidden` in index.html (there is no
// identity connected yet); doConnect() below reveals it. Its panel handles
// the (normally unreachable, since the button is hidden) case of somehow
// being activated while disconnected by rendering only a connect prompt —
// see manage.js's renderManage().
const manageTabBtn = document.querySelector('.tab-btn[data-tab="manage"]');

// Tab switching
document.querySelectorAll('.tab-btn').forEach(btn => {
  btn.addEventListener('click', () => {
    const tab = btn.dataset.tab;

    // Hide all sections
    document.getElementById('browse-panel').classList.add('hidden');
    document.getElementById('manage').classList.add('hidden');

    // Show the selected section
    if (tab === 'browse') {
      document.getElementById('browse-panel').classList.remove('hidden');
    } else if (tab === 'manage') {
      document.getElementById('manage').classList.remove('hidden');
      renderManage(document.getElementById('manage'), { operators, connectedPubkey, getDomainStatus });
    }

    // Update button styles
    document.querySelectorAll('.tab-btn').forEach(b => {
      if (b === btn) {
        b.classList.remove('border-transparent', 'text-gray-500');
        b.classList.add('border-blue-700', 'text-blue-700');
      } else {
        b.classList.remove('border-blue-700', 'text-blue-700');
        b.classList.add('border-transparent', 'text-gray-500');
      }
    });
  });
});

// --- Discovery ---

const operators = new Map();
// Per-domain verification state, keyed `${pubkey}:${domain}`, so it survives
// incidental re-renders triggered by unrelated events. Values are
// "checking" | "verified" | "mismatch" | "unreachable"; classifyOperator()
// in visibility.js reads this indirectly via getDomainStatus below.
const domainStatus = new Map();
// Relays that have reached eose or closed for the current discovery run —
// used only to decide when the "Discovering operators…" placeholder should
// give way to "No operators found" (see renderDiscoveryStatus).
const settledRelays = new Set();
let pool = null;
let activeRelays = [];
let activeSubscriptions = [];
let renderTimer = null;
// Registered-users counts (Task 3): pubkeys already fetched-or-fetching, so
// a republished announcement or a duplicate delivery from another relay
// never triggers a second backup-record query for the same operator.
const countedPubkeys = new Set();
// Newly discovered pubkeys awaiting the next debounced fetchBackupCounts
// call — batched the same way scheduleRenderOperators batches re-renders,
// so several operators announcing in quick succession share one query
// instead of one relay round-trip each.
let pendingCountPubkeys = new Set();
let countFetchTimer = null;
let activeCountFetches = [];
// Set once a Nostr signing extension is connected; passed through to the
// registration modal as the claimed owner_pubkey, and used by the Manage
// tab's NIP-98-authenticated address list.
let connectedPubkey = null;

// --- Nostr connect ---

const connectBtn = document.getElementById("connect-nostr");
// Inserted as a sibling of the button (rather than added to index.html)
// since it's purely a JS-owned inline error slot; `basis-full` makes it
// wrap onto its own line in the header's flex row instead of squeezing the
// button.
const connectError = document.createElement("p");
connectError.id = "connect-error";
connectError.className = "basis-full text-right text-xs text-red-700 hidden";
connectError.setAttribute("role", "alert");
connectBtn.after(connectError);

// Shared by the header button and the per-card Register button (see
// handlers.onRegister below) so both paths update the same connected-pubkey
// state and the same header UI — there is exactly one way to connect.
async function doConnect() {
  connectedPubkey = await connect();
  connectBtn.textContent = `${NostrTools.nip19.npubEncode(connectedPubkey).slice(0, 12)}…`;
  manageTabBtn.classList.remove("hidden");
  if (!document.getElementById("manage").classList.contains("hidden")) {
    renderManage(document.getElementById("manage"), { operators, connectedPubkey, getDomainStatus });
  }
  return connectedPubkey;
}

connectBtn.addEventListener("click", async () => {
  connectError.classList.add("hidden");
  connectError.textContent = "";
  try {
    await doConnect();
  } catch (err) {
    connectError.textContent = err.message;
    connectError.classList.remove("hidden");
  }
});

function getDomainStatus(pubkey, domain) {
  return domainStatus.get(`${pubkey}:${domain}`) ?? "checking";
}

const handlers = {
  // Register is Nostr-identity-only: with no connected identity yet, run the
  // same connect() path the header button uses (updating the shared pubkey
  // state and header UI) before opening the modal. A connect failure (no
  // extension, user rejection) is surfaced next to the clicked button
  // instead — the modal never opens without a signed-in identity.
  async onRegister(row, { showError } = {}) {
    if (!connectedPubkey) {
      try {
        await doConnect();
      } catch {
        showError?.("Connect a Nostr extension to register");
        return;
      }
    }
    openRegisterModal({ origin: row.origin, domain: row.domain, ownerPubkey: connectedPubkey });
  },
};

// --- Browse controls (search / name-check / sort) ---
//
// Purely in-memory: every input/change event just re-runs renderOperators(),
// which re-derives rows from the already-discovered operator/verification
// state via buildRows/filterRows/sortRows — no relay or network work happens
// here.
const searchInput = document.getElementById("browse-search");
const nameInput = document.getElementById("browse-name");
const sortSelect = document.getElementById("browse-sort");
// Which rows currently show their supplier detail line, keyed
// `${pubkey}:${domain}` (per-row, not per-operator — see render.js's
// domainRow doc comment).
const expandedRows = new Set();

function rowKey(row) {
  return `${row.pubkey}:${row.domain}`;
}

searchInput.addEventListener("input", () => renderOperators());
nameInput.addEventListener("input", () => renderOperators());
sortSelect.addEventListener("change", () => renderOperators());

function startDiscovery() {
  const relays = currentRelays();
  // Close previous per-relay subscriptions, then the underlying relay
  // connections themselves (subscription close() only sends CLOSE for the
  // REQ; the WebSocket connections are cached on the pool by URL).
  activeSubscriptions.forEach(sub => sub.close());
  activeCountFetches.forEach(countFetch => countFetch.close());
  if (pool) pool.close(activeRelays);
  operators.clear();
  domainStatus.clear();
  settledRelays.clear();
  countedPubkeys.clear();
  pendingCountPubkeys = new Set();
  if (countFetchTimer) {
    clearTimeout(countFetchTimer);
    countFetchTimer = null;
  }
  activeRelays = relays;
  activeSubscriptions = [];
  activeCountFetches = [];
  pool = new NostrTools.SimplePool();
  renderRelayStatus();
  const filter = { kinds: [ANNOUNCEMENT_KIND], "#t": [ANNOUNCEMENT_TAG] };
  // Subscribe per relay (rather than one subscribeMany call across all
  // relays) so each relay's chip reflects only its own connection state.
  // The bundled SimplePool aggregates oneose/onclose across every relay in
  // a single subscribeMany call - including relays that never connected -
  // which would otherwise paint a never-connected relay green.
  for (const relay of relays) {
    setRelayState(relay, "checking");
    const sub = pool.subscribeMany([relay], [filter], {
      onevent(event) {
        if (!NostrTools.verifyEvent(event)) return;
        const now = Math.floor(Date.now() / 1000);
        const validated = validateAnnouncement(event, now);
        const key = `${event.pubkey}:${validated.dtag ?? ""}`;
        const before = operators.get(key);
        upsertByCoordinate(operators, validated, event);
        const after = operators.get(key);
        scheduleRenderOperators();
        // Registered-users count: fetched once per newly discovered operator
        // pubkey (never per keystroke, never re-fetched for a duplicate or
        // republished announcement from a pubkey already counted-or-queued).
        // Gated on validated.ok so a malformed announcement never burns the
        // pubkey's one shot at being counted — an operator that republishes
        // a corrected, valid announcement later still gets its count fetched.
        if (validated.ok && !countedPubkeys.has(event.pubkey)) {
          countedPubkeys.add(event.pubkey);
          pendingCountPubkeys.add(event.pubkey);
          scheduleCountFetch();
        }
        // Re-verify domains for brand-new coordinates, and for republished
        // announcements whose stored event actually changed (so updated
        // domain lists get checked too), but not for duplicate deliveries
        // of the same event arriving from another relay.
        const changed = validated.ok && (!before || before.event.id !== after.event.id);
        if (changed) verifyDomains(validated, event);
      },
      oneose() {
        setRelayState(relay, "connected");
        settledRelays.add(relay);
        scheduleRenderOperators();
      },
      onclose(reasons) {
        setRelayState(relay, "error", reasons?.[0]);
        settledRelays.add(relay);
        scheduleRenderOperators();
      },
    });
    activeSubscriptions.push(sub);
  }
  renderOperators();
}

function scheduleRenderOperators() {
  if (renderTimer) return;
  renderTimer = setTimeout(() => {
    renderTimer = null;
    renderOperators();
  }, 100);
}

// Debounced the same way scheduleRenderOperators is: several operators
// announcing within the same 100ms window share one fetchBackupCounts call
// (one query per relay for the whole batch) instead of one per pubkey.
function scheduleCountFetch() {
  if (countFetchTimer) return;
  countFetchTimer = setTimeout(() => {
    countFetchTimer = null;
    const batch = [...pendingCountPubkeys];
    pendingCountPubkeys = new Set();
    if (batch.length === 0) return;
    activeCountFetches.push(fetchBackupCounts(pool, activeRelays, batch, applyCounts));
  }, 100);
}

/**
 * fetchBackupCounts' onUpdate callback: writes usersCount/usersApprox onto
 * every `operators` Map entry authored by `pubkey` (there can be more than
 * one — the map is keyed `${pubkey}:${dtag}`, so a single service pubkey
 * publishing under more than one d-tag/origin has one entry per coordinate)
 * and triggers the existing debounced re-render so buildOperatorRows picks
 * up the new counts on its next pass.
 */
function applyCounts(pubkey, usersCount, usersApprox) {
  let changed = false;
  for (const entry of operators.values()) {
    if (entry.event.pubkey === pubkey) {
      entry.usersCount = usersCount;
      entry.usersApprox = usersApprox;
      changed = true;
    }
  }
  if (changed) scheduleRenderOperators();
}

/**
 * Flattens the discovered-operator map into `buildRows`' input shape (one
 * entry per visible operator, verified domains only), plus a side lookup of
 * per-operator detail fields (about/contact/terms/announced-at) keyed by
 * origin. `buildRows` (browse.js, Task 1) only knows about the fields its
 * own interface lists — about/contact/terms_url aren't among them — so
 * those are merged onto the resulting rows afterward rather than widening
 * that pure module's interface for a UI-only concern.
 */
function buildOperatorRows() {
  const sorted = [...operators.values()].sort((a, b) => a.validated.origin.localeCompare(b.validated.origin));

  const operatorsForRows = [];
  const metaByOrigin = new Map();
  let hidden = 0;
  let pending = false;

  for (const entry of sorted) {
    const pubkey = entry.event.pubkey;
    const { announcement, origin } = entry.validated;
    const domains = announcement.domains;
    const { verified, category } = classifyOperator(domains, domain => getDomainStatus(pubkey, domain));
    if (category === "visible") {
      operatorsForRows.push({
        origin,
        name: announcement.name || origin,
        pubkey,
        capabilities: announcement.capabilities,
        verifiedDomains: verified,
        pricing: announcement.pricing,
        // Populated asynchronously by applyCounts() (see the discovery
        // onevent handler / scheduleCountFetch above) once counts.js's
        // fetchBackupCounts resolves for this pubkey; undefined until then,
        // which buildRows carries through unchanged and render.js's
        // usersBadgeText renders as "…".
        usersCount: entry.usersCount,
        usersApprox: entry.usersApprox,
      });
      metaByOrigin.set(origin, {
        about: announcement.about,
        contact: announcement.contact,
        termsUrl: announcement.terms_url,
        announcedAt: entry.event.created_at,
      });
    } else if (category === "hidden") {
      hidden++;
    } else {
      pending = true;
    }
  }

  const rows = buildRows(operatorsForRows);
  for (const row of rows) {
    const meta = metaByOrigin.get(row.origin);
    if (meta) Object.assign(row, meta);
  }

  return { rows, hidden, pending };
}

function renderOperators() {
  const container = document.getElementById("operators");
  const { rows, hidden, pending } = buildOperatorRows();

  const query = searchInput.value.trim();
  const name = nameInput.value.trim();
  const filtered = filterRows(rows, { query, name });
  const sorted = sortRows(filtered, { by: sortSelect.value, length: name ? name.length : DEFAULT_SORT_LENGTH });

  container.replaceChildren();
  for (const row of sorted) {
    const key = rowKey(row);
    container.append(
      domainRow(row, handlers, {
        expanded: expandedRows.has(key),
        onToggleDetail() {
          if (expandedRows.has(key)) {
            expandedRows.delete(key);
          } else {
            expandedRows.add(key);
          }
          renderOperators();
        },
        nameQuery: name,
      })
    );
  }

  renderDiscoveryStatus(rows.length, sorted.length, hidden, pending);
}

function relaysSettled() {
  return activeRelays.every(relay => settledRelays.has(relay));
}

/**
 * Updates the "Discovering operators…" / "No operators found" / "No domains
 * match" placeholder and the muted "N operator(s) hidden (unverified)" line
 * beneath the row list.
 *
 * `totalRows` is the row count *before* search/name filtering — the
 * discovery-level states below only care whether any verified domain exists
 * at all, never whether the current filters happen to hide it. `shownRows`
 * is the count *after* filtering: when discovery has produced at least one
 * row but the filters emptied the list, that's "No domains match" rather
 * than any of the discovery states.
 *
 * Discovery states: the placeholder reads "No operators found" only once
 * every relay has settled (eose/closed) and no operator's domain checks are
 * still in flight — otherwise it keeps reading "Discovering operators…".
 */
function renderDiscoveryStatus(totalRows, shownRows, hidden, pending) {
  const placeholder = document.getElementById("operators-placeholder");
  const hiddenCount = document.getElementById("operators-hidden-count");
  const settled = !pending && relaysSettled();

  if (totalRows > 0) {
    if (shownRows === 0) {
      placeholder.textContent = "No domains match";
      placeholder.classList.remove("hidden");
    } else {
      placeholder.classList.add("hidden");
    }
  } else if (!settled) {
    placeholder.textContent = "Discovering operators…";
    placeholder.classList.remove("hidden");
  } else if (hidden === 0) {
    placeholder.textContent = "No operators found";
    placeholder.classList.remove("hidden");
  } else {
    // Nothing to show, but the hidden-count line below already explains why.
    placeholder.classList.add("hidden");
  }

  if (hidden > 0) {
    hiddenCount.textContent = `${hidden} operator${hidden === 1 ? "" : "s"} hidden (unverified)`;
    hiddenCount.classList.remove("hidden");
  } else {
    hiddenCount.classList.add("hidden");
  }
}

const relayChips = new Map();

function renderRelayStatus() {
  const container = document.getElementById("relay-status");
  container.replaceChildren();
  relayChips.clear();
  for (const relay of currentRelays()) {
    const chip = document.createElement("span");
    chip.className = `${RELAY_CHIP_BASE} bg-gray-100 text-gray-600`;
    chip.textContent = relay;
    container.append(chip);
    relayChips.set(relay, chip);
  }
}

const RELAY_CHIP_BASE = "rounded px-2.5 py-0.5 text-xs font-medium font-mono";

function setRelayState(relay, state, reason) {
  const chip = relayChips.get(relay);
  if (!chip) return;
  if (state === "connected") {
    chip.className = `${RELAY_CHIP_BASE} bg-green-100 text-green-800`;
    chip.textContent = relay;
  } else if (state === "checking") {
    chip.className = `${RELAY_CHIP_BASE} bg-blue-100 text-blue-800`;
    chip.textContent = relay;
  } else {
    chip.className = `${RELAY_CHIP_BASE} bg-red-100 text-red-800`;
    chip.textContent = reason ? `${relay} (${reason})` : relay;
  }
}

async function verifyDomains(validated, event) {
  const pubkey = event.pubkey;
  const domains = validated.announcement.domains;
  const keep = new Set(domains);

  // Drop status entries for domains this pubkey no longer announces, then
  // seed the surviving + new domain list via reconcileDomainStatuses:
  // domains already known keep their last status (stale-but-correct) so an
  // already-verified card doesn't flicker away on a routine republish that
  // only bumps the event id; only genuinely new domains start "checking".
  for (const key of domainStatus.keys()) {
    if (key.startsWith(`${pubkey}:`) && !keep.has(key.slice(pubkey.length + 1))) {
      domainStatus.delete(key);
    }
  }
  const next = reconcileDomainStatuses(domains, domain => domainStatus.get(`${pubkey}:${domain}`));
  for (const [domain, status] of next) {
    domainStatus.set(`${pubkey}:${domain}`, status);
  }
  scheduleRenderOperators();

  // Concurrent, not sequential: an operator with several domains shouldn't
  // pay N x the 5s well-known timeout before its last domain even starts.
  await Promise.all(domains.map(async domain => {
    const state = await verifyDomain(domain, pubkey, validated.dtag);
    domainStatus.set(`${pubkey}:${domain}`, state);
    scheduleRenderOperators();
  }));
}

async function verifyDomain(domain, pubkey, dtag) {
  try {
    const response = await fetch(`https://${domain}/.well-known/lnaddrd.json`, { signal: AbortSignal.timeout(5000) });
    if (!response.ok) return "unreachable";
    const doc = await response.json();
    return doc.schema === 1 && doc.service_pubkey === pubkey &&
      doc.announcement === `30078:${pubkey}:${dtag}` ? "verified" : "mismatch";
  } catch { return "unreachable"; }
}

function onRelaysChanged() {
  renderRelayEditor(document.getElementById("relay-editor"), onRelaysChanged);
  startDiscovery();
}

onRelaysChanged();
