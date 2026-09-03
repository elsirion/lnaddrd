use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescriptionRef};
use lnurl::{LnUrlResponse, pay::PayResponse};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    domain::Destination,
    nostr::codec::{PaymentPolicyRecord, PaymentTierRecord},
    outbound::SafeHttpClient,
};

#[derive(Debug, Clone, Deserialize)]
pub struct InvoiceResponse {
    pub status: Option<String>,
    pub reason: Option<String>,
    pub pr: Option<String>,
    pub verify: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerifyResponse {
    pub status: String,
    pub reason: Option<String>,
    pub settled: Option<bool>,
    pub pr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VerifiableInvoice {
    pub bolt11: String,
    pub verify_url: String,
}

#[derive(Debug, Clone, Default)]
pub struct PaymentClient {
    http: SafeHttpClient,
}

#[async_trait]
pub trait DestinationValidator: Send + Sync {
    async fn validate(&self, destination: &Destination) -> Result<()>;
}

#[async_trait]
impl DestinationValidator for PaymentClient {
    async fn validate(&self, destination: &Destination) -> Result<()> {
        self.resolve(destination).await.map(|_| ())
    }
}

impl PaymentClient {
    pub async fn resolve(&self, destination: &Destination) -> Result<PayResponse> {
        match self.http.get_json(&destination.url()).await? {
            LnUrlResponse::LnUrlPayResponse(pay) => Ok(pay),
            _ => bail!("Destination does not return an LNURL-pay response"),
        }
    }

    pub async fn invoice(&self, pay: &PayResponse, amount_msat: u64) -> Result<VerifiableInvoice> {
        ensure!(
            (pay.min_sendable..=pay.max_sendable).contains(&amount_msat),
            "Payment amount is outside recipient bounds"
        );
        let mut callback = Url::parse(&pay.callback).context("Invalid LNURL callback URL")?;
        ensure!(
            !callback.query_pairs().any(|(key, _)| key == "amount"),
            "LNURL callback already contains an amount"
        );
        callback
            .query_pairs_mut()
            .append_pair("amount", &amount_msat.to_string());
        let response: InvoiceResponse = self.http.get_json(callback.as_str()).await?;
        ensure!(
            response.status.as_deref() != Some("ERROR"),
            "Recipient rejected invoice request: {}",
            response.reason.as_deref().unwrap_or("unspecified error")
        );
        let bolt11 = response.pr.context("Recipient did not return an invoice")?;
        let parsed = Bolt11Invoice::from_str(&bolt11).map_err(|error| {
            anyhow::anyhow!("Recipient returned an invalid BOLT11 invoice: {error:?}")
        })?;
        ensure!(
            parsed.amount_milli_satoshis() == Some(amount_msat),
            "Recipient returned an invoice for the wrong amount"
        );
        match parsed.description() {
            Bolt11InvoiceDescriptionRef::Hash(hash) => ensure!(
                hash.0.to_string() == hex::encode(pay.metadata_hash()),
                "Invoice description hash does not match LNURL metadata"
            ),
            Bolt11InvoiceDescriptionRef::Direct(_) => {
                bail!("LNURL invoice must contain a metadata description hash")
            }
        }
        let verify_url = response
            .verify
            .context("Recipient does not support LUD-21 verification")?;
        // Parse now so malformed, non-HTTPS URLs fail before the invoice is exposed.
        let verify = Url::parse(&verify_url).context("Invalid LUD-21 verify URL")?;
        ensure!(
            verify.scheme() == "https",
            "LUD-21 verify URL must use HTTPS"
        );
        Ok(VerifiableInvoice { bolt11, verify_url })
    }

    pub async fn verify(&self, invoice: &VerifiableInvoice) -> Result<bool> {
        let response: VerifyResponse = self.http.get_json(&invoice.verify_url).await?;
        ensure!(
            response.status == "OK",
            "LUD-21 verifier returned an error: {}",
            response.reason.as_deref().unwrap_or("unspecified error")
        );
        ensure!(
            response.pr.as_deref() == Some(invoice.bolt11.as_str()),
            "LUD-21 verifier returned a different invoice"
        );
        response
            .settled
            .context("LUD-21 verifier omitted settlement status")
    }

    pub async fn validate_policy(&self, policy: &PaymentPolicyRecord) -> Result<()> {
        validate_tiers(&policy.tiers)?;
        let destination = Destination::try_from(policy.destination.clone())?;
        let pay = self.resolve(&destination).await?;
        for tier in policy.tiers.iter().filter(|tier| tier.price_msat > 0) {
            ensure!(
                (pay.min_sendable..=pay.max_sendable).contains(&tier.price_msat),
                "Price for usernames up to {} characters is outside recipient bounds",
                tier.max_length
            );
        }
        if let Some(amount) = policy
            .tiers
            .iter()
            .filter(|tier| tier.price_msat > 0)
            .map(|tier| tier.price_msat)
            .min()
        {
            let invoice = self.invoice(&pay, amount).await?;
            ensure!(
                !self.verify(&invoice).await?,
                "LUD-21 test invoice unexpectedly reports as settled"
            );
        }
        Ok(())
    }
}

pub fn validate_tiers(tiers: &[PaymentTierRecord]) -> Result<()> {
    for pair in tiers.windows(2) {
        ensure!(
            pair[0].max_length < pair[1].max_length,
            "Tier lengths must be strictly increasing"
        );
        ensure!(
            pair[0].price_msat >= pair[1].price_msat,
            "Tier prices must be non-increasing"
        );
    }
    ensure!(
        tiers
            .iter()
            .all(|tier| tier.max_length > 0 && tier.max_length <= 64),
        "Tier length must be between 1 and 64"
    );
    Ok(())
}

pub fn policy_price(policy: &PaymentPolicyRecord, username_length: usize) -> Option<u64> {
    policy
        .tiers
        .iter()
        .find(|tier| usize::from(tier.max_length) >= username_length)
        .map(|tier| tier.price_msat)
}

pub fn policy_fingerprint(policy: &PaymentPolicyRecord) -> Result<String> {
    #[derive(Serialize)]
    struct CanonicalPolicy<'a> {
        destination: &'a crate::nostr::codec::BackupDestination,
        tiers: &'a [PaymentTierRecord],
    }
    let bytes = serde_json::to_vec(&CanonicalPolicy {
        destination: &policy.destination,
        tiers: &policy.tiers,
    })?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub fn recipient_fingerprint(policy: &PaymentPolicyRecord) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &policy.destination,
    )?)))
}

pub fn parse_policy(destination: &str, tiers: &str) -> Result<PaymentPolicyRecord> {
    let destination = Destination::from_str(destination)?;
    let tiers = tiers
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (length, price) = line
                .split_once('=')
                .context("Each tier must use max_length=price_msat")?;
            Ok(PaymentTierRecord {
                max_length: length.trim().parse().context("Invalid tier length")?,
                price_msat: price.trim().parse().context("Invalid tier price")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!tiers.is_empty(), "Add at least one pricing tier");
    ensure!(
        tiers.iter().any(|tier| tier.price_msat > 0),
        "At least one pricing tier must have a positive price"
    );
    validate_tiers(&tiers)?;
    Ok(PaymentPolicyRecord {
        destination: (&destination).into(),
        tiers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_order_and_price_are_constrained() {
        assert!(
            validate_tiers(&[
                PaymentTierRecord {
                    max_length: 2,
                    price_msat: 100
                },
                PaymentTierRecord {
                    max_length: 4,
                    price_msat: 10
                },
                PaymentTierRecord {
                    max_length: 64,
                    price_msat: 0
                },
            ])
            .is_ok()
        );
        assert!(
            validate_tiers(&[
                PaymentTierRecord {
                    max_length: 4,
                    price_msat: 10
                },
                PaymentTierRecord {
                    max_length: 2,
                    price_msat: 100
                },
            ])
            .is_err()
        );
        assert!(
            validate_tiers(&[
                PaymentTierRecord {
                    max_length: 2,
                    price_msat: 10
                },
                PaymentTierRecord {
                    max_length: 4,
                    price_msat: 100
                },
            ])
            .is_err()
        );
    }

    #[test]
    fn policy_requires_a_real_payment_tier() {
        assert!(parse_policy("receiver@example.com", "").is_err());
        assert!(parse_policy("receiver@example.com", "64=0").is_err());
        assert!(parse_policy("receiver@example.com", "5=1000\n64=0").is_ok());
    }

    #[test]
    fn selects_first_inclusive_tier() {
        let policy = parse_policy("recipient@example.com", "2=100\n4=10\n64=0").unwrap();
        assert_eq!(policy_price(&policy, 2), Some(100));
        assert_eq!(policy_price(&policy, 3), Some(10));
        assert_eq!(policy_price(&policy, 65), None);
        assert_eq!(policy_fingerprint(&policy).unwrap().len(), 64);
    }
}
