# lnaddrd Nostr architecture implementation plan

Status: implemented; retained as the design and verification checklist

This plan turns [the design specification](spec.md) into small, reviewable
increments. Every milestone leaves the existing Lightning Address resolution
path usable. Features are enabled only after their failure behavior is tested.

## Guiding constraints

- Keep one process and one SQLite file; do not introduce a worker service or
  message broker.
- Keep Axum, Maud, Clap, Tokio, and the existing service/repository boundaries
  where useful.
- Use `nostr-sdk` for NIP-01 signing, relay connections, and NIP-44 rather than
  implementing Nostr cryptography.
- Continue server-rendered HTML. HTMX adds fragments, not a client-side
  application architecture.
- The root-secret file is the only irreplaceable application state. The admin
  password is resettable local state.
- Do not add paid registration until free registration survives a tested
  database-loss restore.

## Target module layout

```text
src/
  admin/             authentication, sessions, admin handlers
  config.rs          CLI/environment configuration
  crypto.rs          root-secret loading and deterministic derivation
  domain.rs          canonical address, destination, policy value types
  http/
    api.rs           JSON/LUD routes
    ui.rs            Maud pages and HTMX fragments
  nostr/
    codec.rs         backup/config/announcement event encoding
    outbox.rs        publish and acknowledgement worker
    restore.rs       relay fetch, merge, validation, rebuild
    sync.rs          repair and relay health
  payment/
    lud21.rs         recipient probe and settlement verification
    policy.rs        tier selection and policy fingerprint
  repository/
    mod.rs           narrow repository traits
    sqlite.rs        SQLite implementation
  service.rs         registration state machine and resolution
```

This is a direction, not a requirement to move every existing file at once.
File moves should occur only when a milestone needs the separation.

## Milestone 0: executable decisions and test scaffolding

Goal: settle choices that affect stored data before writing migrations.

Tasks:

- Add architecture decision records for:
  - Diesel SQLite versus `sqlx`; prefer Diesel SQLite initially because the
    repository already uses Diesel and embedded migrations.
  - `nostr-sdk` version and the exact NIP-44 API it exposes.
  - How asynchronous handlers call synchronous Diesel: a small connection pool
    plus `spawn_blocking`, never blocking Tokio executor threads directly.
- Add unit-test helpers for canonical domains, usernames, fixed root secrets,
  deterministic timestamps, and signed Nostr fixtures.
- Add an integration-test harness that starts the Axum router without binding a
  public socket.
- Record protocol version constants in one module.

Exit gate:

- A test can construct the router with a temporary repository.
- Dependency choices and SQLite concurrency assumptions are documented.
- No production behavior changes.

## Milestone 1: domain model and SQLite repository

Goal: replace PostgreSQL at the repository boundary without adding Nostr yet.

Tasks:

- Introduce validated `Domain`, `Username`, `LightningAddress`, and
  `Destination` types. Canonicalization happens during parsing, never ad hoc in
  handlers.
- Define explicit address states and revisions.
- Add SQLite support and migrations for `addresses`, `registration_attempts`,
  `domain_payment_policies`, `reserved_names`, `nostr_outbox`,
  `nostr_sync_state`, `admin_sessions`, and an initialization metadata row.
- Set SQLite pragmas on every connection: foreign keys on, WAL mode, bounded
  busy timeout, and an appropriate synchronous mode.
- Refactor repository methods around transactions instead of mirroring the old
  CRUD trait. Claim reservation must be one atomic operation.
- Keep registration free and make existing resolution/UI tests pass on SQLite.
- Replace `LNADDRD_DATABASE_URL` with `LNADDRD_DATABASE_PATH`, while accepting
  the old variable only in the import command.

Tests:

- Canonicalization and rejection vectors from LUD-16.
- Unique claims under concurrent tasks.
- Address state transitions and revision monotonicity.
- SQLite close/reopen persistence.
- Existing LNURL manifest proxy behavior.

Exit gate:

- The application runs entirely on SQLite.
- Concurrent free claims cannot produce duplicates.
- There is no Nostr-backed recovery claim yet; documentation and UI say so.

## Milestone 2: root secret and event codec

Goal: encode and decode the protocol deterministically without networking.

Tasks:

- Implement atomic root-secret creation, strict permissions, parsing, and
  error handling.
- Implement HKDF derivation for signing, encryption, and address lookup keys.
- Add fixed derivation test vectors to the repository so later refactors cannot
  silently change service identity.
- Implement HMAC address keys.
- Define versioned Serde plaintext types for active records, tombstones, and
  service configuration.
- Use `nostr-sdk` to NIP-44 encrypt/decrypt and sign/verify NIP-78 events.
- Validate author, `d`, `p`, recomputed address key, canonical address, schema,
  state fields, revision, and timestamps during decode.
- Ensure sensitive structs use redacted `Debug` implementations.

Tests:

- Golden derivation and event fixtures from a fixed root secret.
- Round trips for active, deleted, and configuration records.
- Tampered signature, ciphertext, tags, address, revision, and wrong-root
  failures.
- An encrypted event contains none of the address/destination/token substrings.

Exit gate:

- Event fixtures are stable and independently inspectable.
- Wrong keys and malformed events always fail closed.
- No relay networking is required for the test suite.

## Milestone 3: transactional outbox and Nostr publication

Goal: every acknowledged mutation has at least one remotely accepted backup.

Tasks:

- Add configured relay parsing and a single shared `nostr-sdk` client.
- In the same SQLite transaction as a proposed mutation, create the new
  revision and insert its fully signed event into `nostr_outbox`.
- Implement an outbox worker with per-relay results, bounded exponential
  backoff, and graceful shutdown.
- Make the service wait for one positive relay `OK` before transitioning a new
  registration from `publishing` to `active`.
- Keep locally cached active addresses resolvable when all relays are offline.
- Add liveness/readiness checks and admin-neutral structured sync logs.
- Implement tombstone-first deletion.

Tests:

- A local test relay accepts, rejects, delays, and disconnects publication.
- Registration is inactive before acknowledgement and active afterward.
- Duplicate retries publish the identical event id.
- Relay outage does not break resolution but prevents new activation.
- Deletion cannot resurrect from an older active event.

Exit gate:

- Free registration is Nostr-backed.
- Failure after local reservation is retryable and never charges a user.
- The current address read path never performs a relay request.

## Milestone 4: cold restore and repair

Goal: prove the core operational promise before building optional features.

Tasks:

- Implement author/kind restore queries, local `d`-prefix filtering, EOSE
  tracking, and multi-relay deduplication.
- Implement revision/timestamp/event-id merge rules.
- Add `lnaddrd initialize-empty`, `lnaddrd restore --database <path>`, and
  `lnaddrd restore --dry-run` commands.
- Refuse an uninitialized empty result unless `initialize-empty` was explicitly
  used.
- Restore the encrypted service configuration first, then addresses and
  tombstones in one SQLite transaction.
- Add a repair loop that republishes current records to missing relays without
  blocking HTTP resolution.
- Surface last successful sync and per-relay health without exposing private
  record data.

Tests:

- Delete a populated SQLite file and reproduce it from relay fixtures.
- Restore with one unavailable relay and one complete relay.
- Fail closed for no EOSE, wrong root, missing config marker, conflicting equal
  revisions, corrupt events, and unsupported schemas.
- Restore a legitimate service with zero addresses.
- Compare restored LUD responses with pre-loss responses.

Exit gate:

- The database-loss acceptance test runs automatically.
- Only the root-secret plus deployment configuration is required to recover
  durable application state.
- This milestone should be released before admin/payment functionality.

## Milestone 5: PostgreSQL import and rollout

Goal: move existing installations without risking live records.

Tasks:

- Add a separate `import-postgres` command behind the PostgreSQL feature so the
  normal binary/runtime no longer needs libpq after migration support sunsets.
- Read legacy rows without mutating them, validate/canonicalize all rows, and
  report conflicts before publication.
- Preserve existing management tokens by hashing them with Argon2id for local
  and encrypted storage.
- Publish revision-1 events and require acknowledgement for every imported
  address.
- Produce a redacted, signed import report containing counts and event ids.
- Document stop-the-world cutover and rollback to the untouched PostgreSQL
  instance.

Tests:

- Import fixture databases with valid rows, duplicates, invalid destinations,
  and interrupted relay publication.
- Re-running an import is idempotent.

Exit gate:

- No legacy row is considered migrated without a relay-acknowledged event.
- The old database remains a usable rollback point.

## Milestone 6: resettable admin authentication and HTMX shell

Goal: add local administration without coupling it to service identity.

Tasks:

- Generate the admin-password file whenever absent; never derive it from the
  root secret and never include it in Nostr backups.
- On startup, bind admin sessions to a hash of the current password-file
  contents. Replacing/removing the file and restarting invalidates all sessions.
- Add Argon2id password verification, constant-time comparison where
  applicable, rate-limited login, server-side sessions, CSRF, and secure cookie
  settings.
- Vendor a pinned HTMX asset and remove third-party runtime dependencies from
  admin pages.
- Add relay health, address list, retry, delete, and dry-run restore views.
- Ensure destructive admin actions show exact targets and publish backups or
  tombstones before reporting success.

Tests:

- Missing password creates a new one; reset changes no derived Nostr key or
  address record.
- Password reset invalidates old sessions.
- CSRF, expired session, login throttling, and cookie attributes.
- HTMX and non-JavaScript form fallbacks for essential actions.

Exit gate:

- Admin access can be reset with no user-visible address change.
- Admin UI remains optional and bindable to a private interface/reverse-proxy
  policy.

## Milestone 7: reserved names and payment-policy configuration

Goal: establish policy management before accepting payments.

Tasks:

- Implement exact canonical reserved names with visible packaged defaults.
- Implement validated, ordered username-length tiers and deterministic policy
  fingerprints.
- Add HTMX policy editor and quote endpoint.
- Publish the encrypted configuration replacement before acknowledging an
  admin change.
- Implement LUD-21 recipient probing, including a test invoice and matching
  unsettled verification response.
- Build a hardened outbound LNURL client with DNS/IP checks, redirect checks,
  response limits, and timeouts shared by proxy and payment code.

Tests:

- Tier boundaries, monotonicity, missing tiers, Unicode/IDNA cases, and reserved
  name precedence.
- Configuration restores after deleting SQLite.
- SSRF attempts through initial URL, DNS result, and redirect.
- LUD-21 missing/malformed/mismatched verification responses.

Exit gate:

- Quotes and policy backups work, but no invoice is yet offered publicly.

## Milestone 8: paid registration state machine

Goal: safely activate exact-name claims after externally verified payment.

Tasks:

- Add pending-attempt creation with an atomic name reservation and TTL.
- Request an exact-amount invoice from the configured recipient and persist its
  payment hash, BOLT11, verify URL, policy fingerprint, and expiry.
- Return invoice/QR fragments and poll status through HTMX.
- Verify `settled`, identical invoice, expected amount, payment hash, and claim
  binding entirely server-side.
- After settlement, publish the active backup and activate only after relay
  acknowledgement.
- Retry post-payment publication without requesting a second payment.
- Expire unpaid attempts and disclose no-refund/failure semantics before invoice
  creation.

Tests:

- Full free and paid flows against fake LNURL/LUD-21 endpoints.
- Payment for address A cannot activate B.
- Old-policy invoices cannot activate a new quote.
- Concurrent attempts, expired invoice, replay, verifier outage, and relay
  outage after settlement.
- Restart at every state boundary.

Exit gate:

- No test path activates without both verified settlement and relay
  acknowledgement.
- A paid user never has to pay twice solely because Nostr publication failed.

## Milestone 9: public service announcements

Goal: make operators discoverable without putting registration data in public
events.

Tasks:

- Encode/sign the service announcement NIP-78 event and serve
  `/.well-known/lnaddrd.json`.
- Publish on relevant configuration changes and before NIP-40 expiry.
- Add a standalone discovery/validation command as the first client proof of
  concept; do not embed a general-purpose directory into the server.
- Validate HTTPS domain control, event replacement, expiry, schemas, and live
  pricing differences.
- Document spam/trust policy and make clear that discovery is not failover.

Tests:

- Standard NIP-01 tag query discovers fixture announcements.
- Well-known pubkey/coordinate mismatch is unverified.
- Expired, retired, malformed, or unreachable services are not offered.
- Announcement contains no usernames, destinations, receipts, or management
  data.

Exit gate:

- Two independently configured test instances can be discovered and selected
  by the proof-of-concept client.

## Cross-cutting verification

Run throughout development:

- `cargo fmt --check`, Clippy with warnings denied for project code, unit tests,
  and integration tests.
- Property tests for canonicalization, tier selection, event decoding, and
  merge ordering.
- Dependency audit and license review when adding Nostr/crypto/SQLite crates.
- Secret scanning of logs and serialized public events in tests.
- Restore tests with real relay software in CI in addition to protocol fakes.
- A documented manual disaster-recovery drill before calling the database
  disposable in the README.

## Suggested pull-request boundaries

Keep reviews narrow. A practical sequence is:

1. Test harness and domain types.
2. SQLite repository and free flow.
3. Root-secret derivation and golden vectors.
4. NIP-78 codec.
5. Outbox and relay client.
6. Cold restore and repair.
7. PostgreSQL importer.
8. Admin authentication and HTMX shell.
9. Policy/reserved names and hardened LNURL client.
10. Paid registration.
11. Announcements and discovery client.

The first major release boundary is after PR 6: at that point the central
promise—recovering a free Lightning Address service after database loss—is
real. Payment and discovery can follow without holding that reliability work
hostage.
