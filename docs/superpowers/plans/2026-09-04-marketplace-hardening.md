# Marketplace Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reject announcements for non-public hosts, render only verified operators, and make the marketplace app Nostr-identity-only (NIP-98-signed registration and management, no tokens shown, no external operator links).

**Architecture:** One shared "public host" rule implemented twice (Rust `src/nostr/announcement.rs`, JS `marketplace/js/announcement.js`) with identical test vectors; the marketplace app gates cards on well-known verification and gates register/manage flows on a connected NIP-07 identity.

**Tech Stack:** Rust (axum, nostr-sdk, anyhow), vanilla ES modules, `node --test`.

**Spec:** docs/superpowers/specs/2026-09-04-marketplace-hardening-design.md

## Global Constraints

- The public-host rule is EXACTLY: split host on `.`; at least 2 labels; every label 1–63 chars, lowercase `[a-z0-9-]`, not starting or ending with `-`; the last label (TLD) must not be all digits and must not be one of `localhost`, `local`, `internal`, `test`, `invalid`, `example`. Both languages implement exactly this — no extra checks in one but not the other.
- Marketplace state discipline is unchanged: no localStorage/sessionStorage/cookies; keys stay in the NIP-07 extension; `textContent` only (the sole `innerHTML` exception remains the self-generated QR SVG).
- Design language unchanged: `bg-gray-50` page, white `rounded-lg shadow` cards, `bg-blue-700` primary buttons.
- NIP-98 signing: the signed `u` tag must equal the exact fetch URL and the `payload` tag must hash the exact body string sent — build the JSON body string once and use the identical string for both signing and `fetch`.
- No changes to `/api/v1` request/response shapes or server token issuance.
- Tests after every task: `cargo test --all` for Rust tasks, `just marketplace-test` for JS tasks. `cargo fmt --all` + `cargo clippy --all --all-targets -- -D warnings` must stay clean on Rust tasks.
- Commit messages: conventional commits with the session's two trailers.

## Shared test vectors (both languages, verbatim)

Accept: `lnaddr.org`, `pay.lnaddr.org`, `foo-bar.io`, `a.b.co`, `xn--ls8h.net`, `svc.example2.com`
Reject: `localhost`, `foo.localhost`, `mybox.local`, `svc.internal`, `demo.test`, `x.invalid`, `site.example`, `1.2.3.4`, `192.168.0.10`, `[::1]`, `::1`, `lnaddr.org.` (trailing dot), `.lnaddr.org`, `foo..bar`, `UPPER.org`, `single`, `-bad.org`, `bad-.org`, `foo.123`

---

### Task 1: Rust public-host rule, discovery validation, announce skip, doc 02

**Files:**
- Modify: `src/nostr/announcement.rs` (add `is_public_host`, gate `build_event`)
- Modify: `src/nostr/discovery.rs` (`validate_event` enforces the rule)
- Modify: `docs/protocol/02-service-announcements.md` (normative rule)

**Interfaces:**
- Produces: `pub fn is_public_host(host: &str) -> bool` in `src/nostr/announcement.rs`.

- [ ] **Step 1: Failing tests.** In `announcement.rs` tests: `is_public_host` over ALL shared vectors above. In `discovery.rs` tests: `validate_event` rejects an otherwise-valid announcement whose origin is `https://localhost` (build via `build_event` is impossible after this task — construct the event by hand or by relaxing a fixture) and one whose `domains` include `mybox.local`; error text `Host is not public`. In `announcement.rs` tests: `build_event` returns `Ok(None)` when `public_base_url = "https://localhost"`, and when the configuration contains a domain `dev.local` alongside a public origin.
- [ ] **Step 2: Run, expect FAIL** (`cargo test -p lnaddrd is_public_host`, etc.).
- [ ] **Step 3: Implement.**

```rust
/// Whether `host` is a public registrable DNS name (see docs/protocol/02).
pub fn is_public_host(host: &str) -> bool {
    const RESERVED_TLDS: [&str; 6] =
        ["localhost", "local", "internal", "test", "invalid", "example"];
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    for label in &labels {
        let bytes = label.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 63
            || !bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
            || bytes[0] == b'-'
            || bytes[bytes.len() - 1] == b'-'
        {
            return false;
        }
    }
    let tld = labels.last().expect("at least two labels");
    if tld.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    !RESERVED_TLDS.contains(tld)
}
```

  In `validate_event` (after the "Non-canonical origin identifier" check): parse `origin` with `url::Url::parse`, take `host_str()`, `ensure!(is_public_host(host), "Host is not public")`. After the domains checks: `ensure!` every domain in `announcement.domains` passes `is_public_host` with the same message.
  In `build_event` (after `normalized_origin`): if the origin's host or any key of `service_configuration.domains` fails `is_public_host`, `tracing::warn!` ("Origin or domain is not public, skipping service announcement") and `return Ok(None);`. Note `AnnouncementWorker::publish` and admin republish both go through `build_event`, so this one gate covers all publishing paths.
- [ ] **Step 4: Doc.** In `docs/protocol/02-service-announcements.md`, add to the validation rules section: origin host and every entry of `domains` MUST be a public registrable DNS name — at least two dot-separated labels, each 1–63 characters of lowercase `a-z0-9-` not starting or ending with `-`, whose final label is neither all-digits nor one of `localhost`, `local`, `internal`, `test`, `invalid`, `example`. Consumers MUST reject non-conforming announcements; producers MUST NOT publish them.
- [ ] **Step 5: `cargo test --all`, fmt, clippy — green. Commit** `feat(nostr): reject announcements for non-public hosts`.

### Task 2: JS mirror of the public-host rule

**Files:**
- Modify: `marketplace/js/announcement.js` (export `isPublicHost`, enforce in `validateAnnouncement`)
- Modify: `marketplace/test/announcement.test.mjs`

**Interfaces:**
- Consumes: rule definition from Global Constraints (NOT the Rust code — reimplement from the rule text; vectors keep them honest).
- Produces: `export function isPublicHost(host)`.

- [ ] **Step 1: Failing tests.** Add a test iterating the shared accept/reject vectors verbatim. Add validation tests: an otherwise-valid announcement with origin `https://localhost` → `{ ok: false, error: "Host is not public" }`; same for a listed domain `mybox.local`. Update any existing fixtures that use non-public hosts so previously-green tests stay green.
- [ ] **Step 2: `just marketplace-test`, expect FAIL.**
- [ ] **Step 3: Implement.**

```js
const RESERVED_TLDS = new Set(["localhost", "local", "internal", "test", "invalid", "example"]);

// Mirrors is_public_host in src/nostr/announcement.rs — keep the two in sync.
export function isPublicHost(host) {
  if (typeof host !== "string") return false;
  const labels = host.split(".");
  if (labels.length < 2) return false;
  for (const label of labels) {
    if (label.length > 63 || !/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(label)) return false;
  }
  const tld = labels[labels.length - 1];
  if (/^[0-9]+$/.test(tld)) return false;
  return !RESERVED_TLDS.has(tld);
}
```

  In `validateAnnouncement`: after the canonical-origin check, `new URL(origin).hostname` must pass `isPublicHost` (error `"Host is not public"`); each entry of `domains` must pass too (same error).
- [ ] **Step 4: `just marketplace-test` green. Commit** `feat(marketplace): mirror public-host rule in announcement validation`.

### Task 3: Verified-only rendering

**Files:**
- Modify: `marketplace/js/app.js`, `marketplace/js/render.js`, `marketplace/index.html` (if a hidden-count / loading element is needed)

**Interfaces:**
- Consumes: existing per-domain well-known verification in `app.js` (`verifyDomain`), existing card renderer in `render.js`.
- Produces: cards render only verified domains; operators with zero verified domains never render.

Behavioral requirements (read the current files first; keep their structure):

- [ ] **Step 1:** An operator card is inserted/updated only once ≥1 of its domains has a **passing** well-known check. The card lists only verified domains (keep the ✓ badge). Unverified domains never appear, in the card or in the registration modal's domain choices.
- [ ] **Step 2:** Track operators whose announcement validated but whose domain checks have all settled with zero passes; show a single muted line under the list: `N operator(s) hidden (unverified)` (hidden when N=0). While relays/verifications are still in flight and no card is shown yet, show a muted "Discovering operators…" placeholder; remove it when the first card appears or all work settles (show "No operators found" if the end state is empty).
- [ ] **Step 3:** Re-verification on changed event id (existing behavior) must be able to both add and REMOVE a card — an operator whose new announcement fails verification disappears and joins the hidden count.
- [ ] **Step 4:** `just marketplace-test` still green (pure modules untouched or updated). Manual check via `just marketplace-serve` against real relays: localhost entry absent even before Task 2's filter (its well-known can't verify), hidden-count appears.
- [ ] **Step 5: Commit** `feat(marketplace): render verified operators only`.

### Task 4: Nostr-only registration

**Files:**
- Modify: `marketplace/js/render.js` (button gating, drop external link), `marketplace/js/modal.js` (NIP-98 signing, token removal), `marketplace/js/api.js` (auth headers on register calls), `marketplace/js/app.js` (connect-before-register), `marketplace/js/nostr-auth.js` (only if a helper is missing)

**Interfaces:**
- Consumes: `connect()`, `nip98Header(url, method, body)` from `nostr-auth.js`; connected-pubkey state in `app.js`.
- Produces: `registerFree(origin, bodyString, authHeader)` / `registerStart(origin, bodyString, authHeader)` where `bodyString` is the pre-serialized JSON string.

- [ ] **Step 1: Card gating (render.js).** The Register button appears only when the operator's capabilities include BOTH `registration-api-v1` and `nostr-auth`. Operators without them render info-only cards: profile, verified domains, pricing, contact, terms — but no register button and NO anchor to `registration_url` anywhere.
- [ ] **Step 2: Connect-before-register.** Clicking Register with no connected identity runs the same `connect()` path as the header button (updating the shared connected state and header UI), then opens the modal; if `connect()` throws (no extension / user rejection), surface the error near the button ("Connect a Nostr extension to register") and do not open the modal.
- [ ] **Step 3: Signed registration (modal.js + api.js).** Change `registerFree`/`registerStart` to take a pre-serialized body string and an `authorization` header value. In the modal: build `body = JSON.stringify({domain, username, destination, owner_pubkey})` with `owner_pubkey` = connected pubkey; `authHeader = await nip98Header(url, "POST", body)` where `url` is the exact full request URL; send that identical string. The quote GET stays unsigned.
- [ ] **Step 4: Token removal.** Free-flow success shows the address and an "active" confirmation only. Paid-flow completion shows the address only. Delete the token display, copy-token button, and "store this now" warning; ignore `management_token` fields in responses. No token-related copy remains anywhere in the registration UI.
- [ ] **Step 5:** `just marketplace-test` green; manual smoke of free registration against a dev server (`just run <relay>` + `just marketplace-serve`, NIP-07 extension) if available, else note untested in the report. **Commit** `feat(marketplace): require nostr identity for registration`.

### Task 5: Nostr-only Manage tab + README

**Files:**
- Modify: `marketplace/js/manage.js` (rewrite), `marketplace/js/app.js` + `marketplace/index.html` (tab visibility), `marketplace/js/api.js` (drop token parameters), `README.md` (marketplace section)

**Interfaces:**
- Consumes: verified-operator set from Task 3's state (operators with ≥1 verified domain AND `nostr-auth` capability), `listAddresses`, `updateAddress`, `removeAddress`, `nip98Header`.
- Produces: Manage tab = aggregated address table for the connected identity.

- [ ] **Step 1: Tab visibility.** The Manage tab button is hidden until an identity is connected (toggle a `hidden` class from the shared connect state). If somehow active while disconnected, the panel shows only a "Connect Nostr to manage your addresses" prompt.
- [ ] **Step 2: Aggregation.** On tab activation (and on later connect), fan out NIP-98-signed `GET /api/v1/addresses` to every verified `nostr-auth` operator concurrently. Render one table: address (`user@domain`), destination, operator origin. Per-operator failures are non-fatal: collect them into a muted note ("Could not reach N operator(s)") and render the rest. Empty result: "No addresses found for this identity."
- [ ] **Step 3: Row actions.** Update: inline destination input per row, NIP-98-signed `PUT /lnaddress/update`. Delete: confirm, then NIP-98-signed `DELETE /lnaddress/remove`. Refresh that operator's rows afterward. Remove the operator picker, the manual domain/username/token form, and every token parameter/branch in `manage.js`; in `api.js` drop the token path so `updateAddress`/`removeAddress` require `authHeader` (keep the exact-body-string signing discipline for both).
- [ ] **Step 4: README.** Update the Marketplace section: remove the token-fallback sentence; describe the Nostr-only model (NIP-07 identity required to register and manage; tokens no longer surfaced by the app; operator HTML flow still issues them for wallet-less users).
- [ ] **Step 5:** `just marketplace-test` green. **Commit** `feat(marketplace): nostr-only manage tab`.

### Task 6: Final verification

- [ ] `cargo test --all`, `just marketplace-test`, fmt, clippy — all green.
- [ ] `grep -ri "token" marketplace/js marketplace/index.html` — no user-facing token UI remains (comments referencing the phase-out are fine).
- [ ] Whole-branch review, then finish per superpowers:finishing-a-development-branch.
