use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use nostr_sdk::prelude::{Keys, PublicKey, SecretKey};
use rand::{RngCore, rngs::OsRng};
use sha2::Sha256;

const ROOT_SECRET_BYTES: usize = 32;
const SIGNING_KEY_INFO: &[u8] = b"lnaddrd/v1/nostr-signing-key";
const ENCRYPTION_KEY_INFO: &[u8] = b"lnaddrd/v1/nostr-encryption-key";
const LOOKUP_KEY_INFO: &[u8] = b"lnaddrd/v1/address-lookup-key";

pub struct RootSecret([u8; ROOT_SECRET_BYTES]);

impl fmt::Debug for RootSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RootSecret([REDACTED])")
    }
}

impl RootSecret {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match Self::load(path) {
            Ok(secret) => Ok(secret),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Self::create(path)
            }
            Err(error) => Err(error),
        }
    }

    pub fn from_bytes(bytes: [u8; ROOT_SECRET_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(encoded: &str) -> Result<Self> {
        Self::decode(encoded)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("Failed to inspect root secret at {}", path.display()))?;
        ensure!(
            metadata.file_type().is_file(),
            "Root secret must be a regular file"
        );
        ensure!(
            !metadata.file_type().is_symlink(),
            "Root secret must not be a symlink"
        );
        ensure_private_permissions(&metadata, path)?;

        let mut encoded = String::new();
        OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("Failed to open root secret at {}", path.display()))?
            .read_to_string(&mut encoded)
            .context("Failed to read root secret")?;
        Self::decode(&encoded)
    }

    pub fn create(path: &Path) -> Result<Self> {
        let parent = path.parent().filter(|path| !path.as_os_str().is_empty());
        if let Some(parent) = parent {
            create_private_directory(parent)?;
        }

        let mut bytes = [0_u8; ROOT_SECRET_BYTES];
        OsRng.fill_bytes(&mut bytes);
        let encoded = format!("{}\n", hex::encode(bytes));

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Self::load(path);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Failed to create root secret at {}", path.display())
                });
            }
        };
        file.write_all(encoded.as_bytes())
            .context("Failed to write root secret")?;
        file.sync_all().context("Failed to sync root secret")?;

        Ok(Self(bytes))
    }

    pub fn install(path: &Path, encoded: &str) -> Result<Self> {
        let secret = Self::decode(encoded)?;
        let parent = path.parent().filter(|path| !path.as_os_str().is_empty());
        if let Some(parent) = parent {
            create_private_directory(parent)?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = Self::load(path)?;
                ensure!(
                    existing.0 == secret.0,
                    "A different root seed is already installed"
                );
                return Ok(existing);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Failed to install root secret at {}", path.display())
                });
            }
        };
        file.write_all(format!("{}\n", secret.expose_hex()).as_bytes())?;
        file.sync_all()?;
        Ok(secret)
    }

    pub fn expose_hex(&self) -> String {
        hex::encode(self.0)
    }

    fn decode(encoded: &str) -> Result<Self> {
        let encoded = encoded.trim();
        ensure!(
            encoded.len() == ROOT_SECRET_BYTES * 2,
            "Root secret must be 64 lowercase hex characters"
        );
        ensure!(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
            "Root secret must be lowercase hexadecimal"
        );
        let decoded = hex::decode(encoded).context("Invalid root-secret hex")?;
        let bytes: [u8; ROOT_SECRET_BYTES] = decoded
            .try_into()
            .map_err(|_| anyhow::anyhow!("Root secret must decode to 32 bytes"))?;
        Ok(Self(bytes))
    }

    pub fn derive(&self) -> Result<ServiceKeys> {
        let signing_secret = derive_valid_secret_key(&self.0, SIGNING_KEY_INFO)?;
        let encryption_secret = derive_valid_secret_key(&self.0, ENCRYPTION_KEY_INFO)?;
        let lookup_key = derive_bytes(&self.0, LOOKUP_KEY_INFO, 0)?;

        Ok(ServiceKeys {
            signing: Keys::new(signing_secret),
            encryption: Keys::new(encryption_secret),
            lookup_key,
        })
    }
}

pub struct ServiceKeys {
    signing: Keys,
    encryption: Keys,
    lookup_key: [u8; 32],
}

impl fmt::Debug for ServiceKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServiceKeys")
            .field("service_public_key", &self.service_public_key())
            .field("encryption_public_key", &self.encryption_public_key())
            .field("secrets", &"[REDACTED]")
            .finish()
    }
}

impl ServiceKeys {
    pub fn signing_keys(&self) -> &Keys {
        &self.signing
    }

    pub fn encryption_secret_key(&self) -> &SecretKey {
        self.encryption.secret_key()
    }

    pub fn service_public_key(&self) -> PublicKey {
        self.signing.public_key()
    }

    pub fn encryption_public_key(&self) -> PublicKey {
        self.encryption.public_key()
    }

    pub fn address_key(&self, canonical_address: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.lookup_key)
            .expect("HMAC accepts keys of any size");
        mac.update(canonical_address.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

fn derive_valid_secret_key(root: &[u8; 32], info: &[u8]) -> Result<SecretKey> {
    for counter in 0..=u32::MAX {
        let candidate = derive_bytes(root, info, counter)?;
        if let Ok(secret_key) = SecretKey::from_slice(&candidate) {
            return Ok(secret_key);
        }
    }
    bail!("Unable to derive a valid secp256k1 key")
}

fn derive_bytes(root: &[u8; 32], info: &[u8], counter: u32) -> Result<[u8; 32]> {
    let hkdf = Hkdf::<Sha256>::new(None, root);
    let mut expanded_info = Vec::with_capacity(info.len() + 4);
    expanded_info.extend_from_slice(info);
    expanded_info.extend_from_slice(&counter.to_be_bytes());

    let mut output = [0_u8; 32];
    hkdf.expand(&expanded_info, &mut output)
        .map_err(|_| anyhow::anyhow!("HKDF output length is invalid"))?;
    Ok(output)
}

fn create_private_directory(path: &Path) -> Result<()> {
    if path.exists() {
        ensure!(path.is_dir(), "Root-secret parent is not a directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                fs::metadata(path)?.permissions().mode() & 0o077 == 0,
                "Root-secret directory {} must not be accessible by group or others",
                path.display()
            );
        }
        return Ok(());
    }

    fs::create_dir_all(path)
        .with_context(|| format!("Failed to create state directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn ensure_private_permissions(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        ensure!(
            mode & 0o077 == 0,
            "Root secret at {} is accessible by group or others",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_reloads_the_same_secret() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/root-secret");

        let first = RootSecret::load_or_create(&path).unwrap().derive().unwrap();
        let second = RootSecret::load_or_create(&path).unwrap().derive().unwrap();
        assert_eq!(first.service_public_key(), second.service_public_key());
        assert_eq!(
            first.encryption_public_key(),
            second.encryption_public_key()
        );
        assert_eq!(
            first.address_key("alice@example.com"),
            second.address_key("alice@example.com")
        );
    }

    #[test]
    fn derivation_is_domain_separated_and_stable() {
        let keys = RootSecret::from_bytes([0x42; 32]).derive().unwrap();
        assert_eq!(
            keys.service_public_key().to_string(),
            "a4dc1e742d5fdbacf524d0d35055ff83f9564d97ef4108c5c4e6f4a7ee0eb6f9"
        );
        assert_eq!(
            keys.encryption_public_key().to_string(),
            "59810cd36070306aa3a7c974a16b1949dc17bb0133f37a11c2ac2bc5c4ccf31b"
        );
        assert_eq!(
            keys.address_key("alice@example.com"),
            "5dbd1cec8390ea673f4c3f193aee6582b01ab189acf123c9aa19392632f44889"
        );
        assert_ne!(
            keys.address_key("alice@example.com"),
            keys.address_key("bob@example.com")
        );
    }

    #[test]
    fn rejects_uppercase_or_malformed_secret() {
        assert!(RootSecret::decode(&"AA".repeat(32)).is_err());
        assert!(RootSecret::decode("abcd").is_err());
    }

    #[test]
    fn installs_seed_without_replacing_a_different_one() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/root-secret");
        let encoded = "42".repeat(32);
        assert_eq!(
            RootSecret::install(&path, &encoded).unwrap().expose_hex(),
            encoded
        );
        assert_eq!(
            RootSecret::install(&path, &encoded).unwrap().expose_hex(),
            encoded
        );
        assert!(RootSecret::install(&path, &"24".repeat(32)).is_err());
        assert_eq!(RootSecret::load(&path).unwrap().expose_hex(), encoded);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_public_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("root-secret");
        fs::write(&path, format!("{}\n", "11".repeat(32))).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(RootSecret::load_or_create(&path).is_err());
    }
}
