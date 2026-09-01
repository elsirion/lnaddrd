ALTER TABLE payment_addresses ADD COLUMN state TEXT NOT NULL DEFAULT 'active'
    CHECK (state IN ('publishing', 'active', 'deleting'));
ALTER TABLE payment_addresses ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE payment_addresses ADD COLUMN address_key TEXT;
ALTER TABLE payment_addresses ADD COLUMN backup_event_id TEXT;

CREATE UNIQUE INDEX payment_addresses_address_key
    ON payment_addresses (address_key)
    WHERE address_key IS NOT NULL;
CREATE INDEX payment_addresses_backup_event
    ON payment_addresses (backup_event_id);
