CREATE TABLE nostr_outbox (
    event_id TEXT PRIMARY KEY NOT NULL,
    event_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'acknowledged')),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_error TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    acknowledged_at INTEGER
);

CREATE INDEX nostr_outbox_pending
    ON nostr_outbox (status, next_attempt_at, created_at);

CREATE TABLE nostr_sync_state (
    relay_url TEXT PRIMARY KEY NOT NULL,
    last_success_at INTEGER,
    last_error TEXT,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE service_metadata (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
