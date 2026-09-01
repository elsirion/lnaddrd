CREATE TABLE nostr_event_relays (
    event_id TEXT NOT NULL,
    relay_url TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('acknowledged', 'failed')),
    last_error TEXT,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (event_id, relay_url)
);

CREATE INDEX nostr_event_relays_status
    ON nostr_event_relays (relay_url, status);
