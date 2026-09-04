# Marketplace hardening: public-host validation, verified-only rendering, Nostr-only flows — design

Date: 2026-09-04
Status: approved

## Goal

Tighten the marketplace and announcement validation after the first day of
real-world use: reject announcements for non-public hosts, render only
verified operators, and make the marketplace app Nostr-identity-first —
registration and management happen exclusively through NIP-98-signed API
calls, with management tokens hidden from the app entirely.

## Context

Live findings from the 2026-09-03 marketplace deployment (mkt.lnaddr.org):

1. A stray `https://localhost` announcement (published by our own e2e dev
   instance against public relays) renders as a normal operator card —
   nothing in the validation rules rejects loopback or otherwise non-public
   hostnames.
2. Operators whose well-known check fails still render, with an
   "Unreachable" badge. Since every announce-capable lnaddrd build also
   serves CORS on the well-known document, an unverifiable listing carries
   no value and mismatches are potential impersonation.
3. The app links non-API operators out to their `registration_url`, and the
   Manage tab has a manual token-entry fallback. Management tokens are being
   phased out; the app should be Nostr-identity-first.

## Decisions

- **Public-host rule (protocol + both validators).** An announcement is
  invalid unless its origin host and every listed domain is a public
  registrable DNS name: must contain at least one dot, must not be an IP
  literal (v4 or v6), and must not be `localhost` or end in `.localhost`,
  `.local`, `.internal`, `.test`, `.invalid`, or `.example`. Enforced
  identically in `src/nostr/discovery.rs` and `marketplace/js/announcement.js`,
  and documented normatively in `docs/protocol/02-service-announcements.md`.
- **Don't announce non-public origins.** The announcement worker skips
  publishing (with a warning log) when the service's own origin or any
  configured domain fails the same rule. Dev instances on `localhost` stop
  polluting public relays; explicit opt-out is not needed.
- **Verified-only rendering.** A domain row renders only after its
  well-known check passes (reachable AND pubkey+coordinate match). An
  operator card renders only once it has at least one verified domain.
  Unverified/mismatched/unreachable operators are counted in a muted
  "N operator(s) hidden (unverified)" note — no toggle, no cards. Cards may
  appear progressively as verifications settle; a lightweight "checking
  relays/operators…" state covers the initial load.
- **Nostr-only registration.** The in-app register flow requires a connected
  NIP-07 identity. Register buttons appear on verified operators that
  advertise `registration-api-v1` + `nostr-auth`; clicking without a
  connected identity prompts connect-first. `POST /api/v1/register` and
  `/register/start` are sent with a NIP-98 `Authorization` header and
  `owner_pubkey` set to the connected pubkey (the server already binds and
  cross-checks these). Operators without the required capabilities get an
  info-only card: no register button and **no external registration link**.
- **Tokens invisible in the app.** Success screens show the registered
  address only. The `management_token` in API responses is ignored and never
  rendered, in both the free and paid flows. (Server-side token issuance is
  unchanged; full token retirement is a separate future project.)
- **Nostr-only Manage tab.** The Manage tab is hidden until an identity is
  connected. When opened, the app fans out NIP-98-signed
  `GET /api/v1/addresses` to every verified operator advertising
  `nostr-auth`, and aggregates the results into one table of the identity's
  addresses (operator, address, destination) with per-row update-destination
  and delete actions, also NIP-98-signed. The operator picker and the manual
  domain/username/token fallback form are removed.
- **Stray event cleanup.** Best-effort: if the repo's dev `root-secret`
  still matches the stray `https://localhost` event's pubkey, publish a
  NIP-40-expired retirement replacement at the same coordinate. Optional and
  operational; the public-host rule already hides the entry from all
  compliant clients.

## Out of scope

- Server-side removal of management tokens (issuance, HTML flow, doc 03
  semantics stay as-is).
- NIP-46 remote signers; NIP-07 remains the only signer.
- Any change to `/api/v1` request/response shapes.

## Testing

- Rust: unit tests for the public-host rule (accept/reject vectors incl.
  IPv4, IPv6, single-label, `localhost`, reserved suffixes, punycode public
  name) in discovery validation; announcement-worker skip behavior.
- JS: mirrored accept/reject vectors in `marketplace/test/announcement.test.mjs`
  so both validators share the exact rule; existing tests updated where the
  fixtures used non-public hosts.
- Manual: marketplace against prod once redeployed; localhost card gone.
