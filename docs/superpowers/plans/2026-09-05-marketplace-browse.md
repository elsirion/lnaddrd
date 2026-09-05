# Marketplace Domain Browser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Flat searchable/sortable domain list with locally computed pricing, supplier tags, and per-operator registered-user counts from public Nostr data.

**Architecture:** Two new pure modules (`pricing.js`, `browse.js`) carry all logic that can be unit-tested; `app.js` derives flat row state from the existing operator/verification maps; `render.js` renders rows instead of cards; a small counts fetcher queries relays for backup-record counts.

**Tech Stack:** vanilla ES modules, nostr-tools SimplePool (vendored), `node --test`.

**Spec:** docs/superpowers/specs/2026-09-05-marketplace-browse-design.md

## Global Constraints

- Server pricing rule to mirror EXACTLY (src/payment.rs::price_for): sort tiers ascending by `max_length`; the price is `price_msat` of the FIRST tier with `max_length >= length`; if none matches the price is 0. Prices are msat in announcements; display sats (divide by 1000, trim trailing zeros, "free" for 0).
- Verified-only rendering, capability gating (`registration-api-v1` + `nostr-auth` for Register), Nostr-only registration, and the hidden-count / "Discovering operators…" / "No operators found" states from the hardening pass MUST survive the redesign.
- State discipline: no localStorage/sessionStorage/cookies; only `?relays=` in the URL; `textContent` only (sole innerHTML exception stays the QR SVG in modal.js).
- Design language: bg-gray-50 page, white rounded-lg shadow cards, bg-blue-700 primary buttons, Tailwind/Flowbite.
- Backup-record counting: events `kinds:[30078]`, author = operator pubkey, client-side filter d-tag prefix `lnaddrd:backup:v1:`; dedupe by `(pubkey, d)` across relays; per-relay query limit 1000; if any relay returned exactly the limit for that pubkey, mark the count approximate ("N+").
- Tests after each task: `just marketplace-test` green. Commits: conventional, with the session's two trailers.

---

### Task 1: Pure modules — pricing.js and browse.js

**Files:**
- Create: `marketplace/js/pricing.js`, `marketplace/js/browse.js`
- Create: `marketplace/test/pricing.test.mjs`, `marketplace/test/browse.test.mjs`

**Interfaces (produced, consumed by Tasks 2–3):**
- `priceForLength(tiers, length) -> number` (msat; tiers = `[{max_length, price}]` as announced — note announcement JSON uses `price`, msat, per docs/protocol/02; handle missing/empty tiers → 0; do not assume sorted input)
- `formatSats(msat) -> string` ("free" for 0; otherwise sats with up to 3 decimals, trailing zeros trimmed, thousands as plain digits)
- `tierSummary(tiers) -> string` (compact ranges: consecutive tiers become "a–b: X sats" segments joined with " · "; trailing catch-all/no-tier range rendered "n+: free" or "n+: X sats"; empty/absent tiers → "free")
- `buildRows(operators) -> row[]` in browse.js where `operators` is an array of `{origin, name, pubkey, capabilities, verifiedDomains, pricing, usersCount, usersApprox}` and each row is `{domain, origin, operatorName, pubkey, canRegister, tiers, usersCount, usersApprox}` (tiers = the pricing entry for that domain or []).
- `filterRows(rows, {query, name}) -> row[]` (query: case-insensitive substring on domain or operatorName; name non-empty: keep only rows with `priceForLength(tiers, name.length) === 0`)
- `sortRows(rows, {by, length}) -> row[]` (by: "alpha" | "price" | "users"; price uses `length`; users descending, missing counts last; every order ties-breaks by domain A→Z; pure — returns a new array)

Steps: TDD — write the tests first (tier boundaries incl. a 64 catch-all, unsorted tiers, empty tiers, name filter zero-price semantics, both filters combined, all sorts + tie-breaks + missing counts), watch them fail, implement, `just marketplace-test` green, single commit `feat(marketplace): pure pricing and browse modules`.

### Task 2: Flat domain list UI

**Files:**
- Modify: `marketplace/js/app.js`, `marketplace/js/render.js`, `marketplace/index.html`
- (modal.js unchanged except how it is invoked with a single preselected domain, if its current signature needs it)

Requirements:
- Replace the operator-card grid in `#operators` with one list: header controls (text search input, name-check input with helper text "shows domains where this name is free to register; availability is checked when you register", sort `<select>`: Alphabetical / Price / Registered users) + a single-column stack of domain rows styled like the existing cards (white rounded-lg border rows).
- Row content: domain (font-medium), supplier tag (rounded bg-gray-100 chip with operator name, `title` = origin), pricing preview from `tierSummary`, a name-price chip when the name input is non-empty ("<name>@<domain>: free" / ": 21 sats"), users badge ("N users" / "N+ users" / "…" while loading), Register button per the existing capability + connect gating (reuse the Task-4 hardening flow; modal opens with that one domain).
- Derive rows via `buildRows` from the existing operator/verification state (verified domains only); re-render on relay events, verification settles, search/name/sort input (input events re-filter/re-sort in memory — no relay or network work).
- Keep "Discovering operators…", "No operators found", and the hidden-unverified count line working; add "No domains match" when filters empty a non-empty list.
- Contact npub / terms / about no longer have card space: put operator name chip's row in a `<details>`-free simple layout — clicking the supplier chip toggles an inline detail line (about, contact `nostr:` link, terms link) under the row, textContent/href-validated exactly as the old card did.
- `just marketplace-test` stays green. Commit `feat(marketplace): flat searchable domain list`.

### Task 3: Registered-users counts

**Files:**
- Create: `marketplace/js/counts.js`
- Modify: `marketplace/js/app.js` (invoke + merge into row state), `marketplace/js/config.js` (BACKUP_D_PREFIX, COUNT_QUERY_LIMIT = 1000), README.md marketplace section (one paragraph: what the users number means — operator-wide backup records, may include deleted addresses, "N+" when capped).
- Test: extend `marketplace/test/browse.test.mjs` only if pure logic is added to browse.js (e.g. merging counts into rows).

Requirements:
- `fetchBackupCounts(pool, relays, pubkeys, onUpdate)`: per-relay `subscribeMany`/query `{kinds:[30078], authors: pubkeys, limit: COUNT_QUERY_LIMIT}`, client-filter `d` prefix `lnaddrd:backup:v1:`, dedupe by `pubkey + d`, call `onUpdate(pubkey, count, approx)` as results settle; approx = any relay delivered exactly the limit for that pubkey. Fetch once per operator set change (new pubkey discovered), not per keystroke.
- Rows re-render with counts; users sort uses them (missing → last).
- Commit `feat(marketplace): registered-user counts from backup records`.

### Task 4: Final verification

- `just marketplace-test`, `cargo test --all` (should be untouched), fmt/clippy clean.
- Whole-branch review; finish per superpowers:finishing-a-development-branch.
