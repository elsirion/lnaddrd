# Nostr-backed lnaddrd specification

Status: implemented protocol and operational specification

This document defines the next small, operationally forgiving version of
`lnaddrd`. It is normative where it uses MUST, SHOULD, and MAY as described by
RFC 2119.

The design has two goals:

1. An operator can lose the local database and reconstruct every active
   Lightning Address from Nostr.
2. Wallets can discover independently operated Lightning Address services and
   let users choose a domain they trust.

It does **not** make a Lightning Address itself federated. Resolution still
depends on its DNS name and HTTPS server. Nostr makes operators replaceable and
state recoverable; it does not keep `name@domain` online while that domain is
offline.

The two protocol components are specified separately:

- [Private backup records](protocol/01-private-backup-records.md)
- [Service announcements](protocol/02-service-announcements.md)

Both initially reuse NIP-78 application data rather than claiming unregistered
event kinds. If multiple independent implementations adopt service discovery,
that component can later be proposed as a NIP with an assigned kind without
changing its JSON data model.

## 1. Scope

### In scope

- LUD-16 Lightning Address proxying to an LNURL-pay endpoint or another
  Lightning Address.
- SQLite as a disposable local cache and transaction coordinator.
- Encrypted, addressable Nostr backups.
- Nostr service discovery.
- Optional, per-domain registration pricing by username length.
- A local password-protected administration UI.
- Server-rendered HTML with HTMX for interaction.

### Out of scope for the first version

- Serving LNURL-pay while the domain or `lnaddrd` process is offline.
- Moving an existing address between unrelated operator keys.
- Consensus between several live `lnaddrd` replicas.
- User accounts, password reset, or email.
- Custody, invoice settlement, or forwarding funds through `lnaddrd`.
- A public API for changing pricing or reserved names.
- Guaranteed archival storage by arbitrary public Nostr relays.

## 2. Definitions

- **Root secret**: the only durable service secret from which Nostr keys and
  lookup keys are derived.
- **Service key**: the derived secp256k1 key that signs backup and announcement
  events.
- **Encryption key**: a separate derived secp256k1 key used as the NIP-44
  recipient for backup contents.
- **Address key**: an opaque deterministic identifier for one canonical
  Lightning Address.
- **Local cache**: SQLite state that can be recreated from Nostr, except for
  transient registration/payment attempts, admin sessions, and sync cursors.
- **Active record**: the newest valid backup event for an address key whose
  plaintext state is `active`.

## 3. Architecture

The process contains five small components:

1. Axum HTTP routes for LUD-16, registration, and administration.
2. A service layer containing validation, pricing, payment, and claim rules.
3. A SQLite repository.
4. A Nostr synchronizer that publishes and restores records.
5. A destination LNURL client with strict outbound-request protections.

SQLite is the read path during normal operation. A request to
`/.well-known/lnurlp/<username>` MUST NOT wait for Nostr. The synchronizer runs
at startup and in the background.

The service is "Nostr-recoverable", not literally stateless. The root-secret
file is the only irreplaceable application state and MUST be backed up. Losing
both SQLite and that file makes encrypted records unrecoverable. The admin
password can be reset without affecting addresses or the service identity.
Pending, unpaid registrations MAY be lost with SQLite; active addresses MUST be
recoverable.

## 4. Configuration and secret files

Configuration remains CLI/environment based. Proposed variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `LNADDRD_DOMAINS` | required | Comma-separated served domains |
| `LNADDRD_BIND` | `127.0.0.1:8080` | Listen address |
| `LNADDRD_DATABASE_PATH` | `/var/lib/lnaddrd/lnaddrd.sqlite3` | SQLite cache |
| `LNADDRD_ROOT_SECRET_FILE` | `/var/lib/lnaddrd/root-secret` | Root secret |
| `LNADDRD_ADMIN_PASSWORD_FILE` | `/var/lib/lnaddrd/admin-password` | Admin password |
| `LNADDRD_NOSTR_RELAYS` | required for backup | Comma-separated `wss:` relay URLs |
| `LNADDRD_PUBLIC_BASE_URL` | inferred only when safe | Canonical HTTPS URL for announcements |
| `LNADDRD_WARNING` | unset | Registration warning text |

The `/var/lib/lnaddrd` defaults are preferable to `/etc`: both files are
generated mutable state, not static configuration. Only the root-secret file is
critical recovery material. Packagers MAY use a platform-appropriate state
directory.

On first start, the missing admin-password file MUST be created atomically with
mode `0600` inside a directory inaccessible to other users. The root-secret is
created only after an authenticated administrator explicitly chooses fresh
initialization, or installed after an entered seed validates as a recoverable
Nostr identity.
Existing files with group/other permissions SHOULD cause a prominent warning or
startup failure. The service MUST never replace a non-empty root-secret file
automatically.

The root secret is 32 random bytes, encoded as lowercase hex. The initial
implementation SHOULD avoid mnemonic phrases: a BIP-39 phrase suggests wallet
interoperability that this derivation scheme does not provide. A future import
command MAY accept a mnemonic and deterministically turn it into the same
32-byte input.

The generated admin password MUST contain at least 128 bits of entropy. The
service logs only the file path and instructions for reading it; the password
itself MUST NOT be logged. Packaging SHOULD give the human administrator a
privileged command that reads the file.

If the admin-password file is absent on any start, the service generates a new
one and invalidates all admin sessions. An operator may therefore reset admin
access by atomically replacing or removing this file and restarting the
service. This does not change the root secret, service public key, encrypted
records, announcements, or user management tokens.

### Key derivation

Use HKDF-SHA256 with the root secret as input keying material, no salt, and
these exact UTF-8 `info` strings:

- `lnaddrd/v1/nostr-signing-key`
- `lnaddrd/v1/nostr-encryption-key`
- `lnaddrd/v1/address-lookup-key`

For secp256k1 keys, append a big-endian 32-bit counter to `info`, starting at
zero, and repeat HKDF-Expand until the result is a valid non-zero scalar. This
makes derivation portable and domain-separated.

The address key is:

```text
hex(HMAC-SHA256(lookup_key, canonical_address_utf8))
```

HMAC is used instead of `SHA256(secret_salt || address)` to avoid inventing a
keyed-hash construction.

## 5. Canonical addresses and destinations

Domains MUST be converted to lowercase ASCII using IDNA, without a trailing
dot or port. Usernames MUST follow LUD-16: lowercase `a-z`, digits, `-`, `_`,
and `.`. The first release MUST reject `+` tags for registration. It MAY accept
`_` if the operator has not reserved it.

The canonical address is exactly `username@ascii-domain`.

Usernames MUST be between 1 and 64 ASCII characters. A domain MUST be one of
the configured domains. Reserved-name matching is performed after
canonicalization and is case-insensitive by construction.

A destination is stored as a typed value:

```json
{"type":"ln_address","value":"alice@example.com"}
```

or

```json
{"type":"lnurl","value":"LNURL1..."}
```

Raw callback URLs are not accepted from public registration. LNURLs MUST decode
to HTTPS, except for explicitly enabled `.onion` HTTP destinations.

Before creating a registration, `lnaddrd` fetches the destination's LUD-06
payRequest and verifies its type and bounds. This check proves only that the
destination is currently well-formed; it cannot guarantee future availability.
Redirects, DNS resolution, and every subsequent callback request MUST reject
loopback, link-local, private, multicast, and otherwise non-public targets to
prevent SSRF. Responses MUST have tight byte and time limits.

## 6. Registration states and claim semantics

Registrations use these local states:

```text
pending_payment -> publishing -> active
       |               |
       +-> expired     +-> publish_failed
active -> deleted
```

Free registrations start at `publishing`. Paid registrations start at
`pending_payment`. Only `active` records resolve through LUD-16.

SQLite MUST enforce a unique `(domain, username)` constraint. Registration and
deletion MUST use a transaction or compare-and-swap update. This prevents two
local requests from receiving the same name. Running multiple writable
instances against separate SQLite databases is unsupported.

Nostr is not used as a global lock. A service key is authoritative only for its
own configured domains, so cross-operator name collisions are irrelevant.

The successful claim order is:

1. Validate and reserve the name locally.
2. If required, complete and verify payment.
3. Publish the encrypted active record to the configured relays.
4. Require acknowledgement from at least one relay.
5. Commit the local state as `active` and return the management token.

If step 3 or 4 fails, the reservation remains retryable as `publish_failed` and
MUST NOT resolve. A background worker retries with bounded exponential backoff.
This ordering avoids acknowledging an address that has no remote backup.

Deletion publishes a tombstone record before removing sensitive local data.
The tombstone is an `active` record replacement whose plaintext state is
`deleted`; NIP-09 deletion events alone are insufficient because relays are not
required to honor them and an older active record could otherwise resurrect.

## 7. Optional registration payment gate

Payment configuration is per domain and disabled by default. It contains:

- A payment recipient expressed as LNURL or Lightning Address.
- Zero or more username-length tiers.
- A payment attempt TTL, default 15 minutes.

Tiers are inclusive maximum lengths and prices in millisatoshis. Example:

```json
[
  {"max_length": 2, "price_msat": 1000000},
  {"max_length": 4, "price_msat": 100000},
  {"max_length": 64, "price_msat": 0}
]
```

The first tier whose `max_length` is at least the canonical username length
sets the price. Tiers MUST have strictly increasing lengths and non-increasing
prices. A missing matching tier means registration is disabled for that length.

### Setting the recipient

When an admin saves a non-free payment configuration, the server MUST:

1. Resolve the recipient as LUD-06/LUD-16.
2. Ensure each configured non-zero amount is within `minSendable` and
   `maxSendable`.
3. Request a test invoice for the cheapest non-zero tier.
4. Require a valid HTTPS `verify` URL in the callback response as defined by
   LUD-21.
5. Fetch it once and require a valid unsettled response referring to the same
   BOLT11 invoice.

The test invoice is not paid. Admin UI copy MUST say that this probes current
LUD-21 support, which the recipient can later remove.

### Paid registration

The server requests an exact-amount invoice from the configured recipient and
stores the BOLT11, verify URL, amount, expiry, and destination fingerprint in a
pending attempt. The browser receives the invoice and polls an HTMX endpoint.
The server checks LUD-21 itself; it MUST NOT trust a browser-supplied preimage or
receipt.

Before activation, the verify response MUST say `settled: true`, return the same
invoice, and the decoded invoice MUST match the expected amount and payment
hash. The server then stores a receipt object in the encrypted record. The
preimage SHOULD NOT be stored because proof of payment is already available
from the configured verifier and minimizing backup data is preferable.

Payment buys one claim attempt for the exact canonical address and pricing
configuration fingerprint. It cannot be replayed for another address. If
publishing temporarily fails after settlement, the same pending record is
retried without another payment. Refunds are out of scope and this MUST be
disclosed before invoice creation.

Changing prices affects only new registrations. Changing the payment recipient
invalidates unpaid attempts; paid attempts remain retryable.

Payment policies and reserved names are durable service configuration. Every
admin change to either MUST publish the encrypted configuration record defined
by the backup microstandard before the UI reports success. This makes SQLite
disposable without making an operator reconstruct policy from memory.

## 8. Administration and user management

`/admin` uses a session cookie established by the password from the configured
file. Password comparison MUST be constant-time. Sessions MUST be random,
server-side, short-lived (12 hours by default), `Secure`, `HttpOnly`, and
`SameSite=Strict`. State-changing requests MUST require both the session and a
CSRF token. Login attempts MUST be rate-limited.

The first release has one administrator and no roles. The admin can:

- View sync/relay health and the last successful backup.
- Configure payment recipient and length tiers per domain.
- Add and remove exact reserved usernames.
- View registrations and their state.
- Retry publishing or delete an address.
- Trigger a dry-run restore comparison.

Secrets, management tokens, invoices, and encrypted backup contents MUST NOT
appear in normal logs or admin list pages.

Public users receive a high-entropy management token on successful
registration. The token is stored only as an Argon2id hash locally and inside
the encrypted backup. It authorizes destination changes and deletion. The token
is shown once. Admin deletion does not require it but MUST be recorded in the
encrypted record's update metadata.

## 9. HTMX interface

Maud remains the HTML renderer. HTMX is served locally as a pinned asset; the
application MUST NOT require a third-party CDN for administration or
registration.

HTMX endpoints return HTML fragments and use normal HTTP status codes:

- `POST /register/quote` validates a name and returns free/price/reserved state.
- `POST /register/start` reserves the name and returns an invoice or completion
  fragment.
- `GET /register/:attempt/status` polls settlement/publication state.
- `POST /admin/login` creates a session.
- `GET /admin/payment/:domain` and `PUT /admin/payment/:domain` manage pricing.
- `GET /admin/reserved` and mutation routes manage reserved names.
- `POST /admin/addresses/:domain/:username/retry` retries publication.
- `DELETE /admin/addresses/:domain/:username` publishes a tombstone.

The JSON API MAY remain, but public registration must use the same service-layer
validation and rate limits as the HTML flow.

## 10. SQLite schema

The exact Diesel schema may evolve, but these logical tables are required:

- `addresses`: canonical identity, destination, state, token hash, record
  revision, Nostr event id, timestamps.
- `registration_attempts`: quoted price, invoice, payment hash, verify URL,
  expiry, state, configuration fingerprint.
- `domain_payment_policies`: destination and tier JSON.
- `reserved_names`: domain plus canonical username.
- `nostr_outbox`: signed event, relay acknowledgements, attempt schedule.
- `nostr_sync_state`: per-relay cursor/last successful sync.
- `admin_sessions`: hashed session id, CSRF secret, expiry.

Invoices and verify URLs are sensitive operational data. SQLite SHOULD use
filesystem protection and MAY later support application-level encryption, but
the first release relies on the service user's directory permissions.

## 11. Startup, backup, and restore

Startup MUST NOT silently serve a partially restored empty database.

1. Load/create the resettable admin password and open SQLite migrations without
   requiring a root secret.
2. Open SQLite and run migrations.
3. If the database is marked initialized, start serving its active records and
   sync Nostr in the background.
4. If new/uninitialized, expose only liveness and an authenticated setup UI.
   The administrator chooses either fresh seed generation or recovery using an
   entered seed; no LNURL, registration, or normal admin route is available.
5. For recovery, derive the candidate author, fetch all backup events by that
   author and the backup `d`-tag prefix from every configured relay, and validate
   the complete restore before installing the seed file.
6. Merge, authenticate, decrypt, and validate the records.
7. Require all reachable relays to reach EOSE, with at least one successful
   relay, before marking the database initialized.
8. Require and restore the encrypted service configuration record.
9. Rebuild active and deleted records transactionally, then switch the running
   process to the normal HTTP router.

An authenticated administrator of an initialized instance MAY export the root
seed from the UI only after re-entering the current admin password. The response
MUST be an attachment with `Cache-Control: no-store`; pages and logs MUST never
embed the seed.

An empty result is ambiguous. On an uninitialized database, absence of an
encrypted service configuration record requires the explicit fresh-service
choice in the setup UI (or the CLI `initialize-empty` command). That action
creates and publishes revision 1 of the configuration before normal routes are
enabled. This prevents a relay outage from being mistaken for a fresh
installation while supporting a legitimate service with zero addresses.

The merge winner for one address key is the valid event with the greatest
plaintext `revision`; ties use greatest `created_at`, then lexicographically
smallest event id. Wall-clock time alone is not authoritative. Revisions start
at 1 and increment for every update/tombstone.

The synchronizer periodically republishes current records to relays missing
them. The operator SHOULD configure 2-4 independent relays. Relay
acknowledgement is evidence of receipt, not durable retention; deployments that
need stronger guarantees should use at least one paid or self-hosted archival
relay.

## 12. Availability and health

Nostr outages MUST NOT interrupt resolution of locally cached active addresses.
They do block new activations and mutations once no relay can acknowledge the
new backup.

Expose:

- `/health/live`: process is running.
- `/health/ready`: SQLite is initialized and HTTP can resolve cached records.
- Admin-only detailed relay/sync health.

Public health responses MUST not disclose relay credentials, event ids, or
address counts unless explicitly configured.

## 13. Abuse controls

Pricing is optional and is not the only abuse control. The service SHOULD
provide coarse per-IP rate limits for quote, start, and login routes; bounded
pending attempts per IP/name; request-body limits; and expiration cleanup.

Reserved names always win over tiers. Suggested initial defaults include
`admin`, `administrator`, `api`, `help`, `info`, `lnurl`, `root`, `security`,
`support`, `www`, and `_`, but the packaged default list MUST be visible and
editable rather than surprising the operator.

## 14. Migration from the current server

The repository currently uses PostgreSQL and plaintext management tokens.
Migration is explicit, not automatic:

1. Stop writes to the PostgreSQL instance.
2. Start the new binary with the root secret and an empty SQLite database in
   import mode.
3. Read and validate every PostgreSQL row, canonicalize it, hash its management
   token locally, and create revision-1 encrypted records.
4. Require relay acknowledgement for every record.
5. Write a signed import report and only then enable the new HTTP listener.

Conflicting or invalid legacy rows fail the import and are listed without
secrets. The PostgreSQL backend can be removed after one release containing the
import command.

## 15. Implementation sequence

1. Canonical value types, SQLite repository, and PostgreSQL import command.
2. Secret-file handling, derivation test vectors, encrypted event codec.
3. Transactional Nostr outbox and restore command with fixture relays.
4. Existing free registration and resolution on the new repository.
5. Admin authentication, reserved names, and HTMX UI.
6. LUD-21 payment policy and registration state machine.
7. Service announcements and a wallet-facing discovery proof of concept.

Each stage must preserve a working free-registration server. Payment and wallet
discovery remain optional features.

## 16. Acceptance criteria

- Deleting only the SQLite file and restarting with the same root secret and
  reachable configured relay reconstructs every active address, tombstone,
  payment policy, and reserved name.
- Starting with the wrong root secret restores nothing and fails closed.
- A Nostr relay cannot learn the address, destination, management token, price,
  or receipt from event tags/content.
- A service can resolve cached addresses while every relay is offline.
- A new or changed address is not acknowledged until one relay accepts its
  backup event.
- Two concurrent local claims for one name produce at most one invoice/claim.
- A paid claim activates only after a matching LUD-21 settled response.
- Reserved names cannot be quoted or claimed, including through case/IDNA
  variants.
- Admin mutations require authenticated sessions and CSRF protection.
- Announcements can be found with a standard NIP-01 filter and verified against
  the serving domain.

## 17. Standards references

- NIP-01 event and addressable-event rules:
  https://github.com/nostr-protocol/nips/blob/master/01.md
- NIP-44 encrypted payloads:
  https://github.com/nostr-protocol/nips/blob/master/44.md
- NIP-65 relay-list conventions:
  https://github.com/nostr-protocol/nips/blob/master/65.md
- NIP-78 application-specific data:
  https://github.com/nostr-protocol/nips/blob/master/78.md
- LUD-06 LNURL-pay:
  https://github.com/lnurl/luds/blob/luds/06.md
- LUD-16 Lightning Address identifiers:
  https://github.com/lnurl/luds/blob/luds/16.md
- LUD-21 payment verification:
  https://github.com/lnurl/luds/blob/luds/21.md
