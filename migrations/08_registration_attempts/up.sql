CREATE TABLE registration_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    domain TEXT NOT NULL,
    username TEXT NOT NULL,
    destination TEXT NOT NULL,
    state TEXT NOT NULL,
    amount_msat BIGINT NOT NULL,
    policy_fingerprint TEXT NOT NULL,
    recipient_fingerprint TEXT NOT NULL,
    bolt11 TEXT NOT NULL,
    payment_hash TEXT NOT NULL,
    verify_url TEXT NOT NULL,
    authentication_token TEXT NOT NULL,
    authentication_token_hash TEXT NOT NULL,
    backup_event_id TEXT,
    paid_at BIGINT,
    expires_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE(domain, username)
);

CREATE INDEX registration_attempts_state_idx ON registration_attempts(state, expires_at);
