use std::{
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{ILnaddrService, LnaddrService, RegisterResponse};
use crate::crypto::ServiceKeys;
use crate::domain::{Domain, LightningAddress, Username};
use crate::nostr::{
    codec::{AddressRecord, BackupCodec, UpdatedBy},
    publisher::Publisher,
};
use crate::outbound::SafeHttpClient;
use crate::payment::{DestinationValidator, PaymentClient, policy_price};
use crate::repository::{
    DestinationPaymentAddress, IPaymentAddressRepository, sqlite::SqlitePaymentAddressRepository,
};
use anyhow::{Context, Result, bail, ensure};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use async_trait::async_trait;
use lnurl::{LnUrlResponse, pay::PayResponse};
use rand::distributions::DistString;

pub struct DirectLnaddrService {
    repo: SqlitePaymentAddressRepository,
    domains: Vec<Domain>,
    client: SafeHttpClient,
    keys: Arc<ServiceKeys>,
    publisher: Publisher,
    destination_validator: Arc<dyn DestinationValidator>,
}

impl DirectLnaddrService {
    pub fn new(
        repo: SqlitePaymentAddressRepository,
        domains: Vec<String>,
        keys: Arc<ServiceKeys>,
        publisher: Publisher,
    ) -> Result<Self> {
        let domains = domains
            .into_iter()
            .map(|domain| domain.parse())
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            repo,
            domains,
            client: SafeHttpClient,
            keys,
            publisher,
            destination_validator: Arc::new(PaymentClient::default()),
        })
    }

    pub fn into_dyn(self) -> LnaddrService {
        Arc::new(self)
    }
}

#[async_trait]
impl ILnaddrService for DirectLnaddrService {
    async fn list_domains(&self) -> Result<Vec<String>> {
        Ok(self.domains.iter().map(ToString::to_string).collect())
    }

    async fn get_lnaddr_manifest(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<PayResponse>> {
        let domain = domain.parse::<Domain>()?;
        let username = username.parse::<Username>()?;
        let Some(lnaddr_entry) = self
            .repo
            .get_payment_address(domain.as_str(), username.as_str())
            .await?
        else {
            return Ok(None);
        };

        let destination_url = lnaddr_entry.destination.url();
        let response = match self
            .client
            .get_json(&destination_url)
            .await
            .map_err(|error| {
                tracing::warn!(
                    domain = %domain,
                    username = %username,
                    destination_host = url::Url::parse(&destination_url)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_owned))
                        .unwrap_or_else(|| "invalid".to_owned()),
                    %error,
                    "Failed to fetch backing LNURL manifest"
                );
                error
            })? {
            LnUrlResponse::LnUrlPayResponse(response) => response,
            LnUrlResponse::LnUrlWithdrawResponse(_) => bail!("Invalid LNURL type: LNURLwithdraw"),
            LnUrlResponse::LnUrlChannelResponse(_) => bail!("Invalid LNURL type: LNURLchannel"),
        };

        Ok(Some(response))
    }

    async fn get_destination(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<DestinationPaymentAddress>> {
        let domain = domain.parse::<Domain>()?;
        let username = username.parse::<Username>()?;
        let Some(lnaddr_entry) = self
            .repo
            .get_payment_address(domain.as_str(), username.as_str())
            .await?
        else {
            return Ok(None);
        };

        Ok(Some(lnaddr_entry.destination))
    }

    async fn register_lnaddr(
        &self,
        domain: &str,
        username: &str,
        destination: &str,
    ) -> Result<RegisterResponse> {
        let domain = domain.parse::<Domain>()?;
        let username = username.parse::<Username>()?;
        if !self.domains.contains(&domain) {
            bail!("Unsupported domain: {domain}");
        }
        if self
            .repo
            .is_reserved(domain.as_str(), username.as_str())
            .await?
        {
            bail!("Reserved username: {username}");
        }
        if let Some(policy) = self.repo.payment_policy(domain.as_str()).await? {
            let price = policy_price(&policy, username.as_str().len()).ok_or_else(|| {
                anyhow::anyhow!("Registration is disabled for this username length")
            })?;
            ensure!(price == 0, "Payment required: {price} msat");
        }

        // Test if the lnurl is valid
        let destination = DestinationPaymentAddress::from_str(destination)?;
        self.destination_validator.validate(&destination).await?;

        let authentication_token =
            rand::distributions::Alphanumeric.sample_string(&mut rand::thread_rng(), 32);
        let salt = SaltString::generate(&mut OsRng);
        let token_hash = Argon2::default()
            .hash_password(authentication_token.as_bytes(), &salt)
            .map_err(|error| anyhow::anyhow!("Failed to hash management token: {error}"))?
            .to_string();
        let now = unix_now()?;
        let address = LightningAddress {
            username: username.clone(),
            domain: domain.clone(),
        };
        let record = AddressRecord::active(
            &self.keys,
            address,
            1,
            &destination,
            token_hash.clone(),
            now,
            now,
            UpdatedBy::Token,
        );
        let event = BackupCodec::new(&self.keys).encode_address(&record)?;
        self.repo
            .stage_payment_address(
                domain.as_str(),
                username.as_str(),
                &destination,
                &token_hash,
                &record.address_key,
                &event,
                None,
            )
            .await?;

        let active = match self.publisher.publish(&event).await {
            Ok(publication) => {
                self.repo
                    .record_publication(&event.id.to_string(), &publication)
                    .await?;
                self.repo.acknowledge_event(&event.id.to_string()).await?;
                true
            }
            Err(error) => {
                self.repo
                    .fail_event(&event.id.to_string(), &error.to_string())
                    .await?;
                false
            }
        };

        Ok(RegisterResponse {
            lnaddr: format!("{username}@{domain}"),
            authentication_token,
            active,
        })
    }

    async fn remove_lnaddr(
        &self,
        domain: &str,
        username: &str,
        authentication_token: &str,
    ) -> Result<()> {
        let domain = domain.parse::<Domain>()?;
        let username = username.parse::<Username>()?;
        let managed = self
            .repo
            .get_address_for_management(domain.as_str(), username.as_str())
            .await?
            .ok_or_else(|| anyhow::anyhow!("Address not found"))?;
        let parsed_hash = PasswordHash::new(&managed.authentication_token_hash)
            .map_err(|_| anyhow::anyhow!("Invalid management token"))?;
        Argon2::default()
            .verify_password(authentication_token.as_bytes(), &parsed_hash)
            .map_err(|_| anyhow::anyhow!("Invalid management token"))?;

        let record = AddressRecord::tombstone(
            &self.keys,
            managed.address,
            managed.revision + 1,
            managed.created_at,
            unix_now()?,
            UpdatedBy::Token,
        );
        let event = BackupCodec::new(&self.keys).encode_address(&record)?;
        self.repo
            .stage_deletion(domain.as_str(), username.as_str(), &event)
            .await?;
        match self.publisher.publish(&event).await {
            Ok(publication) => {
                self.repo
                    .record_publication(&event.id.to_string(), &publication)
                    .await?;
                self.repo.acknowledge_event(&event.id.to_string()).await?;
            }
            Err(error) => {
                self.repo
                    .fail_event(&event.id.to_string(), &error.to_string())
                    .await?;
            }
        }
        Ok(())
    }

    async fn update_lnaddr(
        &self,
        domain: &str,
        username: &str,
        destination: &str,
        authentication_token: &str,
    ) -> Result<bool> {
        let domain = domain.parse::<Domain>()?;
        let username = username.parse::<Username>()?;
        let managed = self
            .repo
            .get_address_for_management(domain.as_str(), username.as_str())
            .await?
            .context("Address not found")?;
        ensure!(managed.state == "active", "Address is not active");
        let parsed_hash = PasswordHash::new(&managed.authentication_token_hash)
            .map_err(|_| anyhow::anyhow!("Invalid management token"))?;
        Argon2::default()
            .verify_password(authentication_token.as_bytes(), &parsed_hash)
            .map_err(|_| anyhow::anyhow!("Invalid management token"))?;
        let destination = DestinationPaymentAddress::from_str(destination)?;
        self.destination_validator.validate(&destination).await?;
        let now = unix_now()?;
        let registration = if let Some(event_id) = &managed.backup_event_id {
            match self.repo.backup_event(event_id).await? {
                Some(event) => match BackupCodec::new(&self.keys).decode(&event)? {
                    crate::nostr::codec::DecodedBackup::Address(record) => record.registration,
                    crate::nostr::codec::DecodedBackup::Configuration(_) => {
                        bail!("Address points to a configuration backup")
                    }
                },
                None => bail!("Current address backup event is missing"),
            }
        } else {
            None
        };
        let mut record = AddressRecord::active(
            &self.keys,
            managed.address,
            managed.revision + 1,
            &destination,
            managed.authentication_token_hash,
            managed.created_at,
            now,
            UpdatedBy::Token,
        );
        if let Some(registration) = registration {
            record = record.with_registration(registration);
        }
        let event = BackupCodec::new(&self.keys).encode_address(&record)?;
        self.repo
            .stage_address_update(domain.as_str(), username.as_str(), &destination, &event)
            .await?;
        match self.publisher.publish(&event).await {
            Ok(publication) => {
                self.repo
                    .record_publication(&event.id.to_string(), &publication)
                    .await?;
                self.repo.acknowledge_event(&event.id.to_string()).await?;
                Ok(true)
            }
            Err(error) => {
                self.repo
                    .fail_event(&event.id.to_string(), &error.to_string())
                    .await?;
                Ok(false)
            }
        }
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
    use nostr_sdk::prelude::Event;

    struct FixturePublisher {
        succeeds: bool,
    }

    struct FixtureDestinationValidator;

    #[async_trait]
    impl DestinationValidator for FixtureDestinationValidator {
        async fn validate(&self, _destination: &DestinationPaymentAddress) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl EventPublisher for FixturePublisher {
        async fn publish(&self, _event: &Event) -> Result<Publication> {
            if self.succeeds {
                Ok(Publication {
                    accepted_by: Vec::new(),
                    failed: Vec::new(),
                })
            } else {
                bail!("fixture relay unavailable")
            }
        }
    }

    fn service(succeeds: bool) -> (tempfile::TempDir, DirectLnaddrService) {
        let directory = tempfile::tempdir().unwrap();
        let repository = SqlitePaymentAddressRepository::new(
            directory.path().join("db.sqlite3").to_str().unwrap(),
        )
        .unwrap();
        let keys = Arc::new(RootSecret::from_bytes([0x42; 32]).derive().unwrap());
        let publisher: Publisher = Arc::new(FixturePublisher { succeeds });
        let mut service =
            DirectLnaddrService::new(repository, vec!["example.com".to_owned()], keys, publisher)
                .unwrap();
        service.destination_validator = Arc::new(FixtureDestinationValidator);
        (directory, service)
    }

    #[tokio::test]
    async fn activates_only_after_publication() {
        let (_directory, service) = service(true);
        let response = service
            .register_lnaddr("example.com", "alice", "receiver@example.net")
            .await
            .unwrap();
        assert!(response.active);
        assert!(
            service
                .get_destination("example.com", "alice")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn failed_publication_stays_inactive_and_retryable() {
        let (_directory, service) = service(false);
        let response = service
            .register_lnaddr("example.com", "alice", "receiver@example.net")
            .await
            .unwrap();
        assert!(!response.active);
        assert!(
            service
                .get_destination("example.com", "alice")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(service.repo.pending_event_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn deletion_requires_token_and_tombstone_acknowledgement() {
        let (_directory, service) = service(true);
        let response = service
            .register_lnaddr("example.com", "alice", "receiver@example.net")
            .await
            .unwrap();
        assert!(
            service
                .remove_lnaddr("example.com", "alice", "wrong")
                .await
                .is_err()
        );
        service
            .remove_lnaddr("example.com", "alice", &response.authentication_token)
            .await
            .unwrap();
        assert!(
            service
                .get_destination("example.com", "alice")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn management_token_updates_destination_with_a_backed_revision() {
        let (_directory, service) = service(true);
        let registration = service
            .register_lnaddr("example.com", "alice", "receiver@example.net")
            .await
            .unwrap();
        assert!(
            service
                .update_lnaddr(
                    "example.com",
                    "alice",
                    "updated@example.net",
                    &registration.authentication_token,
                )
                .await
                .unwrap()
        );
        let managed = service
            .repo
            .get_address_for_management("example.com", "alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(managed.destination.to_string(), "updated@example.net");
        assert_eq!(managed.revision, 2);
        assert!(
            service
                .update_lnaddr("example.com", "alice", "other@example.net", "wrong")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_destination_update_keeps_old_destination_resolvable() {
        let (_directory, service) = service(true);
        let registration = service
            .register_lnaddr("example.com", "alice", "receiver@example.net")
            .await
            .unwrap();
        let mut failing = DirectLnaddrService::new(
            service.repo.clone(),
            vec!["example.com".to_owned()],
            service.keys.clone(),
            Arc::new(FixturePublisher { succeeds: false }),
        )
        .unwrap();
        failing.destination_validator = Arc::new(FixtureDestinationValidator);
        assert!(
            !failing
                .update_lnaddr(
                    "example.com",
                    "alice",
                    "updated@example.net",
                    &registration.authentication_token,
                )
                .await
                .unwrap()
        );
        assert_eq!(
            failing
                .get_destination("example.com", "alice")
                .await
                .unwrap()
                .unwrap()
                .to_string(),
            "receiver@example.net"
        );
        assert_eq!(failing.repo.pending_event_count().await.unwrap(), 1);
    }
}
