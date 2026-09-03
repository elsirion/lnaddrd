# Registration API v1

Status: lnaddrd microstandard, version 1

## Abstract

This document specifies the JSON HTTP API a Lightning Address operator exposes
for programmatic registration, payment, status polling, ownership listing, and
address management. It is the machine-readable counterpart to the HTML
registration form: wallets and other clients that discover an operator via the
[service announcement microstandard](02-service-announcements.md) use this API
instead of the HTML UI.

The API has two authentication surfaces. New endpoints under `/api/v1` accept
an optional [NIP-98](https://github.com/nostr-protocol/nips/blob/master/98.md)
signed HTTP event to bind a registration or an ownership query to a Nostr
public key. A legacy surface under `/lnaddress` predates NIP-98 support and
authenticates management operations with an opaque bearer token, now with
NIP-98 accepted as an alternative. Neither identity is required to use the
service: a caller who never supplies a NIP-98 header can register and manage
its own free address with only the bearer token.

## Discovery

An operator that implements this microstandard advertises it in its
announcement content (document 02) via one or both capability strings:

- `registration-api-v1`: the service exposes this JSON API at `<origin>/api/v1`,
  where `<origin>` is the same normalized public origin used as the `d`-tag
  suffix of the announcement (scheme `https`, lowercase host, optional
  non-default port, no trailing slash, no path/query/fragment).
- `nostr-auth`: the service accepts a NIP-98 `Authorization` header on the
  endpoints described in the [Authentication](#authentication) section, in
  addition to (or instead of) the legacy bearer token.

A client that sees `registration-api-v1` should call `GET
<origin>/api/v1/register/quote` before showing a price, `POST
<origin>/api/v1/register` or `.../register/start` to register, and `GET
<origin>/api/v1/addresses` (only meaningful together with `nostr-auth`) to
list addresses it owns.

All `/api/v1` and `/lnaddress` routes are served with a permissive CORS policy
(`Access-Control-Allow-Origin: *`, methods `GET, POST, PUT, DELETE, OPTIONS`,
headers `Content-Type, Authorization`) so browser-based wallets can call them
directly from another origin. The administration UI and other operator-only
routes are not part of this CORS policy and are out of scope for this
document.

## Quote

```
GET /api/v1/register/quote?domain=<domain>&username=<username>
```

Returns the current price for a name without reserving it or generating an
invoice. Prices can change between a quote and a registration attempt; the
quote is informational.

Request:

```
GET /api/v1/register/quote?domain=pay.example.com&username=alice
```

Response `200 OK` (free name):

```json
{ "price_msat": 0 }
```

Response `200 OK` (priced name):

```json
{ "price_msat": 100000 }
```

Errors: `invalid_input`, `unsupported_domain`, `taken`, `reserved`,
`length_disabled`, `rate_limited` (see [Errors](#errors)). This endpoint
shares a 30-requests-per-minute-per-IP bucket with the HTML registration UI's
own quote action.

## Free registration

```
POST /api/v1/register
Content-Type: application/json
```

Registers a name immediately, without payment. Only usable when the quoted
price for the name is `0`; a priced name must use
[paid registration](#paid-registration) instead.

Request body:

```json
{
  "domain": "pay.example.com",
  "username": "alice",
  "destination": "alice@getalby.com",
  "owner_pubkey": "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459"
}
```

`destination` is any Lightning Address or LNURL-pay string the registered
address will forward to. `owner_pubkey`, and the `Authorization` header
described in [Authentication](#authentication), are both optional and
independent of each other; see that section for how they interact.

Response `200 OK`:

```json
{
  "address": "alice@pay.example.com",
  "management_token": "aZ3kP9mQ2xT7vB1nL5cR8wY4hJ6sD0fG",
  "active": true
}
```

`management_token` is a bearer secret for the legacy
[management endpoints](#management) and is returned exactly once, in this
response body; it is not recoverable later through the API. `active: false`
means the address is registered but its encrypted Nostr backup has not yet
been acknowledged by a relay (see the project README's note on relay
outages); the address still resolves from local state either way.

Errors: `invalid_input` (also returned for a downstream registration failure,
e.g. an unreachable or invalid `destination`), `unsupported_domain`, `taken`,
`reserved`, `length_disabled`, `payment_required` (the name is not free; use
`/api/v1/register/start`), `owner_mismatch`, `unauthorized` (an `Authorization`
header was present but failed NIP-98 verification — this endpoint runs the
same NIP-98 check as `owner_pubkey` resolution even though NIP-98 itself is
optional here), `rate_limited`. This endpoint shares a
10-requests-per-minute-per-IP bucket with `/api/v1/register/start` and the
legacy `POST /lnaddress/register`.

## Paid registration

Paid registration is a two-step flow: start an attempt to receive a BOLT11
invoice, pay it out-of-band, then poll for completion.

### Start

```
POST /api/v1/register/start
Content-Type: application/json
```

Request body is the same shape as [free registration](#free-registration):

```json
{
  "domain": "pay.example.com",
  "username": "ab",
  "destination": "alice@getalby.com"
}
```

The registration policy's recipient must be a Lightning Address or LNURL-pay
endpoint that supports
[LUD-21](https://github.com/lnurl/luds/blob/luds/21.md) (`verify`), since the
service settles the attempt by polling the recipient's verify URL rather than
trusting client-reported payment.

Response `200 OK`:

```json
{
  "id": "K4hN2pQ8xT1vB6mL9cR3wY7sD0fG5jH2",
  "bolt11": "lnbc1000n1p...",
  "amount_msat": 1000000,
  "expires_at": 1750000900
}
```

`id` identifies the attempt for the [status poll](#poll); `expires_at` is a
Unix timestamp (seconds). Unpaid attempts expire after 15 minutes (or the
invoice's own expiry, if shorter).

Errors: `invalid_input` (also returned for a downstream failure, e.g. the
policy's recipient rejecting the invoice request), `unsupported_domain`,
`taken`, `reserved`, `length_disabled`, `free_registration` (the name is
free; use `POST /api/v1/register`), `owner_mismatch`, `unauthorized` (an
`Authorization` header was present but failed NIP-98 verification, for the
same reason as in free registration, above), `rate_limited` (same shared
bucket as free registration, above).

### Poll

```
GET /api/v1/register/:id
```

Response `200 OK`, while unpaid:

```json
{ "state": "pending_payment" }
```

Response `200 OK`, once payment is detected and the backup event is being
published:

```json
{ "state": "publishing" }
```

Response `200 OK`, if the attempt was not paid before it expired:

```json
{ "state": "expired" }
```

Response `200 OK`, once complete:

```json
{
  "state": "complete",
  "address": "ab@pay.example.com",
  "management_token": "aZ3kP9mQ2xT7vB1nL5cR8wY4hJ6sD0fG"
}
```

`management_token` is present and non-null only in the *first* `complete`
response a client observes; every response after that repeats
`"state": "complete"` and `"address"` but carries `"management_token": null`.
A client must capture the token from that first response — there is no way
to retrieve it again afterwards.

Errors: `not_found` (404, only for an `:id` the service has never issued),
`internal` (500, including a transient failure verifying payment on an
attempt that does exist — this is deliberately distinguished from
`not_found`). This endpoint is not rate-limited.

## Authentication

`/api/v1` and the legacy `/lnaddress/update` and `/lnaddress/remove`
endpoints accept an optional
[NIP-98](https://github.com/nostr-protocol/nips/blob/master/98.md) HTTP Auth
event, carried as:

```
Authorization: Nostr <base64>
```

`Nostr` is matched case-insensitively; `<base64>` is the standard (padded)
base64 encoding of the signed event's JSON serialization. The service only
attempts NIP-98 verification when this header is present at all; requests
with no `Authorization` header fall back to whichever other authentication
the endpoint supports (or are rejected, for endpoints that require it).

The event must satisfy, exactly as implemented:

- `kind` is `27235`.
- The signature verifies and `created_at` is within 60 seconds of the
  server's clock (either direction).
- A `u` tag whose value equals the operator's normalized public origin (the
  same one advertised in the service announcement) concatenated with the
  request's path and query string, e.g.
  `https://pay.example.com/api/v1/register/quote?domain=pay.example.com&username=alice`.
  NIP-98 verification is impossible — every request gets `401` — on a
  deployment that has not configured a public base URL.
- A `method` tag equal to the HTTP method, case-insensitively.
- A `payload` tag equal to the lowercase hex SHA-256 digest of the raw
  request body, if and only if the request has a body. A `payload` tag on a
  bodyless request (e.g. `GET`), or a missing `payload` tag on a request with
  a body, is rejected.

Each event id is accepted at most once: a successfully verified event is
remembered for 120 seconds, and a repeat of the same event id within that
window is rejected as a replay. Because 120 seconds exceeds the 60-second
freshness window, an event cannot age out of the replay guard while it is
still otherwise valid — it is effectively single-use for its whole lifetime.

Any header that fails any of the above checks — malformed, wrong signature,
wrong `u`/`method`/`payload`, stale, or replayed — is rejected with
`401 unauthorized`; the server never falls back to another authentication
method (such as a body token) after rejecting a header that was present.

Endpoint-specific behavior:

- `GET /api/v1/addresses` requires a valid NIP-98 header; there is no
  fallback, and a missing or invalid header is `401 unauthorized`.
- `POST /api/v1/register` and `POST /api/v1/register/start` accept an
  optional `owner_pubkey` field in the body *and* an optional NIP-98 header,
  independently:
  - Neither present: the address has no owner.
  - Only `owner_pubkey`: it is stored as given, as an **unauthenticated
    claim** — see [Security and privacy considerations](#security-and-privacy-considerations).
  - Only NIP-98: the signer's pubkey is stored as owner.
  - Both present and equal: the signer's pubkey is stored as owner.
  - Both present and different: rejected with `400 owner_mismatch`.
- `PUT /lnaddress/update` and `DELETE /lnaddress/remove` accept a valid
  NIP-98 header as proof of ownership (checked against the address's stored
  `owner_pubkey`) in place of the legacy `authentication_token` field; see
  [Management](#management).

## Owned addresses

```
GET /api/v1/addresses
```

Requires the [NIP-98](#authentication) header described above; there is no
unauthenticated form of this endpoint.

Request:

```
GET /api/v1/addresses
Authorization: Nostr <base64>
```

Response `200 OK`:

```json
{
  "addresses": [
    {
      "domain": "pay.example.com",
      "username": "alice",
      "destination": "alice@getalby.com"
    }
  ]
}
```

The list contains every address whose stored `owner_pubkey` equals the
signer's pubkey; it is empty (not an error) if the signer owns nothing.
Addresses registered without an `owner_pubkey`, or with a different one,
never appear here regardless of who calls it.

Errors: `unauthorized` (missing or invalid header), `internal`.

## Management

Two legacy endpoints, predating this API version, update or remove an
existing address. Unlike `/api/v1`, error responses here are bare HTTP status
codes with **no JSON body** — there is no `{"error": "..."}` envelope.

Both accept two ways to authenticate the caller as the address's owner,
checked in this order:

1. A valid [NIP-98](#authentication) header — the signer's pubkey must match
   the address's stored `owner_pubkey` exactly, or the request is rejected.
2. Otherwise, the body's `authentication_token` field, checked against the
   Argon2id hash of the token issued at registration time.

If neither is present (no header and no token in the body), the request is
`401` with an empty body. A NIP-98 header that is present but
cryptographically invalid (bad signature, wrong `u`/`method`/`payload`,
stale, replayed) is always rejected outright with `401` — it never falls
back to the body token, even if the token is otherwise valid. A header or
token that is present, well-formed, and *resolves* (so `resolve_management_auth`
succeeds) but names the wrong owner or the wrong token is a separate check,
made by the service layer once it loads the address — and its status code
differs per endpoint: `400` on Update, `401` on Remove. See each endpoint
below.

### Update

```
PUT /lnaddress/update
Content-Type: application/json
```

Request body (token form):

```json
{
  "domain": "pay.example.com",
  "username": "alice",
  "destination": "newdest@getalby.com",
  "authentication_token": "aZ3kP9mQ2xT7vB1nL5cR8wY4hJ6sD0fG"
}
```

Request body (NIP-98 form — `authentication_token` omitted, `Authorization`
header carries proof of ownership instead):

```json
{
  "domain": "pay.example.com",
  "username": "alice",
  "destination": "newdest@getalby.com"
}
```

Response `200 OK`:

```json
{ "active": true }
```

`active` has the same meaning as in [free registration](#free-registration).

Errors: `401` (no `Authorization` header and no `authentication_token`, or
an `Authorization` header that is present but fails NIP-98 verification —
`resolve_management_auth` itself failed). `400` for every other failure:
malformed JSON, an invalid `destination`, the address does not exist, the
address exists but is not currently active, **or** a well-formed
`authentication_token`/NIP-98 pubkey that simply names the wrong owner (a
wrong token or a pubkey that isn't the address's owner) — the service layer
maps all of these to a blanket `400`, so a caller cannot distinguish "wrong
credentials" from "bad request" on this endpoint by status code alone.

### Remove

```
DELETE /lnaddress/remove
Content-Type: application/json
```

Request body:

```json
{
  "domain": "pay.example.com",
  "username": "alice",
  "authentication_token": "aZ3kP9mQ2xT7vB1nL5cR8wY4hJ6sD0fG"
}
```

(`authentication_token` may be omitted when authenticating via NIP-98, as in
Update above.)

Response `204 No Content` on success (empty body).

Errors: `400` (malformed JSON), `401` (missing/invalid/mismatched
authentication, or the address does not exist).

### Legacy free registration

```
POST /lnaddress/register
Content-Type: application/json
```

Predates `/api/v1/register`: no `owner_pubkey`, no NIP-98, and the `lnurl`
field name instead of `destination`.

Request body:

```json
{
  "domain": "pay.example.com",
  "username": "alice",
  "lnurl": "alice@getalby.com"
}
```

Response `200 OK` — note the field is `lnaddr`, not `address`:

```json
{
  "lnaddr": "alice@pay.example.com",
  "authentication_token": "aZ3kP9mQ2xT7vB1nL5cR8wY4hJ6sD0fG",
  "active": true
}
```

Errors: `400` (unsupported domain, reserved name, priced name, invalid
`lnurl`, or other downstream failure — all indistinguishable bare `400`s),
`429` (shares the same 10-per-minute-per-IP bucket as `/api/v1/register` and
`/api/v1/register/start`). New integrations should use
[`POST /api/v1/register`](#free-registration) instead, which returns a
structured error code on failure.

## Errors

`/api/v1` endpoints that fail return a JSON body `{"error": "<code>"}` with
one of the following codes and HTTP statuses. (The legacy `/lnaddress/*`
endpoints never return this JSON body — see [Management](#management).)

| Code | Status | Meaning |
| --- | --- | --- |
| `invalid_input` | 400 | Malformed JSON body, invalid/missing query parameters, a malformed `domain`/`username`, an invalid `owner_pubkey` (not 64 lowercase hex characters), **or** a downstream registration failure (e.g. an unreachable `destination`, an invoice request rejected by the recipient). |
| `unsupported_domain` | 400 | `domain` is not one of the service's configured domains. |
| `length_disabled` | 400 | The payment policy has no tier covering this username length. |
| `taken` | 409 | The name is already registered, or an attempt already claims it (state `completed`, `publishing`, or `pending_payment`). |
| `reserved` | 409 | The name is on the service's reserved list. |
| `payment_required` | 400 | `POST /api/v1/register` was called for a name whose quoted price is non-zero. |
| `free_registration` | 400 | `POST /api/v1/register/start` was called for a name whose quoted price is zero. |
| `owner_mismatch` | 400 | Both a NIP-98 header and a body `owner_pubkey` were present and disagreed. |
| `unauthorized` | 401 | A NIP-98 header was required or supplied and failed verification (or `GET /api/v1/addresses` was called with no header at all). |
| `rate_limited` | 429 | The calling IP exceeded the endpoint's per-minute request budget. |
| `not_found` | 404 | `GET /api/v1/register/:id` was called with an `:id` the service never issued. Never returned for any other reason. |
| `internal` | 500 | An unexpected server-side failure, including a transient error verifying payment status on an attempt that does exist. |

## Security and privacy considerations

An `owner_pubkey` supplied in a registration body **without** an accompanying
NIP-98 header is stored as-is and is an **unauthenticated claim**: nothing
proves the caller controls the corresponding private key. A client consuming
`GET /api/v1/addresses` or any other ownership-derived data should treat
addresses whose owner was set this way as attributed, not proven, and prefer
NIP-98-authenticated registration when ownership matters.

NIP-98 verification requires the operator to have configured a public base
URL; without one, every NIP-98 header is rejected regardless of validity, and
NIP-98-only endpoints (`GET /api/v1/addresses`) become entirely unusable.

Both the per-IP rate limiter and the NIP-98 replay guard are in-memory and
per-process: they reset on restart, and provide no protection against
distributed clients (many IPs) or clients behind a shared NAT/proxy.

`owner_pubkey` is validated only as 64 lowercase hex characters — this
confirms the shape of a Nostr public key, not that it lies on the curve or is
in current use.

The `management_token` returned by free registration and by the paid-flow's
first `complete` poll response is a bearer secret with no expiry: anyone who
obtains it can update or delete the address via the legacy management
endpoints. It is shown exactly once and cannot be recovered from the API
afterward; a client that fails to record it must ask the address owner to
re-register or use NIP-98 management going forward (if the address carries
an `owner_pubkey`).

CORS is deliberately open (`Access-Control-Allow-Origin: *`) across this
entire public surface, so that browser-based wallets can call it directly
from any origin; this is a considered trade-off, not an oversight, and mirrors
the fact that these endpoints require no ambient authority (cookies,
sessions) to be misused across origins. The administration UI is not part of
this CORS policy.

## Standards composed

- NIP-01: https://github.com/nostr-protocol/nips/blob/master/01.md
- NIP-98: https://github.com/nostr-protocol/nips/blob/master/98.md
