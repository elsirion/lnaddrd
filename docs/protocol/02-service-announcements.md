# Lightning Address service announcements over Nostr

Status: lnaddrd microstandard, version 1

## Abstract

This document lets a Lightning Address operator announce its domains,
registration endpoint, capabilities, and pricing summary. Wallets may discover
several operators and let a user select a domain. An announcement is a claim by
its Nostr author, not proof that the author controls a domain or is trustworthy.

This version uses NIP-78 `kind:30078` so implementations can deploy before a
dedicated event kind is warranted.

## Announcement event

One addressable event describes one HTTPS service origin:

```json
{
  "kind": 30078,
  "pubkey": "<service-pubkey-hex>",
  "created_at": 1750000000,
  "tags": [
    ["d", "lnaddrd:service:v1:https://pay.example.com"],
    ["t", "lightning-address-service"],
    ["r", "wss://relay-one.example", "backup"],
    ["r", "wss://relay-two.example", "backup"]
  ],
  "content": "{...}"
}
```

The `d` value is `lnaddrd:service:v1:` followed by the normalized public origin
(`https` scheme, lowercase ASCII host, optional non-default port, no trailing
slash). The visible origin is intentional: announcements are public.

The `t` tag MUST be exactly `lightning-address-service`, enabling discovery with
a standard exact tag filter. `r` tags are optional backup relay hints. The
third value, when present, MUST be `backup`. Clients MUST treat these as hints,
not endorsements or a complete relay list.

The content is UTF-8 JSON:

```json
{
  "schema": 1,
  "name": "Example Lightning Addresses",
  "about": "Community-operated address forwarding",
  "origin": "https://pay.example.com",
  "domains": ["pay.example.com", "tips.example.org"],
  "registration_url": "https://pay.example.com/register",
  "terms_url": "https://pay.example.com/terms",
  "contact": "npub1...",
  "capabilities": ["free-registration", "paid-registration", "lud21-gate"],
  "pricing": [
    {
      "domain": "pay.example.com",
      "currency": "msat",
      "tiers": [
        {"max_length": 2, "price": 1000000},
        {"max_length": 4, "price": 100000},
        {"max_length": 64, "price": 0}
      ]
    }
  ],
  "software": {"name": "lnaddrd", "version": "0.2.0"}
}
```

Required fields are `schema`, `origin`, `domains`, `registration_url`, and
`capabilities`. Unknown fields and capabilities MUST be ignored. Wallets MUST
ignore unsupported schema versions.

All advertised URLs MUST use the same origin, except `terms_url`, which MAY use
another HTTPS origin. Domains are normalized as in the main specification,
unique, and sorted lexicographically. Pricing is an informational snapshot;
the service's live quote is authoritative.

The origin host and every entry of `domains` MUST be a public registrable DNS
name: at least two dot-separated labels, each 1–63 characters of lowercase
`a-z0-9-` not starting or ending with `-`, whose final label is neither
all-digits nor one of `localhost`, `local`, `internal`, `test`, `invalid`, or
`example`. Consumers MUST reject non-conforming announcements; producers MUST
NOT publish them.

Allowed initial capabilities are:

- `free-registration`: at least one name can currently be registered for zero.
- `paid-registration`: at least one name requires payment.
- `lud21-gate`: paid registration is verified using LUD-21.
- `management-token`: users can later update/delete with a bearer token.
- `nostr-recoverable`: active records are backed up using the companion private
  backup record microstandard.
- `registration-api-v1`: the service exposes the JSON registration API at
  `<origin>/api/v1` as defined in the registration API microstandard
  (document 03).
- `nostr-auth`: the service accepts NIP-98 HTTP authentication for address
  management as defined in document 03.

## Discovery

A wallet or directory queries its chosen discovery relays with:

```json
{
  "kinds": [30078],
  "#t": ["lightning-address-service"]
}
```

It then validates the event and content, groups replacements by the NIP-01
coordinate `(30078, pubkey, d)`, and applies local trust policy.

Because broad tag queries are easy to spam, clients SHOULD use curated relay
sets, recommendations from trusted users, or explicit allowlists. Proof of work
or relay payment policy MAY be an input but is not defined here.

## Domain-control verification

Before presenting a domain as verified, a client MUST fetch:

```text
GET https://<domain>/.well-known/lnaddrd.json
```

Expected response:

```json
{
  "schema": 1,
  "service_pubkey": "<same 32-byte lowercase hex pubkey>",
  "announcement": "30078:<pubkey>:<d-value>",
  "relays": ["wss://relay-one.example", "wss://relay-two.example"]
}
```

The response MUST be HTTPS with a valid certificate, MUST NOT redirect to a
different registrable domain, and `service_pubkey` plus announcement coordinate
MUST match the event. This proves current control of the web origin serving the
Lightning Address. It does not prove future availability, solvency, identity,
or favorable legal/compliance treatment.

The well-known response gives wallets exact relay hints and an event coordinate
without overloading NIP-05, which maps names to user identities rather than
services.

## Publication and expiry

The service publishes its announcement to its configured relays on startup,
after configuration changes, and at least once every 24 hours if content has
changed. It SHOULD NOT create replacements merely to refresh a timestamp.

Announcements SHOULD include a NIP-40 `expiration` tag no more than 30 days in
the future and be republished before expiry. Clients SHOULD hide expired
announcements. If NIP-40 is used, an otherwise identical example gains:

```json
["expiration", "1752592000"]
```

An operator that is shutting down SHOULD replace the announcement content with
`"status":"retired"` and an optional `migration_url`, using a short expiration.
Wallets MUST NOT offer retired services for new registrations.

## Wallet behavior

Wallets MUST make operator choice explicit and show the resulting full address
before registration. They SHOULD display:

- Domain and verification status.
- Price returned by a fresh service quote.
- Operator name/about and contact if present.
- Terms link.
- Last announcement time and recent HTTPS reachability.

Wallets MUST NOT imply that discovery makes an operator federated or trusted.
They SHOULD allow users or wallet distributors to add/remove discovery relays
and pin operators.

Registration itself remains ordinary HTTPS. This specification does not put
usernames, invoices, or user destinations into public Nostr events.

## Security and privacy considerations

Anyone can publish a convincing-looking announcement. Domain-control checking
is mandatory for a verified badge, and user selection should incorporate a
trust source beyond mere event existence.

Fetching well-known files and live quotes leaks interest to service operators.
Wallets should fetch a small candidate set, use normal network privacy measures,
and avoid querying availability for a proposed username until the user selects
an operator.

Publishing exact prices makes business policy public and may become stale.
Operators MAY omit `pricing`; clients then fetch a live quote.

Backup relay tags reveal infrastructure choices but no private address records.
Operators concerned about this MAY omit them because the well-known document
also supplies relay hints.

## Why existing NIPs are composed this way

- NIP-78 is appropriate for the initial addressable application record and
  avoids squatting on an unassigned kind.
- NIP-65 is not reused directly because its `kind:10002` describes a person's
  general read/write relays, not a service's backup storage.
- NIP-89 describes handlers for event kinds, not network services selected by
  domain.
- NIP-05 associates internet identifiers with user keys and explicitly is not a
  generic service-verification protocol.
- NIP-40 supplies optional expiry without inventing custom relay behavior.

## Standards composed

- NIP-01: https://github.com/nostr-protocol/nips/blob/master/01.md
- NIP-05: https://github.com/nostr-protocol/nips/blob/master/05.md
- NIP-40: https://github.com/nostr-protocol/nips/blob/master/40.md
- NIP-65: https://github.com/nostr-protocol/nips/blob/master/65.md
- NIP-78: https://github.com/nostr-protocol/nips/blob/master/78.md
- NIP-89: https://github.com/nostr-protocol/nips/blob/master/89.md

