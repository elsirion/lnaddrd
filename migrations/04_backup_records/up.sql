CREATE TABLE backup_records (
    coordinate TEXT PRIMARY KEY NOT NULL,
    event_id TEXT NOT NULL,
    event_json TEXT NOT NULL,
    record_type TEXT NOT NULL CHECK (record_type IN ('address', 'configuration')),
    record_state TEXT,
    revision INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX backup_records_event_id ON backup_records (event_id);
