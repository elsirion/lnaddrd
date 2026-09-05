// Pure pricing helpers shared by the domain browser and (later) the
// registration modal. Dependency-free (no DOM) so they can be unit tested
// directly.

/**
 * The price (in msat) an operator would charge for a username of `length`
 * characters, given its announced `tiers` (`[{max_length, price}]`, msat —
 * see docs/protocol/02-service-announcements.md).
 *
 * Mirrors the server-side rule in src/payment.rs (`policy_price`, applied to
 * tiers already validated ascending by `validate_tiers`): sort tiers
 * ascending by `max_length`, then the price is that of the FIRST tier whose
 * `max_length >= length`; if none matches, the price is 0. Announced tiers
 * are an untrusted, arbitrary-order claim from the operator, so — unlike the
 * server's already-validated tiers — this does not assume sorted input.
 */
export function priceForLength(tiers, length) {
  if (!Array.isArray(tiers) || tiers.length === 0) return 0;
  const sorted = [...tiers].sort((a, b) => a.max_length - b.max_length);
  const tier = sorted.find(t => t.max_length >= length);
  return tier ? tier.price : 0;
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
 * The final segment is rendered open-ended ("n+: X sats"/"n+: free") in two
 * cases that both mean "everything from n up has this price": the tiers
 * stop short of the 64-character maximum (so priceForLength falls through to
 * 0 for any longer name), or the last tier's own max_length is 64 (the
 * hard maximum, so there is nothing above it to bound the range).
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
    parts.push(`${last.end + 1}+: free`);
  }

  return parts.join(" · ");
}
