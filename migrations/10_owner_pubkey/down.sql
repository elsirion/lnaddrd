DROP INDEX payment_addresses_owner;
ALTER TABLE payment_addresses DROP COLUMN owner_pubkey;
ALTER TABLE registration_attempts DROP COLUMN owner_pubkey;
