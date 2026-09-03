use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use nostr_sdk::prelude::{Event, Events, Filter};
use rand::{RngCore, rngs::OsRng};
use tracing::{info, warn};

use crate::{
    crypto::ServiceKeys,
    domain::{Destination, Domain},
    nostr::{
        codec::{
            ADDRESS_D_PREFIX, AddressRecord, AddressRecordState, BackupCodec, CONFIG_D_TAG,
            DecodedBackup, DomainConfigurationRecord, ServiceConfigurationRecord,
        },
        publisher::{EventPublisher, NostrPublisher},
    },
    repository::sqlite::{
        NewBackupRecord, RestoredAddress, RestoredConfiguration, SqlitePaymentAddressRepository,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSummary {
    pub active_addresses: usize,
    pub tombstones: usize,
    pub configuration_revision: u64,
}

#[async_trait]
pub trait BackupSource: EventPublisher {
    async fn fetch_backups(&self, filter: Filter) -> Result<Events>;
}

#[async_trait]
impl BackupSource for NostrPublisher {
    async fn fetch_backups(&self, filter: Filter) -> Result<Events> {
        self.fetch(filter, Duration::from_secs(15)).await
    }
}

pub async fn initialize_empty(
    repository: &SqlitePaymentAddressRepository,
    network: &dyn BackupSource,
    keys: &ServiceKeys,
    domains: &[String],
) -> Result<()> {
    ensure!(
        repository.metadata("initialized").await?.is_none(),
        "Database is already initialized"
    );

    let mut instance_id = [0_u8; 32];
    OsRng.fill_bytes(&mut instance_id);
    let domains = domains
        .iter()
        .map(|domain| {
            Ok((
                domain.parse::<Domain>()?,
                DomainConfigurationRecord {
                    payment_policy: None,
                    reserved_names: crate::configuration::DEFAULT_RESERVED_NAMES
                        .iter()
                        .map(|name| name.parse())
                        .collect::<Result<Vec<_>>>()?,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let now = unix_now()?;
    let record = ServiceConfigurationRecord {
        schema: 1,
        revision: 1,
        instance_id: hex::encode(instance_id),
        domains,
        profile: None,
        updated_at: now,
    };
    let event = BackupCodec::new(keys).encode_configuration(&record)?;
    repository.enqueue_event(&event).await?;
    let publication = network.publish(&event).await?;
    repository
        .record_publication(&event.id.to_string(), &publication)
        .await?;
    repository.acknowledge_event(&event.id.to_string()).await?;
    repository
        .store_backup_record(CONFIG_D_TAG, &event, "configuration", None, 1, now)
        .await?;
    repository
        .set_metadata("instance_id", &record.instance_id)
        .await?;
    repository
        .set_metadata("configuration_revision", "1")
        .await?;
    repository.set_metadata("initialized", "true").await?;
    info!(instance_id=%record.instance_id, "Initialized empty Nostr-backed service");
    Ok(())
}

pub async fn restore(
    repository: &SqlitePaymentAddressRepository,
    network: &dyn BackupSource,
    keys: Arc<ServiceKeys>,
    configured_domains: &[String],
    dry_run: bool,
) -> Result<RestoreSummary> {
    ensure!(
        repository.metadata("initialized").await?.is_none(),
        "Refusing to restore over an initialized database"
    );

    let filter = Filter::new()
        .kind(crate::nostr::codec::BACKUP_KIND)
        .author(keys.service_public_key());
    let events = network.fetch_backups(filter).await?;
    ensure!(!events.is_empty(), "No backup events found");

    let codec = BackupCodec::new(&keys);
    let mut configuration: Option<(ServiceConfigurationRecord, Event)> = None;
    let mut addresses: BTreeMap<String, (AddressRecord, Event)> = BTreeMap::new();
    let mut unknown_records = Vec::<(String, Event, u64)>::new();

    for event in events.iter() {
        let Some(coordinate) = event_coordinate(event) else {
            continue;
        };
        if coordinate != CONFIG_D_TAG && !coordinate.starts_with(ADDRESS_D_PREFIX) {
            continue;
        }
        match codec.decode(event) {
            Ok(DecodedBackup::Configuration(record)) => {
                if configuration
                    .as_ref()
                    .is_none_or(|(current, current_event)| {
                        is_newer(
                            record.revision,
                            record.updated_at,
                            event,
                            current.revision,
                            current.updated_at,
                            current_event,
                        )
                    })
                {
                    configuration = Some((record, event.clone()));
                }
            }
            Ok(DecodedBackup::Address(record)) => {
                let replace =
                    addresses
                        .get(&record.address_key)
                        .is_none_or(|(current, current_event)| {
                            is_newer(
                                record.revision,
                                record.updated_at,
                                event,
                                current.revision,
                                current.updated_at,
                                current_event,
                            )
                        });
                if replace {
                    addresses.insert(record.address_key.clone(), (*record, event.clone()));
                }
            }
            Err(error) => match codec.plaintext_schema(event) {
                Ok(schema) if schema != 1 => {
                    unknown_records.push((coordinate.to_owned(), event.clone(), schema));
                    warn!(event_id=%event.id, schema, "Retaining unsupported backup schema without applying it");
                }
                _ => warn!(event_id=%event.id, %error, "Ignoring invalid backup event"),
            },
        }
    }

    let (configuration, configuration_event) =
        configuration.context("Encrypted service configuration was not found")?;
    let allowed_domains = configured_domains
        .iter()
        .map(|domain| domain.parse::<Domain>())
        .collect::<Result<Vec<_>>>()?;
    for domain in configuration.domains.keys() {
        ensure!(
            allowed_domains.contains(domain),
            "Backup contains domain {domain}, which is not configured locally"
        );
    }

    let active_addresses = addresses
        .values()
        .filter(|(record, _)| record.state == AddressRecordState::Active)
        .count();
    let summary = RestoreSummary {
        active_addresses,
        tombstones: addresses.len() - active_addresses,
        configuration_revision: configuration.revision,
    };
    if dry_run {
        return Ok(summary);
    }

    let restored_configuration = RestoredConfiguration {
        instance_id: configuration.instance_id.clone(),
        revision: configuration.revision,
        backup: backup_entry(
            CONFIG_D_TAG,
            &configuration_event,
            "configuration",
            None,
            configuration.revision,
            configuration.updated_at,
        )?,
        configuration,
    };
    let restored_addresses = addresses
        .into_values()
        .map(|(record, event)| {
            let state = match record.state {
                AddressRecordState::Active => "active",
                AddressRecordState::Deleted => "deleted",
            };
            let destination = record
                .destination
                .clone()
                .map(Destination::try_from)
                .transpose()?
                .map(|destination| destination.to_string());
            let token_hash = record
                .management
                .as_ref()
                .map(|value| value.token_hash.clone());
            Ok(RestoredAddress {
                username: record.address.username.to_string(),
                domain: record.address.domain.to_string(),
                destination,
                token_hash,
                owner_pubkey: record.owner_pubkey.clone(),
                state: state.to_owned(),
                revision: record.revision,
                address_key: record.address_key.clone(),
                created_at: record.created_at,
                backup: backup_entry(
                    &format!("{ADDRESS_D_PREFIX}{}", record.address_key),
                    &event,
                    "address",
                    Some(state),
                    record.revision,
                    record.updated_at,
                )?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    repository
        .restore_records(restored_configuration, restored_addresses)
        .await?;
    for (coordinate, event, schema) in unknown_records {
        repository
            .store_backup_record(
                &format!("unknown:v{schema}:{coordinate}:{}", event.id),
                &event,
                "unknown",
                None,
                0,
                event.created_at.as_secs(),
            )
            .await?;
    }
    Ok(summary)
}

fn backup_entry(
    coordinate: &str,
    event: &Event,
    record_type: &str,
    record_state: Option<&str>,
    revision: u64,
    updated_at: u64,
) -> Result<NewBackupRecord> {
    Ok(NewBackupRecord {
        coordinate: coordinate.to_owned(),
        event_id: event.id.to_string(),
        event_json: serde_json::to_string(event)?,
        record_type: record_type.to_owned(),
        record_state: record_state.map(ToOwned::to_owned),
        revision: i64::try_from(revision)?,
        updated_at: i64::try_from(updated_at)?,
    })
}

fn event_coordinate(event: &Event) -> Option<&str> {
    let mut coordinates = event.tags.iter().filter_map(|tag| {
        let values = tag.as_slice();
        (values.first().map(String::as_str) == Some("d"))
            .then(|| values.get(1).map(String::as_str))
            .flatten()
    });
    let coordinate = coordinates.next()?;
    coordinates.next().is_none().then_some(coordinate)
}

fn is_newer(
    revision: u64,
    updated_at: u64,
    event: &Event,
    current_revision: u64,
    current_updated_at: u64,
    current_event: &Event,
) -> bool {
    revision > current_revision
        || (revision == current_revision && updated_at > current_updated_at)
        || (revision == current_revision
            && updated_at == current_updated_at
            && event.id.to_string() < current_event.id.to_string())
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::codec::{AddressRecord, BackupCodec, UpdatedBy};
    use crate::repository::IPaymentAddressRepository;

    struct FixtureSource(Events);

    #[async_trait]
    impl EventPublisher for FixtureSource {
        async fn publish(&self, _event: &Event) -> Result<crate::nostr::publisher::Publication> {
            unreachable!("restore fixture does not publish")
        }
    }

    #[async_trait]
    impl BackupSource for FixtureSource {
        async fn fetch_backups(&self, _filter: Filter) -> Result<Events> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn rebuilds_active_addresses_and_preserves_tombstones() {
        let restored_directory = tempfile::tempdir().unwrap();
        let repository = SqlitePaymentAddressRepository::new(
            restored_directory
                .path()
                .join("restored.sqlite3")
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let keys = Arc::new(
            crate::crypto::RootSecret::from_bytes([0x42; 32])
                .derive()
                .unwrap(),
        );
        let codec = BackupCodec::new(&keys);
        let configuration = ServiceConfigurationRecord {
            schema: 1,
            revision: 1,
            instance_id: "01".repeat(32),
            domains: BTreeMap::from([(
                "example.com".parse().unwrap(),
                DomainConfigurationRecord::default(),
            )]),
            profile: None,
            updated_at: 100,
        };
        let owner_pubkey = "a".repeat(64);
        let active = AddressRecord::active(
            &keys,
            "alice@example.com".parse().unwrap(),
            1,
            &"receiver@example.net".parse().unwrap(),
            "$argon2id$fixture".to_owned(),
            100,
            101,
            UpdatedBy::Token,
        )
        .with_owner(Some(owner_pubkey.clone()));
        let deleted = AddressRecord::tombstone(
            &keys,
            "bob@example.com".parse().unwrap(),
            2,
            100,
            102,
            UpdatedBy::Admin,
        );
        let filter = Filter::new();
        let mut events = Events::new(&filter);
        events.insert(codec.encode_configuration(&configuration).unwrap());
        events.insert(codec.encode_address(&active).unwrap());
        events.insert(codec.encode_address(&deleted).unwrap());
        let source = FixtureSource(events);

        let summary = restore(
            &repository,
            &source,
            keys,
            &["example.com".to_owned()],
            false,
        )
        .await
        .unwrap();
        assert_eq!(summary.active_addresses, 1);
        assert_eq!(summary.tombstones, 1);
        assert!(
            repository
                .get_payment_address("example.com", "alice")
                .await
                .unwrap()
                .is_some()
        );
        let managed = repository
            .get_address_for_management("example.com", "alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(managed.owner_pubkey.as_deref(), Some(owner_pubkey.as_str()));
        assert!(
            repository
                .get_payment_address("example.com", "bob")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repository.metadata("initialized").await.unwrap().as_deref(),
            Some("true")
        );
    }
}
