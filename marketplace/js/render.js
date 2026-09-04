import { priceSummary } from "./announcement.js";

const VERIFIED_BADGE_CLASS = "text-xs font-medium px-2.5 py-0.5 rounded bg-green-100 text-green-800";

/**
 * Returns a <span> badge marking a domain as having passed its well-known
 * check. A card only ever lists verified domains (see app.js's visibility
 * logic in visibility.js), so there is no "mismatch"/"unreachable"/
 * "checking" variant to render here — those states just keep a domain out
 * of the card entirely instead of showing a different badge.
 */
function verifiedBadge() {
  const el = document.createElement("span");
  el.className = VERIFIED_BADGE_CLASS;
  el.textContent = "✓ verified";
  return el;
}

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
 * Builds a card element for one discovered operator.
 * entry: { validated: {origin, dtag, announcement}, event }
 * verifiedDomains: the subset of announcement.domains that passed their
 *   well-known check (from visibility.js's classifyOperator) — the only
 *   domains this card, or the registration modal it opens, ever shows.
 * handlers: { onRegister(entry, domain) }
 */
export function operatorCard(entry, verifiedDomains, handlers) {
  const { validated, event } = entry;
  const { announcement, origin } = validated;
  const pubkey = event.pubkey;

  const card = document.createElement("div");
  card.className = "rounded-lg border border-gray-200 bg-white p-5 shadow-sm flex flex-col gap-3";

  const title = document.createElement("h3");
  title.className = "text-lg font-semibold";
  title.textContent = announcement.name || origin;
  card.append(title);

  if (announcement.about) {
    const about = document.createElement("p");
    about.className = "text-sm text-gray-500";
    about.textContent = announcement.about;
    card.append(about);
  }

  const domainList = document.createElement("div");
  domainList.className = "flex flex-col gap-2";

  for (const domain of verifiedDomains) {
    const row = document.createElement("div");
    row.className = "flex flex-wrap items-center justify-between gap-2 text-sm";

    const left = document.createElement("div");
    left.className = "flex min-w-0 items-center gap-2";

    const domainText = document.createElement("span");
    domainText.className = "truncate font-mono text-gray-800";
    domainText.textContent = domain;
    left.append(domainText);

    left.append(verifiedBadge());

    const price = priceSummary(announcement, domain);
    if (price) {
      const priceEl = document.createElement("span");
      priceEl.className = "text-xs text-gray-500";
      priceEl.textContent = price;
      left.append(priceEl);
    }

    const right = document.createElement("div");
    right.className = "flex flex-wrap items-center gap-2";

    if (Array.isArray(announcement.capabilities) && announcement.capabilities.includes("registration-api-v1")) {
      const registerBtn = document.createElement("button");
      registerBtn.type = "button";
      registerBtn.id = `register-${pubkey}-${domain}`;
      registerBtn.className =
        "text-white bg-blue-700 hover:bg-blue-800 font-medium rounded-lg text-xs px-3 py-1.5";
      registerBtn.textContent = "Register";
      registerBtn.addEventListener("click", () => handlers.onRegister?.(entry, domain));
      right.append(registerBtn);
    } else {
      const link = httpsLink(
        announcement.registration_url,
        "Register on operator's site",
        "text-xs font-medium text-blue-700 hover:underline"
      );
      if (link) right.append(link);
    }

    row.append(left, right);
    domainList.append(row);
  }
  card.append(domainList);

  if (Array.isArray(announcement.capabilities) && announcement.capabilities.length) {
    const capabilitiesRow = document.createElement("div");
    capabilitiesRow.className = "flex flex-wrap gap-1.5";
    for (const capability of announcement.capabilities) {
      const chip = document.createElement("span");
      chip.className = "bg-gray-100 text-gray-600 text-xs font-medium px-2 py-0.5 rounded";
      chip.textContent = capability;
      capabilitiesRow.append(chip);
    }
    card.append(capabilitiesRow);
  }

  const footer = document.createElement("div");
  footer.className = "flex flex-wrap items-center gap-3 border-t border-gray-100 pt-2 text-xs text-gray-400";

  const contactUri = contactNostrUri(announcement.contact);
  if (contactUri) {
    const contactLink = document.createElement("a");
    contactLink.href = contactUri;
    contactLink.className = "hover:text-gray-600";
    contactLink.textContent = "Contact";
    footer.append(contactLink);
  }

  if (announcement.terms_url) {
    const termsLink = httpsLink(announcement.terms_url, "Terms", "hover:text-gray-600");
    if (termsLink) footer.append(termsLink);
  }

  const updated = document.createElement("span");
  updated.textContent = `Announced ${new Date(event.created_at * 1000).toLocaleDateString()}`;
  footer.append(updated);

  card.append(footer);

  return card;
}
