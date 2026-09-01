ALTER TABLE payment_addresses ADD COLUMN pending_destination TEXT;
ALTER TABLE payment_addresses ADD COLUMN pending_revision INTEGER;
ALTER TABLE payment_addresses ADD COLUMN pending_backup_event_id TEXT;
