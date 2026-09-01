# ADR 0001: SQLite and Diesel

Status: accepted

`lnaddrd` uses bundled SQLite through Diesel 2.2. The database is a local read
cache and transaction coordinator, while encrypted Nostr events are recovery
state. Keeping Diesel preserves the existing repository model and embedded
migrations without introducing a second query stack.

Every pooled connection enables foreign keys, WAL, `synchronous=FULL`, and a
five-second busy timeout. Diesel calls run in `spawn_blocking`; synchronous
database work never runs directly on a Tokio executor thread. One writable
process per SQLite file is supported.
