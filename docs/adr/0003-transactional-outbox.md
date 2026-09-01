# ADR 0003: Signed transactional outbox

Status: accepted

Each address or configuration mutation is fully encoded, encrypted, and signed
before the SQLite transaction stores both the proposed local state and exact
event JSON. Activation requires one positive relay acknowledgement. Retries
therefore resend the identical event id and cannot produce divergent revisions.

Resolution never waits for Nostr. A relay outage leaves existing active rows
available while new registrations, updates, and deletions remain staged until
the outbox succeeds.
