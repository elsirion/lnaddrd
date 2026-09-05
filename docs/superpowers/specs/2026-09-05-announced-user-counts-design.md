# Announced per-domain user counts — design

Date: 2026-09-05
Status: approved (user-directed)

## Goal

Let operators publish per-domain registered-user counts in their service
announcement so the marketplace can show per-domain numbers without scanning
backup events for every operator.

## Decisions

- **Protocol (doc 02).** Announcement content gains an optional `users`
  array: `"users": [{"domain": "pay.example.com", "count": 123}]`. `count`
  is the operator's number of active addresses on that domain at publish
  time. Self-reported and unverifiable by design — consumers MUST treat it
  as an operator claim, MAY cross-check against public backup records, and
  MUST ignore entries whose domain is not in `domains` or whose count is not
  a non-negative integer. Absent field = operator publishes no counts
  (pre-existing announcements stay valid; no schema bump).
- **Server.** The repository gains an active-address count per domain
  (`SELECT domain, COUNT(*) ... WHERE state = 'active' GROUP BY domain`
  equivalent via diesel). `build_event` fills `users` for every announced
  domain (0 included). Counts refresh whenever the announcement republishes
  (startup, weekly timer, every admin-triggered republish) — staleness
  between publishes is acceptable and avoids leaking registration timing.
- **Marketplace.** `validateAnnouncement` surfaces a sanitized per-domain
  count map (invalid entries dropped, never a validation failure). Rows use
  the announced per-domain count when the announcement carries one
  (badge title: "Self-reported by the operator"); the backup-record scan
  runs ONLY for operators whose announcement lacks the field, keeping the
  existing operator-wide badge and "observed" semantics ("Supplier-wide
  count of backup records..."). Users sort consumes whichever number the
  row has.

## Out of scope

- Verifying self-reported counts against backup records (future cross-check).
- Publishing counts outside the announcement cadence.

## Testing

Rust: repository count query (active-only, per-domain, zero for empty);
build_event includes `users` matching configured domains. JS: validation
sanitization vectors (unknown domain, negative, non-integer, absent field);
row derivation prefers announced counts and falls back to scan counts.
