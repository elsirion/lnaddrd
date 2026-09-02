use anyhow::Result;
use clap::Parser;
use lnaddrd::{
    config::{Config, Operation},
    initialize_empty, restore_database, serve,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::parse();
    match &config.operation {
        Some(Operation::InitializeEmpty) => initialize_empty(&config).await,
        Some(Operation::Restore { dry_run }) => restore_database(&config, *dry_run).await,
        Some(Operation::Discover { json }) => {
            let services = lnaddrd::nostr::discovery::discover(&config.nostr_relays).await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&services)?);
            } else {
                for service in services {
                    println!(
                        "{} ({})",
                        service
                            .announcement
                            .name
                            .as_deref()
                            .unwrap_or("Unnamed service"),
                        service.announcement.origin
                    );
                    println!("  pubkey: {}", service.pubkey);
                    println!(
                        "  verified domains: {}",
                        service.verified_domains.join(", ")
                    );
                    for error in service.verification_errors {
                        println!("  unverified: {error}");
                    }
                }
            }
            Ok(())
        }
        #[cfg(feature = "postgres-import")]
        Some(Operation::ImportPostgres {
            database_url,
            dry_run,
            skip_empty_usernames,
            prefer_newest_duplicates,
            canonicalize_usernames,
        }) => {
            use lnaddrd::{
                crypto::RootSecret,
                nostr::publisher::{NostrPublisher, Publisher},
                repository::sqlite::SqlitePaymentAddressRepository,
            };
            use std::sync::Arc;
            let keys = Arc::new(RootSecret::load_or_create(&config.root_secret_file)?.derive()?);
            let repository = SqlitePaymentAddressRepository::new(&config.database_path)?;
            let publisher: Publisher =
                Arc::new(NostrPublisher::connect(&config.nostr_relays).await?);
            let report = lnaddrd::import_postgres::import(
                database_url,
                &repository,
                keys,
                publisher,
                &config.domains,
                lnaddrd::import_postgres::ImportOptions {
                    dry_run: *dry_run,
                    skip_empty_usernames: *skip_empty_usernames,
                    prefer_newest_duplicates: *prefer_newest_duplicates,
                    canonicalize_usernames: *canonicalize_usernames,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        None => serve(&config).await,
    }
}
