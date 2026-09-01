CREATE TABLE reserved_names (
    domain TEXT NOT NULL,
    username TEXT NOT NULL,
    PRIMARY KEY (domain, username)
);

CREATE TABLE domain_payment_policies (
    domain TEXT PRIMARY KEY NOT NULL,
    destination_json TEXT NOT NULL,
    tiers_json TEXT NOT NULL
);

CREATE TABLE pending_configurations (
    event_id TEXT PRIMARY KEY NOT NULL,
    configuration_json TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
