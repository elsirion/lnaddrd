import { formatSats, priceForLength, tierSummary } from "./pricing.js";

/**
 * Converts an announcement's `contact` field into a `nostr:` URI, or null
 * if it cannot be interpreted as a pubkey. `contact` may already be an
 * npub, or a 64-char hex pubkey.
 */
function contactNostrUri(contact) {
  if (typeof contact !== "string" || !contact) return null;
  if (contact.startsWith("npub1")) {
    try {
      if (window.NostrTools.nip19.decode(contact).type !== "npub") return null;
    } catch {
      return null;
    }
    return `nostr:${contact}`;
  }
  if (/^[0-9a-f]{64}$/i.test(contact)) {
    try {
      const npub = window.NostrTools.nip19.npubEncode(contact);
      return `nostr:${npub}`;
    } catch {
      return null;
    }
  }
  return null;
}

/** Returns an <a> element for `url` only if it parses as https:, else null. */
function httpsLink(url, text, className) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  if (parsed.protocol !== "https:") return null;
  const link = document.createElement("a");
  link.href = parsed.href;
  link.target = "_blank";
  link.rel = "noopener noreferrer";
  link.className = className;
  link.textContent = text;
  return link;
}

/**
 * The inline detail line shown under a row once its supplier chip has been
 * clicked: about text, a `nostr:` contact link, an HTTPS terms link, and the
 * announcement's last-updated date — exactly the fields the old per-operator
 * card footer showed, validated the same way (textContent only; the two
 * link hrefs are only ever set from values that already passed contact/terms
 * validation above).
 */
function detailLine({ about, contact, termsUrl, announcedAt }) {
  const detail = document.createElement("div");
  detail.className = "flex flex-wrap items-center gap-3 border-t border-gray-100 pt-2 text-xs text-gray-500";

  if (about) {
    const aboutText = document.createElement("span");
    aboutText.textContent = about;
    detail.append(aboutText);
  }

  const contactUri = contactNostrUri(contact);
  if (contactUri) {
    const contactLink = document.createElement("a");
    contactLink.href = contactUri;
    contactLink.className = "hover:text-gray-700";
    contactLink.textContent = "Contact";
    detail.append(contactLink);
  }

  if (termsUrl) {
    const termsLink = httpsLink(termsUrl, "Terms", "hover:text-gray-700");
    if (termsLink) detail.append(termsLink);
  }

  if (announcedAt) {
    const updated = document.createElement("span");
    updated.className = "text-gray-400";
    updated.textContent = `Announced ${new Date(announcedAt * 1000).toLocaleDateString()}`;
    detail.append(updated);
  }

  return detail;
}

/** Formats the registered-users badge text: "…" while the count is still
 * loading (usersCount not yet set for this operator), otherwise "N users" or
 * "N+ users" when the count is approximate (a relay query hit its cap — see
 * counts.js). */
function usersBadgeText(usersCount, usersApprox) {
  if (usersCount === undefined || usersCount === null) return "…";
  return `${usersCount}${usersApprox ? "+" : ""} user${usersCount === 1 && !usersApprox ? "" : "s"}`;
}

/**
 * Builds one row element for the flat domain list.
 *
 * `row` is a browse.js row (`{domain, origin, operatorName, pubkey,
 * canRegister, tiers, usersCount, usersApprox}`) augmented by app.js with
 * `about`/`contact`/`termsUrl`/`announcedAt` from the operator's
 * announcement (see buildOperatorRows in app.js) — browse.js's own
 * `buildRows` doesn't know about those fields, so they're merged in
 * afterward rather than widening that pure module's interface.
 *
 * `handlers`: `{ onRegister(row, { showError }) }` — showError(message)
 * surfaces a connect-failure message next to the clicked Register button,
 * mirroring the header connect button's own error slot in app.js.
 *
 * `options`: `{ expanded, onToggleDetail, nameQuery }` — `expanded` and
 * `onToggleDetail` drive the supplier-chip detail toggle (state lives in
 * app.js, keyed per row, so it survives re-renders); `nameQuery` is the
 * trimmed name-check input value, used to render the "<name>@<domain>: …"
 * price chip when non-empty.
 */
export function domainRow(row, handlers, { expanded, onToggleDetail, nameQuery } = {}) {
  const { domain, operatorName, origin, pubkey, canRegister, tiers, usersCount, usersApprox } = row;

  const wrapper = document.createElement("div");
  wrapper.className = "rounded-lg border border-gray-200 bg-white p-4 shadow-sm flex flex-col gap-2";

  const main = document.createElement("div");
  main.className = "flex flex-wrap items-center justify-between gap-2 text-sm";

  const left = document.createElement("div");
  left.className = "flex min-w-0 flex-wrap items-center gap-2";

  const domainText = document.createElement("span");
  domainText.className = "truncate font-medium text-gray-900";
  domainText.textContent = domain;
  left.append(domainText);

  const supplierChip = document.createElement("button");
  supplierChip.type = "button";
  supplierChip.className =
    "rounded bg-gray-100 text-gray-600 text-xs font-medium px-2 py-0.5 hover:bg-gray-200";
  supplierChip.textContent = operatorName || origin;
  supplierChip.title = origin;
  supplierChip.setAttribute("aria-expanded", expanded ? "true" : "false");
  supplierChip.addEventListener("click", () => onToggleDetail?.());
  left.append(supplierChip);

  const priceChip = document.createElement("span");
  priceChip.className = "text-xs text-gray-500";
  priceChip.textContent = tierSummary(tiers);
  left.append(priceChip);

  if (nameQuery) {
    const namePriceChip = document.createElement("span");
    namePriceChip.className = "rounded bg-blue-50 text-blue-700 text-xs font-medium px-2 py-0.5";
    namePriceChip.textContent = `${nameQuery}@${domain}: ${formatSats(priceForLength(tiers, nameQuery.length))}`;
    left.append(namePriceChip);
  }

  const right = document.createElement("div");
  right.className = "flex flex-wrap items-center gap-2";

  const usersBadge = document.createElement("span");
  usersBadge.className = "text-xs text-gray-500";
  usersBadge.textContent = usersBadgeText(usersCount, usersApprox);
  right.append(usersBadge);

  // Operators without both capabilities get an info-only row: no button,
  // and (per the Nostr-only registration model) no link out to their own
  // registration page either.
  if (canRegister) {
    const registerBtn = document.createElement("button");
    registerBtn.type = "button";
    registerBtn.id = `register-${pubkey}-${domain}`;
    registerBtn.className =
      "text-white bg-blue-700 hover:bg-blue-800 font-medium rounded-lg text-xs px-3 py-1.5";
    registerBtn.textContent = "Register";

    // Sibling error slot for "connect failed" feedback (mirrors the header
    // connect button's own error slot in app.js). Kept hidden until
    // handlers.onRegister's showError callback fires; basis-full makes it
    // wrap onto its own line in this flex row instead of squeezing the
    // button.
    const registerError = document.createElement("p");
    registerError.className = "basis-full text-xs text-red-700 hidden";
    registerError.setAttribute("role", "alert");

    registerBtn.addEventListener("click", () => {
      registerError.classList.add("hidden");
      registerError.textContent = "";
      handlers.onRegister?.(row, {
        showError(message) {
          registerError.textContent = message;
          registerError.classList.remove("hidden");
        },
      });
    });
    right.append(registerBtn, registerError);
  }

  main.append(left, right);
  wrapper.append(main);

  if (expanded) {
    wrapper.append(detailLine(row));
  }

  return wrapper;
}
