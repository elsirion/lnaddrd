use std::{net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, Parser)]
pub struct Config {
    #[command(subcommand)]
    pub operation: Option<Operation>,

    /// One or more domain names to serve. Specify multiple times for multiple domains.
    #[clap(
        long,
        num_args = 1..,
        env = "LNADDRD_DOMAINS",
        value_delimiter = ',',
    )]
    pub domains: Vec<String>,

    /// The address to bind the server to
    #[clap(long, default_value = "127.0.0.1:8080", env = "LNADDRD_BIND")]
    pub bind: SocketAddr,

    /// Path to the SQLite database
    #[clap(long, env = "LNADDRD_DATABASE_PATH", default_value = "lnaddrd.sqlite3")]
    pub database_path: String,

    /// File containing the stable root secret used for Nostr identity and backup encryption
    #[clap(
        long,
        env = "LNADDRD_ROOT_SECRET_FILE",
        default_value = "/var/lib/lnaddrd/root-secret"
    )]
    pub root_secret_file: PathBuf,

    /// File containing the resettable administrator password
    #[clap(
        long,
        env = "LNADDRD_ADMIN_PASSWORD_FILE",
        default_value = "/var/lib/lnaddrd/admin-password"
    )]
    pub admin_password_file: PathBuf,

    /// Nostr relays used for encrypted backups
    #[clap(
        long,
        num_args = 1..,
        env = "LNADDRD_NOSTR_RELAYS",
        value_delimiter = ','
    )]
    pub nostr_relays: Vec<String>,

    /// Canonical public HTTPS origin used in Nostr service announcements
    #[clap(long, env = "LNADDRD_PUBLIC_BASE_URL")]
    pub public_base_url: Option<String>,

    /// Human-readable service name used in public announcements
    #[clap(long, env = "LNADDRD_SERVICE_NAME", default_value = "lnaddrd")]
    pub service_name: String,

    /// Warning displayed on registration page
    #[clap(long, env = "LNADDRD_WARNING")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Operation {
    /// Explicitly initialize a new service with no addresses
    InitializeEmpty,
    /// Rebuild an uninitialized SQLite database from encrypted Nostr records
    Restore {
        /// Validate and summarize remote records without changing SQLite
        #[arg(long)]
        dry_run: bool,
    },
    /// Discover and validate advertised Lightning Address services
    Discover {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Import a stopped legacy PostgreSQL installation into initialized SQLite
    #[cfg(feature = "postgres-import")]
    ImportPostgres {
        /// Legacy read-only PostgreSQL connection URL
        #[arg(long, env = "LNADDRD_DATABASE_URL")]
        database_url: String,
        /// Validate and report without writing or publishing
        #[arg(long)]
        dry_run: bool,
    },
}
