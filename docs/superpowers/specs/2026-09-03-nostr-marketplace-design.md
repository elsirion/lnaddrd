# LN Address marketplace over Nostr — design

Date: 2026-09-03
Status: approved

## Goal

Let Lightning Address operators announce themselves — profile, domains, and
prices — over Nostr, and give users a stateless static marketplace site that
discovers all offers and drives registration and management directly against
each operator's HTTP API from the browser.

## Context

The nostrification work (commit `17510b8`) already ships most of the operator
side:

- `docs/protocol/02-service-announcements.md`: a microstandard using NIP-78
  `kind:30078` addressable events tagged `t=lightning-address-service`, with
  NIP-40 expiry. Content JSON carries `origin`, `domains`, `registration_url`,
  `capabilities`, and per-domain pricing tiers.
- `AnnouncementWorker` publishes the announcement on startup and weekly;
  pricing is derived from admin-configured payment policies.
- `/.well-known/lnaddrd.json` proves domain control; `lnaddrd discover`
  implements server-side discovery and verification.

Gaps this design closes:

1. `about`, `contact`, and `terms_url` are defined by the protocol but never
   published — operators cannot describe themselves.
2. No marketplace frontend exists.
3. No CORS headers — browsers on other origins cannot call operator APIs or
   fetch the well-known document.
4. The paid registration flow (`/register/quote|start|status`) returns htmx
   HTML fragments, unusable as a third-party API.
5. Addresses are managed only by a bearer token; users cannot authenticate
   with a Nostr identity.

## Decisions made

- Marketplace ships as a standalone static bundle in `marketplace/`,
  deployable to any static host; lnaddrd does not serve it.
- A documented JSON registration API v1 with permissive CORS is added.
- Operator profile is configured in the admin UI and stored in the replicated
  (Nostr-recoverable) service configuration record.
- Discovery relays: baked-in defaults, editable in-page, reflected in the URL
  query string. No localStorage, no cookies, no server state.
- Stack: no build step. Plain HTML + vanilla ES modules, vendored
  nostr-tools browser bundle (relay WebSockets + schnorr verification),
  vendored Tailwind runtime JS / Flowbite / qrcode-generator, matching the
  repo's existing vendored-asset convention and visual design language.
- Nostr auth (NIP-98/NIP-07) is added for address ownership, with the
  management token retained as a fallback.

## Workstream A — operator profile

- Extend `ServiceConfigurationRecord` (stays `schema: 1`) with an optional
  `profile` field:

  ```json
  {"profile": {"about": "...", "contact": "npub1...", "terms_url": "https://..."}}
  ```

  All three subfields optional. Serde `default` + `skip_serializing_if` keeps
  old records decodable and old instances tolerant of new records; the
  configuration remains Nostr-recoverable without a schema bump.
- `ConfigurationManager::set_profile(...)`: bumps revision, publishes the
  encrypted configuration backup, same pattern as `set_payment_policy`.
- Validation: `about` at most 500 characters; `contact` must parse as an
  `npub`; `terms_url` must be HTTPS.
- Admin dashboard gains a service-level "Public profile" card (not
  per-domain) with these three fields.
- `announcement::build_event` fills `about`, `contact`, and `terms_url` from
  the profile. The announcement protocol already defines these fields; no
  protocol change.
- Re-announcement on change: `ConfigurationManager` signals a
  `tokio::sync::Notify` that `AnnouncementWorker` selects on alongside its
  weekly tick. This also fixes the existing gap where pricing edits are not
  announced until the next tick.

## Workstream B — registration JSON API v1 + CORS

New endpoints wrapping the existing `RegistrationManager` and
`LnaddrService` logic (which stay unchanged):

- `GET /api/v1/register/quote?domain=&username=` →
  `{"price_msat": <u64>}` (0 = free) or HTTP 4xx with
  `{"error": "taken" | "reserved" | "length_disabled" | "unsupported_domain" | "invalid_input"}`.
- `POST /api/v1/register` — free path only. Body
  `{domain, username, destination, owner_pubkey?}` →
  `{"address": "user@domain", "management_token": "...", "active": bool}`.
  `destination` accepts an LNURL or a Lightning Address. Returns an error if
  the name is priced (client must use the paid path).
- `POST /api/v1/register/start` — paid path. Body
  `{domain, username, destination, owner_pubkey?}` →
  `{"id": "...", "bolt11": "...", "amount_msat": <u64>, "expires_at": <unix>}`.
- `GET /api/v1/register/{id}` — poll →
  `{"state": "pending_payment" | "publishing" | "complete" | "expired",
    "address"?: "...", "management_token"?: "..."}` (token appears exactly
  once, on the first `complete` response, matching existing semantics).
- `GET /api/v1/addresses` — NIP-98 auth required; returns
  `{"addresses": [{"domain": "...", "username": "...", "destination": "..."}]}`
  for the authenticated `owner_pubkey`.
- Existing `/lnaddress/update` (PUT) and `/lnaddress/remove` (DELETE) remain
  the management API and gain CORS plus NIP-98 acceptance (Workstream D).

CORS: permissive (`Access-Control-Allow-Origin: *`, no credentials, allow
`Authorization` and `Content-Type` headers) via `tower-http::cors` applied to
the public API routes, `/domains`, `/.well-known/lnaddrd.json`, and
`/.well-known/lnurlp/*`. Not applied to `/admin` or the htmx UI routes.

Rate limiting: same per-IP `RegistrationManager::allow_request` limits as the
HTML flow (quote 30/min, start 10/min).

Documentation: new `docs/protocol/03-registration-api.md` microstandard. The
API base is `<origin>/api/v1`. Announcements gain the capability string
`registration-api-v1`; doc 02 already requires unknown capabilities to be
ignored, so this is backward compatible.

## Workstream C — marketplace static site

Layout: `marketplace/index.html` plus ES modules under `marketplace/js/` and
vendored assets under `marketplace/assets/` (Tailwind runtime JS + Flowbite
copied from `assets/`, nostr-tools browser bundle, qrcode-generator). Visual
language matches the existing UI: `bg-gray-50` page, white `rounded-lg
shadow-lg` cards, `bg-blue-700` primary buttons, Flowbite components.

Data flow:

1. Parse `?relays=wss://a,wss://b` (fall back to baked-in defaults defined in
   one place, `marketplace/js/config.js`: `wss://relay.damus.io`,
   `wss://nos.lol`, `wss://relay.nostr.band`). Relay set is editable in-page;
   edits update the URL via `history.replaceState`.
2. nostr-tools `SimplePool` queries
   `{"kinds": [30078], "#t": ["lightning-address-service"]}` across the set.
3. Per event: verify the schnorr signature, then apply the same validation
   rules as `src/nostr/discovery.rs` (d-tag prefix `lnaddrd:service:v1:`,
   canonical origin equals identifier and content origin, `schema == 1`, not
   retired, non-empty sorted unique domains, registration URL on the same
   origin, NIP-40 expiry in the future). Keep the newest event per
   `(pubkey, d)` coordinate.
4. Render operator cards as results arrive; per-relay connection status is
   shown.
5. Lazily fetch `https://<domain>/.well-known/lnaddrd.json` per listed domain
   and compare pubkey + coordinate: Verified ✓, Mismatch ✗, or
   Unreachable/no-CORS ? (three distinct states).

Cards show: operator name, about, domains with verification badge, price
summary derived from tiers ("free" / "from N sats"), capabilities, contact
npub link (`nostr:` URI), terms link, and last-announced time.

Registration (modal per domain):

- Username input with debounced live quote against the operator's API.
- Destination input (LNURL or Lightning Address).
- Free: register, then show the address and one-time management token with a
  copy button and a prominent "store this now" warning.
- Paid: show the BOLT11 invoice as QR + copyable text with amount and expiry,
  poll `GET /api/v1/register/{id}` every 3 s until complete or expired.
- Operators whose announcement lacks `registration-api-v1` get a "Register on
  operator's site" link to their `registration_url` instead of the in-page
  flow.

Manage tab:

- With a NIP-07 extension connected: pick an operator, list own addresses via
  NIP-98-signed `GET /api/v1/addresses`, then update destination or delete.
- Without an extension: manual entry of domain + username + management token,
  driving `/lnaddress/update` / `/lnaddress/remove`.

Statelessness: no localStorage, no cookies, no backend. State lives in the
URL query string and in-memory page state only; keys stay in the NIP-07
extension.

## Workstream D — Nostr auth for address ownership

- Registration endpoints accept optional `owner_pubkey` (64-char lowercase
  hex, x-only). If the request carries a valid NIP-98 `Authorization` header,
  the signer's pubkey is used; when both are present they must match.
- Storage: the SQLite payment-address row and the encrypted `AddressRecord`
  backup gain an optional `owner_pubkey`, so ownership survives Nostr
  recovery. New migration; new `UpdatedBy::Owner` variant. Optional serde
  field keeps `AddressRecord` schema-compatible.
- Management: update/remove (legacy `/lnaddress/*` and `/api/v1` forms)
  accept either the management token or NIP-98 auth whose signer equals the
  stored `owner_pubkey`. Tokens remain issued at registration regardless, as
  the fallback path.
- NIP-98 verification: event kind 27235, `created_at` within ±60 s, `u` tag
  equals the full request URL, `method` tag equals the HTTP method, `payload`
  tag equals the SHA-256 of the body when a body is present, valid signature.
  Replay protection via an in-memory seen-event-id cache with a 2-minute TTL.
- Announcement gains the capability string `nostr-auth`.
- Marketplace: "Connect Nostr" button using NIP-07 (`window.nostr`); when
  connected, registrations carry the user's pubkey and the Manage tab signs
  API requests via the extension.

## Error handling

- Relay failures: per-relay status in the UI; partial results render as they
  arrive; a relay timeout does not block others.
- Verification failures distinguish cryptographic/coordinate mismatch from
  network/CORS unreachability.
- API errors surface the structured `error` code (or raw message) in the
  modal; rate-limit responses (429) prompt the user to retry later.
- Server-side NIP-98 failures return 401 with a structured error; malformed
  registration input returns 400 with `invalid_input`.

## Testing

- Rust unit tests (existing in-module style): profile validation and
  round-trip through the configuration record; announcement content includes
  profile and new capabilities; JSON handler behavior for quote/register/
  start/status including error codes; NIP-98 verification (valid, expired,
  wrong URL, wrong method, bad payload hash, replay); CORS headers present on
  public routes and absent on `/admin`.
- JS: announcement validation and pricing-summary logic live in a pure module
  tested with `node --test` (Node used only for tests; no build step).
- Manual e2e: docker-compose operator + `just marketplace-serve` recipe that
  serves `marketplace/` locally.

## Documentation

- New `docs/protocol/03-registration-api.md` (JSON API + NIP-98 auth).
- Update `docs/protocol/02-service-announcements.md`: register the
  `registration-api-v1` and `nostr-auth` capability strings.
- README: marketplace section (what it is, how to host it, relay URL
  parameter) and API overview.

## Out of scope

- NIP-46 remote signers (bunker) in the marketplace — NIP-07 only for now.
- Reputation, reviews, or ranking of operators.
- A dedicated Nostr event kind (stays on NIP-78 `kind:30078` per doc 02).
- Serving the marketplace from lnaddrd itself.
