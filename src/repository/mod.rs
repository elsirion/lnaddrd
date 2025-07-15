pub mod pg;

use std::{fmt::Display, str::FromStr, sync::Arc, time::SystemTime};

use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum DestinationPaymentAddress {
    Lnurl(lnurl::lnurl::LnUrl),
    LnAddress { user: String, domain: String },
}

impl DestinationPaymentAddress {
    pub fn url(&self) -> String {
        match self {
            DestinationPaymentAddress::Lnurl(lnurl) => lnurl.url.clone(),
            DestinationPaymentAddress::LnAddress { user, domain } => {
                format!("https://{domain}/.well-known/lnurlp/{user}")
            }
        }
    }
}

impl Display for DestinationPaymentAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DestinationPaymentAddress::Lnurl(lnurl) => write!(f, "{}", lnurl),
            DestinationPaymentAddress::LnAddress { user, domain } => {
                write!(f, "{}@{}", user, domain)
            }
        }
    }
}

impl FromStr for DestinationPaymentAddress {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(lnurl) = lnurl::lnurl::LnUrl::decode(s.to_owned()) {
            Ok(DestinationPaymentAddress::Lnurl(lnurl))
        } else {
            let parts: Vec<&str> = s.split('@').collect();

            ensure!(
                parts.len() == 2,
                "Invalid destination payment address, neither lnurl nor lnaddress"
            );

            Ok(DestinationPaymentAddress::LnAddress {
                user: parts[0].to_string(),
                domain: parts[1].to_string(),
            })
        }
    }
}
