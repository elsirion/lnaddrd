use std::{fmt::Display, str::FromStr};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Domain(String);

impl Domain {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Domain {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim();
        ensure!(!value.is_empty(), "Domain must not be empty");
        ensure!(!value.ends_with('.'), "Domain must not have a trailing dot");
        ensure!(
            !value.contains(['/', ':', '@']),
            "Domain must not contain a scheme, port, path, or user information"
        );

        let ascii = idna::domain_to_ascii(value)
            .map_err(|error| anyhow::anyhow!("Invalid domain: {error}"))?
            .to_ascii_lowercase();
        ensure!(ascii.len() <= 253, "Domain is too long");

        for label in ascii.split('.') {
            ensure!(!label.is_empty(), "Domain contains an empty label");
            ensure!(label.len() <= 63, "Domain label is too long");
            ensure!(
                !label.starts_with('-') && !label.ends_with('-'),
                "Domain labels must not start or end with '-'"
            );
            ensure!(
                label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
                "Domain contains invalid characters"
            );
        }

        Ok(Self(ascii))
    }
}

impl Display for Domain {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for Domain {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl From<Domain> for String {
    fn from(value: Domain) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Username(String);

impl Username {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Username {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        ensure!(!value.is_empty(), "Username must not be empty");
        ensure!(value.len() <= 64, "Username is too long");
        ensure!(value.is_ascii(), "Username must be ASCII");
        ensure!(
            value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            }),
            "Username must contain only lowercase a-z, 0-9, '-', '_', or '.'"
        );
        Ok(Self(value.to_owned()))
    }
}

impl Display for Username {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for Username {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl From<Username> for String {
    fn from(value: Username) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LightningAddress {
    pub username: Username,
    pub domain: Domain,
}

impl FromStr for LightningAddress {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let mut parts = value.split('@');
        let username = parts.next().unwrap_or_default();
        let domain = parts.next().unwrap_or_default();
        ensure!(
            parts.next().is_none() && !username.is_empty() && !domain.is_empty(),
            "Invalid Lightning Address"
        );

        Ok(Self {
            username: username.parse()?,
            domain: domain.parse()?,
        })
    }
}

impl Display for LightningAddress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}@{}", self.username, self.domain)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Destination {
    Lnurl(lnurl::lnurl::LnUrl),
    LnAddress { address: LightningAddress },
}

impl Destination {
    pub fn url(&self) -> String {
        match self {
            Self::Lnurl(lnurl) => lnurl.url.clone(),
            Self::LnAddress { address } => format!(
                "https://{}/.well-known/lnurlp/{}",
                address.domain, address.username
            ),
        }
    }
}

impl Display for Destination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lnurl(lnurl) => write!(formatter, "{lnurl}"),
            Self::LnAddress { address } => write!(formatter, "{address}"),
        }
    }
}

impl FromStr for Destination {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        if let Ok(lnurl) = lnurl::lnurl::LnUrl::decode(value.to_owned()) {
            return Ok(Self::Lnurl(lnurl));
        }

        if value.contains('@') {
            return Ok(Self::LnAddress {
                address: value.parse()?,
            });
        }

        bail!("Invalid destination: expected an LNURL or Lightning Address")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_is_canonicalized() {
        assert_eq!(
            "EXAMPLE.COM".parse::<Domain>().unwrap().as_str(),
            "example.com"
        );
        assert_eq!(
            "Bücher.example".parse::<Domain>().unwrap().as_str(),
            "xn--bcher-kva.example"
        );
    }

    #[test]
    fn domain_rejects_origins_and_ports() {
        for invalid in [
            "https://example.com",
            "example.com:443",
            "example.com/",
            ".example",
        ] {
            assert!(invalid.parse::<Domain>().is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn username_follows_lud16_subset() {
        for valid in ["a", "satoshi_21", "first.last", "_"] {
            assert!(valid.parse::<Username>().is_ok(), "rejected {valid}");
        }
        for invalid in ["", "Alice", "has+tag", "two words", "ümlaut"] {
            assert!(invalid.parse::<Username>().is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn lightning_address_round_trip_is_canonical() {
        let address = "alice@EXAMPLE.COM".parse::<LightningAddress>().unwrap();
        assert_eq!(address.to_string(), "alice@example.com");
    }

    #[test]
    fn destination_accepts_lightning_address() {
        let destination = "alice@example.com".parse::<Destination>().unwrap();
        assert_eq!(destination.to_string(), "alice@example.com");
        assert_eq!(
            destination.url(),
            "https://example.com/.well-known/lnurlp/alice"
        );
    }
}
