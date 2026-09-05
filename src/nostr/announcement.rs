use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub users: Option<Vec<DomainUsers>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<Software>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainUsers {
    pub domain: String,
    pub count: u64,
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

/// Whether `host` is a public registrable DNS name (see docs/protocol/02).
pub fn is_public_host(host: &str) -> bool {
    const RESERVED_TLDS: [&str; 6] = [
        "localhost",
        "local",
        "internal",
        "test",
        "invalid",
        "example",
    ];
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    for label in &labels {
        let bytes = label.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 63
            || !bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
            || bytes[0] == b'-'
            || bytes[bytes.len() - 1] == b'-'
        {
            return false;
        }
    }
    let tld = labels.last().expect("at least two labels");
    if tld.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    !RESERVED_TLDS.contains(tld)
}

pub fn build_event(
    config: &Config,
    service_configuration: &ServiceConfigurationRecord,
    keys: &ServiceKeys,
    now: u64,
    active_address_counts: &BTreeMap<String, u64>,
) -> Result<Option<Event>> {
    let Some(origin) = config.public_base_url.as_deref() else {
        return Ok(None);
    };
    let origin = normalized_origin(origin)?;
    let origin_host = Url::parse(&origin)?
        .host_str()
        .context("Public base URL has no host")?
        .to_owned();
    if !is_public_host(&origin_host)
        || service_configuration
            .domains
            .keys()
            .any(|domain| !is_public_host(domain.as_str()))
    {
        warn!("Origin or domain is not public, skipping service announcement");
        return Ok(None);
    }
    build_event_from_origin(
        config,
        service_configuration,
        keys,
        now,
        origin,
        active_address_counts,
    )
    .map(Some)
}

/// Constructs an announcement event for an already-normalized origin, without
/// the public-host gate in [`build_event`]. Only used by tests that need to
/// exercise [`crate::nostr::discovery::validate_event`]'s own rejection of a
/// non-public host, since a real publisher can no longer produce one.
#[cfg(test)]
pub(crate) fn build_event_unchecked(
    config: &Config,
    service_configuration: &ServiceConfigurationRecord,
    keys: &ServiceKeys,
    now: u64,
    origin: &str,
    active_address_counts: &BTreeMap<String, u64>,
) -> Result<Event> {
    let origin = normalized_origin(origin)?;
    build_event_from_origin(
        config,
        service_configuration,
        keys,
        now,
        origin,
        active_address_counts,
    )
}

fn build_event_from_origin(
    config: &Config,
    service_configuration: &ServiceConfigurationRecord,
    keys: &ServiceKeys,
    now: u64,
    origin: String,
    active_address_counts: &BTreeMap<String, u64>,
) -> Result<Event> {
    let mut domains = service_configuration
        .domains
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    domains.sort();
    let users = domains
        .iter()
        .map(|domain| DomainUsers {
            domain: domain.as_str().to_owned(),
            count: active_address_counts
                .get(domain.as_str())
                .copied()
                .unwrap_or(0),
        })
        .collect::<Vec<_>>();
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
        "registration-api-v1".to_owned(),
        "nostr-auth".to_owned(),
    ]);
    if has_free {
        capabilities.insert("free-registration".to_owned());
    }
    if has_paid {
        capabilities.insert("paid-registration".to_owned());
        capabilities.insert("lud21-gate".to_owned());
    }
    let profile = service_configuration.profile.clone().unwrap_or(
        crate::nostr::codec::ServiceProfileRecord {
            about: None,
            contact: None,
            terms_url: None,
        },
    );
    let announcement = ServiceAnnouncement {
        schema: 1,
        name: Some(config.service_name.clone()),
        about: profile.about,
        registration_url: format!("{origin}/"),
        terms_url: profile.terms_url,
        contact: profile.contact,
        origin: origin.clone(),
        domains,
        capabilities: capabilities.into_iter().collect(),
        pricing,
        users: if users.is_empty() { None } else { Some(users) },
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
    Ok(EventBuilder::new(
        Kind::ApplicationSpecificData,
        serde_json::to_string(&announcement)?,
    )
    .tags(tags)
    .custom_created_at(Timestamp::from_secs(now))
    .sign_with_keys(keys.signing_keys())?)
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
        let active_address_counts = self.repository.active_address_counts(&domains).await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        if let Some(event) = build_event(
            &self.config,
            &configuration,
            &self.keys,
            now,
            &active_address_counts,
        )? {
            let publication = self.publisher.publish(&event).await?;
            info!(event_id=%event.id, relay_count=publication.accepted_by.len(), "Service announcement published");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        crypto::RootSecret,
        nostr::codec::{DomainConfigurationRecord, ServiceConfigurationRecord},
    };
    use std::{net::SocketAddr, path::PathBuf};

    fn fixture(
        public_base_url: &str,
        domains: &[&str],
    ) -> (Config, ServiceConfigurationRecord, ServiceKeys) {
        let config = Config {
            operation: None,
            domains: domains.iter().map(|domain| domain.to_string()).collect(),
            bind: "127.0.0.1:8080".parse::<SocketAddr>().unwrap(),
            database_path: "unused".to_owned(),
            root_secret_file: PathBuf::from("unused"),
            admin_password_file: PathBuf::from("unused"),
            nostr_relays: vec!["wss://relay.example".to_owned()],
            public_base_url: Some(public_base_url.to_owned()),
            service_name: "Example".to_owned(),
            warning: None,
        };
        let configuration = ServiceConfigurationRecord {
            schema: 1,
            revision: 1,
            instance_id: "01".repeat(32),
            domains: domains
                .iter()
                .map(|domain| {
                    (
                        domain.parse().unwrap(),
                        DomainConfigurationRecord::default(),
                    )
                })
                .collect(),
            profile: None,
            updated_at: 1_700_000_000,
        };
        (
            config,
            configuration,
            RootSecret::from_bytes([0x42; 32]).derive().unwrap(),
        )
    }

    #[test]
    fn is_public_host_accepts_valid_registrable_names() {
        for host in [
            "lnaddr.org",
            "pay.lnaddr.org",
            "foo-bar.io",
            "a.b.co",
            "xn--ls8h.net",
            "svc.example2.com",
        ] {
            assert!(is_public_host(host), "rejected {host}");
        }
    }

    #[test]
    fn is_public_host_rejects_reserved_and_malformed_hosts() {
        for host in [
            "localhost",
            "foo.localhost",
            "mybox.local",
            "svc.internal",
            "demo.test",
            "x.invalid",
            "site.example",
            "1.2.3.4",
            "192.168.0.10",
            "[::1]",
            "::1",
            "lnaddr.org.",
            ".lnaddr.org",
            "foo..bar",
            "UPPER.org",
            "single",
            "-bad.org",
            "bad-.org",
            "foo.123",
        ] {
            assert!(!is_public_host(host), "accepted {host}");
        }
    }

    #[test]
    fn build_event_skips_publication_for_non_public_origin() {
        let (config, configuration, keys) = fixture("https://localhost", &["example.com"]);
        assert!(
            build_event(
                &config,
                &configuration,
                &keys,
                1_700_000_000,
                &BTreeMap::new()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn build_event_skips_publication_for_non_public_domain() {
        let (config, mut configuration, keys) = fixture("https://example.com", &["example.com"]);
        configuration.domains.insert(
            "dev.local".parse().unwrap(),
            DomainConfigurationRecord::default(),
        );
        assert!(
            build_event(
                &config,
                &configuration,
                &keys,
                1_700_000_000,
                &BTreeMap::new()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn build_event_includes_announced_user_counts_per_domain() {
        let (config, configuration, keys) = fixture(
            "https://example.com",
            &["example.com", "second.example.net"],
        );
        let counts = BTreeMap::from([
            ("example.com".to_owned(), 3u64),
            ("second.example.net".to_owned(), 0u64),
        ]);
        let event = build_event(&config, &configuration, &keys, 1_700_000_000, &counts)
            .unwrap()
            .unwrap();
        let announcement: ServiceAnnouncement = serde_json::from_str(&event.content).unwrap();
        let users = announcement.users.expect("users field should be present");
        assert_eq!(users.len(), 2);
        assert!(
            users
                .iter()
                .any(|entry| entry.domain == "example.com" && entry.count == 3)
        );
        assert!(
            users
                .iter()
                .any(|entry| entry.domain == "second.example.net" && entry.count == 0)
        );
    }

    #[test]
    fn build_event_omits_users_field_when_no_domains_are_configured() {
        let (config, mut configuration, keys) = fixture("https://example.com", &["example.com"]);
        configuration.domains.clear();
        let event = build_event(
            &config,
            &configuration,
            &keys,
            1_700_000_000,
            &BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
        let announcement: ServiceAnnouncement = serde_json::from_str(&event.content).unwrap();
        assert!(announcement.users.is_none());
    }
}
