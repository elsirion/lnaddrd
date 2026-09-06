NIP-XX
======

Lightning Address Service Announcements
---------------------------------------

`draft` `optional`

This NIP defines how operators of Lightning Address services — servers that
let users register [LUD-16](https://github.com/lnurl/luds/blob/luds/16.md)
addresses (`user@domain`) on domains the operator controls — announce their
service, domains, and registration pricing over Nostr, and how clients
discover and verify those announcements to build provider directories and
registration marketplaces.

It deliberately covers only *discovery and verification*. How a client
registers an address with a discovered service (HTTP API, authentication,
payment) is out of scope and advertised opaquely through capability strings.

## Service announcement

A service publishes an addressable event of `kind:38477`, signed by its
long-lived service key:

```jsonc
{
  "kind": 38477,
  "tags": [
    ["d", "https://pay.example.com"],
    ["expiration", "1758000000"]
  ],
  "content": "{ /* see below */ }"
}
```

- The `d` tag MUST be the service's canonical HTTPS origin: an `https://`
  URL with no userinfo, path, query, or fragment, serialized as an ASCII
  origin (scheme, host, and non-default port only).
- An `expiration` tag ([NIP-40](40.md)) SHOULD be present so abandoned
  services age out of relays; publishers SHOULD republish well before
  expiry (for example weekly with a multi-week expiration).
- Clients MUST keep only the newest event per `(pubkey, d)` coordinate.

### Content

`content` is stringified JSON:

```jsonc
{
  "schema": 1,
  "name": "Example Pay",
  "about": "Community-operated address forwarding",
  "origin": "https://pay.example.com",
  "domains": ["pay.example.com", "tips.example.org"],
  "registration_url": "https://pay.example.com/register",
  "capabilities": ["free-registration", "paid-registration", "lud21-gate"],
  "pricing": [
    {
      "domain": "pay.example.com",
      "currency": "msat",
      "tiers": [
        { "max_length": 2, "price": 1000000 },
        { "max_length": 4, "price": 100000 },
        { "max_length": 64, "price": 0 }
      ]
    }
  ],
  "users": [
    { "domain": "pay.example.com", "count": 1312 },
    { "domain": "tips.example.org", "count": 0 }
  ],
  "contact": "npub1...",
  "terms_url": "https://pay.example.com/terms",
  "status": "active",
  "software": { "name": "lnaddrd", "version": "0.2.0" }
}
```

Required fields: `schema` (this document describes schema `1`), `origin`,
`domains`, `registration_url`, `capabilities`. All others are optional.
Consumers MUST ignore unknown fields.

Validation rules — clients MUST reject announcements where any of these
fail:

- `origin` equals the `d` tag and is a canonical HTTPS origin as above.
- The origin host and every entry of `domains` is a public registrable DNS
  name: at least two dot-separated labels, each 1–63 characters of
  lowercase `a-z0-9-` neither starting nor ending with `-`, whose final
  label is not all digits and not one of `localhost`, `local`, `internal`,
  `test`, `invalid`, `example`.
- `domains` is non-empty, sorted, and free of duplicates.
- `registration_url` parses and has the same origin as `origin`.
- `terms_url`, when present, is HTTPS.
- `status`, when present, is not `"retired"` (a retired announcement is a
  tombstone: clients MUST NOT list the service but SHOULD keep honoring the
  coordinate so the tombstone replaces older listings).

### Pricing

`pricing` entries describe registration cost per domain. `tiers` are
ordered by strictly increasing `max_length` with non-increasing `price`
(in the given `currency`; only `"msat"` is defined by schema 1). A
username's price is the `price` of the first tier whose `max_length` is
greater than or equal to the username's length. If no tier matches,
registration at that length is unavailable. A domain without a `pricing`
entry is free at every length.

### User counts

`users` entries are the operator's self-reported number of active
addresses per domain. They are unverifiable claims: clients MUST drop
entries whose `domain` is not listed in `domains` or whose `count` is not
a non-negative integer (dropping entries never invalidates the
announcement), SHOULD label the numbers as self-reported when displaying
them, and MAY cross-check them against other public signals.

### Capabilities

`capabilities` is a set of strings advertising optional features. Clients
MUST ignore unknown values. Defined values:

| capability          | meaning                                             |
|---------------------|-----------------------------------------------------|
| `free-registration` | usernames can be registered without payment         |
| `paid-registration` | some usernames require payment                      |
| `lud21-gate`        | paid registrations are verified via LUD-21          |
| `management-token`  | registrations issue a bearer management token       |
| `nostr-auth`        | address management accepts NIP-98 HTTP auth         |

Implementations MAY define further values (for example to advertise a
machine-readable registration API); such values SHOULD be documented by
the implementation that introduces them.

## Domain verification

Announcements are claims. Before presenting a domain as belonging to an
announcement, clients MUST fetch:

```
https://<domain>/.well-known/lnaddress.json
```

which the service serves for every domain it announces:

```jsonc
{
  "schema": 1,
  "service_pubkey": "<hex pubkey of the announcement's signing key>",
  "announcement": "38477:<hex pubkey>:<canonical origin>",
  "relays": ["wss://relay.example"]
}
```

A domain is verified when the document is reachable, `service_pubkey`
equals the announcement event's pubkey, and `announcement` equals the
event's `kind:pubkey:d-tag` coordinate. Clients MUST NOT present
unverified domains as belonging to the service and SHOULD distinguish
cryptographic mismatch from network unreachability. Services SHOULD serve
the document with permissive CORS so browser clients can verify.

The `relays` array hints where the service publishes its announcements.

## Client behavior

- Discover with `{"kinds": [38477]}` filters; verify event signatures
  before validating content.
- Apply every validation rule above; drop non-conforming announcements
  entirely (except `users` entries, which are dropped individually).
- Verify domains before display; re-verify when the announcement event id
  changes.
- Honor `expiration`; treat expired announcements as absent.

## Rationale

- **Why not [NIP-87](87.md)?** NIP-87 discovers ecash mints through info
  events and peer recommendations; it has no domain model, no pricing, and
  no proof of domain control — the core of this NIP.
- **Why not kind 31402 (Paid API Service Announcements, proposed)?** That
  proposal describes generic paid HTTP APIs. Lightning Address services
  need per-domain semantics: username-length price tiers, per-domain
  verification via a well-known document, and per-domain user counts.
- **Why not [NIP-89](89.md)/[NIP-99](99.md)?** Handler recommendations and
  classified listings target different consumers and data models.
- **Relation to [NIP-05](05.md):** NIP-05 maps nostr identifiers to
  pubkeys via a well-known document; this NIP announces services that host
  LUD-16 *Lightning* addresses. A provider may offer both, but the
  announcements are independent.

## Reference implementation

[lnaddrd](https://github.com/elsirion/lnaddrd) publishes announcements and
serves the well-known document; a static discovery/registration client
runs at <https://mkt.lnaddr.org>. (Deployed versions predating this NIP
publish the same content as `kind:30078` [NIP-78] events with a
`lnaddrd:service:v1:` identifier prefix, a `t=lightning-address-service`
tag, and the well-known path `/.well-known/lnaddrd.json`; they will
migrate to the dedicated kind.)
