use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use argon2::{
    Argon2, PasswordHasher,
    password_hash::{SaltString, rand_core::OsRng},
};
use lightning_invoice::Bolt11Invoice;
use rand::distributions::DistString;
use tokio::sync::Mutex;

use crate::{
    crypto::ServiceKeys,
    domain::{Destination, Domain, LightningAddress, Username},
    nostr::{
        codec::{AddressRecord, BackupCodec, RegistrationReceipt, UpdatedBy},
        publisher::Publisher,
    },
    payment::{
        PaymentClient, VerifiableInvoice, policy_fingerprint, policy_price, recipient_fingerprint,
    },
    repository::sqlite::{RegistrationAttempt, SqlitePaymentAddressRepository},
};

const ATTEMPT_TTL_SECONDS: i64 = 15 * 60;
const ATTEMPT_RETENTION_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone)]
pub enum Quote {
    Free,
    Paid(u64),
}

#[derive(Debug, Clone)]
pub struct StartedRegistration {
    pub id: String,
    pub invoice: String,
    pub amount_msat: u64,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub enum RegistrationStatus {
    Pending,
    Publishing,
    Complete {
        address: String,
        management_token: Option<String>,
    },
    Expired,
}

pub struct RegistrationManager {
    repository: SqlitePaymentAddressRepository,
    domains: Vec<Domain>,
    keys: Arc<ServiceKeys>,
    publisher: Publisher,
    payments: PaymentClient,
    mutation_lock: Mutex<()>,
    rate_limits: Mutex<HashMap<(IpAddr, &'static str), VecDeque<std::time::Instant>>>,
}

impl RegistrationManager {
    pub fn new(
        repository: SqlitePaymentAddressRepository,
        domains: &[String],
        keys: Arc<ServiceKeys>,
        publisher: Publisher,
    ) -> Result<Self> {
        Ok(Self {
            repository,
            domains: domains
                .iter()
                .map(|domain| domain.parse())
                .collect::<Result<_>>()?,
            keys,
            publisher,
            payments: PaymentClient::default(),
            mutation_lock: Mutex::new(()),
            rate_limits: Mutex::new(HashMap::new()),
        })
    }

    pub async fn allow_request(&self, ip: IpAddr, action: &'static str, limit: usize) -> bool {
        let now = std::time::Instant::now();
        let mut limits = self.rate_limits.lock().await;
        let attempts = limits.entry((ip, action)).or_default();
        while attempts
            .front()
            .is_some_and(|time| now.duration_since(*time).as_secs() >= 60)
        {
            attempts.pop_front();
        }
        if attempts.len() >= limit {
            return false;
        }
        attempts.push_back(now);
        true
    }

    pub async fn quote(&self, domain: &str, username: &str) -> Result<Quote> {
        self.prune_attempts().await?;
        let domain = domain.parse::<Domain>()?;
        let username = username.parse::<Username>()?;
        ensure!(self.domains.contains(&domain), "Unsupported domain");
        ensure!(
            !self
                .repository
                .address_is_claimed(domain.as_str(), username.as_str())
                .await?,
            "Address is already registered or reserved"
        );
        ensure!(
            !self
                .repository
                .is_reserved(domain.as_str(), username.as_str())
                .await?,
            "Reserved username"
        );
        let configuration = self.repository.service_configuration(&self.domains).await?;
        let Some(policy) = &configuration.domains[&domain].payment_policy else {
            return Ok(Quote::Free);
        };
        let price = policy_price(policy, username.as_str().len())
            .context("Registration is disabled for this username length")?;
        Ok(if price == 0 {
            Quote::Free
        } else {
            Quote::Paid(price)
        })
    }

    pub async fn start(
        &self,
        domain: &str,
        username: &str,
        destination: &str,
    ) -> Result<StartedRegistration> {
        let _guard = self.mutation_lock.lock().await;
        self.prune_attempts().await?;
        let domain = domain.parse::<Domain>()?;
        let username = username.parse::<Username>()?;
        ensure!(self.domains.contains(&domain), "Unsupported domain");
        ensure!(
            !self
                .repository
                .address_is_claimed(domain.as_str(), username.as_str())
                .await?,
            "Address is already registered or reserved"
        );
        ensure!(
            !self
                .repository
                .is_reserved(domain.as_str(), username.as_str())
                .await?,
            "Reserved username"
        );
        let destination = Destination::from_str(destination)?;
        self.payments.resolve(&destination).await?;
        let configuration = self.repository.service_configuration(&self.domains).await?;
        let policy = configuration.domains[&domain]
            .payment_policy
            .as_ref()
            .context("Registration is free; use the normal registration endpoint")?;
        let amount = policy_price(policy, username.as_str().len())
            .context("Registration is disabled for this username length")?;
        ensure!(
            amount > 0,
            "Registration is free; use the normal registration endpoint"
        );
        let recipient = Destination::try_from(policy.destination.clone())?;
        let pay = self.payments.resolve(&recipient).await?;
        let verifiable = self.payments.invoice(&pay, amount).await?;
        let invoice = Bolt11Invoice::from_str(&verifiable.bolt11).map_err(|error| {
            anyhow::anyhow!("Recipient returned an invalid BOLT11 invoice: {error:?}")
        })?;
        ensure!(
            invoice.amount_milli_satoshis() == Some(amount),
            "Recipient returned an invoice for the wrong amount"
        );
        ensure!(
            !invoice.is_expired(),
            "Recipient returned an expired invoice"
        );
        let now = unix_now()?;
        let invoice_ttl = i64::try_from(invoice.duration_until_expiry().as_secs())?;
        let expires_at = now.saturating_add(ATTEMPT_TTL_SECONDS.min(invoice_ttl));
        let id = rand::distributions::Alphanumeric.sample_string(&mut rand::thread_rng(), 32);
        let authentication_token =
            rand::distributions::Alphanumeric.sample_string(&mut rand::thread_rng(), 32);
        let salt = SaltString::generate(&mut OsRng);
        let authentication_token_hash = Argon2::default()
            .hash_password(authentication_token.as_bytes(), &salt)
            .map_err(|error| anyhow::anyhow!("Failed to hash management token: {error}"))?
            .to_string();
        self.repository
            .create_registration_attempt(RegistrationAttempt {
                id: id.clone(),
                domain: domain.to_string(),
                username: username.to_string(),
                destination: destination.to_string(),
                state: "pending_payment".to_owned(),
                amount_msat: i64::try_from(amount)?,
                policy_fingerprint: policy_fingerprint(policy)?,
                recipient_fingerprint: recipient_fingerprint(policy)?,
                bolt11: verifiable.bolt11.clone(),
                payment_hash: invoice.payment_hash().to_string(),
                verify_url: verifiable.verify_url,
                authentication_token,
                authentication_token_hash,
                backup_event_id: None,
                paid_at: None,
                expires_at,
                created_at: now,
                updated_at: now,
            })
            .await?;
        Ok(StartedRegistration {
            id,
            invoice: verifiable.bolt11,
            amount_msat: amount,
            expires_at,
        })
    }

    pub async fn status(&self, id: &str) -> Result<RegistrationStatus> {
        let _guard = self.mutation_lock.lock().await;
        let attempt = self
            .repository
            .registration_attempt(id)
            .await?
            .context("Registration attempt not found")?;
        match attempt.state.as_str() {
            "completed" => {
                let management_token = self.repository.take_registration_token(id).await?;
                return Ok(RegistrationStatus::Complete {
                    address: format!("{}@{}", attempt.username, attempt.domain),
                    management_token,
                });
            }
            "publishing" => return Ok(RegistrationStatus::Publishing),
            "expired" => return Ok(RegistrationStatus::Expired),
            "pending_payment" => {}
            _ => bail!("Unknown registration state"),
        }
        let now = unix_now()?;
        let invoice = VerifiableInvoice {
            bolt11: attempt.bolt11.clone(),
            verify_url: attempt.verify_url.clone(),
        };
        if !self.payments.verify(&invoice).await? {
            let recipient_changed = match self.repository.payment_policy(&attempt.domain).await? {
                Some(policy) => recipient_fingerprint(&policy)? != attempt.recipient_fingerprint,
                None => true,
            };
            if now >= attempt.expires_at || recipient_changed {
                self.repository
                    .update_registration_attempt(id, "expired", None, None)
                    .await?;
                return Ok(RegistrationStatus::Expired);
            }
            return Ok(RegistrationStatus::Pending);
        }
        let parsed = Bolt11Invoice::from_str(&attempt.bolt11)
            .map_err(|error| anyhow::anyhow!("Stored BOLT11 invoice is invalid: {error:?}"))?;
        ensure!(
            parsed.amount_milli_satoshis() == Some(u64::try_from(attempt.amount_msat)?),
            "Stored invoice amount mismatch"
        );
        ensure!(
            parsed.payment_hash().to_string() == attempt.payment_hash,
            "Stored invoice payment hash mismatch"
        );
        let address = LightningAddress {
            username: attempt.username.parse()?,
            domain: attempt.domain.parse()?,
        };
        let destination = Destination::from_str(&attempt.destination)?;
        let record = AddressRecord::active(
            &self.keys,
            address,
            1,
            &destination,
            attempt.authentication_token_hash.clone(),
            u64::try_from(now)?,
            u64::try_from(now)?,
            UpdatedBy::Token,
        )
        .with_registration(RegistrationReceipt {
            price_msat: u64::try_from(attempt.amount_msat)?,
            policy_fingerprint: attempt.policy_fingerprint.clone(),
            payment_hash: attempt.payment_hash.clone(),
            paid_at: u64::try_from(now)?,
        });
        let event = BackupCodec::new(&self.keys).encode_address(&record)?;
        self.repository
            .stage_payment_address(
                &attempt.domain,
                &attempt.username,
                &destination,
                &attempt.authentication_token_hash,
                &record.address_key,
                &event,
                Some(id),
            )
            .await?;
        match self.publisher.publish(&event).await {
            Ok(publication) => {
                self.repository
                    .record_publication(&event.id.to_string(), &publication)
                    .await?;
                self.repository
                    .acknowledge_event(&event.id.to_string())
                    .await?;
                let management_token = self.repository.take_registration_token(id).await?;
                Ok(RegistrationStatus::Complete {
                    address: format!("{}@{}", attempt.username, attempt.domain),
                    management_token,
                })
            }
            Err(error) => {
                self.repository
                    .fail_event(&event.id.to_string(), &error.to_string())
                    .await?;
                Ok(RegistrationStatus::Publishing)
            }
        }
    }

    async fn prune_attempts(&self) -> Result<()> {
        let cutoff = unix_now()?.saturating_sub(ATTEMPT_RETENTION_SECONDS);
        self.repository.prune_registration_attempts(cutoff).await?;
        Ok(())
    }
}

fn unix_now() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}
