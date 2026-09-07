NIP-XX
======

Lightning Address Services
--------------------------

`draft` `optional`

This NIP defines how operators of Lightning Address services — servers that
let users register [LUD-16](https://github.com/lnurl/luds/blob/luds/16.md)
addresses (`user@domain`) on domains the operator controls — announce their
service over Nostr, how clients verify those announcements, and the HTTP API
through which clients register and manage addresses. Any service
implementing this NIP can be used interchangeably by any client.

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
  origin (scheme, host, and non-default port only). The origin is also the
  base URL of the service API defined below.
- An `expiration` tag ([NIP-40](40.md)) MUST be present. Publishers SHOULD
  republish well before expiry (for example weekly with a multi-week
  expiration); a service that stops republishing disappears when its last
  announcement expires. To retire early, publish a replacement whose
  expiration is in the near past or present.
- Clients MUST keep only the newest event per `(pubkey, d)` coordinate and
  MUST treat expired announcements as absent.

### Content

`content` is stringified JSON:

```jsonc
{
  "name": "Example Pay",
  "about": "Community-operated address forwarding",
  "domains": ["pay.example.com", "tips.example.org"],
  "pricing": [
    {
      "domain": "pay.example.com",
      "price": 0,
      "tiers": [
        { "max_length": 2, "price": 1000000 },
        { "max_length": 4, "price": 100000 }
      ]
    }
  ],
  "users": [
    { "domain": "pay.example.com", "count": 1312 }
  ],
  "reserved": [
    { "domain": "pay.example.com", "names": ["admin", "www"] }
  ],
  "contact": "npub1...",
  "terms_url": "https://pay.example.com/terms"
}
```

`domains` is required; everything else is optional. Consumers MUST ignore
unknown fields. Incompatible future revisions of this specification will
use a different event kind rather than a version field.

Validation rules — clients MUST reject announcements where any of these
fail:

- The `d` tag is a canonical HTTPS origin as defined above.
- The origin host and every entry of `domains` is a public registrable DNS
  name: at least two dot-separated labels, each 1–63 characters of
  lowercase `a-z0-9-` neither starting nor ending with `-`, whose final
  label is not all digits and not one of `localhost`, `local`, `internal`,
  `test`, `invalid`, `example`.
- `domains` is non-empty, sorted, and free of duplicates.
- `terms_url`, when present, is HTTPS.

### Pricing

All prices are millisatoshi. A `pricing` entry describes registration cost
on one domain:

- `price` (required): the cost of a username of any length not covered by
  a tier — the base price. `0` means free.
- `tiers` (optional): overrides for short usernames, ordered by strictly
  increasing `max_length` (1–63) with non-increasing `price`; the base
  `price` MUST NOT exceed the last tier's `price`. A username's cost is
  the `price` of the first tier whose `max_length` is greater than or
  equal to the username's length, or the base `price` if none matches.

A domain without a `pricing` entry is free at every length. Prices are
previews for display and sorting; the authoritative price is the API's
quote below.

### User counts

`users` entries are OPTIONAL self-reported numbers of active addresses per
domain. They are unverifiable claims: clients MUST drop entries whose
`domain` is not listed in `domains` or whose `count` is not a non-negative
integer (dropping entries never invalidates the announcement), SHOULD
label the numbers as self-reported when displaying them, and MAY
cross-check them against other public signals.

### Reserved names

`reserved` entries are OPTIONAL per-domain lists of usernames the service
will not register publicly, so clients can preview a name as unavailable
instead of free or priced. Clients MUST drop entries whose `domain` is
not listed in `domains` and individual `names` values that are not
strings; dropping entries never invalidates the announcement. The list is
a preview — the API's quote is authoritative.

## Domain verification

Announcements are claims. Before presenting a domain as belonging to an
announcement, clients MUST fetch:

```
https://<domain>/.well-known/lnaddress.json
```

which the service MUST serve for every domain it announces:

```jsonc
{
  "service_pubkey": "<hex pubkey of the announcement's signing key>",
  "announcement": "38477:<hex pubkey>:<canonical origin>",
  "relays": ["wss://relay.example"]
}
```

A domain is verified when the document is reachable, `service_pubkey`
equals the announcement event's pubkey, and `announcement` equals the
event's `kind:pubkey:d-tag` coordinate. Clients MUST NOT present
unverified domains as belonging to the service and SHOULD distinguish
cryptographic mismatch from network unreachability. The `relays` array
hints where the service publishes its announcements.

## Service API

All endpoints live under the announced origin. Responses are JSON. Public
endpoints (everything below, plus the well-known document) MUST be served
with permissive CORS (`Access-Control-Allow-Origin: *`, the
`Authorization` and `Content-Type` request headers allowed, and preflight
`OPTIONS` answered) so browser clients can call them cross-origin.

Errors use status 4xx/5xx with body `{"error": "<code>"}` where `<code>`
is one of `invalid_input`, `unsupported_domain`, `taken`, `reserved`,
`length_disabled`, `unauthorized`, `not_found`, `expired`,
`rate_limited`, `internal`. Clients MUST treat unknown codes as opaque
failures.

Authentication is [NIP-98](98.md) HTTP auth. The signed URL MUST be the
exact request URL under the announced origin; requests with bodies MUST
include the `payload` hash tag. The signing key is the *user's* key — the
registered address is bound to it as its owner.

### Quote

```
GET /api/v1/register/quote?domain=<domain>&username=<username>
```

→ `200 {"price_msat": <u64>}` (0 = free) or an error (`taken`,
`reserved`, `length_disabled`, `unsupported_domain`, `invalid_input`).
The quote is authoritative and MAY differ from the announcement preview.

### Register (free)

```
POST /api/v1/register
Authorization: Nostr <NIP-98 event>
{"domain": "...", "username": "...", "destination": "<LNURL or Lightning Address>", "owner_pubkey": "<hex>"}
```

`owner_pubkey` MUST equal the NIP-98 signer. Only valid when the quoted
price is 0 → `200 {"address": "user@domain", "active": <bool>}`
(`active: false` while the service finalizes publication). A non-zero
price returns `invalid_input`; use the paid flow.

### Register (paid)

```
POST /api/v1/register/start        (same body and auth as above)
```

→ `200 {"id": "...", "bolt11": "...", "amount_msat": <u64>, "expires_at": <unix>}`.

The client pays the invoice and polls:

```
GET /api/v1/register/<id>
```

→ `200 {"state": "pending_payment" | "publishing" | "complete" | "expired",
"address"?: "user@domain"}`. Services SHOULD verify payment via
[LUD-21](https://github.com/lnurl/luds/blob/luds/21.md).

### List own addresses

```
GET /api/v1/addresses
Authorization: Nostr <NIP-98 event>
```

→ `200 {"addresses": [{"domain": "...", "username": "...", "destination": "..."}]}`
for every active address owned by the signing key.

### Update and delete

```
PUT /api/v1/address
{"domain": "...", "username": "...", "destination": "<new LNURL or Lightning Address>"}

DELETE /api/v1/address
{"domain": "...", "username": "..."}
```

Both require NIP-98 auth by the address owner → `200 {}` /
`204`. A signer that does not own the address receives `unauthorized`.

## Client behavior

- Discover with `{"kinds": [38477]}` filters; verify event signatures
  before validating content; apply every validation rule; verify domains
  before display and re-verify when the announcement event id changes.
- Registration and management calls go directly from the client to the
  announced origin; the user's key never leaves their signer.

## Rationale

- **Why not [NIP-87](87.md)?** NIP-87 discovers ecash mints through info
  events and peer recommendations; it has no domain model, no pricing, no
  proof of domain control, and no service API.
- **Why not kind 31402 (Paid API Service Announcements, proposed)?** That
  proposal describes generic paid HTTP APIs. Lightning Address services
  need per-domain semantics — username pricing, domain-control proof, a
  fixed registration API — not generic API metering.
- **Why not [NIP-89](89.md)/[NIP-99](99.md)?** Handler recommendations and
  classified listings target different consumers and data models.
- **Relation to [NIP-05](05.md):** NIP-05 maps Nostr identifiers to
  pubkeys via a well-known document; this NIP announces services hosting
  LUD-16 *Lightning* addresses. A provider may offer both, but the
  specifications are independent.

## Reference implementation

[lnaddrd](https://github.com/elsirion/lnaddrd) publishes announcements,
serves the well-known document, and implements the API; a static
discovery/registration client runs at <https://mkt.lnaddr.org>. (Deployed
versions predating this NIP publish `kind:30078` [NIP-78] events with a
`lnaddrd:service:v1:` identifier prefix, encode the base price as a
64-length tier, issue bearer management tokens alongside NIP-98, and use
`/.well-known/lnaddrd.json` and `/lnaddress/*` management paths; they
will migrate.)
