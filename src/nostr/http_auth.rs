use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, ensure};
use base64::Engine;
use nostr_sdk::prelude::{Event, JsonUtil, Kind};
use sha2::{Digest, Sha256};

/// Time-of-flight tolerance (in seconds) for the `created_at` field of a NIP-98 event.
const MAX_CLOCK_SKEW_SECS: u64 = 60;

/// Replay-guard TTL (in seconds): event ids are remembered for this long.
const REPLAY_TTL_SECS: u64 = 120;

/// A successfully verified NIP-98 HTTP auth event.
pub struct Nip98Auth {
    /// The signer's public key, as 64-char lowercase hex.
    pub pubkey: String,
    /// The event id of the verified auth event (for replay-guard bookkeeping).
    pub event_id: String,
}

/// Verify a NIP-98 `Authorization: Nostr <base64>` header against the given request.
///
/// `body` should be `Some` when the request carried a body, `None` otherwise; this must
/// match how the client computed the `payload` tag.
pub fn verify_nip98(
    authorization: &str,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    now: u64,
) -> Result<Nip98Auth> {
    let encoded = authorization
        .get(..6)
        .filter(|prefix| prefix.eq_ignore_ascii_case("Nostr "))
        .map(|_| &authorization[6..])
        .ok_or_else(|| anyhow::anyhow!("Missing Nostr authorization scheme"))?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(encoded.trim())?;
    let json = String::from_utf8(decoded)?;
    let event = Event::from_json(&json)?;
    event.verify()?;
    ensure!(event.kind == Kind::HttpAuth, "Unexpected event kind");
    ensure!(
        event.created_at.as_secs().abs_diff(now) <= MAX_CLOCK_SKEW_SECS,
        "Auth event is stale or from the future"
    );

    let mut tag_url: Option<String> = None;
    let mut tag_method: Option<String> = None;
    let mut tag_payload: Option<String> = None;
    for tag in event.tags.iter() {
        let values = tag.as_slice();
        match values.first().map(String::as_str) {
            Some("u") => tag_url = values.get(1).cloned(),
            Some("method") => tag_method = values.get(1).cloned(),
            Some("payload") => tag_payload = values.get(1).cloned(),
            _ => {}
        }
    }

    ensure!(tag_url.as_deref() == Some(url), "URL does not match");
    let tag_method = tag_method.ok_or_else(|| anyhow::anyhow!("Missing method tag"))?;
    ensure!(
        tag_method.eq_ignore_ascii_case(method),
        "Method does not match"
    );

    match body {
        Some(body) => {
            let expected = hex::encode(Sha256::digest(body));
            ensure!(
                tag_payload.as_deref() == Some(expected.as_str()),
                "Payload hash does not match body"
            );
        }
        None => {
            ensure!(tag_payload.is_none(), "Unexpected payload tag");
        }
    }

    Ok(Nip98Auth {
        pubkey: event.pubkey.to_string(),
        event_id: event.id.to_string(),
    })
}

/// In-memory replay guard for NIP-98 auth events, keyed by event id.
///
/// Entries older than [`REPLAY_TTL_SECS`] are pruned on every call.
pub struct Nip98ReplayGuard {
    seen: Mutex<HashMap<String, u64>>,
}

impl Nip98ReplayGuard {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Records `event_id` as seen at `now`. Returns `false` if the event id was already
    /// seen within the replay TTL (i.e. this is a replay), `true` otherwise.
    pub fn check_and_insert(&self, event_id: &str, now: u64) -> bool {
        let mut seen = self.seen.lock().expect("replay guard mutex poisoned");
        seen.retain(|_, inserted_at| now.saturating_sub(*inserted_at) <= REPLAY_TTL_SECS);
        if seen.contains_key(event_id) {
            return false;
        }
        seen.insert(event_id.to_owned(), now);
        true
    }
}

impl Default for Nip98ReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::{EventBuilder, Keys, Kind, Tag, Timestamp};

    #[cfg(test)]
    pub fn auth_header(
        keys: &Keys,
        url: &str,
        method: &str,
        payload: Option<&[u8]>,
        created_at: u64,
    ) -> String {
        let mut tags = vec![
            Tag::parse(["u", url]).unwrap(),
            Tag::parse(["method", method]).unwrap(),
        ];
        if let Some(payload) = payload {
            use sha2::{Digest, Sha256};
            tags.push(Tag::parse(["payload", &hex::encode(Sha256::digest(payload))]).unwrap());
        }
        let event = EventBuilder::new(Kind::HttpAuth, "")
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(created_at))
            .sign_with_keys(keys)
            .unwrap();
        use base64::Engine;
        format!(
            "Nostr {}",
            base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_string(&event).unwrap())
        )
    }

    #[test]
    fn accepts_valid_header_and_rejects_tampering() {
        let keys = Keys::generate();
        let url = "https://pay.example.com/api/v1/addresses";
        let header = auth_header(&keys, url, "GET", None, 1_700_000_000);
        let auth = verify_nip98(&header, "GET", url, None, 1_700_000_010).unwrap();
        assert_eq!(auth.pubkey, keys.public_key().to_string());
        assert!(verify_nip98(&header, "POST", url, None, 1_700_000_010).is_err()); // wrong method
        assert!(
            verify_nip98(
                &header,
                "GET",
                "https://other.example/x",
                None,
                1_700_000_010
            )
            .is_err()
        ); // wrong url
        assert!(verify_nip98(&header, "GET", url, None, 1_700_009_999).is_err()); // stale (> 60 s)
        let with_body = auth_header(&keys, url, "POST", Some(b"{}"), 1_700_000_000);
        assert!(verify_nip98(&with_body, "POST", url, Some(b"{}"), 1_700_000_010).is_ok());
        assert!(verify_nip98(&with_body, "POST", url, Some(b"{-}"), 1_700_000_010).is_err()); // body mismatch
    }

    #[test]
    fn replay_guard_blocks_second_use() {
        let guard = Nip98ReplayGuard::new();
        assert!(guard.check_and_insert("id1", 1000));
        assert!(!guard.check_and_insert("id1", 1010));
        assert!(guard.check_and_insert("id1", 1200)); // pruned after 120 s
    }

    #[test]
    fn rejects_future_dated_event() {
        let keys = Keys::generate();
        let url = "https://pay.example.com/api/v1/addresses";
        // created_at is more than 60s ahead of `now`.
        let header = auth_header(&keys, url, "GET", None, 1_700_000_100);
        assert!(verify_nip98(&header, "GET", url, None, 1_700_000_010).is_err());
    }

    #[test]
    fn rejects_wrong_kind() {
        let keys = Keys::generate();
        let url = "https://pay.example.com/api/v1/addresses";
        let tags = vec![
            Tag::parse(["u", url]).unwrap(),
            Tag::parse(["method", "GET"]).unwrap(),
        ];
        let event = EventBuilder::new(Kind::TextNote, "")
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(1_700_000_000))
            .sign_with_keys(&keys)
            .unwrap();
        use base64::Engine;
        let header = format!(
            "Nostr {}",
            base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_string(&event).unwrap())
        );
        assert!(verify_nip98(&header, "GET", url, None, 1_700_000_010).is_err());
    }

    #[test]
    fn rejects_bad_signature() {
        let keys = Keys::generate();
        let url = "https://pay.example.com/api/v1/addresses";
        let header = auth_header(&keys, url, "GET", None, 1_700_000_000);
        let scheme_stripped = &header["Nostr ".len()..];
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(scheme_stripped)
            .unwrap();
        let mut json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        // Flip a hex digit in the signature so the signature no longer verifies.
        let sig = json["sig"].as_str().unwrap().to_owned();
        let mut chars: Vec<char> = sig.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == '0' { '1' } else { '0' };
        json["sig"] = serde_json::Value::String(chars.into_iter().collect());
        let tampered = base64::engine::general_purpose::STANDARD.encode(json.to_string());
        let tampered_header = format!("Nostr {tampered}");
        assert!(verify_nip98(&tampered_header, "GET", url, None, 1_700_000_010).is_err());
    }

    #[test]
    fn rejects_missing_payload_tag_when_body_present() {
        let keys = Keys::generate();
        let url = "https://pay.example.com/api/v1/addresses";
        let header = auth_header(&keys, url, "POST", None, 1_700_000_000);
        assert!(verify_nip98(&header, "POST", url, Some(b"{}"), 1_700_000_010).is_err());
    }

    #[test]
    fn rejects_unexpected_payload_tag_when_no_body() {
        let keys = Keys::generate();
        let url = "https://pay.example.com/api/v1/addresses";
        let header = auth_header(&keys, url, "GET", Some(b"{}"), 1_700_000_000);
        assert!(verify_nip98(&header, "GET", url, None, 1_700_000_010).is_err());
    }

    #[test]
    fn accepts_case_insensitive_scheme_and_method() {
        let keys = Keys::generate();
        let url = "https://pay.example.com/api/v1/addresses";
        let header = auth_header(&keys, url, "GET", None, 1_700_000_000);
        let lower_scheme = header.replacen("Nostr ", "nostr ", 1);
        assert!(verify_nip98(&lower_scheme, "GET", url, None, 1_700_000_010).is_ok());

        let header = auth_header(&keys, url, "get", None, 1_700_000_000);
        assert!(verify_nip98(&header, "GET", url, None, 1_700_000_010).is_ok());
    }

    #[test]
    fn replay_guard_allows_distinct_ids() {
        let guard = Nip98ReplayGuard::new();
        assert!(guard.check_and_insert("id1", 1000));
        assert!(guard.check_and_insert("id2", 1000));
    }
}
