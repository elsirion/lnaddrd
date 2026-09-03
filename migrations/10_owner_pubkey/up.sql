ALTER TABLE payment_addresses ADD COLUMN owner_pubkey TEXT;
CREATE INDEX payment_addresses_owner ON payment_addresses (owner_pubkey) WHERE owner_pubkey IS NOT NULL;
ALTER TABLE registration_attempts ADD COLUMN owner_pubkey TEXT;
