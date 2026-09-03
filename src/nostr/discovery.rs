use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use nostr_sdk::prelude::{Event, Filter, Kind};
use serde::Serialize;

use crate::{
    nostr::{
        announcement::{
            ANNOUNCEMENT_PREFIX, ServiceAnnouncement, WellKnownAnnouncement, normalized_origin,
        },
        publisher::NostrPublisher,
    },
    outbound::SafeHttpClient,
};

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredService {
    pub pubkey: String,
    pub event_id: String,
    pub created_at: u64,
    pub announcement: ServiceAnnouncement,
    pub verified_domains: Vec<String>,
    pub verification_errors: Vec<String>,
}

pub async fn discover(relays: &[String]) -> Result<Vec<DiscoveredService>> {
    let network = NostrPublisher::connect(relays).await?;
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .hashtag("lightning-address-service");
    let events = network.fetch(filter, Duration::from_secs(15)).await?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut replacements = BTreeMap::<(String, String), Event>::new();
    for event in events.iter() {
        if validate_event(event, now).is_err() {
            continue;
        }
        let Some(identifier) = event.tags.identifier() else {
            continue;
        };
        let key = (event.pubkey.to_string(), identifier.to_owned());
        if replacements
            .get(&key)
            .is_none_or(|current| (event.created_at, event.id) > (current.created_at, current.id))
        {
            replacements.insert(key, event.clone());
        }
    }

    let http = SafeHttpClient;
    let mut result = Vec::new();
    for event in replacements.into_values() {
        let announcement: ServiceAnnouncement = serde_json::from_str(&event.content)?;
        let mut verified_domains = Vec::new();
        let mut verification_errors = Vec::new();
        for domain in &announcement.domains {
            let url = format!("https://{domain}/.well-known/lnaddrd.json");
            match http.get_json::<WellKnownAnnouncement>(&url).await {
                Ok(document) if verify_well_known(&document, &event) => {
                    verified_domains.push(domain.to_string());
                }
                Ok(_) => verification_errors.push(format!("{domain}: identity mismatch")),
                Err(error) => verification_errors.push(format!("{domain}: {error}")),
            }
        }
        result.push(DiscoveredService {
            pubkey: event.pubkey.to_string(),
            event_id: event.id.to_string(),
            created_at: event.created_at.as_secs(),
            announcement,
            verified_domains,
            verification_errors,
        });
    }
    result.sort_by(|left, right| left.announcement.origin.cmp(&right.announcement.origin));
    Ok(result)
}

fn validate_event(event: &Event, now: u64) -> Result<()> {
    event.verify().context("Invalid announcement signature")?;
    ensure!(
        event.kind == Kind::ApplicationSpecificData,
        "Unexpected event kind"
    );
    let identifier = event
        .tags
        .identifier()
        .context("Missing announcement identifier")?;
    let origin = identifier
        .strip_prefix(ANNOUNCEMENT_PREFIX)
        .context("Unexpected identifier")?;
    ensure!(
        normalized_origin(origin)? == origin,
        "Non-canonical origin identifier"
    );
    let announcement: ServiceAnnouncement = serde_json::from_str(&event.content)?;
    ensure!(announcement.schema == 1, "Unsupported announcement schema");
    ensure!(
        announcement.status.as_deref() != Some("retired"),
        "Service is retired"
    );
    ensure!(
        announcement.origin == origin,
        "Origin does not match identifier"
    );
    ensure!(
        !announcement.domains.is_empty(),
        "Announcement has no domains"
    );
    let registration = url::Url::parse(&announcement.registration_url)?;
    ensure!(
        registration.origin().ascii_serialization() == origin,
        "Registration URL has another origin"
    );
    if let Some(terms) = &announcement.terms_url {
        ensure!(
            url::Url::parse(terms)?.scheme() == "https",
            "Terms URL must use HTTPS"
        );
    }
    let mut sorted = announcement.domains.clone();
    sorted.sort();
    sorted.dedup();
    ensure!(
        sorted == announcement.domains,
        "Domains are not sorted and unique"
    );
    for tag in event.tags.iter() {
        let values = tag.as_slice();
        if values.first().map(String::as_str) == Some("expiration") {
            let expiration = values
                .get(1)
                .context("Malformed expiration tag")?
                .parse::<u64>()?;
            ensure!(expiration > now, "Announcement is expired");
        }
    }
    Ok(())
}

fn verify_well_known(document: &WellKnownAnnouncement, event: &Event) -> bool {
    let Some(identifier) = event.tags.identifier() else {
        return false;
    };
    document.schema == 1
        && document.service_pubkey == event.pubkey.to_string()
        && document.announcement == format!("30078:{}:{identifier}", event.pubkey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::codec::{DomainConfigurationRecord, ServiceConfigurationRecord};
    use crate::{config::Config, crypto::RootSecret, nostr::announcement::build_event};
    use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};

    fn fixture() -> (
        Config,
        ServiceConfigurationRecord,
        crate::crypto::ServiceKeys,
    ) {
        let config = Config {
            operation: None,
            domains: vec!["example.com".to_owned()],
            bind: "127.0.0.1:8080".parse::<SocketAddr>().unwrap(),
            database_path: "unused".to_owned(),
            root_secret_file: PathBuf::from("unused"),
            admin_password_file: PathBuf::from("unused"),
            nostr_relays: vec!["wss://relay.example".to_owned()],
            public_base_url: Some("https://example.com".to_owned()),
            service_name: "Example".to_owned(),
            warning: None,
        };
        let configuration = ServiceConfigurationRecord {
            schema: 1,
            revision: 1,
            instance_id: "01".repeat(32),
            domains: BTreeMap::from([(
                "example.com".parse().unwrap(),
                DomainConfigurationRecord::default(),
            )]),
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
    fn validates_matching_announcement_and_well_known_document() {
        let (config, configuration, keys) = fixture();
        let event = build_event(&config, &configuration, &keys, 1_700_000_000)
            .unwrap()
            .unwrap();
        assert!(validate_event(&event, 1_700_000_001).is_ok());
        let document = crate::nostr::announcement::well_known(&config, &keys)
            .unwrap()
            .unwrap();
        assert!(verify_well_known(&document, &event));
    }

    #[test]
    fn announcement_includes_profile_and_new_capabilities() {
        let (config, mut configuration, keys) = fixture();
        configuration.profile = Some(crate::nostr::codec::ServiceProfileRecord {
            about: Some("About us".to_owned()),
            contact: None,
            terms_url: Some("https://example.com/terms".to_owned()),
        });
        let event = build_event(&config, &configuration, &keys, 1_700_000_000)
            .unwrap()
            .unwrap();
        let announcement: ServiceAnnouncement = serde_json::from_str(&event.content).unwrap();
        assert_eq!(announcement.about.as_deref(), Some("About us"));
        assert_eq!(
            announcement.terms_url.as_deref(),
            Some("https://example.com/terms")
        );
        assert!(
            announcement
                .capabilities
                .iter()
                .any(|c| c == "registration-api-v1")
        );
        assert!(announcement.capabilities.iter().any(|c| c == "nostr-auth"));
        assert!(validate_event(&event, 1_700_000_001).is_ok());
    }
}
