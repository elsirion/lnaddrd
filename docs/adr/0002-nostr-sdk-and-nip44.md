# ADR 0002: nostr-sdk and NIP-44

Status: accepted

The implementation uses `nostr-sdk` 0.44 for event construction, signature
verification, relay I/O, NIP-44 v2 encryption, NIP-78 addressable application
records, and NIP-40 expiration tags. Cryptographic event formats are not
implemented independently.

The dependency is intentionally pinned by `Cargo.lock`. Protocol constants and
the deterministic HKDF labels live in the source and have golden-vector tests,
so a dependency upgrade cannot silently change service identity.
