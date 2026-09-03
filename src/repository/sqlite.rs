use std::{
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use diesel::{
    connection::SimpleConnection,
    prelude::*,
    r2d2::{ConnectionManager, CustomizeConnection, Pool},
};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use nostr_sdk::prelude::Event;
use std::collections::BTreeMap;

use super::{
    DestinationPaymentAddress, IPaymentAddressRepository, PaymentAddress, PaymentAddressRepository,
};

type ConnectionPool = Pool<ConnectionManager<SqliteConnection>>;

#[derive(Debug, Clone)]
pub struct SqlitePaymentAddressRepository {
    pool: ConnectionPool,
}

impl SqlitePaymentAddressRepository {
    pub fn new(database_path: &str) -> Result<Self> {
        let manager = ConnectionManager::<SqliteConnection>::new(database_path);
        let pool = Pool::builder()
            .max_size(8)
            .connection_customizer(Box::new(SqliteConnectionCustomizer))
            .build(manager)?;

        {
            let mut connection = pool.get()?;
            run_migrations(&mut connection)?;
        }

        Ok(Self { pool })
    }

    pub fn into_dyn(self) -> PaymentAddressRepository {
        Arc::new(self)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stage_payment_address(
        &self,
        domain: &str,
        username: &str,
        destination: &DestinationPaymentAddress,
        authentication_token_hash: &str,
        address_key: &str,
        event: &Event,
        registration_attempt_id: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();
        let destination = destination.to_string();
        let authentication_token_hash = authentication_token_hash.to_owned();
        let address_key = address_key.to_owned();
        let event_id = event.id.to_string();
        let event_json = serde_json::to_string(event)?;
        let event_timestamp = event.created_at.as_secs();
        let registration_attempt_id = registration_attempt_id.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                if let Some(attempt_id) = &registration_attempt_id {
                    let changed = diesel::update(
                        registration_attempts::table
                            .filter(registration_attempts::id.eq(attempt_id))
                            .filter(registration_attempts::domain.eq(&domain))
                            .filter(registration_attempts::username.eq(&username))
                            .filter(registration_attempts::state.eq("pending_payment")),
                    )
                    .set((
                        registration_attempts::state.eq("publishing"),
                        registration_attempts::backup_event_id.eq(Some(&event_id)),
                        registration_attempts::paid_at.eq(Some(unix_now()?)),
                        registration_attempts::updated_at.eq(unix_now()?),
                    ))
                    .execute(connection)?;
                    ensure!(changed == 1, "Registration attempt is no longer payable");
                } else {
                    ensure!(
                        registration_attempts::table
                            .filter(registration_attempts::domain.eq(&domain))
                            .filter(registration_attempts::username.eq(&username))
                            .filter(
                                registration_attempts::state
                                    .eq_any(["pending_payment", "publishing"])
                            )
                            .count()
                            .get_result::<i64>(connection)?
                            == 0,
                        "A paid registration reserves this address"
                    );
                }
                diesel::insert_into(payment_addresses::table)
                    .values(NewStagedPaymentAddressEntry {
                        domain: &domain,
                        username: &username,
                        destination: &destination,
                        authentication_token: &authentication_token_hash,
                        state: "publishing",
                        revision: 1,
                        address_key: &address_key,
                        backup_event_id: &event_id,
                    })
                    .execute(connection)?;
                insert_outbox(connection, &event_id, &event_json)?;
                upsert_backup_record(
                    connection,
                    &NewBackupRecord {
                        coordinate: format!(
                            "{}{}",
                            crate::nostr::codec::ADDRESS_D_PREFIX,
                            address_key
                        ),
                        event_id: event_id.clone(),
                        event_json: event_json.clone(),
                        record_type: "address".to_owned(),
                        record_state: Some("active".to_owned()),
                        revision: 1,
                        updated_at: i64::try_from(event_timestamp)?,
                    },
                )?;
                Ok(())
            })
        })
        .await?
    }

    pub async fn stage_deletion(&self, domain: &str, username: &str, event: &Event) -> Result<()> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();
        let event_id = event.id.to_string();
        let event_json = serde_json::to_string(event)?;
        let event_timestamp = event.created_at.as_secs();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                let existing = payment_addresses::table
                    .filter(payment_addresses::domain.eq(&domain))
                    .filter(payment_addresses::username.eq(&username))
                    .filter(payment_addresses::state.eq("active"))
                    .first::<PaymentAddressEntry>(connection)?;
                ensure!(
                    existing._pending_backup_event_id.is_none(),
                    "Address has a pending update"
                );
                let address_key = existing
                    ._address_key
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Address has no backup key"))?;
                let revision = existing.revision.saturating_add(1);
                let changed = diesel::update(
                    payment_addresses::table
                        .filter(payment_addresses::domain.eq(&domain))
                        .filter(payment_addresses::username.eq(&username))
                        .filter(payment_addresses::state.eq("active")),
                )
                .set((
                    payment_addresses::state.eq("deleting"),
                    payment_addresses::revision.eq(revision),
                    payment_addresses::backup_event_id.eq(&event_id),
                    payment_addresses::updated_at.eq(unix_now()?),
                ))
                .execute(connection)?;
                ensure!(changed == 1, "Active address not found");
                insert_outbox(connection, &event_id, &event_json)?;
                upsert_backup_record(
                    connection,
                    &NewBackupRecord {
                        coordinate: format!(
                            "{}{}",
                            crate::nostr::codec::ADDRESS_D_PREFIX,
                            address_key
                        ),
                        event_id: event_id.clone(),
                        event_json: event_json.clone(),
                        record_type: "address".to_owned(),
                        record_state: Some("deleted".to_owned()),
                        revision: i64::from(revision),
                        updated_at: i64::try_from(event_timestamp)?,
                    },
                )?;
                Ok(())
            })
        })
        .await?
    }

    pub async fn stage_address_update(
        &self,
        domain: &str,
        username: &str,
        destination: &DestinationPaymentAddress,
        event: &Event,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();
        let destination = destination.to_string();
        let event_id = event.id.to_string();
        let event_json = serde_json::to_string(event)?;
        let event_timestamp = event.created_at.as_secs();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                let existing = payment_addresses::table
                    .filter(payment_addresses::domain.eq(&domain))
                    .filter(payment_addresses::username.eq(&username))
                    .filter(payment_addresses::state.eq("active"))
                    .first::<PaymentAddressEntry>(connection)?;
                let address_key = existing
                    ._address_key
                    .as_deref()
                    .context("Address has no backup key")?;
                ensure!(
                    existing._pending_backup_event_id.is_none(),
                    "Address already has a pending update"
                );
                let revision = existing.revision.saturating_add(1);
                let changed = diesel::update(
                    payment_addresses::table
                        .filter(payment_addresses::id.eq(existing._id))
                        .filter(payment_addresses::state.eq("active"))
                        .filter(payment_addresses::revision.eq(existing.revision)),
                )
                .set((
                    payment_addresses::pending_destination.eq(Some(destination)),
                    payment_addresses::pending_revision.eq(Some(revision)),
                    payment_addresses::pending_backup_event_id.eq(Some(&event_id)),
                    payment_addresses::updated_at.eq(unix_now()?),
                ))
                .execute(connection)?;
                ensure!(changed == 1, "Address changed concurrently");
                insert_outbox(connection, &event_id, &event_json)?;
                upsert_backup_record(
                    connection,
                    &NewBackupRecord {
                        coordinate: format!(
                            "{}{}",
                            crate::nostr::codec::ADDRESS_D_PREFIX,
                            address_key
                        ),
                        event_id: event_id.clone(),
                        event_json,
                        record_type: "address".to_owned(),
                        record_state: Some("active".to_owned()),
                        revision: i64::from(revision),
                        updated_at: i64::try_from(event_timestamp)?,
                    },
                )?;
                Ok(())
            })
        })
        .await?
    }

    pub async fn get_address_for_management(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<ManagedAddress>> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            payment_addresses::table
                .filter(payment_addresses::domain.eq(domain))
                .filter(payment_addresses::username.eq(username))
                .first::<PaymentAddressEntry>(&mut connection)
                .optional()?
                .map(TryInto::try_into)
                .transpose()
        })
        .await?
    }

    pub async fn enqueue_event(&self, event: &Event) -> Result<()> {
        let pool = self.pool.clone();
        let event_id = event.id.to_string();
        let event_json = serde_json::to_string(event)?;
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            insert_outbox(&mut connection, &event_id, &event_json)
        })
        .await?
    }

    pub async fn pending_events(&self, limit: i64) -> Result<Vec<OutboxEvent>> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let now = unix_now()?;
            nostr_outbox::table
                .filter(nostr_outbox::status.eq("pending"))
                .filter(nostr_outbox::next_attempt_at.le(now))
                .order(nostr_outbox::created_at.asc())
                .limit(limit)
                .load::<OutboxEntry>(&mut connection)?
                .into_iter()
                .map(TryInto::try_into)
                .collect()
        })
        .await?
    }

    pub async fn pending_event_count(&self) -> Result<i64> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            nostr_outbox::table
                .filter(nostr_outbox::status.eq("pending"))
                .count()
                .get_result(&mut connection)
                .map_err(Into::into)
        })
        .await?
    }

    pub async fn admin_addresses(&self) -> Result<Vec<AdminAddressRecord>> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            payment_addresses::table
                .select((
                    payment_addresses::domain,
                    payment_addresses::username,
                    payment_addresses::destination,
                    payment_addresses::state,
                    payment_addresses::revision,
                    payment_addresses::backup_event_id,
                    payment_addresses::updated_at,
                ))
                .order((
                    payment_addresses::domain.asc(),
                    payment_addresses::username.asc(),
                ))
                .load::<(String, String, String, String, i32, Option<String>, i64)>(
                    &mut connection,
                )?
                .into_iter()
                .map(
                    |(
                        domain,
                        username,
                        destination,
                        state,
                        revision,
                        backup_event_id,
                        updated_at,
                    )| {
                        Ok(AdminAddressRecord {
                            domain,
                            username,
                            destination,
                            state,
                            revision: u64::try_from(revision)?,
                            backup_event_id,
                            updated_at,
                        })
                    },
                )
                .collect()
        })
        .await?
    }

    pub async fn admin_addresses_for_domain(
        &self,
        domain: &str,
    ) -> Result<Vec<AdminAddressRecord>> {
        let domain = domain.to_owned();
        Ok(self
            .admin_addresses()
            .await?
            .into_iter()
            .filter(|address| address.domain == domain)
            .collect())
    }

    pub async fn paid_income_by_domain(&self) -> Result<BTreeMap<String, u64>> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            registration_attempts::table
                .filter(registration_attempts::paid_at.is_not_null())
                .select((
                    registration_attempts::domain,
                    registration_attempts::amount_msat,
                ))
                .load::<(String, i64)>(&mut connection)?
                .into_iter()
                .try_fold(BTreeMap::new(), |mut totals, (domain, amount)| {
                    let amount = u64::try_from(amount)?;
                    *totals.entry(domain).or_default() += amount;
                    Ok(totals)
                })
        })
        .await?
    }

    pub async fn relay_replication(&self) -> Result<Vec<RelayReplicationRecord>> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let current = backup_records::table
                .select(backup_records::event_id)
                .load::<String>(&mut connection)?
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            let mut counts = BTreeMap::<String, u64>::new();
            for (event_id, relay_url) in nostr_event_relays::table
                .filter(nostr_event_relays::status.eq("acknowledged"))
                .select((nostr_event_relays::event_id, nostr_event_relays::relay_url))
                .load::<(String, String)>(&mut connection)?
            {
                if current.contains(&event_id) {
                    *counts.entry(relay_url).or_default() += 1;
                }
            }
            Ok(counts
                .into_iter()
                .map(|(relay_url, confirmed_events)| RelayReplicationRecord {
                    relay_url,
                    confirmed_events,
                })
                .collect())
        })
        .await?
    }

    pub async fn relay_health(&self) -> Result<Vec<RelayHealthRecord>> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let records = nostr_sync_state::table
                .order(nostr_sync_state::relay_url.asc())
                .load::<(String, Option<i64>, Option<String>, i64)>(&mut connection)?
                .into_iter()
                .map(
                    |(relay_url, last_success_at, last_error, updated_at)| RelayHealthRecord {
                        relay_url,
                        last_success_at,
                        last_error,
                        updated_at,
                    },
                )
                .collect::<Vec<_>>();
            Ok(records)
        })
        .await?
    }

    pub async fn retry_event_now(&self, event_id: &str) -> Result<()> {
        let pool = self.pool.clone();
        let event_id = event_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let changed =
                diesel::update(nostr_outbox::table.filter(nostr_outbox::event_id.eq(event_id)))
                    .set((
                        nostr_outbox::status.eq("pending"),
                        nostr_outbox::next_attempt_at.eq(unix_now()?),
                        nostr_outbox::last_error.eq::<Option<String>>(None),
                    ))
                    .execute(&mut connection)?;
            ensure!(changed == 1, "Backup event not found in outbox");
            Ok(())
        })
        .await?
    }

    pub async fn current_backup_events(&self) -> Result<Vec<Event>> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            backup_records::table
                .filter(backup_records::record_type.ne("unknown"))
                .select(backup_records::event_json)
                .load::<String>(&mut connection)?
                .into_iter()
                .map(|json| serde_json::from_str(&json).map_err(Into::into))
                .collect()
        })
        .await?
    }

    pub async fn backup_event(&self, event_id: &str) -> Result<Option<Event>> {
        let pool = self.pool.clone();
        let event_id = event_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            backup_records::table
                .filter(backup_records::event_id.eq(event_id))
                .select(backup_records::event_json)
                .first::<String>(&mut connection)
                .optional()?
                .map(|json| serde_json::from_str(&json).map_err(Into::into))
                .transpose()
        })
        .await?
    }

    pub async fn record_publication(
        &self,
        event_id: &str,
        publication: &crate::nostr::publisher::Publication,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let event_id = event_id.to_owned();
        let successes = publication
            .accepted_by
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let failures = publication
            .failed
            .iter()
            .map(|(relay, error)| (relay.to_string(), error.chars().take(1000).collect()))
            .collect::<Vec<(String, String)>>();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                let now = unix_now()?;
                for relay in successes {
                    upsert_relay_health(connection, &relay, Some(now), None, now)?;
                    upsert_event_relay(connection, &event_id, &relay, "acknowledged", None, now)?;
                }
                for (relay, error) in failures {
                    upsert_relay_health(connection, &relay, None, Some(&error), now)?;
                    upsert_event_relay(connection, &event_id, &relay, "failed", Some(&error), now)?;
                }
                Ok(())
            })
        })
        .await?
    }

    pub async fn acknowledge_event(&self, event_id: &str) -> Result<()> {
        let pool = self.pool.clone();
        let event_id = event_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let now = unix_now()?;
            connection.transaction(|connection| {
                let changed = diesel::update(
                    nostr_outbox::table.filter(nostr_outbox::event_id.eq(&event_id)),
                )
                .set((
                    nostr_outbox::status.eq("acknowledged"),
                    nostr_outbox::acknowledged_at.eq(Some(now)),
                    nostr_outbox::last_error.eq::<Option<String>>(None),
                ))
                .execute(connection)?;
                ensure!(changed == 1, "Outbox event not found");

                diesel::update(
                    payment_addresses::table
                        .filter(payment_addresses::backup_event_id.eq(&event_id))
                        .filter(payment_addresses::state.eq("publishing")),
                )
                .set(payment_addresses::state.eq("active"))
                .execute(connection)?;
                if let Some((id, destination, revision)) = payment_addresses::table
                    .filter(payment_addresses::pending_backup_event_id.eq(&event_id))
                    .select((
                        payment_addresses::id,
                        payment_addresses::pending_destination,
                        payment_addresses::pending_revision,
                    ))
                    .first::<(i32, Option<String>, Option<i32>)>(connection)
                    .optional()?
                {
                    diesel::update(payment_addresses::table.filter(payment_addresses::id.eq(id)))
                        .set((
                            payment_addresses::destination
                                .eq(destination.context("Pending update has no destination")?),
                            payment_addresses::revision
                                .eq(revision.context("Pending update has no revision")?),
                            payment_addresses::backup_event_id.eq(Some(&event_id)),
                            payment_addresses::pending_destination.eq::<Option<String>>(None),
                            payment_addresses::pending_revision.eq::<Option<i32>>(None),
                            payment_addresses::pending_backup_event_id.eq::<Option<String>>(None),
                        ))
                        .execute(connection)?;
                }
                diesel::update(
                    registration_attempts::table
                        .filter(registration_attempts::backup_event_id.eq(&event_id))
                        .filter(registration_attempts::state.eq("publishing")),
                )
                .set((
                    registration_attempts::state.eq("completed"),
                    registration_attempts::updated_at.eq(now),
                ))
                .execute(connection)?;
                diesel::delete(
                    payment_addresses::table
                        .filter(payment_addresses::backup_event_id.eq(&event_id))
                        .filter(payment_addresses::state.eq("deleting")),
                )
                .execute(connection)?;

                if let Some(configuration_json) = pending_configurations::table
                    .filter(pending_configurations::event_id.eq(&event_id))
                    .select(pending_configurations::configuration_json)
                    .first::<String>(connection)
                    .optional()?
                {
                    let configuration: crate::nostr::codec::ServiceConfigurationRecord =
                        serde_json::from_str(&configuration_json)?;
                    apply_configuration(connection, &configuration)?;
                    diesel::delete(
                        pending_configurations::table
                            .filter(pending_configurations::event_id.eq(&event_id)),
                    )
                    .execute(connection)?;
                }
                Ok(())
            })
        })
        .await?
    }

    pub async fn fail_event(&self, event_id: &str, error: &str) -> Result<()> {
        let pool = self.pool.clone();
        let event_id = event_id.to_owned();
        let error = error.chars().take(1000).collect::<String>();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let entry = nostr_outbox::table
                .filter(nostr_outbox::event_id.eq(&event_id))
                .first::<OutboxEntry>(&mut connection)?;
            let attempts = entry.attempts.saturating_add(1);
            let exponent = u32::try_from(attempts.clamp(0, 8)).unwrap_or(8);
            let delay = 2_i64.pow(exponent).min(300);
            diesel::update(nostr_outbox::table.filter(nostr_outbox::event_id.eq(event_id)))
                .set((
                    nostr_outbox::attempts.eq(attempts),
                    nostr_outbox::next_attempt_at.eq(unix_now()?.saturating_add(delay)),
                    nostr_outbox::last_error.eq(Some(error)),
                ))
                .execute(&mut connection)?;
            Ok(())
        })
        .await?
    }

    pub async fn metadata(&self, key: &str) -> Result<Option<String>> {
        let pool = self.pool.clone();
        let key = key.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            service_metadata::table
                .filter(service_metadata::key.eq(key))
                .select(service_metadata::value)
                .first::<String>(&mut connection)
                .optional()
                .map_err(Into::into)
        })
        .await?
    }

    pub async fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        let pool = self.pool.clone();
        let key = key.to_owned();
        let value = value.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            diesel::insert_into(service_metadata::table)
                .values((
                    service_metadata::key.eq(&key),
                    service_metadata::value.eq(&value),
                ))
                .on_conflict(service_metadata::key)
                .do_update()
                .set(service_metadata::value.eq(&value))
                .execute(&mut connection)?;
            Ok(())
        })
        .await?
    }

    pub async fn service_configuration(
        &self,
        configured_domains: &[crate::domain::Domain],
    ) -> Result<crate::nostr::codec::ServiceConfigurationRecord> {
        let pool = self.pool.clone();
        let configured_domains = configured_domains.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let instance_id = service_metadata::table
                .filter(service_metadata::key.eq("instance_id"))
                .select(service_metadata::value)
                .first::<String>(&mut connection)?;
            let revision = service_metadata::table
                .filter(service_metadata::key.eq("configuration_revision"))
                .select(service_metadata::value)
                .first::<String>(&mut connection)?
                .parse::<u64>()?;
            let mut domains = BTreeMap::new();
            for domain in configured_domains {
                let names = reserved_names::table
                    .filter(reserved_names::domain.eq(domain.as_str()))
                    .select(reserved_names::username)
                    .order(reserved_names::username.asc())
                    .load::<String>(&mut connection)?
                    .into_iter()
                    .map(|value| value.parse())
                    .collect::<Result<Vec<_>>>()?;
                let policy = domain_payment_policies::table
                    .filter(domain_payment_policies::domain.eq(domain.as_str()))
                    .select((
                        domain_payment_policies::destination_json,
                        domain_payment_policies::tiers_json,
                    ))
                    .first::<(String, String)>(&mut connection)
                    .optional()?
                    .map(|(destination, tiers)| {
                        Ok::<_, anyhow::Error>(crate::nostr::codec::PaymentPolicyRecord {
                            destination: serde_json::from_str(&destination)?,
                            tiers: serde_json::from_str(&tiers)?,
                        })
                    })
                    .transpose()?;
                domains.insert(
                    domain,
                    crate::nostr::codec::DomainConfigurationRecord {
                        payment_policy: policy,
                        reserved_names: names,
                    },
                );
            }
            let profile = service_metadata::table
                .filter(service_metadata::key.eq("service_profile"))
                .select(service_metadata::value)
                .first::<String>(&mut connection)
                .optional()?
                .map(|json| serde_json::from_str(&json))
                .transpose()?;
            Ok(crate::nostr::codec::ServiceConfigurationRecord {
                schema: 1,
                revision,
                instance_id,
                domains,
                profile,
                updated_at: u64::try_from(unix_now()?)?,
            })
        })
        .await?
    }

    pub async fn stage_configuration(
        &self,
        configuration: &crate::nostr::codec::ServiceConfigurationRecord,
        event: &Event,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let configuration_json = serde_json::to_string(configuration)?;
        let event_id = event.id.to_string();
        let event_json = serde_json::to_string(event)?;
        let backup = NewBackupRecord {
            coordinate: crate::nostr::codec::CONFIG_D_TAG.to_owned(),
            event_id: event_id.clone(),
            event_json: event_json.clone(),
            record_type: "configuration".to_owned(),
            record_state: None,
            revision: i64::try_from(configuration.revision)?,
            updated_at: i64::try_from(configuration.updated_at)?,
        };
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                let pending = pending_configurations::table
                    .count()
                    .get_result::<i64>(connection)?;
                ensure!(
                    pending == 0,
                    "A configuration update is already pending publication"
                );
                diesel::insert_into(pending_configurations::table)
                    .values((
                        pending_configurations::event_id.eq(&event_id),
                        pending_configurations::configuration_json.eq(configuration_json),
                    ))
                    .execute(connection)?;
                insert_outbox(connection, &event_id, &event_json)?;
                upsert_backup_record(connection, &backup)?;
                Ok(())
            })
        })
        .await?
    }

    pub async fn is_reserved(&self, domain: &str, username: &str) -> Result<bool> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            Ok(reserved_names::table
                .filter(reserved_names::domain.eq(domain))
                .filter(reserved_names::username.eq(username))
                .count()
                .get_result::<i64>(&mut connection)?
                > 0)
        })
        .await?
    }

    pub async fn create_registration_attempt(&self, attempt: RegistrationAttempt) -> Result<()> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                ensure!(
                    payment_addresses::table
                        .filter(payment_addresses::domain.eq(&attempt.domain))
                        .filter(payment_addresses::username.eq(&attempt.username))
                        .count()
                        .get_result::<i64>(connection)?
                        == 0,
                    "Address is already registered"
                );
                diesel::delete(
                    registration_attempts::table
                        .filter(registration_attempts::domain.eq(&attempt.domain))
                        .filter(registration_attempts::username.eq(&attempt.username))
                        .filter(registration_attempts::state.eq("expired")),
                )
                .execute(connection)?;
                diesel::insert_into(registration_attempts::table)
                    .values(&attempt)
                    .execute(connection)?;
                Ok(())
            })
        })
        .await?
    }

    pub async fn prune_registration_attempts(&self, older_than: i64) -> Result<usize> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            Ok(diesel::delete(
                registration_attempts::table
                    .filter(registration_attempts::updated_at.lt(older_than))
                    .filter(
                        registration_attempts::state
                            .eq("expired")
                            .or(registration_attempts::state
                                .eq("completed")
                                .and(registration_attempts::authentication_token.eq(""))),
                    ),
            )
            .execute(&mut connection)?)
        })
        .await?
    }

    pub async fn payment_policy(
        &self,
        domain: &str,
    ) -> Result<Option<crate::nostr::codec::PaymentPolicyRecord>> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            domain_payment_policies::table
                .filter(domain_payment_policies::domain.eq(domain))
                .select((
                    domain_payment_policies::destination_json,
                    domain_payment_policies::tiers_json,
                ))
                .first::<(String, String)>(&mut connection)
                .optional()?
                .map(|(destination, tiers)| {
                    Ok(crate::nostr::codec::PaymentPolicyRecord {
                        destination: serde_json::from_str(&destination)?,
                        tiers: serde_json::from_str(&tiers)?,
                    })
                })
                .transpose()
        })
        .await?
    }

    pub async fn address_is_claimed(&self, domain: &str, username: &str) -> Result<bool> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let address = payment_addresses::table
                .filter(payment_addresses::domain.eq(&domain))
                .filter(payment_addresses::username.eq(&username))
                .count()
                .get_result::<i64>(&mut connection)?;
            let attempt = registration_attempts::table
                .filter(registration_attempts::domain.eq(domain))
                .filter(registration_attempts::username.eq(username))
                .filter(registration_attempts::state.eq_any([
                    "pending_payment",
                    "publishing",
                    "completed",
                ]))
                .count()
                .get_result::<i64>(&mut connection)?;
            Ok(address > 0 || attempt > 0)
        })
        .await?
    }

    pub async fn registration_attempt(&self, id: &str) -> Result<Option<RegistrationAttempt>> {
        let pool = self.pool.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            registration_attempts::table
                .filter(registration_attempts::id.eq(id))
                .first::<RegistrationAttempt>(&mut connection)
                .optional()
                .map_err(Into::into)
        })
        .await?
    }

    pub async fn update_registration_attempt(
        &self,
        id: &str,
        state: &str,
        backup_event_id: Option<&str>,
        paid_at: Option<i64>,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let id = id.to_owned();
        let state = state.to_owned();
        let backup_event_id = backup_event_id.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let changed = diesel::update(
                registration_attempts::table.filter(registration_attempts::id.eq(id)),
            )
            .set((
                registration_attempts::state.eq(state),
                registration_attempts::backup_event_id.eq(backup_event_id),
                registration_attempts::paid_at.eq(paid_at),
                registration_attempts::updated_at.eq(unix_now()?),
            ))
            .execute(&mut connection)?;
            ensure!(changed == 1, "Registration attempt not found");
            Ok(())
        })
        .await?
    }

    pub async fn take_registration_token(&self, id: &str) -> Result<Option<String>> {
        let pool = self.pool.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                let token = registration_attempts::table
                    .filter(registration_attempts::id.eq(&id))
                    .filter(registration_attempts::state.eq("completed"))
                    .select(registration_attempts::authentication_token)
                    .first::<String>(connection)
                    .optional()?;
                let Some(token) = token.filter(|value| !value.is_empty()) else {
                    return Ok(None);
                };
                diesel::update(
                    registration_attempts::table.filter(registration_attempts::id.eq(id)),
                )
                .set(registration_attempts::authentication_token.eq(""))
                .execute(connection)?;
                Ok(Some(token))
            })
        })
        .await?
    }

    pub async fn create_admin_session(
        &self,
        session_hash: &str,
        password_fingerprint: &str,
        csrf_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let session_hash = session_hash.to_owned();
        let password_fingerprint = password_fingerprint.to_owned();
        let csrf_token = csrf_token.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                diesel::delete(
                    admin_sessions::table.filter(admin_sessions::expires_at.le(unix_now()?)),
                )
                .execute(connection)?;
                diesel::insert_into(admin_sessions::table)
                    .values((
                        admin_sessions::session_hash.eq(session_hash),
                        admin_sessions::password_fingerprint.eq(password_fingerprint),
                        admin_sessions::csrf_token.eq(csrf_token),
                        admin_sessions::expires_at.eq(expires_at),
                    ))
                    .execute(connection)?;
                Ok(())
            })
        })
        .await?
    }

    pub async fn admin_session(
        &self,
        session_hash: &str,
        password_fingerprint: &str,
    ) -> Result<Option<AdminSessionRecord>> {
        let pool = self.pool.clone();
        let session_hash = session_hash.to_owned();
        let password_fingerprint = password_fingerprint.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            admin_sessions::table
                .filter(admin_sessions::session_hash.eq(session_hash))
                .filter(admin_sessions::password_fingerprint.eq(password_fingerprint))
                .filter(admin_sessions::expires_at.gt(unix_now()?))
                .select((admin_sessions::csrf_token, admin_sessions::expires_at))
                .first::<(String, i64)>(&mut connection)
                .optional()
                .map(|record| {
                    record.map(|(csrf_token, expires_at)| AdminSessionRecord {
                        csrf_token,
                        expires_at,
                    })
                })
                .map_err(Into::into)
        })
        .await?
    }

    pub async fn delete_admin_session(&self, session_hash: &str) -> Result<()> {
        let pool = self.pool.clone();
        let session_hash = session_hash.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            diesel::delete(
                admin_sessions::table.filter(admin_sessions::session_hash.eq(session_hash)),
            )
            .execute(&mut connection)?;
            Ok(())
        })
        .await?
    }

    pub async fn store_backup_record(
        &self,
        coordinate: &str,
        event: &Event,
        record_type: &str,
        record_state: Option<&str>,
        revision: u64,
        updated_at: u64,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let entry = NewBackupRecord {
            coordinate: coordinate.to_owned(),
            event_id: event.id.to_string(),
            event_json: serde_json::to_string(event)?,
            record_type: record_type.to_owned(),
            record_state: record_state.map(ToOwned::to_owned),
            revision: i64::try_from(revision)?,
            updated_at: i64::try_from(updated_at)?,
        };
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            upsert_backup_record(&mut connection, &entry)
        })
        .await?
    }

    pub async fn restore_records(
        &self,
        configuration: RestoredConfiguration,
        addresses: Vec<RestoredAddress>,
    ) -> Result<()> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                let initialized = service_metadata::table
                    .filter(service_metadata::key.eq("initialized"))
                    .select(service_metadata::value)
                    .first::<String>(connection)
                    .optional()?;
                ensure!(initialized.is_none(), "Database is already initialized");

                diesel::delete(payment_addresses::table).execute(connection)?;
                diesel::delete(backup_records::table).execute(connection)?;

                upsert_backup_record(connection, &configuration.backup)?;
                apply_configuration(connection, &configuration.configuration)?;
                for restored in addresses {
                    upsert_backup_record(connection, &restored.backup)?;
                    if restored.state == "active" {
                        diesel::insert_into(payment_addresses::table)
                            .values(NewRestoredPaymentAddressEntry {
                                username: &restored.username,
                                domain: &restored.domain,
                                destination: restored.destination.as_deref().ok_or_else(|| {
                                    anyhow::anyhow!("Active restored address has no destination")
                                })?,
                                authentication_token: restored.token_hash.as_deref().ok_or_else(
                                    || anyhow::anyhow!("Active restored address has no token hash"),
                                )?,
                                state: "active",
                                revision: i32::try_from(restored.revision)?,
                                address_key: &restored.address_key,
                                backup_event_id: &restored.backup.event_id,
                                created_at: i64::try_from(restored.created_at)?,
                                updated_at: restored.backup.updated_at,
                            })
                            .execute(connection)?;
                    }
                }

                for (key, value) in [
                    ("initialized", "true"),
                    ("instance_id", configuration.instance_id.as_str()),
                    (
                        "configuration_revision",
                        &configuration.revision.to_string(),
                    ),
                ] {
                    diesel::insert_into(service_metadata::table)
                        .values((
                            service_metadata::key.eq(key),
                            service_metadata::value.eq(value),
                        ))
                        .on_conflict(service_metadata::key)
                        .do_update()
                        .set(service_metadata::value.eq(value))
                        .execute(connection)?;
                }
                Ok(())
            })
        })
        .await?
    }
}

#[derive(Debug, Clone)]
pub struct RestoredConfiguration {
    pub instance_id: String,
    pub revision: u64,
    pub backup: NewBackupRecord,
    pub configuration: crate::nostr::codec::ServiceConfigurationRecord,
}

#[derive(Debug, Clone)]
pub struct RestoredAddress {
    pub username: String,
    pub domain: String,
    pub destination: Option<String>,
    pub token_hash: Option<String>,
    pub state: String,
    pub revision: u64,
    pub address_key: String,
    pub created_at: u64,
    pub backup: NewBackupRecord,
}

#[derive(Debug, Clone)]
pub struct OutboxEvent {
    pub event: Event,
    pub attempts: i32,
}

#[derive(Debug, Clone)]
pub struct ManagedAddress {
    pub address: crate::domain::LightningAddress,
    pub destination: DestinationPaymentAddress,
    pub authentication_token_hash: String,
    pub revision: u64,
    pub created_at: u64,
    pub state: String,
    pub backup_event_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AdminAddressRecord {
    pub domain: String,
    pub username: String,
    pub destination: String,
    pub state: String,
    pub revision: u64,
    pub backup_event_id: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct RelayHealthRecord {
    pub relay_url: String,
    pub last_success_at: Option<i64>,
    pub last_error: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct RelayReplicationRecord {
    pub relay_url: String,
    pub confirmed_events: u64,
}

#[derive(Debug, Clone)]
pub struct AdminSessionRecord {
    pub csrf_token: String,
    pub expires_at: i64,
}

#[derive(Clone, Queryable, Insertable)]
#[diesel(table_name = registration_attempts)]
pub struct RegistrationAttempt {
    pub id: String,
    pub domain: String,
    pub username: String,
    pub destination: String,
    pub state: String,
    pub amount_msat: i64,
    pub policy_fingerprint: String,
    pub recipient_fingerprint: String,
    pub bolt11: String,
    pub payment_hash: String,
    pub verify_url: String,
    pub authentication_token: String,
    pub authentication_token_hash: String,
    pub backup_event_id: Option<String>,
    pub paid_at: Option<i64>,
    pub expires_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl std::fmt::Debug for RegistrationAttempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistrationAttempt")
            .field("id", &self.id)
            .field(
                "address",
                &format_args!("{}@{}", self.username, self.domain),
            )
            .field("state", &self.state)
            .field("amount_msat", &self.amount_msat)
            .field("expires_at", &self.expires_at)
            .field("sensitive_fields", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug)]
struct SqliteConnectionCustomizer;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for SqliteConnectionCustomizer {
    fn on_acquire(
        &self,
        connection: &mut SqliteConnection,
    ) -> std::result::Result<(), diesel::r2d2::Error> {
        connection
            .batch_execute(
                "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA busy_timeout = 5000;",
            )
            .map_err(diesel::r2d2::Error::QueryError)
    }
}

#[async_trait]
impl IPaymentAddressRepository for SqlitePaymentAddressRepository {
    async fn get_payment_address(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<PaymentAddress>> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();

        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            let entry = payment_addresses::table
                .filter(payment_addresses::domain.eq(domain))
                .filter(payment_addresses::username.eq(username))
                .filter(payment_addresses::state.eq_any(["active", "deleting"]))
                .first::<PaymentAddressEntry>(&mut connection)
                .optional()?;
            entry.map(TryInto::try_into).transpose()
        })
        .await?
    }

    async fn add_payment_address(
        &self,
        domain: &str,
        username: &str,
        destination: DestinationPaymentAddress,
        authentication_token: &str,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();
        let destination = destination.to_string();
        let authentication_token = authentication_token.to_owned();

        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            diesel::insert_into(payment_addresses::table)
                .values(NewPaymentAddressEntry {
                    domain: &domain,
                    username: &username,
                    destination: &destination,
                    authentication_token: &authentication_token,
                })
                .execute(&mut connection)?;
            Ok(())
        })
        .await?
    }

    async fn remove_payment_address(
        &self,
        domain: &str,
        username: &str,
        token: &str,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let domain = domain.to_owned();
        let username = username.to_owned();
        let token = token.to_owned();

        tokio::task::spawn_blocking(move || {
            let mut connection = pool.get()?;
            connection.transaction(|connection| {
                let entry = payment_addresses::table
                    .filter(payment_addresses::domain.eq(&domain))
                    .filter(payment_addresses::username.eq(&username))
                    .first::<PaymentAddressEntry>(connection)
                    .optional()?;

                let Some(entry) = entry else {
                    return Ok(());
                };
                if entry.authentication_token != token {
                    bail!("Invalid authentication token for payment address {username}@{domain}");
                }

                diesel::delete(
                    payment_addresses::table
                        .filter(payment_addresses::domain.eq(&domain))
                        .filter(payment_addresses::username.eq(&username)),
                )
                .execute(connection)?;
                Ok(())
            })
        })
        .await?
    }
}

diesel::table! {
    registration_attempts (id) {
        id -> Text,
        domain -> Text,
        username -> Text,
        destination -> Text,
        state -> Text,
        amount_msat -> BigInt,
        policy_fingerprint -> Text,
        recipient_fingerprint -> Text,
        bolt11 -> Text,
        payment_hash -> Text,
        verify_url -> Text,
        authentication_token -> Text,
        authentication_token_hash -> Text,
        backup_event_id -> Nullable<Text>,
        paid_at -> Nullable<BigInt>,
        expires_at -> BigInt,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    payment_addresses (id) {
        id -> Integer,
        username -> Text,
        domain -> Text,
        destination -> Text,
        authentication_token -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
        state -> Text,
        revision -> Integer,
        address_key -> Nullable<Text>,
        backup_event_id -> Nullable<Text>,
        pending_destination -> Nullable<Text>,
        pending_revision -> Nullable<Integer>,
        pending_backup_event_id -> Nullable<Text>,
    }
}

diesel::table! {
    admin_sessions (session_hash) {
        session_hash -> Text,
        password_fingerprint -> Text,
        csrf_token -> Text,
        expires_at -> BigInt,
        created_at -> BigInt,
    }
}

diesel::table! {
    reserved_names (domain, username) {
        domain -> Text,
        username -> Text,
    }
}

diesel::table! {
    domain_payment_policies (domain) {
        domain -> Text,
        destination_json -> Text,
        tiers_json -> Text,
    }
}

diesel::table! {
    pending_configurations (event_id) {
        event_id -> Text,
        configuration_json -> Text,
        created_at -> BigInt,
    }
}

diesel::table! {
    service_metadata (key) {
        key -> Text,
        value -> Text,
    }
}

diesel::table! {
    backup_records (coordinate) {
        coordinate -> Text,
        event_id -> Text,
        event_json -> Text,
        record_type -> Text,
        record_state -> Nullable<Text>,
        revision -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    nostr_outbox (event_id) {
        event_id -> Text,
        event_json -> Text,
        status -> Text,
        attempts -> Integer,
        next_attempt_at -> BigInt,
        last_error -> Nullable<Text>,
        created_at -> BigInt,
        acknowledged_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    nostr_sync_state (relay_url) {
        relay_url -> Text,
        last_success_at -> Nullable<BigInt>,
        last_error -> Nullable<Text>,
        updated_at -> BigInt,
    }
}

diesel::table! {
    nostr_event_relays (event_id, relay_url) {
        event_id -> Text,
        relay_url -> Text,
        status -> Text,
        last_error -> Nullable<Text>,
        updated_at -> BigInt,
    }
}

#[derive(Queryable)]
struct PaymentAddressEntry {
    _id: i32,
    username: String,
    domain: String,
    destination: String,
    authentication_token: String,
    created_at: i64,
    updated_at: i64,
    _state: String,
    revision: i32,
    _address_key: Option<String>,
    _backup_event_id: Option<String>,
    _pending_destination: Option<String>,
    _pending_revision: Option<i32>,
    _pending_backup_event_id: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = payment_addresses)]
struct NewPaymentAddressEntry<'a> {
    username: &'a str,
    domain: &'a str,
    destination: &'a str,
    authentication_token: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = payment_addresses)]
struct NewStagedPaymentAddressEntry<'a> {
    username: &'a str,
    domain: &'a str,
    destination: &'a str,
    authentication_token: &'a str,
    state: &'a str,
    revision: i32,
    address_key: &'a str,
    backup_event_id: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = payment_addresses)]
struct NewRestoredPaymentAddressEntry<'a> {
    username: &'a str,
    domain: &'a str,
    destination: &'a str,
    authentication_token: &'a str,
    created_at: i64,
    updated_at: i64,
    state: &'a str,
    revision: i32,
    address_key: &'a str,
    backup_event_id: &'a str,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = backup_records)]
pub struct NewBackupRecord {
    pub coordinate: String,
    pub event_id: String,
    pub event_json: String,
    pub record_type: String,
    pub record_state: Option<String>,
    pub revision: i64,
    pub updated_at: i64,
}

#[derive(Queryable)]
struct OutboxEntry {
    _event_id: String,
    event_json: String,
    _status: String,
    attempts: i32,
    _next_attempt_at: i64,
    _last_error: Option<String>,
    _created_at: i64,
    _acknowledged_at: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = nostr_outbox)]
struct NewOutboxEntry<'a> {
    event_id: &'a str,
    event_json: &'a str,
    status: &'a str,
}

impl TryFrom<OutboxEntry> for OutboxEvent {
    type Error = anyhow::Error;

    fn try_from(entry: OutboxEntry) -> Result<Self> {
        Ok(Self {
            event: serde_json::from_str(&entry.event_json).context("Invalid outbox event JSON")?,
            attempts: entry.attempts,
        })
    }
}

impl TryFrom<PaymentAddressEntry> for PaymentAddress {
    type Error = anyhow::Error;

    fn try_from(entry: PaymentAddressEntry) -> Result<Self> {
        Ok(Self {
            username: entry.username,
            domain: entry.domain,
            destination: DestinationPaymentAddress::from_str(&entry.destination)?,
            authentication_token: entry.authentication_token,
            created_at: unix_time(entry.created_at)?,
            updated_at: unix_time(entry.updated_at)?,
        })
    }
}

impl TryFrom<PaymentAddressEntry> for ManagedAddress {
    type Error = anyhow::Error;

    fn try_from(entry: PaymentAddressEntry) -> Result<Self> {
        Ok(Self {
            address: format!("{}@{}", entry.username, entry.domain).parse()?,
            destination: DestinationPaymentAddress::from_str(&entry.destination)?,
            authentication_token_hash: entry.authentication_token,
            revision: u64::try_from(entry.revision)?,
            created_at: u64::try_from(entry.created_at)?,
            state: entry._state,
            backup_event_id: entry._backup_event_id,
        })
    }
}

fn insert_outbox(
    connection: &mut SqliteConnection,
    event_id: &str,
    event_json: &str,
) -> Result<()> {
    diesel::insert_into(nostr_outbox::table)
        .values(NewOutboxEntry {
            event_id,
            event_json,
            status: "pending",
        })
        .on_conflict(nostr_outbox::event_id)
        .do_nothing()
        .execute(connection)?;
    Ok(())
}

fn upsert_backup_record(connection: &mut SqliteConnection, entry: &NewBackupRecord) -> Result<()> {
    diesel::insert_into(backup_records::table)
        .values(entry)
        .on_conflict(backup_records::coordinate)
        .do_update()
        .set((
            backup_records::event_id.eq(&entry.event_id),
            backup_records::event_json.eq(&entry.event_json),
            backup_records::record_type.eq(&entry.record_type),
            backup_records::record_state.eq(&entry.record_state),
            backup_records::revision.eq(entry.revision),
            backup_records::updated_at.eq(entry.updated_at),
        ))
        .execute(connection)?;
    Ok(())
}

fn apply_configuration(
    connection: &mut SqliteConnection,
    configuration: &crate::nostr::codec::ServiceConfigurationRecord,
) -> Result<()> {
    diesel::delete(reserved_names::table).execute(connection)?;
    diesel::delete(domain_payment_policies::table).execute(connection)?;
    for (domain, domain_configuration) in &configuration.domains {
        for username in &domain_configuration.reserved_names {
            diesel::insert_into(reserved_names::table)
                .values((
                    reserved_names::domain.eq(domain.as_str()),
                    reserved_names::username.eq(username.as_str()),
                ))
                .execute(connection)?;
        }
        if let Some(policy) = &domain_configuration.payment_policy {
            diesel::insert_into(domain_payment_policies::table)
                .values((
                    domain_payment_policies::domain.eq(domain.as_str()),
                    domain_payment_policies::destination_json
                        .eq(serde_json::to_string(&policy.destination)?),
                    domain_payment_policies::tiers_json.eq(serde_json::to_string(&policy.tiers)?),
                ))
                .execute(connection)?;
        }
    }
    diesel::insert_into(service_metadata::table)
        .values((
            service_metadata::key.eq("configuration_revision"),
            service_metadata::value.eq(configuration.revision.to_string()),
        ))
        .on_conflict(service_metadata::key)
        .do_update()
        .set(service_metadata::value.eq(configuration.revision.to_string()))
        .execute(connection)?;
    match &configuration.profile {
        Some(profile) => {
            let json = serde_json::to_string(profile)?;
            diesel::insert_into(service_metadata::table)
                .values((
                    service_metadata::key.eq("service_profile"),
                    service_metadata::value.eq(&json),
                ))
                .on_conflict(service_metadata::key)
                .do_update()
                .set(service_metadata::value.eq(&json))
                .execute(connection)?;
        }
        None => {
            diesel::delete(
                service_metadata::table.filter(service_metadata::key.eq("service_profile")),
            )
            .execute(connection)?;
        }
    }
    Ok(())
}

fn upsert_relay_health(
    connection: &mut SqliteConnection,
    relay_url: &str,
    last_success_at: Option<i64>,
    last_error: Option<&str>,
    updated_at: i64,
) -> Result<()> {
    diesel::insert_into(nostr_sync_state::table)
        .values((
            nostr_sync_state::relay_url.eq(relay_url),
            nostr_sync_state::last_success_at.eq(last_success_at),
            nostr_sync_state::last_error.eq(last_error),
            nostr_sync_state::updated_at.eq(updated_at),
        ))
        .on_conflict(nostr_sync_state::relay_url)
        .do_update()
        .set((
            nostr_sync_state::last_success_at.eq(last_success_at),
            nostr_sync_state::last_error.eq(last_error),
            nostr_sync_state::updated_at.eq(updated_at),
        ))
        .execute(connection)?;
    Ok(())
}

fn upsert_event_relay(
    connection: &mut SqliteConnection,
    event_id: &str,
    relay_url: &str,
    status: &str,
    last_error: Option<&str>,
    updated_at: i64,
) -> Result<()> {
    diesel::insert_into(nostr_event_relays::table)
        .values((
            nostr_event_relays::event_id.eq(event_id),
            nostr_event_relays::relay_url.eq(relay_url),
            nostr_event_relays::status.eq(status),
            nostr_event_relays::last_error.eq(last_error),
            nostr_event_relays::updated_at.eq(updated_at),
        ))
        .on_conflict((nostr_event_relays::event_id, nostr_event_relays::relay_url))
        .do_update()
        .set((
            nostr_event_relays::status.eq(status),
            nostr_event_relays::last_error.eq(last_error),
            nostr_event_relays::updated_at.eq(updated_at),
        ))
        .execute(connection)?;
    Ok(())
}

fn unix_time(seconds: i64) -> Result<SystemTime> {
    let seconds = u64::try_from(seconds).map_err(|_| anyhow::anyhow!("Negative timestamp"))?;
    Ok(UNIX_EPOCH + Duration::from_secs(seconds))
}

fn unix_now() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

fn run_migrations(connection: &mut SqliteConnection) -> Result<()> {
    connection
        .run_pending_migrations(MIGRATIONS)
        .map_err(|error| anyhow::anyhow!("Failed to run migrations: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn repository() -> (TempDir, SqlitePaymentAddressRepository) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lnaddrd.sqlite3");
        let repository = SqlitePaymentAddressRepository::new(path.to_str().unwrap()).unwrap();
        (directory, repository)
    }

    #[tokio::test]
    async fn persists_an_address() {
        let (_directory, repository) = repository();
        repository
            .add_payment_address(
                "example.com",
                "alice",
                "receiver@example.net".parse().unwrap(),
                "secret",
            )
            .await
            .unwrap();

        let address = repository
            .get_payment_address("example.com", "alice")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(address.destination.to_string(), "receiver@example.net");
    }

    #[tokio::test]
    async fn unique_constraint_prevents_duplicate_claims() {
        let (_directory, repository) = repository();
        let destination: DestinationPaymentAddress = "receiver@example.net".parse().unwrap();
        repository
            .add_payment_address("example.com", "alice", destination.clone(), "one")
            .await
            .unwrap();
        assert!(
            repository
                .add_payment_address("example.com", "alice", destination, "two")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn admin_stats_are_scoped_to_paid_and_current_records() {
        use crate::{
            crypto::RootSecret,
            nostr::{
                codec::{AddressRecord, BackupCodec, UpdatedBy},
                publisher::Publication,
            },
        };

        let (_directory, repository) = repository();
        repository
            .add_payment_address(
                "example.com",
                "alice",
                "receiver@example.net".parse().unwrap(),
                "secret",
            )
            .await
            .unwrap();
        assert_eq!(
            repository
                .admin_addresses_for_domain("example.com")
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            repository
                .admin_addresses_for_domain("other.example")
                .await
                .unwrap()
                .is_empty()
        );

        repository
            .create_registration_attempt(RegistrationAttempt {
                id: "paid".to_owned(),
                domain: "example.com".to_owned(),
                username: "bob".to_owned(),
                destination: "receiver@example.net".to_owned(),
                state: "completed".to_owned(),
                amount_msat: 12_000,
                policy_fingerprint: "policy".to_owned(),
                recipient_fingerprint: "recipient".to_owned(),
                bolt11: "invoice".to_owned(),
                payment_hash: "hash".to_owned(),
                verify_url: "https://example.net/verify".to_owned(),
                authentication_token: "token".to_owned(),
                authentication_token_hash: "token-hash".to_owned(),
                backup_event_id: None,
                paid_at: Some(1_700_000_000),
                expires_at: 1_700_000_100,
                created_at: 1_700_000_000,
                updated_at: 1_700_000_000,
            })
            .await
            .unwrap();
        assert_eq!(
            repository
                .paid_income_by_domain()
                .await
                .unwrap()
                .get("example.com"),
            Some(&12_000)
        );

        let keys = RootSecret::from_bytes([0x24; 32]).derive().unwrap();
        let destination = "receiver@example.net".parse().unwrap();
        let record = AddressRecord::active(
            &keys,
            "alice@example.com".parse().unwrap(),
            1,
            &destination,
            "$argon2id$example".to_owned(),
            1_700_000_000,
            1_700_000_001,
            UpdatedBy::Token,
        );
        let event = BackupCodec::new(&keys).encode_address(&record).unwrap();
        repository
            .store_backup_record("current", &event, "address", Some("active"), 1, 1)
            .await
            .unwrap();
        repository
            .record_publication(
                &event.id.to_string(),
                &Publication {
                    accepted_by: vec!["wss://relay.example.com".parse().unwrap()],
                    failed: vec![],
                },
            )
            .await
            .unwrap();
        let replication = repository.relay_replication().await.unwrap();
        assert_eq!(replication.len(), 1);
        assert_eq!(replication[0].confirmed_events, 1);
    }

    #[tokio::test]
    async fn outbox_retries_the_identical_signed_event() {
        use crate::{
            crypto::RootSecret,
            nostr::codec::{AddressRecord, BackupCodec, UpdatedBy},
        };

        let (_directory, repository) = repository();
        let keys = RootSecret::from_bytes([0x42; 32]).derive().unwrap();
        let destination = "receiver@example.net".parse().unwrap();
        let record = AddressRecord::active(
            &keys,
            "alice@example.com".parse().unwrap(),
            1,
            &destination,
            "$argon2id$example".to_owned(),
            1_700_000_000,
            1_700_000_001,
            UpdatedBy::Token,
        );
        let event = BackupCodec::new(&keys).encode_address(&record).unwrap();
        repository.enqueue_event(&event).await.unwrap();
        repository.enqueue_event(&event).await.unwrap();

        let pending = repository.pending_events(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event.id, event.id);
        repository
            .acknowledge_event(&event.id.to_string())
            .await
            .unwrap();
        assert!(repository.pending_events(10).await.unwrap().is_empty());
    }
}
