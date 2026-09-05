use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, redirect::Policy};
use serde::de::DeserializeOwned;
use tokio::net::lookup_host;
use url::Url;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// HTTP client for attacker-controlled LNURL destinations.
///
/// A fresh client is deliberately built for every request so that the hostname is
/// pinned to the public addresses validated immediately before the connection.
#[derive(Debug, Clone, Default)]
pub struct SafeHttpClient;

impl SafeHttpClient {
    pub async fn get_json<T: DeserializeOwned>(&self, value: &str) -> Result<T> {
        self.get_json_with_timeout(value, REQUEST_TIMEOUT).await
    }

    /// Like [`Self::get_json`] with a caller-chosen timeout: payment flows hit
    /// gateway-backed invoice generation that legitimately outlives the
    /// default budget.
    pub async fn get_json_with_timeout<T: DeserializeOwned>(
        &self,
        value: &str,
        timeout: Duration,
    ) -> Result<T> {
        let url = Url::parse(value).context("Invalid destination URL")?;
        ensure!(
            url.scheme() == "https",
            "Only HTTPS destinations are allowed"
        );
        ensure!(
            url.username().is_empty(),
            "URL user information is not allowed"
        );
        ensure!(
            url.password().is_none(),
            "URL user information is not allowed"
        );
        let host = url.host_str().context("Destination URL has no host")?;
        let port = url.port_or_known_default().context("Unknown URL port")?;

        let addresses = lookup_host((host, port))
            .await
            .context("Could not resolve destination host")?
            .collect::<Vec<_>>();
        ensure!(!addresses.is_empty(), "Destination host has no addresses");
        for address in &addresses {
            ensure!(
                is_public_ip(address.ip()),
                "Destination resolves to a non-public address"
            );
        }

        let client = Client::builder()
            .timeout(timeout)
            .redirect(Policy::none())
            .resolve_to_addrs(host, &addresses)
            .build()
            .context("Could not construct outbound HTTP client")?;
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "Destination request to {host} failed: {:#}",
                    anyhow::Error::from(error)
                )
            })?
            .error_for_status()
            .context("Destination returned an HTTP error")?;
        if let Some(length) = response.content_length() {
            ensure!(
                length <= MAX_RESPONSE_BYTES as u64,
                "Destination response is too large"
            );
        }

        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.context("Could not read response")? {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                bail!("Destination response is too large");
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).context("Destination returned invalid JSON")
    }
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.octets()[0] == 0
        || ip.octets()[0] >= 240
        || matches!(ip.octets(), [100, second, _, _] if (64..=127).contains(&second))
        || matches!(ip.octets(), [192, 0, 0, _])
        || matches!(ip.octets(), [198, second, _, _] if second == 18 || second == 19))
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(ipv4);
    }
    !(ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unspecified()
        || (ip.segments()[0] & 0xfe00) == 0xfc00
        || (ip.segments()[0] & 0xffc0) == 0xfe80
        || (ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_special_use_addresses() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public_ip(value.parse().unwrap()), "accepted {value}");
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
