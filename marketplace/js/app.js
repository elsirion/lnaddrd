import { ANNOUNCEMENT_KIND, ANNOUNCEMENT_TAG } from "./config.js";
import { validateAnnouncement, upsertByCoordinate } from "./announcement.js";
import { currentRelays, renderRelayEditor } from "./relays.js";
import { operatorCard, applyBadgeState } from "./render.js";

// Tab switching
document.querySelectorAll('.tab-btn').forEach(btn => {
  btn.addEventListener('click', () => {
    const tab = btn.dataset.tab;

    // Hide all sections
    document.getElementById('operators').classList.add('hidden');
    document.getElementById('manage').classList.add('hidden');

    // Show the selected section
    if (tab === 'browse') {
      document.getElementById('operators').classList.remove('hidden');
    } else if (tab === 'manage') {
      document.getElementById('manage').classList.remove('hidden');
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
// incidental re-renders triggered by unrelated events.
const domainStatus = new Map();
let pool = null;
let activeRelays = [];
let renderTimer = null;

const handlers = {
  getDomainStatus(pubkey, domain) {
    return domainStatus.get(`${pubkey}:${domain}`) ?? "checking";
  },
  // Stub for Task 16, which will replace this with the real registration
  // modal. Shows an inline note in the card instead of an alert().
  onRegister(entry, domain) {
    const pubkey = entry.event.pubkey;
    const note = document.getElementById(`register-note-${pubkey}-${domain}`);
    if (note) {
      note.textContent = "Registration UI coming in the next step.";
      note.classList.remove("hidden");
    }
    const button = document.getElementById(`register-${pubkey}-${domain}`);
    if (button) button.disabled = true;
  },
};

function startDiscovery() {
  const relays = currentRelays();
  if (pool) pool.close(activeRelays);
  operators.clear();
  domainStatus.clear();
  activeRelays = relays;
  pool = new NostrTools.SimplePool();
  const now = Math.floor(Date.now() / 1000);
  pool.subscribeMany(relays, [{ kinds: [ANNOUNCEMENT_KIND], "#t": [ANNOUNCEMENT_TAG] }], {
    onevent(event) {
      if (!NostrTools.verifyEvent(event)) return;
      const validated = validateAnnouncement(event, now);
      const before = operators.get(`${event.pubkey}:${validated.dtag ?? ""}`);
      upsertByCoordinate(operators, validated, event);
      scheduleRenderOperators();
      if (validated.ok && !before) verifyDomains(validated, event);
    },
    oneose() {
      relays.forEach(relay => setRelayState(relay, "connected"));
    },
    onclose(reasons) {
      reasons.forEach((reason, i) => {
        if (reason) setRelayState(relays[i], "error", reason);
      });
    },
  });
  renderRelayStatus();
  renderOperators();
}

function scheduleRenderOperators() {
  if (renderTimer) return;
  renderTimer = setTimeout(() => {
    renderTimer = null;
    renderOperators();
  }, 100);
}

function renderOperators() {
  const container = document.getElementById("operators");
  container.replaceChildren();
  const sorted = [...operators.values()].sort((a, b) => a.validated.origin.localeCompare(b.validated.origin));
  for (const entry of sorted) {
    container.append(operatorCard(entry, handlers));
  }
}

const relayChips = new Map();

function renderRelayStatus() {
  const container = document.getElementById("relay-status");
  container.replaceChildren();
  relayChips.clear();
  for (const relay of currentRelays()) {
    const chip = document.createElement("span");
    chip.className = "rounded px-2.5 py-0.5 text-xs font-medium font-mono bg-gray-100 text-gray-600";
    chip.textContent = relay;
    container.append(chip);
    relayChips.set(relay, chip);
  }
}

function setRelayState(relay, state, reason) {
  const chip = relayChips.get(relay);
  if (!chip) return;
  if (state === "connected") {
    chip.className = "rounded px-2.5 py-0.5 text-xs font-medium font-mono bg-green-100 text-green-800";
    chip.textContent = relay;
  } else {
    chip.className = "rounded px-2.5 py-0.5 text-xs font-medium font-mono bg-red-100 text-red-800";
    chip.textContent = reason ? `${relay} (${reason})` : relay;
  }
}

async function verifyDomains(validated, event) {
  for (const domain of validated.announcement.domains) {
    updateBadge(event.pubkey, domain, "checking");
    updateBadge(event.pubkey, domain, await verifyDomain(domain, event.pubkey, validated.dtag));
  }
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

function updateBadge(pubkey, domain, state) {
  domainStatus.set(`${pubkey}:${domain}`, state);
  const el = document.getElementById(`badge-${pubkey}-${domain}`);
  if (el) applyBadgeState(el, state);
}

function onRelaysChanged() {
  renderRelayEditor(document.getElementById("relay-editor"), onRelaysChanged);
  startDiscovery();
}

onRelaysChanged();
