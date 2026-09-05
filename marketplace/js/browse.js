// Pure row-building/filter/sort logic for the flat domain browser. Kept
// dependency-free (no DOM) so it can be unit tested directly; app.js/render.js
// wire it to the live operator/verification state.

import { priceForLength } from "./pricing.js";

/**
 * Flattens discovered operators into one row per verified domain.
 *
 * `operators` is `[{origin, name, pubkey, capabilities, verifiedDomains,
 * pricing, usersCount, usersApprox, userCounts}]`. `pricing` is the
 * announcement's pricing array (`[{domain, currency, tiers}]`, see
 * docs/protocol/02); `usersCount`/`usersApprox` are operator-level (not
 * per-domain) backup-record scan counts. `userCounts` is the sanitized
 * per-domain self-reported map from announcement.js's `validateAnnouncement`
 * (`{domain: count}`, only domains the operator actually announced a usable
 * count for).
 *
 * Per docs/superpowers/plans/2026-09-05-announced-user-counts.md's Global
 * Constraints, an announced count wins per row. For each row:
 *  - If `userCounts` has an entry for that row's domain, the row uses it:
 *    `usersCount` from the map, `usersApprox: false`, `usersSource:
 *    "announced"`.
 *  - Else, if the operator announced a usable count for *some* domain (so
 *    app.js never scheduled a backup-record scan for this operator at all —
 *    see app.js's countedPubkeys gating), the row has no count and never
 *    will: `usersCount: undefined`, `usersSource: "unavailable"`.
 *  - Else the operator has no usable announced counts anywhere, so it went
 *    through the normal scan path: `usersCount`/`usersApprox` copied from
 *    the operator (as before Task 2), `usersSource: "observed"`.
 *
 * Each row is `{domain, origin, operatorName, pubkey, canRegister, tiers,
 * usersCount, usersApprox, usersSource}`, where `tiers` is the pricing
 * entry's tiers for that domain, or `[]` if the operator published none.
 */
export function buildRows(operators) {
  const rows = [];
  for (const operator of operators) {
    const capabilities = Array.isArray(operator.capabilities) ? operator.capabilities : [];
    const canRegister = capabilities.includes("registration-api-v1") && capabilities.includes("nostr-auth");
    const pricing = Array.isArray(operator.pricing) ? operator.pricing : [];
    const domains = Array.isArray(operator.verifiedDomains) ? operator.verifiedDomains : [];
    const userCounts = operator.userCounts && typeof operator.userCounts === "object" ? operator.userCounts : {};
    const hasAnnouncedCounts = Object.keys(userCounts).length > 0;

    for (const domain of domains) {
      const pricingEntry = pricing.find(p => p.domain === domain);
      const tiers = pricingEntry && Array.isArray(pricingEntry.tiers) ? pricingEntry.tiers : [];

      let usersCount;
      let usersApprox;
      let usersSource;
      if (Object.prototype.hasOwnProperty.call(userCounts, domain)) {
        usersCount = userCounts[domain];
        usersApprox = false;
        usersSource = "announced";
      } else if (hasAnnouncedCounts) {
        usersCount = undefined;
        usersApprox = false;
        usersSource = "unavailable";
      } else {
        usersCount = operator.usersCount;
        usersApprox = operator.usersApprox;
        usersSource = "observed";
      }

      rows.push({
        domain,
        origin: operator.origin,
        operatorName: operator.name,
        pubkey: operator.pubkey,
        canRegister,
        tiers,
        usersCount,
        usersApprox,
        usersSource,
      });
    }
  }
  return rows;
}

/**
 * Filters rows in memory.
 *
 * `query` (case-insensitive substring on `domain` or `operatorName`) and
 * `name` (non-empty: keep only rows where `priceForLength(tiers,
 * name.length) === 0`, i.e. that name would register free) combine with AND
 * semantics. Either filter is skipped when its value is absent/empty. The
 * strict `=== 0` check means `null` (that name's length is unavailable —
 * see pricing.js's priceForLength) never passes the filter either.
 */
export function filterRows(rows, { query, name } = {}) {
  let result = rows;

  if (query) {
    const needle = query.toLowerCase();
    result = result.filter(
      row =>
        row.domain.toLowerCase().includes(needle) ||
        (row.operatorName ?? "").toLowerCase().includes(needle)
    );
  }

  if (name) {
    result = result.filter(row => priceForLength(row.tiers, name.length) === 0);
  }

  return result;
}

function compareDomain(a, b) {
  return a.domain < b.domain ? -1 : a.domain > b.domain ? 1 : 0;
}

/**
 * Returns a new, sorted array of rows (pure — never mutates `rows`).
 *
 * `by`:
 *  - "alpha": domain ascending.
 *  - "price": `priceForLength(tiers, length)` ascending (cheapest first);
 *    `null` (that length is unavailable — see pricing.js) sorts as if it
 *    were `Infinity`, i.e. last.
 *  - "users": `usersCount` descending; rows with no count sort last.
 * Every order ties-breaks by domain A→Z.
 */
export function sortRows(rows, { by, length } = {}) {
  const copy = [...rows];

  copy.sort((a, b) => {
    let primary = 0;
    if (by === "price") {
      // null (unavailable — see pricing.js's priceForLength) sorts as
      // Infinity, i.e. last; computed via explicit branches rather than
      // `?? Infinity` subtraction so two unavailable rows (Infinity -
      // Infinity = NaN) don't produce a broken comparator result.
      const aPrice = priceForLength(a.tiers, length ?? 0);
      const bPrice = priceForLength(b.tiers, length ?? 0);
      const aUnavailable = aPrice === null;
      const bUnavailable = bPrice === null;
      if (aUnavailable && bUnavailable) {
        primary = 0;
      } else if (aUnavailable) {
        primary = 1;
      } else if (bUnavailable) {
        primary = -1;
      } else {
        primary = aPrice - bPrice;
      }
    } else if (by === "users") {
      const aMissing = a.usersCount === undefined || a.usersCount === null;
      const bMissing = b.usersCount === undefined || b.usersCount === null;
      if (aMissing && bMissing) {
        primary = 0;
      } else if (aMissing) {
        primary = 1;
      } else if (bMissing) {
        primary = -1;
      } else {
        primary = b.usersCount - a.usersCount;
      }
    }
    return primary !== 0 ? primary : compareDomain(a, b);
  });

  return copy;
}
