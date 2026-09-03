use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use nostr_sdk::{
    nips::nip44,
    prelude::{Event, EventBuilder, Kind, Tag, Timestamp},
};
use serde::{Deserialize, Serialize};

use crate::{
    crypto::ServiceKeys,
    domain::{Destination, Domain, LightningAddress, Username},
};

pub const BACKUP_KIND: Kind = Kind::ApplicationSpecificData;
pub const ADDRESS_D_PREFIX: &str = "lnaddrd:backup:v1:";
pub const CONFIG_D_TAG: &str = "lnaddrd:config:v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressRecordState {
    Active,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatedBy {
    Token,
    Admin,
    Import,
    RestoreRepair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum BackupDestination {
    Lnurl(String),
    LnAddress(String),
}

impl From<&Destination> for BackupDestination {
    fn from(destination: &Destination) -> Self {
        match destination {
            Destination::Lnurl(lnurl) => Self::Lnurl(lnurl.to_string()),
            Destination::LnAddress { address } => Self::LnAddress(address.to_string()),
        }
    }
}

impl TryFrom<BackupDestination> for Destination {
    type Error = anyhow::Error;

    fn try_from(destination: BackupDestination) -> Result<Self> {
        match destination {
            BackupDestination::Lnurl(value) => value.parse(),
            BackupDestination::LnAddress(value) => Ok(Self::LnAddress {
                address: value.parse()?,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagementRecord {
    pub token_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationReceipt {
    pub price_msat: u64,
    pub policy_fingerprint: String,
    pub payment_hash: String,
    pub paid_at: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressRecord {
    pub schema: u16,
    pub address_key: String,
    pub address: LightningAddress,
    pub state: AddressRecordState,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<BackupDestination>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub management: Option<ManagementRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration: Option<RegistrationReceipt>,
    pub created_at: u64,
    pub updated_at: u64,
    pub updated_by: UpdatedBy,
}

impl std::fmt::Debug for AddressRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AddressRecord")
            .field("schema", &self.schema)
            .field("address_key", &self.address_key)
            .field("state", &self.state)
            .field("revision", &self.revision)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("updated_by", &self.updated_by)
            .field("sensitive_fields", &"[REDACTED]")
            .finish()
    }
}

impl AddressRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn active(
        keys: &ServiceKeys,
        address: LightningAddress,
        revision: u64,
        destination: &Destination,
        token_hash: String,
        created_at: u64,
        updated_at: u64,
        updated_by: UpdatedBy,
    ) -> Self {
        Self {
            schema: 1,
            address_key: keys.address_key(&address.to_string()),
            address,
            state: AddressRecordState::Active,
            revision,
            destination: Some(destination.into()),
            management: Some(ManagementRecord { token_hash }),
            registration: None,
            created_at,
            updated_at,
            updated_by,
        }
    }

    pub fn tombstone(
        keys: &ServiceKeys,
        address: LightningAddress,
        revision: u64,
        created_at: u64,
        updated_at: u64,
        updated_by: UpdatedBy,
    ) -> Self {
        Self {
            schema: 1,
            address_key: keys.address_key(&address.to_string()),
            address,
            state: AddressRecordState::Deleted,
            revision,
            destination: None,
            management: None,
            registration: None,
            created_at,
            updated_at,
            updated_by,
        }
    }

    pub fn with_registration(mut self, registration: RegistrationReceipt) -> Self {
        self.registration = Some(registration);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentTierRecord {
    pub max_length: u16,
    pub price_msat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentPolicyRecord {
    pub destination: BackupDestination,
    pub tiers: Vec<PaymentTierRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DomainConfigurationRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_policy: Option<PaymentPolicyRecord>,
    pub reserved_names: Vec<Username>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceProfileRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terms_url: Option<String>,
}

impl ServiceProfileRecord {
    pub fn validate(&self) -> Result<()> {
        if let Some(about) = &self.about {
            ensure!(
                about.chars().count() <= 500,
                "About must be at most 500 characters"
            );
        }
        if let Some(contact) = &self.contact {
            use nostr_sdk::prelude::FromBech32;
            nostr_sdk::prelude::PublicKey::from_bech32(contact)
                .map_err(|_| anyhow::anyhow!("Contact must be a valid npub"))?;
        }
        if let Some(terms_url) = &self.terms_url {
            let url = url::Url::parse(terms_url).context("Invalid terms URL")?;
            ensure!(url.scheme() == "https", "Terms URL must use HTTPS");
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.about.is_none() && self.contact.is_none() && self.terms_url.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceConfigurationRecord {
    pub schema: u16,
    pub revision: u64,
    pub instance_id: String,
    pub domains: BTreeMap<Domain, DomainConfigurationRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ServiceProfileRecord>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedBackup {
    Address(Box<AddressRecord>),
    Configuration(ServiceConfigurationRecord),
}

pub struct BackupCodec<'a> {
    keys: &'a ServiceKeys,
}

impl<'a> BackupCodec<'a> {
    pub fn new(keys: &'a ServiceKeys) -> Self {
        Self { keys }
    }

    pub fn encode_address(&self, record: &AddressRecord) -> Result<Event> {
        self.validate_address(record)?;
        let identifier = format!("{ADDRESS_D_PREFIX}{}", record.address_key);
        self.encode(&identifier, record, record.updated_at)
    }

    pub fn encode_configuration(&self, record: &ServiceConfigurationRecord) -> Result<Event> {
        self.validate_configuration(record)?;
        self.encode(CONFIG_D_TAG, record, record.updated_at)
    }

    pub fn decode(&self, event: &Event) -> Result<DecodedBackup> {
        event
            .verify()
            .context("Invalid Nostr event id or signature")?;
        ensure!(event.kind == BACKUP_KIND, "Unexpected Nostr event kind");
        ensure!(
            event.pubkey == self.keys.service_public_key(),
            "Event author is not this service"
        );

        let identifier = exactly_one_tag(event, "d")?;
        let encryption_recipient = exactly_one_tag(event, "p")?;
        ensure!(
            encryption_recipient == self.keys.encryption_public_key().to_string(),
            "Unexpected encryption recipient"
        );

        let plaintext = nip44::decrypt(
            self.keys.encryption_secret_key(),
            &event.pubkey,
            &event.content,
        )
        .context("Failed to authenticate or decrypt backup")?;

        if identifier == CONFIG_D_TAG {
            let record: ServiceConfigurationRecord =
                serde_json::from_str(&plaintext).context("Invalid configuration plaintext")?;
            self.validate_configuration(&record)?;
            return Ok(DecodedBackup::Configuration(record));
        }

        let address_key = identifier
            .strip_prefix(ADDRESS_D_PREFIX)
            .ok_or_else(|| anyhow::anyhow!("Unknown lnaddrd NIP-78 identifier"))?;
        ensure!(is_lower_hex_32(address_key), "Malformed address-key tag");
        let record: AddressRecord =
            serde_json::from_str(&plaintext).context("Invalid address plaintext")?;
        self.validate_address(&record)?;
        ensure!(
            record.address_key == address_key,
            "Address-key tag mismatch"
        );
        Ok(DecodedBackup::Address(Box::new(record)))
    }

    /// Returns the authenticated plaintext schema without interpreting an
    /// unsupported record. This lets restore retain future records verbatim.
    pub fn plaintext_schema(&self, event: &Event) -> Result<u64> {
        event
            .verify()
            .context("Invalid Nostr event id or signature")?;
        ensure!(event.kind == BACKUP_KIND, "Unexpected Nostr event kind");
        ensure!(
            event.pubkey == self.keys.service_public_key(),
            "Unexpected event author"
        );
        let _identifier = exactly_one_tag(event, "d")?;
        let recipient = exactly_one_tag(event, "p")?;
        ensure!(
            recipient == self.keys.encryption_public_key().to_string(),
            "Unexpected encryption recipient"
        );
        let plaintext = nip44::decrypt(
            self.keys.encryption_secret_key(),
            &event.pubkey,
            &event.content,
        )?;
        serde_json::from_str::<serde_json::Value>(&plaintext)?
            .get("schema")
            .and_then(serde_json::Value::as_u64)
            .context("Backup plaintext has no integer schema")
    }

    fn encode<T: Serialize>(&self, identifier: &str, value: &T, timestamp: u64) -> Result<Event> {
        let plaintext = serde_json::to_string(value).context("Failed to serialize backup")?;
        let ciphertext = nip44::encrypt(
            self.keys.signing_keys().secret_key(),
            &self.keys.encryption_public_key(),
            plaintext,
            nip44::Version::V2,
        )
        .context("Failed to encrypt backup")?;

        EventBuilder::new(BACKUP_KIND, ciphertext)
            .tags([
                Tag::parse(["d", identifier]).context("Invalid d tag")?,
                Tag::parse(["p", &self.keys.encryption_public_key().to_string()])
                    .context("Invalid p tag")?,
                Tag::parse(["client", "lnaddrd"]).context("Invalid client tag")?,
            ])
            .custom_created_at(Timestamp::from_secs(timestamp))
            .sign_with_keys(self.keys.signing_keys())
            .context("Failed to sign backup event")
    }

    fn validate_address(&self, record: &AddressRecord) -> Result<()> {
        ensure!(record.schema == 1, "Unsupported address-record schema");
        ensure!(record.revision > 0, "Revision must be positive");
        ensure!(
            record.created_at <= record.updated_at,
            "Invalid record timestamps"
        );
        ensure!(
            is_lower_hex_32(&record.address_key),
            "Malformed address key"
        );
        ensure!(
            record.address_key == self.keys.address_key(&record.address.to_string()),
            "Address key does not match canonical address"
        );

        match record.state {
            AddressRecordState::Active => {
                ensure!(
                    record.destination.is_some(),
                    "Active record has no destination"
                );
                ensure!(
                    record.management.is_some(),
                    "Active record has no management data"
                );
                if let Some(destination) = record.destination.clone() {
                    Destination::try_from(destination)
                        .context("Active record has an invalid destination")?;
                }
                let management = record.management.as_ref().expect("checked above");
                ensure!(
                    !management.token_hash.is_empty(),
                    "Management token hash is empty"
                );
                if let Some(receipt) = &record.registration {
                    ensure!(
                        is_lower_hex_32(&receipt.policy_fingerprint),
                        "Malformed policy fingerprint"
                    );
                    ensure!(
                        is_lower_hex_32(&receipt.payment_hash),
                        "Malformed payment hash"
                    );
                }
            }
            AddressRecordState::Deleted => {
                ensure!(
                    record.destination.is_none(),
                    "Tombstone contains a destination"
                );
                ensure!(
                    record.management.is_none(),
                    "Tombstone contains management data"
                );
                ensure!(
                    record.registration.is_none(),
                    "Tombstone contains payment data"
                );
            }
        }
        Ok(())
    }

    fn validate_configuration(&self, record: &ServiceConfigurationRecord) -> Result<()> {
        ensure!(record.schema == 1, "Unsupported configuration schema");
        ensure!(
            record.revision > 0,
            "Configuration revision must be positive"
        );
        ensure!(
            is_lower_hex_32(&record.instance_id),
            "Malformed instance id"
        );

        for configuration in record.domains.values() {
            ensure!(
                configuration
                    .reserved_names
                    .windows(2)
                    .all(|pair| pair[0].as_str() < pair[1].as_str()),
                "Reserved names must be sorted and unique"
            );
            if let Some(policy) = &configuration.payment_policy {
                Destination::try_from(policy.destination.clone())
                    .context("Invalid payment destination")?;
                let mut previous_length = 0;
                let mut previous_price = u64::MAX;
                for tier in &policy.tiers {
                    ensure!(tier.max_length <= 64, "Tier length cannot exceed 64");
                    ensure!(
                        tier.max_length > previous_length,
                        "Tier lengths must increase"
                    );
                    ensure!(
                        tier.price_msat <= previous_price,
                        "Tier prices must not increase"
                    );
                    previous_length = tier.max_length;
                    previous_price = tier.price_msat;
                }
            }
        }
        if let Some(profile) = &record.profile {
            profile.validate()?;
        }
        Ok(())
    }
}

fn exactly_one_tag<'a>(event: &'a Event, name: &str) -> Result<&'a str> {
    let mut values = event.tags.iter().filter_map(|tag| {
        let values = tag.as_slice();
        (values.first().map(String::as_str) == Some(name))
            .then(|| values.get(1).map(String::as_str))
            .flatten()
    });
    let value = values
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing {name} tag"))?;
    if values.next().is_some() {
        bail!("Duplicate {name} tag");
    }
    Ok(value)
}

fn is_lower_hex_32(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::RootSecret;

    fn keys() -> ServiceKeys {
        RootSecret::from_bytes([0x42; 32]).derive().unwrap()
    }

    fn active_record(keys: &ServiceKeys) -> AddressRecord {
        let address = "alice@example.com".parse().unwrap();
        let destination = "receiver@example.net".parse().unwrap();
        AddressRecord::active(
            keys,
            address,
            1,
            &destination,
            "$argon2id$example".to_owned(),
            1_700_000_000,
            1_700_000_001,
            UpdatedBy::Token,
        )
    }

    #[test]
    fn active_record_round_trip() {
        let keys = keys();
        let codec = BackupCodec::new(&keys);
        let record = active_record(&keys);
        let event = codec.encode_address(&record).unwrap();

        assert_eq!(
            codec.decode(&event).unwrap(),
            DecodedBackup::Address(Box::new(record))
        );
    }

    #[test]
    fn encrypted_event_does_not_leak_plaintext() {
        let keys = keys();
        let codec = BackupCodec::new(&keys);
        let event = codec.encode_address(&active_record(&keys)).unwrap();
        let serialized = serde_json::to_string(&event).unwrap();

        for secret in ["alice", "example.com", "receiver", "$argon2id$example"] {
            assert!(!serialized.contains(secret), "event leaked {secret}");
        }
    }

    #[test]
    fn tombstone_round_trip() {
        let keys = keys();
        let codec = BackupCodec::new(&keys);
        let record = AddressRecord::tombstone(
            &keys,
            "alice@example.com".parse().unwrap(),
            2,
            1_700_000_000,
            1_700_000_100,
            UpdatedBy::Admin,
        );
        let event = codec.encode_address(&record).unwrap();
        assert_eq!(
            codec.decode(&event).unwrap(),
            DecodedBackup::Address(Box::new(record))
        );
    }

    #[test]
    fn configuration_round_trip() {
        let keys = keys();
        let codec = BackupCodec::new(&keys);
        let record = ServiceConfigurationRecord {
            schema: 1,
            revision: 1,
            instance_id: "01".repeat(32),
            domains: BTreeMap::from([(
                "example.com".parse().unwrap(),
                DomainConfigurationRecord {
                    payment_policy: None,
                    reserved_names: vec!["admin".parse().unwrap(), "www".parse().unwrap()],
                },
            )]),
            profile: None,
            updated_at: 1_700_000_000,
        };
        let event = codec.encode_configuration(&record).unwrap();
        assert_eq!(
            codec.decode(&event).unwrap(),
            DecodedBackup::Configuration(record)
        );
    }

    #[test]
    fn tampering_is_rejected() {
        let keys = keys();
        let codec = BackupCodec::new(&keys);
        let mut event = codec.encode_address(&active_record(&keys)).unwrap();
        event.content.push('x');
        assert!(codec.decode(&event).is_err());
    }

    #[test]
    fn wrong_service_is_rejected() {
        let keys = keys();
        let event = BackupCodec::new(&keys)
            .encode_address(&active_record(&keys))
            .unwrap();
        let other_keys = RootSecret::from_bytes([0x24; 32]).derive().unwrap();
        assert!(BackupCodec::new(&other_keys).decode(&event).is_err());
    }

    #[test]
    fn invalid_state_shape_is_rejected_before_encoding() {
        let keys = keys();
        let codec = BackupCodec::new(&keys);
        let mut record = active_record(&keys);
        record.destination = None;
        assert!(codec.encode_address(&record).is_err());
    }

    #[test]
    fn profile_validation_rules() {
        use super::ServiceProfileRecord;
        let ok = ServiceProfileRecord {
            about: Some("Community operator".to_owned()),
            contact: Some(
                "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq".to_owned(),
            ),
            terms_url: Some("https://example.com/terms".to_owned()),
        };
        // contact above is not a valid bech32 npub; build a real one from a fixed key:
        let keys = nostr_sdk::prelude::Keys::generate();
        let ok = ServiceProfileRecord {
            contact: Some(nostr_sdk::prelude::ToBech32::to_bech32(&keys.public_key()).unwrap()),
            ..ok
        };
        ok.validate().unwrap();
        assert!(
            ServiceProfileRecord {
                about: Some("x".repeat(501)),
                contact: None,
                terms_url: None
            }
            .validate()
            .is_err()
        );
        assert!(
            ServiceProfileRecord {
                about: None,
                contact: Some("not-an-npub".to_owned()),
                terms_url: None
            }
            .validate()
            .is_err()
        );
        assert!(
            ServiceProfileRecord {
                about: None,
                contact: None,
                terms_url: Some("http://insecure.example".to_owned())
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn configuration_record_without_profile_still_decodes() {
        // Old records have no "profile" key; serde default must tolerate that.
        let json = r#"{"schema":1,"revision":1,"instance_id":"00","domains":{},"updated_at":1}"#;
        let record: ServiceConfigurationRecord = serde_json::from_str(json).unwrap();
        assert!(record.profile.is_none());
    }
}
