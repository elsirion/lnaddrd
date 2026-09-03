use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use nostr_sdk::prelude::{Client, Event, Events, Filter, RelayUrl};
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use crate::repository::sqlite::SqlitePaymentAddressRepository;

#[derive(Debug, Clone)]
pub struct Publication {
    pub accepted_by: Vec<RelayUrl>,
    pub failed: Vec<(RelayUrl, String)>,
}

#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish(&self, event: &Event) -> Result<Publication>;
}

pub type Publisher = Arc<dyn EventPublisher>;

pub struct NostrPublisher {
    client: Client,
}

impl NostrPublisher {
    pub async fn connect(relays: &[String]) -> Result<Self> {
        ensure!(
            !relays.is_empty(),
            "At least one Nostr relay must be configured"
        );
        let client = Client::default();
        for relay in relays {
            client
                .add_relay(relay)
                .await
                .with_context(|| format!("Invalid Nostr relay URL: {relay}"))?;
        }
        client.connect().await;
        client.wait_for_connection(Duration::from_secs(5)).await;
        Ok(Self { client })
    }

    /// Test-only constructor that skips relay configuration and connection, for
    /// building an `AppState` in unit tests that never exercise Nostr fetch/restore.
    #[cfg(test)]
    pub(crate) fn offline() -> Self {
        Self {
            client: Client::default(),
        }
    }

    pub async fn fetch(&self, filter: Filter, timeout: Duration) -> Result<Events> {
        self.client
            .fetch_events(filter, timeout)
            .await
            .context("Failed to fetch Nostr backup events")
    }
}

#[async_trait]
impl EventPublisher for NostrPublisher {
    async fn publish(&self, event: &Event) -> Result<Publication> {
        let output = self.client.send_event(event).await?;
        ensure!(
            !output.success.is_empty(),
            "No configured relay acknowledged event {}",
            event.id
        );
        Ok(Publication {
            accepted_by: output.success.into_iter().collect(),
            failed: output.failed.into_iter().collect(),
        })
    }
}

pub struct OutboxWorker {
    repository: SqlitePaymentAddressRepository,
    publisher: Publisher,
}

impl OutboxWorker {
    pub fn new(repository: SqlitePaymentAddressRepository, publisher: Publisher) -> Self {
        Self {
            repository,
            publisher,
        }
    }

    pub async fn run(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut next_repair = tokio::time::Instant::now();
        loop {
            interval.tick().await;
            if let Err(error) = self.publish_pending().await {
                warn!(%error, "Failed to process Nostr outbox");
            }
            if tokio::time::Instant::now() >= next_repair {
                if let Err(error) = self.repair_current_records().await {
                    warn!(%error, "Failed to repair Nostr backup replicas");
                }
                next_repair = tokio::time::Instant::now() + Duration::from_secs(6 * 60 * 60);
            }
        }
    }

    async fn publish_pending(&self) -> Result<()> {
        for pending in self.repository.pending_events(32).await? {
            match self.publisher.publish(&pending.event).await {
                Ok(publication) => {
                    self.repository
                        .record_publication(&pending.event.id.to_string(), &publication)
                        .await?;
                    self.repository
                        .acknowledge_event(&pending.event.id.to_string())
                        .await?;
                    info!(
                        event_id=%pending.event.id,
                        relay_count=publication.accepted_by.len(),
                        "Nostr backup acknowledged"
                    );
                }
                Err(error) => {
                    self.repository
                        .fail_event(&pending.event.id.to_string(), &error.to_string())
                        .await?;
                    warn!(event_id=%pending.event.id, attempts=pending.attempts + 1, %error, "Nostr backup publication failed");
                }
            }
        }
        Ok(())
    }

    async fn repair_current_records(&self) -> Result<()> {
        for event in self.repository.current_backup_events().await? {
            match self.publisher.publish(&event).await {
                Ok(publication) => {
                    self.repository
                        .record_publication(&event.id.to_string(), &publication)
                        .await?;
                }
                Err(error) => warn!(event_id=%event.id, %error, "Backup repair publication failed"),
            }
        }
        Ok(())
    }
}
