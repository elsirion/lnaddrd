// Pure logic for deciding whether a discovered operator's card should be
// shown, given the well-known verification status of each of its domains.
// Kept dependency-free (no DOM) so it can be unit tested directly; app.js
// wires it to the live domainStatus map.

/**
 * Classifies one operator's render state.
 *
 * `domains` is the operator's announced domain list; `statusFor(domain)`
 * returns one of "checking" | "verified" | "mismatch" | "unreachable" for
 * that domain's well-known check (see app.js's `verifyDomain`).
 *
 * Returns `{ verified, category }`:
 *   - `verified`: the subset of `domains` currently verified, in their
 *     original order — this is exactly what the card (and, transitively,
 *     the registration modal's domain choices) should list.
 *   - `category`:
 *     - "visible": at least one domain verified — render a card.
 *     - "hidden": no domain verified, and every check has settled (none
 *       still "checking") — count towards the hidden-operator total.
 *     - "pending": no domain verified yet, but at least one check is still
 *       in flight — neither render nor count as hidden yet.
 */
export function classifyOperator(domains, statusFor) {
  const verified = domains.filter(domain => statusFor(domain) === "verified");
  if (verified.length > 0) {
    return { verified, category: "visible" };
  }
  const pending = domains.some(domain => statusFor(domain) === "checking");
  return { verified, category: pending ? "pending" : "hidden" };
}

/**
 * Computes the domain -> status entries an operator's coordinate should
 * carry after its stored event changes (republish, edit, etc.).
 *
 * `domains` is the *new* announcement's domain list; `statusFor(domain)`
 * looks up that domain's status from *before* this change (typically
 * scoped to the operator's pubkey by the caller).
 *
 * A domain already known keeps its last status untouched — stale-but-
 * correct — until a fresh well-known check resolves it one way or the
 * other; only a domain that's genuinely new to this announcement starts at
 * "checking". A domain dropped from the announcement simply isn't present
 * in the result, so the caller can discard its old status entirely.
 *
 * This is what keeps an already-verified card on screen across a routine
 * republish that only bumps the event id (e.g. a weekly re-announce, or an
 * unrelated metadata edit) instead of flickering away and back on every
 * such update — a card only ever disappears if a fresh check actually
 * fails.
 */
export function reconcileDomainStatuses(domains, statusFor) {
  const next = new Map();
  for (const domain of domains) {
    next.set(domain, statusFor(domain) ?? "checking");
  }
  return next;
}
