// Pure pricing helpers shared by the domain browser and (later) the
// registration modal. Dependency-free (no DOM) so they can be unit tested
// directly.

/**
 * The price (in msat) an operator would charge for a username of `length`
 * characters, given its announced `tiers` (`[{max_length, price}]`, msat —
 * see docs/protocol/02-service-announcements.md). Returns `null` when that
 * length is unavailable (registration would be rejected), rather than a
 * price.
 *
 * Mirrors the server-side rule: absent/empty payment policy means free
 * registration at every length (src/registration.rs's `quote_checked` short-
 * circuits to `Ok(Ok(0))` before ever calling `policy_price`), but once a
 * policy has tiers, `policy_price` (src/payment.rs, applied to tiers already
 * validated ascending by `validate_tiers`) returns the price of the FIRST
 * tier (sorted ascending by `max_length`) whose `max_length >= length`, or
 * `None` if no tier matches — which `quote_checked` turns into a
 * `LengthDisabled` rejection, not a free registration. So here: empty/absent
 * `tiers` returns 0 (free), a non-empty `tiers` with no matching tier returns
 * `null` (unavailable), and otherwise the matching tier's price. Announced
 * tiers are an untrusted, arbitrary-order claim from the operator, so —
 * unlike the server's already-validated tiers — this does not assume sorted
 * input.
 */
export function priceForLength(tiers, length) {
  if (!Array.isArray(tiers) || tiers.length === 0) return 0;
  const sorted = [...tiers].sort((a, b) => a.max_length - b.max_length);
  const tier = sorted.find(t => t.max_length >= length);
  return tier ? tier.price : null;
}

/**
 * Formats an msat amount for display: "free" for 0, otherwise the sat value
 * (msat / 1000) with up to 3 decimals, trailing zeros trimmed, and no
 * thousands grouping (plain digits).
 */
export function formatSats(msat) {
  if (!msat) return "free";
  const sats = (msat / 1000).toFixed(3).replace(/\.?0+$/, "");
  return `${sats} sats`;
}

/**
 * A compact, human-readable summary of an operator's pricing tiers for one
 * domain, e.g. "1–2: 1000 sats · 3–4: 100 sats · 5+: free" for the docs
 * example tiers. Empty/absent tiers summarize as "free".
 *
 * Tiers are sorted ascending by `max_length` first (see priceForLength).
 * Consecutive tiers become "a–b: X sats" segments covering the length range
 * they own (from one past the previous tier's max_length up to their own).
 * The final segment is rendered open-ended in two cases: when the last
 * tier's own max_length is 64 (the hard maximum, so its "n+: X sats"/"n+:
 * free" segment already covers everything above n, since there is nothing
 * left to bound the range), and when the tiers stop short of 64 — in which
 * case an extra "n+: unavailable" segment is appended, since priceForLength
 * returns null (not a price) for any length beyond every tier's max_length.
 *
 * Announcements are untrusted relay data: the server rejects duplicate or
 * non-increasing `max_length` values for stored policies (validate_tiers),
 * but nothing stops a relay event from carrying them. A duplicate/
 * non-increasing `max_length` would make a naive "previous max + 1" start
 * exceed that tier's own `max_length`, producing a backwards "b–a" segment.
 * Since such a tier's range is already fully covered by an earlier one —
 * and priceForLength's first-match rule would never select it either — it
 * is skipped entirely rather than rendered.
 */
export function tierSummary(tiers) {
  if (!Array.isArray(tiers) || tiers.length === 0) return "free";
  const sorted = [...tiers].sort((a, b) => a.max_length - b.max_length);

  const segments = [];
  let prevMax = 0;
  for (const tier of sorted) {
    const start = prevMax + 1;
    if (start > tier.max_length) {
      continue;
    }
    segments.push({ start, end: tier.max_length, price: tier.price });
    prevMax = tier.max_length;
  }

  if (segments.length === 0) return "free";

  const parts = segments.map((segment, i) => {
    const isLastRendered = i === segments.length - 1;
    if (isLastRendered && segment.end >= 64) {
      return `${segment.start}+: ${formatSats(segment.price)}`;
    }
    return `${segment.start}–${segment.end}: ${formatSats(segment.price)}`;
  });

  const last = segments[segments.length - 1];
  if (last.end < 64) {
    parts.push(`${last.end + 1}+: unavailable`);
  }

  return parts.join(" · ");
}
