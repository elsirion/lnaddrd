use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use nostr_sdk::prelude::{Event, EventBuilder, Kind, Tag, Timestamp};
use serde::{Deserialize, Serialize};
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};
use url::Url;

use crate::{
    config::Config,
    crypto::ServiceKeys,
    domain::Domain,
    nostr::{codec::ServiceConfigurationRecord, publisher::Publisher},
    repository::sqlite::SqlitePaymentAddressRepository,
};

pub const ANNOUNCEMENT_PREFIX: &str = "lnaddrd:service:v1:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAnnouncement {
    pub schema: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    pub origin: String,
    pub domains: Vec<Domain>,
    pub registration_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terms_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub pricing: Vec<DomainPricing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<Software>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainPricing {
    pub domain: Domain,
    pub currency: String,
    pub tiers: Vec<PublicTier>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicTier {
    pub max_length: u16,
    pub price: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Software {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WellKnownAnnouncement {
    pub schema: u16,
    pub service_pubkey: String,
    pub announcement: String,
    pub relays: Vec<String>,
}

pub fn normalized_origin(value: &str) -> Result<String> {
    let url = Url::parse(value).context("Invalid public base URL")?;
    ensure!(url.scheme() == "https", "Public base URL must use HTTPS");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Public base URL cannot contain user information"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "Public base URL cannot contain a query or fragment"
    );
    ensure!(
        url.path() == "/" || url.path().is_empty(),
        "Public base URL cannot contain a path"
    );
    let origin = url.origin().ascii_serialization();
    ensure!(origin != "null", "Public base URL has no tuple origin");
    Ok(origin)
}

pub fn build_event(
    config: &Config,
    service_configuration: &ServiceConfigurationRecord,
    keys: &ServiceKeys,
    now: u64,
) -> Result<Option<Event>> {
    let Some(origin) = config.public_base_url.as_deref() else {
        return Ok(None);
    };
    let origin = normalized_origin(origin)?;
    let mut domains = service_configuration
        .domains
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    domains.sort();
    let pricing = service_configuration
        .domains
        .iter()
        .filter_map(|(domain, config)| {
            config.payment_policy.as_ref().map(|policy| DomainPricing {
                domain: domain.clone(),
                currency: "msat".to_owned(),
                tiers: policy
                    .tiers
                    .iter()
                    .map(|tier| PublicTier {
                        max_length: tier.max_length,
                        price: tier.price_msat,
                    })
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    let has_paid = pricing
        .iter()
        .any(|policy| policy.tiers.iter().any(|tier| tier.price > 0));
    let has_free = service_configuration.domains.values().any(|domain| {
        domain
            .payment_policy
            .as_ref()
            .is_none_or(|policy| policy.tiers.iter().any(|tier| tier.price_msat == 0))
    });
    let mut capabilities = BTreeSet::from([
        "management-token".to_owned(),
        "nostr-recoverable".to_owned(),
    ]);
    if has_free {
        capabilities.insert("free-registration".to_owned());
    }
    if has_paid {
        capabilities.insert("paid-registration".to_owned());
        capabilities.insert("lud21-gate".to_owned());
    }
    let announcement = ServiceAnnouncement {
        schema: 1,
        name: Some(config.service_name.clone()),
        about: None,
        registration_url: format!("{origin}/"),
        terms_url: None,
        contact: None,
        origin: origin.clone(),
        domains,
        capabilities: capabilities.into_iter().collect(),
        pricing,
        software: Some(Software {
            name: "lnaddrd".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }),
        status: None,
        migration_url: None,
    };
    let identifier = format!("{ANNOUNCEMENT_PREFIX}{origin}");
    let expiration = now.saturating_add(30 * 24 * 60 * 60).to_string();
    let mut tags = vec![
        Tag::parse(["d", identifier.as_str()])?,
        Tag::parse(["t", "lightning-address-service"])?,
        Tag::parse(["expiration", expiration.as_str()])?,
    ];
    for relay in &config.nostr_relays {
        tags.push(Tag::parse(["r", relay, "backup"])?);
    }
    Ok(Some(
        EventBuilder::new(
            Kind::ApplicationSpecificData,
            serde_json::to_string(&announcement)?,
        )
        .tags(tags)
        .custom_created_at(Timestamp::from_secs(now))
        .sign_with_keys(keys.signing_keys())?,
    ))
}

pub fn well_known(config: &Config, keys: &ServiceKeys) -> Result<Option<WellKnownAnnouncement>> {
    let Some(origin) = config.public_base_url.as_deref() else {
        return Ok(None);
    };
    let origin = normalized_origin(origin)?;
    let identifier = format!("{ANNOUNCEMENT_PREFIX}{origin}");
    Ok(Some(WellKnownAnnouncement {
        schema: 1,
        service_pubkey: keys.service_public_key().to_string(),
        announcement: format!("30078:{}:{identifier}", keys.service_public_key()),
        relays: config.nostr_relays.clone(),
    }))
}

pub struct AnnouncementWorker {
    config: Config,
    repository: SqlitePaymentAddressRepository,
    keys: Arc<ServiceKeys>,
    publisher: Publisher,
}

impl AnnouncementWorker {
    pub fn new(
        config: Config,
        repository: SqlitePaymentAddressRepository,
        keys: Arc<ServiceKeys>,
        publisher: Publisher,
    ) -> Self {
        Self {
            config,
            repository,
            keys,
            publisher,
        }
    }

    pub async fn run(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(7 * 24 * 60 * 60));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = self.publish().await {
                warn!(%error, "Failed to publish service announcement");
            }
        }
    }

    async fn publish(&self) -> Result<()> {
        let domains = self
            .config
            .domains
            .iter()
            .map(|value| value.parse())
            .collect::<Result<Vec<Domain>>>()?;
        let configuration = self.repository.service_configuration(&domains).await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        if let Some(event) = build_event(&self.config, &configuration, &self.keys, now)? {
            let publication = self.publisher.publish(&event).await?;
            info!(event_id=%event.id, relay_count=publication.accepted_by.len(), "Service announcement published");
        }
        Ok(())
    }
}
