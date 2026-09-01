DROP INDEX payment_addresses_backup_event;
DROP INDEX payment_addresses_address_key;
ALTER TABLE payment_addresses DROP COLUMN backup_event_id;
ALTER TABLE payment_addresses DROP COLUMN address_key;
ALTER TABLE payment_addresses DROP COLUMN revision;
ALTER TABLE payment_addresses DROP COLUMN state;
