use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use nostr_sdk::prelude::{Event, EventBuilder, Kind, Tag, Timestamp};
use serde::Serialize;
use tokio_postgres::NoTls;

use crate::{
    crypto::ServiceKeys,
    domain::{Destination, Domain, LightningAddress, Username},
    nostr::{
        codec::{AddressRecord, BackupCodec, UpdatedBy},
        publisher::Publisher,
    },
    repository::sqlite::SqlitePaymentAddressRepository,
};

struct LegacyRow {
    address: LightningAddress,
    destination: Destination,
    token: String,
    created_at: u64,
    updated_at: u64,
}

#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub schema: u16,
    pub imported: usize,
    pub skipped_existing: usize,
    pub skipped_invalid: usize,
    pub superseded_duplicates: usize,
    pub event_ids: Vec<String>,
    pub completed_at: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ImportOptions {
    pub dry_run: bool,
    pub skip_empty_usernames: bool,
    pub prefer_newest_duplicates: bool,
}

pub async fn import(
    database_url: &str,
    repository: &SqlitePaymentAddressRepository,
    keys: Arc<ServiceKeys>,
    publisher: Publisher,
    configured_domains: &[String],
    options: ImportOptions,
) -> Result<Event> {
    ensure!(
        repository.metadata("initialized").await?.as_deref() == Some("true"),
        "Initialize the SQLite service before importing"
    );
    let allowed = configured_domains
        .iter()
        .map(|value| value.parse::<Domain>())
        .collect::<Result<BTreeSet<_>>>()?;
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("Could not connect to legacy PostgreSQL")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(%error, "Legacy PostgreSQL connection failed");
        }
    });
    let rows = client.query(
        "SELECT username, domain, lnurl, authentication_token, EXTRACT(EPOCH FROM created_at)::BIGINT, EXTRACT(EPOCH FROM updated_at)::BIGINT FROM payment_addresses ORDER BY domain, username",
        &[],
    ).await.context("Could not read legacy payment_addresses")?;
    let mut parsed = BTreeMap::<String, LegacyRow>::new();
    let mut skipped_invalid = 0;
    let mut superseded_duplicates = 0;
    for row in rows {
        let raw_username = row.get::<_, String>(0);
        if raw_username.is_empty() && options.skip_empty_usernames {
            skipped_invalid += 1;
            continue;
        }
        let username = raw_username.parse::<Username>()?;
        let domain = row.get::<_, String>(1).parse::<Domain>()?;
        ensure!(
            allowed.contains(&domain),
            "Legacy address uses unconfigured domain: {domain}"
        );
        let address = LightningAddress { username, domain };
        let candidate = LegacyRow {
            address,
            destination: row.get::<_, String>(2).parse()?,
            token: row.get(3),
            created_at: u64::try_from(row.get::<_, i64>(4))?,
            updated_at: u64::try_from(row.get::<_, i64>(5))?,
        };
        let key = candidate.address.to_string();
        if let Some(existing) = parsed.get(&key) {
            ensure!(
                options.prefer_newest_duplicates,
                "Duplicate canonical legacy address: {}",
                candidate.address
            );
            superseded_duplicates += 1;
            if (candidate.updated_at, candidate.created_at)
                <= (existing.updated_at, existing.created_at)
            {
                continue;
            }
        }
        parsed.insert(key, candidate);
    }

    let mut report = ImportReport {
        schema: 1,
        imported: 0,
        skipped_existing: 0,
        skipped_invalid,
        superseded_duplicates,
        event_ids: Vec::new(),
        completed_at: unix_now()?,
    };
    for row in parsed.into_values() {
        if let Some(existing) = repository
            .get_address_for_management(row.address.domain.as_str(), row.address.username.as_str())
            .await?
        {
            ensure!(
                existing.destination.to_string() == row.destination.to_string(),
                "Existing SQLite address has a different destination: {}",
                row.address
            );
            let hash = PasswordHash::new(&existing.authentication_token_hash)
                .map_err(|_| anyhow::anyhow!("Existing management-token hash is invalid"))?;
            ensure!(
                Argon2::default()
                    .verify_password(row.token.as_bytes(), &hash)
                    .is_ok(),
                "Existing SQLite address has a different management token: {}",
                row.address
            );
            if existing.state == "publishing" {
                let event_id = existing
                    .backup_event_id
                    .context("Staged import has no event id")?;
                let event = repository
                    .backup_event(&event_id)
                    .await?
                    .context("Staged import event is missing")?;
                report.event_ids.push(event_id.clone());
                if !options.dry_run {
                    let publication = publisher.publish(&event).await?;
                    repository
                        .record_publication(&event_id, &publication)
                        .await?;
                    repository.acknowledge_event(&event_id).await?;
                }
            }
            report.skipped_existing += 1;
            continue;
        }
        let salt = SaltString::generate(&mut OsRng);
        let token_hash = Argon2::default()
            .hash_password(row.token.as_bytes(), &salt)
            .map_err(|error| anyhow::anyhow!("Failed to hash legacy management token: {error}"))?
            .to_string();
        let record = AddressRecord::active(
            &keys,
            row.address.clone(),
            1,
            &row.destination,
            token_hash.clone(),
            row.created_at,
            row.updated_at.max(row.created_at),
            UpdatedBy::Import,
        );
        let event = BackupCodec::new(&keys).encode_address(&record)?;
        report.event_ids.push(event.id.to_string());
        report.imported += 1;
        if options.dry_run {
            continue;
        }
        repository
            .stage_payment_address(
                row.address.domain.as_str(),
                row.address.username.as_str(),
                &row.destination,
                &token_hash,
                &record.address_key,
                &event,
                None,
            )
            .await?;
        match publisher.publish(&event).await {
            Ok(publication) => {
                repository
                    .record_publication(&event.id.to_string(), &publication)
                    .await?;
                repository.acknowledge_event(&event.id.to_string()).await?;
            }
            Err(error) => {
                repository
                    .fail_event(&event.id.to_string(), &error.to_string())
                    .await?;
                return Err(error).context(format!("Import stopped after staging {}", row.address));
            }
        }
    }
    sign_report(&report, &keys)
}

fn sign_report(report: &ImportReport, keys: &ServiceKeys) -> Result<Event> {
    Ok(EventBuilder::new(
        Kind::ApplicationSpecificData,
        serde_json::to_string(report)?,
    )
    .tags([
        Tag::parse(["d", &format!("lnaddrd:import:v1:{}", report.completed_at)])?,
        Tag::parse(["client", "lnaddrd"])?,
    ])
    .custom_created_at(Timestamp::from_secs(report.completed_at))
    .sign_with_keys(keys.signing_keys())?)
}

fn unix_now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
