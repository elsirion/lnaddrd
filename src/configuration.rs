use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, ensure};
use tokio::sync::Mutex;

use crate::{
    crypto::ServiceKeys,
    domain::{Domain, Username},
    nostr::{
        codec::{
            BackupCodec, PaymentPolicyRecord, ServiceConfigurationRecord, ServiceProfileRecord,
        },
        publisher::Publisher,
    },
    repository::sqlite::SqlitePaymentAddressRepository,
};

pub const DEFAULT_RESERVED_NAMES: &[&str] = &[
    "_",
    "admin",
    "administrator",
    "api",
    "help",
    "info",
    "lnurl",
    "root",
    "security",
    "support",
    "www",
];

#[derive(Debug, Clone)]
pub struct ConfigurationUpdate {
    pub revision: u64,
    pub active: bool,
}

pub struct ConfigurationManager {
    repository: SqlitePaymentAddressRepository,
    keys: Arc<ServiceKeys>,
    publisher: Publisher,
    domains: Vec<Domain>,
    mutation_lock: Mutex<()>,
}

impl ConfigurationManager {
    pub fn new(
        repository: SqlitePaymentAddressRepository,
        keys: Arc<ServiceKeys>,
        publisher: Publisher,
        domains: &[String],
    ) -> Result<Self> {
        let domains = domains
            .iter()
            .map(|domain| domain.parse())
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            repository,
            keys,
            publisher,
            domains,
            mutation_lock: Mutex::new(()),
        })
    }

    pub async fn current(&self) -> Result<ServiceConfigurationRecord> {
        self.repository.service_configuration(&self.domains).await
    }

    pub async fn set_reserved(
        &self,
        domain: Domain,
        username: Username,
        reserved: bool,
    ) -> Result<ConfigurationUpdate> {
        ensure!(self.domains.contains(&domain), "Unsupported domain");
        let _guard = self.mutation_lock.lock().await;
        let mut configuration = self.current().await?;
        let domain_configuration = configuration
            .domains
            .get_mut(&domain)
            .expect("configured domain is present");
        if reserved {
            if !domain_configuration.reserved_names.contains(&username) {
                domain_configuration.reserved_names.push(username);
                domain_configuration.reserved_names.sort();
            }
        } else {
            domain_configuration
                .reserved_names
                .retain(|candidate| candidate != &username);
        }
        self.publish(configuration).await
    }

    pub async fn set_payment_policy(
        &self,
        domain: Domain,
        policy: Option<PaymentPolicyRecord>,
    ) -> Result<ConfigurationUpdate> {
        ensure!(self.domains.contains(&domain), "Unsupported domain");
        let _guard = self.mutation_lock.lock().await;
        let mut configuration = self.current().await?;
        configuration
            .domains
            .get_mut(&domain)
            .expect("configured domain is present")
            .payment_policy = policy;
        self.publish(configuration).await
    }

    pub async fn set_profile(
        &self,
        profile: Option<ServiceProfileRecord>,
    ) -> Result<ConfigurationUpdate> {
        let profile = profile.filter(|profile| !profile.is_empty());
        if let Some(profile) = &profile {
            profile.validate()?;
        }
        let _guard = self.mutation_lock.lock().await;
        let mut configuration = self.current().await?;
        configuration.profile = profile;
        self.publish(configuration).await
    }

    async fn publish(
        &self,
        mut configuration: ServiceConfigurationRecord,
    ) -> Result<ConfigurationUpdate> {
        configuration.revision = configuration.revision.saturating_add(1);
        configuration.updated_at = unix_now()?;
        let event = BackupCodec::new(&self.keys).encode_configuration(&configuration)?;
        self.repository
            .stage_configuration(&configuration, &event)
            .await?;
        let active = match self.publisher.publish(&event).await {
            Ok(publication) => {
                self.repository
                    .record_publication(&event.id.to_string(), &publication)
                    .await?;
                self.repository
                    .acknowledge_event(&event.id.to_string())
                    .await?;
                true
            }
            Err(error) => {
                self.repository
                    .fail_event(&event.id.to_string(), &error.to_string())
                    .await?;
                false
            }
        };
        Ok(ConfigurationUpdate {
            revision: configuration.revision,
            active,
        })
    }
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        crypto::RootSecret,
        nostr::publisher::{EventPublisher, Publication},
    };
    use async_trait::async_trait;
    use nostr_sdk::prelude::Event;

    struct FixturePublisher;

    #[async_trait]
    impl EventPublisher for FixturePublisher {
        async fn publish(&self, _event: &Event) -> Result<Publication> {
            Ok(Publication {
                accepted_by: Vec::new(),
                failed: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn reserved_name_becomes_active_only_through_configuration_event() {
        let directory = tempfile::tempdir().unwrap();
        let repository = SqlitePaymentAddressRepository::new(
            directory.path().join("db.sqlite3").to_str().unwrap(),
        )
        .unwrap();
        repository
            .set_metadata("instance_id", &"01".repeat(32))
            .await
            .unwrap();
        repository
            .set_metadata("configuration_revision", "1")
            .await
            .unwrap();
        let keys = Arc::new(RootSecret::from_bytes([0x42; 32]).derive().unwrap());
        let manager = ConfigurationManager::new(
            repository.clone(),
            keys,
            Arc::new(FixturePublisher),
            &["example.com".to_owned()],
        )
        .unwrap();

        let update = manager
            .set_reserved(
                "example.com".parse().unwrap(),
                "admin".parse().unwrap(),
                true,
            )
            .await
            .unwrap();
        assert!(update.active);
        assert_eq!(update.revision, 2);
        assert!(
            repository
                .is_reserved("example.com", "admin")
                .await
                .unwrap()
        );
        assert_eq!(manager.current().await.unwrap().revision, 2);
    }

    #[tokio::test]
    async fn profile_round_trips_through_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let repository = SqlitePaymentAddressRepository::new(
            directory.path().join("db.sqlite3").to_str().unwrap(),
        )
        .unwrap();
        repository
            .set_metadata("instance_id", &"01".repeat(32))
            .await
            .unwrap();
        repository
            .set_metadata("configuration_revision", "1")
            .await
            .unwrap();
        let keys = Arc::new(RootSecret::from_bytes([0x42; 32]).derive().unwrap());
        let manager = ConfigurationManager::new(
            repository.clone(),
            keys,
            Arc::new(FixturePublisher),
            &["example.com".to_owned()],
        )
        .unwrap();

        let profile = ServiceProfileRecord {
            about: Some("Test operator".to_owned()),
            contact: None,
            terms_url: Some("https://example.com/terms".to_owned()),
        };
        let update = manager.set_profile(Some(profile.clone())).await.unwrap();
        assert!(update.active);
        assert_eq!(manager.current().await.unwrap().profile, Some(profile));
        manager.set_profile(None).await.unwrap();
        assert_eq!(manager.current().await.unwrap().profile, None);
    }
}
