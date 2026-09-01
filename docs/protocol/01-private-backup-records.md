# Private Lightning Address backup records over Nostr

Status: lnaddrd microstandard, version 1

## Abstract

This document defines encrypted, independently replaceable address records and
service configuration for a Lightning Address service. It composes NIP-01
addressable events, NIP-44 encryption, and NIP-78 application-specific data. It
does not allocate a new event kind.

## Event

Each record is a NIP-78 `kind:30078` addressable event signed by the service
key.

```json
{
  "kind": 30078,
  "pubkey": "<service-pubkey-hex>",
  "created_at": 1750000000,
  "tags": [
    ["d", "lnaddrd:backup:v1:<64-lowercase-hex-address-key>"],
    ["p", "<encryption-pubkey-hex>"],
    ["client", "lnaddrd"]
  ],
  "content": "<NIP-44-v2 ciphertext>"
}
```

The `d` tag MUST be the literal prefix `lnaddrd:backup:v1:` followed by the
address key defined in the main specification. No domain, username, price, or
other correlatable address metadata may appear in tags.

The single `p` tag identifies the derived encryption recipient. It is not
needed for querying but makes the NIP-44 recipient explicit. The `client` tag is
optional and carries no coordinate.

`content` MUST be NIP-44 version 2 encryption from the service signing secret
to the derived encryption public key. A restorer decrypts using the encryption
secret and service signing public key.

## Plaintext

The decrypted content is canonical UTF-8 JSON:

```json
{
  "schema": 1,
  "address_key": "<same 64-lowercase-hex value>",
  "address": {
    "username": "alice",
    "domain": "pay.example.com"
  },
  "state": "active",
  "revision": 3,
  "destination": {
    "type": "ln_address",
    "value": "alice@wallet.example"
  },
  "management": {
    "token_hash": "$argon2id$..."
  },
  "registration": {
    "price_msat": 100000,
    "policy_fingerprint": "<64-lowercase-hex>",
    "payment_hash": "<64-lowercase-hex>",
    "paid_at": 1750000000
  },
  "created_at": 1740000000,
  "updated_at": 1750000000,
  "updated_by": "token"
}
```

Required fields are `schema`, `address_key`, `address`, `state`, `revision`,
`created_at`, `updated_at`, and `updated_by`.

For `state: active`, `destination` and `management` are required.
`registration` is optional and, when present, MUST contain only durable receipt
metadata. It MUST NOT contain a preimage, invoice, verify URL, IP address, or
browser/session identifier.

For `state: deleted`, `destination`, `management`, and `registration` MUST be
absent. This encrypted replacement is the durable tombstone.

Unknown object members MUST be ignored. Unknown `schema` values MUST be retained
but not activated, allowing a newer implementation to restore them later.

`updated_by` is one of `token`, `admin`, `import`, or `restore_repair`.

## Validation

A restorer MUST reject a record unless:

1. Its NIP-01 id and signature are valid.
2. Its author is the derived service public key.
3. It has exactly one valid backup `d` tag and exactly one expected `p` tag.
4. NIP-44 authentication and decryption succeed.
5. The plaintext address canonicalizes to the stored address.
6. Recomputing the address key produces both the plaintext `address_key` and
   the suffix of the `d` tag.
7. `revision` is a positive integer and timestamps are plausible integers.
8. State-specific required/forbidden fields are respected.

The `created_at` in the outer event is transport metadata. The signed,
encrypted plaintext revision determines application ordering.

## Query and restore

To restore all records, query each configured relay with:

```json
{
  "kinds": [30078],
  "authors": ["<service-pubkey-hex>"],
  "#d": ["<exact identifiers when known>"]
}
```

NIP-01 filters do not define prefix matching. A complete cold restore therefore
queries by author and kind, then locally filters `d` tags by the
`lnaddrd:backup:v1:` prefix. Relays may return the author's other NIP-78 data;
implementations MUST ignore it.

For lookup or repair of one known address, compute its address key and use an
exact `#d` filter.

Relays normally retain only the newest event for a `(kind, pubkey, d)`
coordinate. Clients SHOULD nevertheless handle duplicate versions returned by
imperfect relays. The merge rule from the main specification applies.

## Publication

The service MUST sign the complete event before inserting it into its local
transactional outbox. Retries publish exactly the same signed event, rather than
creating new timestamps and ids.

At least one configured relay must return a successful NIP-01 `OK` before a
mutation becomes externally acknowledged. Negative and timeout results are
retained per relay for retry and diagnostics.

## Privacy and security considerations

The opaque `d` value prevents dictionary lookup without the root-derived lookup
key. It does not hide that one service key has approximately N records, update
times, record sizes, or its configured relay set.

NIP-44 pads messages, but implementations SHOULD additionally serialize absent
optional fields consistently and avoid putting secrets into unusually large
free-form metadata. Randomized ciphertext means identical plaintext updates do
not have identical content.

Compromise of the root secret reveals every backed-up address and permits
forged replacements. NIP-44 does not provide forward secrecy for this storage
use case. Root-key rotation is deliberately deferred because it requires a
careful, atomic re-encryption protocol.

Public relays are not backup services by promise. Operators should use several
independent relays and periodically test a restore into a temporary database.

## Encrypted service configuration

One additional NIP-78 event stores the durable configuration that is not
appropriate for public environment variables:

```json
{
  "kind": 30078,
  "pubkey": "<service-pubkey-hex>",
  "created_at": 1750000000,
  "tags": [
    ["d", "lnaddrd:config:v1"],
    ["p", "<encryption-pubkey-hex>"],
    ["client", "lnaddrd"]
  ],
  "content": "<NIP-44-v2 ciphertext>"
}
```

Its NIP-44 sender/recipient keys are the same as for address records. The
plaintext is:

```json
{
  "schema": 1,
  "revision": 4,
  "instance_id": "<32 random bytes as lowercase hex>",
  "domains": {
    "pay.example.com": {
      "payment_policy": {
        "destination": {"type":"ln_address","value":"fees@example.com"},
        "tiers": [{"max_length":4,"price_msat":100000}]
      },
      "reserved_names": ["admin", "support", "www"]
    }
  },
  "updated_at": 1750000000
}
```

`schema`, positive `revision`, random `instance_id`, `domains`, and
`updated_at` are required. `instance_id` is generated once when an empty
service is explicitly initialized and MUST remain stable. It distinguishes a
real initialized service with no address records from a failed/empty restore.

The plaintext domain keys MUST be a subset of `LNADDRD_DOMAINS`. A mismatch
fails startup and requires operator action; restore MUST NOT silently begin
serving a domain absent from local deployment configuration. Each payment
policy is optional. Reserved names are canonical, unique, and sorted.

The configuration event uses an exact, non-secret `d` tag because there is
exactly one per service author and its existence is not private. Contents
remain encrypted. Merge and publication follow the same revision and outbox
rules as address records. An admin mutation is acknowledged only after at least
one relay accepts the replacement event.

The root secret, admin password, relay URLs/credentials, domain TLS keys, and
admin sessions MUST NOT appear in this record. Those remain deployment secrets
or transient state.

## Standards composed

- NIP-01: https://github.com/nostr-protocol/nips/blob/master/01.md
- NIP-44: https://github.com/nostr-protocol/nips/blob/master/44.md
- NIP-78: https://github.com/nostr-protocol/nips/blob/master/78.md
