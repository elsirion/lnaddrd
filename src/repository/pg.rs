use anyhow::Result;
use std::{str::FromStr, sync::Arc, time::SystemTime};
use tracing::info;

use async_trait::async_trait;
use diesel::{
    prelude::*,
    r2d2::{ConnectionManager, Pool},
};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

use super::{
    DestinationPaymentAddress, IPaymentAddressRepository, PaymentAddress, PaymentAddressRepository,
};

type PooledConnection =
    diesel::r2d2::PooledConnection<diesel::r2d2::ConnectionManager<PgConnection>>;
type ConnectionPool = diesel::r2d2::Pool<diesel::r2d2::ConnectionManager<PgConnection>>;

#[derive(Debug, Clone)]
pub struct PgPaymentAddressRepository {
    pool: ConnectionPool,
}

impl PgPaymentAddressRepository {
    pub fn new(db_url: &str) -> Result<Self> {
        let manager = ConnectionManager::new(db_url);
        let pool = Pool::new(manager)?;

        run_migrations(&mut pool.get()?)?;

        Ok(Self { pool })
    }

    pub fn into_dyn(self) -> PaymentAddressRepository {
        Arc::new(self)
    }
}

#[async_trait]
impl IPaymentAddressRepository for PgPaymentAddressRepository {
    async fn get_payment_address(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<PaymentAddress>> {
        let mut conn = self.pool.get()?;

        match payment_addresses::table
            .filter(payment_addresses::domain.eq(domain))
            .filter(payment_addresses::username.eq(username))
            .first::<PaymentAddressEntry>(&mut conn)
        {
            Ok(lnaddress) => Ok(Some(lnaddress.into())),
            Err(diesel::result::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn add_payment_address(
        &self,
        domain: &str,
        username: &str,
        destination: DestinationPaymentAddress,
        authentication_token: &str,
    ) -> Result<()> {
        let mut conn = self.pool.get()?;

        diesel::insert_into(payment_addresses::table)
            .values((
                payment_addresses::domain.eq(domain),
                payment_addresses::username.eq(username),
                payment_addresses::lnurl.eq(destination.to_string()),
                payment_addresses::authentication_token.eq(authentication_token),
            ))
            .execute(&mut conn)?;

        Ok(())
    }

    async fn remove_payment_address(
        &self,
        domain: &str,
        username: &str,
        token: &str,
    ) -> Result<()> {
        use diesel::prelude::*;

        let mut conn = self.pool.get()?;

        let rows_deleted = diesel::delete(
            payment_addresses::table
                .filter(payment_addresses::domain.eq(domain))
                .filter(payment_addresses::username.eq(username))
                .filter(payment_addresses::authentication_token.eq(token)),
        )
        .execute(&mut conn)?;

        if rows_deleted == 0 {
            anyhow::bail!("No matching payment address with valid authentication token found");
        }

        Ok(())
    }
}

diesel::table! {
    payment_addresses (id) {
        id -> Integer,
        username -> VarChar,
        domain -> VarChar,
        lnurl -> Text,
        authentication_token -> VarChar,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

/// Lnaddress table entry
#[derive(Queryable)]
struct PaymentAddressEntry {
    _id: i32,
    username: String,
    domain: String,
    lnurl: String,
    authentication_token: String,
    created_at: SystemTime,
    updated_at: SystemTime,
}

impl From<PaymentAddressEntry> for PaymentAddress {
    fn from(entry: PaymentAddressEntry) -> Self {
        Self {
            username: entry.username,
            domain: entry.domain,
            destination: DestinationPaymentAddress::from_str(&entry.lnurl).expect("Invalid lnurl"),
            authentication_token: entry.authentication_token,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
        }
    }
}

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");
fn run_migrations(conn: &mut PooledConnection) -> Result<()> {
    let migrations = conn
        .run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;

    for migration in migrations {
        info!("Applied migration {}", migration);
    }

    Ok(())
}
