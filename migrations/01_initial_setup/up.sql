-- payment_addresses table
CREATE TABLE IF NOT EXISTS payment_addresses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL,
    domain TEXT NOT NULL,
    destination TEXT NOT NULL,
    authentication_token TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (username, domain)
);

CREATE INDEX domain_users ON payment_addresses (domain, username);
