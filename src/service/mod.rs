pub mod direct;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use lnurl::pay::PayResponse;
use serde::{Deserialize, Serialize};

use crate::repository::DestinationPaymentAddress;

pub type LnaddrService = Arc<dyn ILnaddrService + Send + Sync>;

#[async_trait]
pub trait ILnaddrService {
    async fn list_domains(&self) -> Result<Vec<String>>;

    async fn get_lnaddr_manifest(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<PayResponse>>;

    async fn get_destination(
        &self,
        domain: &str,
        username: &str,
    ) -> Result<Option<DestinationPaymentAddress>>;

    async fn register_lnaddr(
        &self,
        domain: &str,
        username: &str,
        destination: &str,
        owner_pubkey: Option<&str>,
    ) -> Result<RegisterResponse>;

    async fn remove_lnaddr(
        &self,
        domain: &str,
        username: &str,
        auth: &ManagementAuth,
    ) -> Result<()>;

    async fn update_lnaddr(
        &self,
        domain: &str,
        username: &str,
        destination: &str,
        auth: &ManagementAuth,
    ) -> Result<bool>;
}

/// How a caller is authorized to manage (update/remove) an existing address.
#[derive(Debug, Clone)]
pub enum ManagementAuth {
    /// The opaque per-address management token issued at registration time.
    Token(String),
    /// A verified 64-char hex Nostr pubkey matching the address's owner.
    Owner(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub lnaddr: String,
    pub authentication_token: String,
    pub active: bool,
}
