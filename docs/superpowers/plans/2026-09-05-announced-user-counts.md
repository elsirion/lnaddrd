# Announced User Counts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Operators publish per-domain active-address counts in announcements; the marketplace prefers them and only scans backup records as fallback.

**Spec:** docs/superpowers/specs/2026-09-05-announced-user-counts-design.md

## Global Constraints

- Announcement field, verbatim shape: `"users": [{"domain": "<announced domain>", "count": <non-negative integer>}]`, optional, omitted never invalidates an announcement; consumers drop invalid entries silently (unknown domain, negative, fractional, non-numeric) without failing validation.
- Count = ACTIVE addresses only (state 'active'), zero entries included for announced domains with no addresses.
- Marketplace: announced count wins per row (badge title "Self-reported by the operator"); backup-record scan runs only for operators whose valid announcement lacks a usable `users` field; scan badges keep the existing supplier-wide title. No behavior change for search/sort contracts (`sortRows` by "users" unchanged).
- Suites green after each task: Rust tasks `cargo test --all` + fmt + clippy -D warnings; JS tasks `just marketplace-test`.
- Conventional commits with the session's two trailers.

---

### Task 1: Server publishes per-domain counts

**Files:**
- Modify: `src/repository/sqlite.rs` (count query), `src/repository/mod.rs` or the trait location if counts go through a trait (inspect first — follow how `service_configuration` reaches the announcement worker), `src/nostr/announcement.rs` (`ServiceAnnouncement.users`, `build_event`), callers of `build_event` (worker `publish`, `src/admin.rs::publish_announcement`, discovery tests), `docs/protocol/02-service-announcements.md`.

**Interfaces:**
- Produces: repository method `active_address_counts(&self, domains: &[Domain]) -> Result<BTreeMap<String, u64>>` (name/style per existing repository methods — inspect and match); `ServiceAnnouncement { ..., users: Option<Vec<DomainUsers>> }` with `DomainUsers { domain: String, count: u64 }`, serde default + skip_serializing_if None.

- [ ] TDD: failing tests first — repository test (register active + non-active addresses across two domains, assert per-domain counts and zero for an empty announced domain); announcement test (build_event output JSON contains `users` entries matching configured domains and passed counts).
- [ ] Implement: thread counts into `build_event` (extra parameter following its existing style); worker `publish` and admin `publish_announcement` fetch counts from the repository before building; tests updated where build_event is called with the new parameter.
- [ ] Doc 02: document the field with the consumer rules from Global Constraints (operator claim; MAY cross-check; MUST drop invalid entries; MUST NOT reject absent field).
- [ ] `cargo test --all`, fmt, clippy green. Commit `feat(nostr): announce per-domain user counts`.

### Task 2: Marketplace consumes announced counts

**Files:**
- Modify: `marketplace/js/announcement.js` (sanitize `users` into the validated result, e.g. `userCounts: {domain: count}`), `marketplace/js/browse.js` (row users fields accept per-domain announced counts), `marketplace/js/app.js` (rows prefer announced counts; skip scan scheduling for operators with usable counts), `marketplace/js/render.js` (badge title per source), tests `announcement.test.mjs` + `browse.test.mjs`.

- [ ] TDD: validation vectors (valid entries kept; unknown domain / negative / fractional / string count dropped; absent field → empty map; field never causes `{ok:false}`); buildRows/row-derivation tests for announced-count preference and scan fallback.
- [ ] app.js: an operator entry whose validated announcement has a usable count for at least one domain does not enter the scan set (countedPubkeys logic — inspect Task-3-era wiring in app.js and counts.js); per-row: announced count for that row's domain if present (usersApprox false, source "announced"), else operator-wide scan count with existing semantics.
- [ ] render.js: badge title "Self-reported by the operator" for announced, existing supplier-wide title for scanned.
- [ ] README marketplace paragraph updated: two sources, self-reported preferred, scan fallback.
- [ ] `just marketplace-test` green. Commit `feat(marketplace): prefer announced user counts`.

### Task 3: Final verification

- Full suites, fmt/clippy, whole-branch review, finish per superpowers:finishing-a-development-branch.
