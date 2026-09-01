pub mod sqlite;

use std::{sync::Arc, time::SystemTime};

use anyhow::Result;
use async_trait::async_trait;

pub use crate::domain::Destination as DestinationPaymentAddress;

pub type PaymentAddressRepository = Arc<dyn IPaymentAddressRepository + Send + Sync>;

#[async_trait]
pub trait IPaymentAddressRepository {
    async fn get_payment_address(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<PaymentAddress>>;

    async fn add_payment_address(
        &self,
        domain: &str,
        username: &str,
        destination: DestinationPaymentAddress,
        authentication_token: &str,
    ) -> Result<()>;

    async fn remove_payment_address(
        &self,
        domain: &str,
        username: &str,
        authentication_token: &str,
    ) -> Result<()>;
}

#[derive(Debug)]
pub struct PaymentAddress {
    pub username: String,
    pub domain: String,
    pub destination: DestinationPaymentAddress,
    pub authentication_token: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}
