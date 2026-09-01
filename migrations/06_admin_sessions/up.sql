CREATE TABLE admin_sessions (
    session_hash TEXT PRIMARY KEY NOT NULL,
    password_fingerprint TEXT NOT NULL,
    csrf_token TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX admin_sessions_expiry ON admin_sessions (expires_at);
