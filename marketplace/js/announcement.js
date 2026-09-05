import { ANNOUNCEMENT_KIND, ANNOUNCEMENT_TAG, ANNOUNCEMENT_PREFIX } from "./config.js";

const RESERVED_TLDS = new Set(["localhost", "local", "internal", "test", "invalid", "example"]);

/**
 * Whether `host` is a public registrable DNS name (see docs/protocol/02).
 * Mirrors is_public_host in src/nostr/announcement.rs — keep the two in sync.
 */
export function isPublicHost(host) {
  if (typeof host !== "string") return false;
  const labels = host.split(".");
  if (labels.length < 2) return false;
  for (const label of labels) {
    if (label.length > 63 || !/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(label)) return false;
  }
  const tld = labels[labels.length - 1];
  if (/^[0-9]+$/.test(tld)) return false;
  return !RESERVED_TLDS.has(tld);
}

/**
 * Sanitizes an announcement's optional `users` field (docs/protocol/02, the
 * "users" section) into a plain `{domain: count}` map. Per the doc's
 * consumer rules, this NEVER fails validation of the announcement as a
 * whole — a missing/malformed `users` field just yields an empty map, and
 * individual entries that are invalid (domain not in this announcement's
 * `domains`, non-numeric/negative/fractional count) are dropped silently
 * rather than rejecting anything else in the announcement.
 */
function sanitizeUserCounts(announcement) {
  const result = {};
  if (!announcement || !Array.isArray(announcement.users)) return result;
  const domains = Array.isArray(announcement.domains) ? announcement.domains : [];
  const domainSet = new Set(domains);
  for (const entry of announcement.users) {
    if (!entry || typeof entry !== "object") continue;
    const { domain, count } = entry;
    if (typeof domain !== "string" || !domainSet.has(domain)) continue;
    if (typeof count !== "number" || !Number.isInteger(count) || count < 0 || count > Number.MAX_SAFE_INTEGER) continue;
    result[domain] = count;
  }
  return result;
}

/**
 * Validates an announcement event. Returns {ok: true, origin, dtag, announcement, userCounts}
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

  // Origin host must be a public registrable DNS name
  if (!isPublicHost(new URL(origin).hostname)) {
    return { ok: false, error: "Host is not public" };
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

  // Each domain must be a public registrable DNS name
  if (announcement.domains.some(domain => !isPublicHost(domain))) {
    return { ok: false, error: "Host is not public" };
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

  // Validate terms_url if present (must be HTTPS)
  if (announcement.terms_url) {
    try {
      const termsUrl = new URL(announcement.terms_url);
      if (termsUrl.protocol !== "https:") {
        return { ok: false, error: "Terms URL must use HTTPS" };
      }
    } catch {
      return { ok: false, error: "Terms URL must use HTTPS" };
    }
  }

  // Check expiration tags (strict integer parsing)
  if (event.tags) {
    for (const tag of event.tags) {
      if (tag[0] === "expiration") {
        const expirationStr = tag[1];
        if (!expirationStr) {
          return { ok: false, error: "Malformed expiration tag" };
        }
        // Strict integer check: must be pure decimal digits
        if (!/^\d+$/.test(expirationStr)) {
          return { ok: false, error: "Malformed expiration tag" };
        }
        const expiration = Number(expirationStr);
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
    userCounts: sanitizeUserCounts(announcement),
  };
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
