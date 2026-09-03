import { ANNOUNCEMENT_KIND, ANNOUNCEMENT_TAG, ANNOUNCEMENT_PREFIX } from "./config.js";

/**
 * Validates an announcement event. Returns {ok: true, origin, dtag, announcement}
 * or {ok: false, error}.
 * Mirrors src/nostr/discovery.rs::validate_event (excluding signature check).
 */
export function validateAnnouncement(event, nowSecs) {
  // Check kind
  if (event.kind !== ANNOUNCEMENT_KIND) {
    return { ok: false, error: "Unexpected event kind" };
  }

  // Find and validate d tag
  const dTag = event.tags?.find(tag => tag[0] === "d")?.[1];
  if (!dTag) {
    return { ok: false, error: "Missing announcement identifier" };
  }

  // Extract and validate origin from d tag
  if (!dTag.startsWith(ANNOUNCEMENT_PREFIX)) {
    return { ok: false, error: "Unexpected identifier" };
  }
  const origin = dTag.slice(ANNOUNCEMENT_PREFIX.length);

  // Validate origin is canonical (https and url.origin === origin)
  try {
    const url = new URL(origin);
    if (url.protocol !== "https:") {
      return { ok: false, error: "Non-canonical origin identifier" };
    }
    if (url.origin !== origin) {
      return { ok: false, error: "Non-canonical origin identifier" };
    }
  } catch {
    return { ok: false, error: "Non-canonical origin identifier" };
  }

  // Check for t tag
  const hasLightningAddressServiceTag = event.tags?.some(
    tag => tag[0] === "t" && tag[1] === ANNOUNCEMENT_TAG
  );
  if (!hasLightningAddressServiceTag) {
    return { ok: false, error: "Missing lightning-address-service tag" };
  }

  // Parse and validate content
  let announcement;
  try {
    announcement = JSON.parse(event.content);
  } catch {
    return { ok: false, error: "Invalid announcement content" };
  }

  // Check schema
  if (announcement.schema !== 1) {
    return { ok: false, error: "Unsupported announcement schema" };
  }

  // Check status (not retired)
  if (announcement.status === "retired") {
    return { ok: false, error: "Service is retired" };
  }

  // Check origin matches
  if (announcement.origin !== origin) {
    return { ok: false, error: "Origin does not match identifier" };
  }

  // Check domains are non-empty
  if (!announcement.domains || !Array.isArray(announcement.domains) || announcement.domains.length === 0) {
    return { ok: false, error: "Announcement has no domains" };
  }

  // Check domains are sorted and unique
  const sorted = [...announcement.domains].sort();
  // Dedup while checking
  let isDuplicate = false;
  for (let i = 1; i < sorted.length; i++) {
    if (sorted[i] === sorted[i - 1]) {
      isDuplicate = true;
      break;
    }
  }
  if (isDuplicate || sorted.some((d, i) => d !== announcement.domains[i])) {
    return { ok: false, error: "Domains are not sorted and unique" };
  }

  // Validate registration_url
  try {
    const regUrl = new URL(announcement.registration_url);
    if (regUrl.origin !== origin) {
      return { ok: false, error: "Registration URL has another origin" };
    }
  } catch {
    return { ok: false, error: "Registration URL has another origin" };
  }

  // Check expiration tags
  if (event.tags) {
    for (const tag of event.tags) {
      if (tag[0] === "expiration") {
        const expirationStr = tag[1];
        if (!expirationStr) {
          return { ok: false, error: "Malformed expiration tag" };
        }
        const expiration = parseInt(expirationStr, 10);
        if (isNaN(expiration)) {
          return { ok: false, error: "Malformed expiration tag" };
        }
        if (expiration <= nowSecs) {
          return { ok: false, error: "Announcement is expired" };
        }
      }
    }
  }

  // All validations passed
  return {
    ok: true,
    origin,
    dtag: dTag,
    announcement,
  };
}

/**
 * Returns a price summary string for a domain.
 * "free" if min tier price is 0, "from X sat(s)" if > 0, null if no entry.
 */
export function priceSummary(announcement, domain) {
  if (!announcement.pricing || !Array.isArray(announcement.pricing)) {
    return null;
  }

  const pricingEntry = announcement.pricing.find(p => p.domain === domain);
  if (!pricingEntry) {
    return null;
  }

  if (!pricingEntry.tiers || !Array.isArray(pricingEntry.tiers) || pricingEntry.tiers.length === 0) {
    return null;
  }

  const minPrice = Math.min(...pricingEntry.tiers.map(t => t.price));

  if (minPrice === 0) {
    return "free";
  }

  const satoshis = Math.ceil(minPrice / 1000);
  const formatted = satoshis.toLocaleString("en-US");
  return satoshis === 1 ? `from ${formatted} sat` : `from ${formatted} sats`;
}

/**
 * Deduplicates announcements by pubkey + dtag coordinate.
 * Keeps newest based on (created_at, id) tuple (lexicographic comparison).
 * No-op if validated.ok is false.
 */
export function upsertByCoordinate(map, validated, event) {
  if (!validated.ok) {
    return;
  }

  const key = `${event.pubkey}:${validated.dtag}`;
  const existing = map.get(key);

  if (!existing || (event.created_at > existing.event.created_at ||
      (event.created_at === existing.event.created_at && event.id > existing.event.id))) {
    map.set(key, { validated, event });
  }
}
