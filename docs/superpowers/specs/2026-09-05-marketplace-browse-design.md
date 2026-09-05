# Marketplace domain browser — design

Date: 2026-09-05
Status: approved (user-directed feature list)

## Goal

Replace the operator-card grid with a flat, searchable, sortable list of all
verified domains. Pricing is computed locally from announced tiers — no
quote API call, no invoice fetch — and each row shows the supplier as a tag
plus a registered-users count derived from public Nostr backup events.

## Decisions

- **Flat domain list.** One list, one row per verified domain across all
  operators (verified-only rendering and capability gating from the
  2026-09-04 hardening pass are unchanged). Each row: domain name, supplier
  tag (operator name, with origin as title/tooltip), pricing preview,
  registered-users badge, Register button (only when the operator advertises
  `registration-api-v1` + `nostr-auth`).
- **Local pricing.** New pure module `marketplace/js/pricing.js`:
  `priceForLength(tiers, length)` mirrors the server rule — tiers sorted by
  `max_length` ascending, price of the first tier with `max_length >= length`,
  no match → 0 — and `tierSummary(tiers)` renders a compact preview like
  "1–2: 1M sats · 3–5: 10k · 6+: free" ("free" when no priced tier). The
  registration modal keeps its server quote (that also checks taken/reserved);
  the list never queries operators for pricing.
- **Name check ("mass-search").** A name input computes each domain's price
  for that name's length locally and, while non-empty, filters the list to
  domains where that name is **free** (price 0). The computed price ("alice:
  free / 21k sats") appears in the row while a name is entered. Free means
  zero price — availability (taken/reserved) still surfaces only in the
  registration modal, which is documented next to the input ("free to
  register; availability is checked when you register").
- **Text search.** A separate search field substring-filters rows on domain
  and operator name (case-insensitive).
- **Registered-users column.** Per *operator* (address backups are encrypted
  events `kind:30078`, d-tag `lnaddrd:backup:v1:<opaque hash>` authored by the
  service pubkey — the domain is not recoverable, and deleted-address
  tombstones are indistinguishable, so the badge is the supplier's total
  backup-record count, labeled as such). Counted client-side: per-relay query
  `{kinds:[30078], authors:[pubkeys], limit}` filtered by the d-prefix,
  deduplicated across relays by d-tag per pubkey. If any relay returns
  `limit` events for a pubkey the count renders as "N+".
- **Sorting.** Dropdown: alphabetical (domain A→Z, default), price (for the
  entered name's length, else length 8), registered users (descending).
  Filtering and sorting are pure functions in `marketplace/js/browse.js`
  with node tests; ties break alphabetically.
- **State discipline unchanged:** relay set stays the only URL state; search,
  name, and sort live in page memory. No localStorage, no cookies.

## Out of scope

- Per-domain user counts (not derivable from public data).
- Querying operator APIs for the list (quotes stay in the modal).
- NIP-45 COUNT (poor relay support).

## Testing

Node tests for `pricing.js` (tier boundaries, catch-all 64 tier, empty/free
policies, unsorted input) and `browse.js` (name filter, text filter,
combined, all three sort orders, tie-breaks, missing counts). Rendering is
verified by reading, per the project's pure-modules-only test convention.
