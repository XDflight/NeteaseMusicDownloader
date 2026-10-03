//! Encrypted local storage for the login session. No OS keychain is involved.
//!
//! File layout: `"NMDC" | version | mode | salt(16) | nonce(12) | AES-256-GCM ciphertext + tag`.
//! The header (up to and including the salt) is authenticated as associated data.
//!
//! The key is `Argon2id(password, salt, secret = machine identifier)` where `password` is the
//! optional user passphrase. Without a passphrase the file is bound to this machine and user
//! profile: copying it elsewhere does not reveal the login. With a passphrase an attacker also
//! has to guess the passphrase. Neither protects against malware running as the same user
//! while the app is unlocked.
//!
//! Every save draws a fresh salt, so every save also uses a fresh key: the random 96-bit GCM
//! nonce is never reused under one key.

use std::fs;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use ncm_api::Session;
use rand::Rng;
use zeroize::Zeroizing;

use crate::fsutil::write_atomic;

const MAGIC: &[u8; 4] = b"NMDC";
const VERSION: u8 = 1;
const MODE_MACHINE: u8 = 0;
const MODE_PASSPHRASE: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 4 + 1 + 1 + SALT_LEN;
const FILE_NAME: &str = "credentials.bin";
const MACHINE_KEY_FILE: &str = "machine.key";
const DEFAULT_PASSWORD: &[u8] = b"ncm-dl:machine-bound";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the saved login is protected by a passphrase")]
    NeedPassphrase,
    #[error("could not decrypt the saved login (wrong passphrase, or it was created on another machine)")]
    Decrypt,
    #[error("the saved login file is damaged")]
    Format,
    #[error("key derivation failed: {0}")]
    Kdf(String),
}

pub struct SecureStore {
    path: PathBuf,
    machine_secret: Zeroizing<Vec<u8>>,
}

impl SecureStore {
    /// `dir` is the app data directory; the credentials file lives inside it.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(dir)?;
        Ok(Self { path: dir.join(FILE_NAME), machine_secret: machine_secret(dir)? })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Whether the stored login needs a passphrase (`false` when nothing is stored).
    pub fn needs_passphrase(&self) -> Result<bool, StoreError> {
        match fs::read(&self.path) {
            Ok(data) => Ok(parse_header(&data)?.1 == MODE_PASSPHRASE),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, session: &Session, passphrase: Option<&str>) -> Result<(), StoreError> {
        let plain = Zeroizing::new(serde_json::to_vec(session).map_err(|_| StoreError::Format)?);
        let mut salt = [0u8; SALT_LEN];
        let mut nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut salt);
        rand::rng().fill_bytes(&mut nonce);
        let mode = if passphrase.is_some_and(|p| !p.is_empty()) { MODE_PASSPHRASE } else { MODE_MACHINE };

        let mut out = Vec::with_capacity(HEADER_LEN + NONCE_LEN + plain.len() + TAG_LEN);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(mode);
        out.extend_from_slice(&salt);

        let key = self.derive_key(passphrase.filter(|_| mode == MODE_PASSPHRASE), &salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key[..]).map_err(|_| StoreError::Format)?;
        let ct = cipher
            .encrypt(
                &Nonce::try_from(&nonce[..]).map_err(|_| StoreError::Format)?,
                Payload { msg: &plain, aad: &out[..HEADER_LEN] },
            )
            .map_err(|_| StoreError::Format)?;
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        write_atomic(&self.path, &out)?;
        Ok(())
    }

    /// `Ok(None)` when nothing is stored.
    pub fn load(&self, passphrase: Option<&str>) -> Result<Option<Session>, StoreError> {
        let data = match fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let (salt, mode) = parse_header(&data)?;
        if data.len() < HEADER_LEN + NONCE_LEN + TAG_LEN {
            return Err(StoreError::Format);
        }
        let passphrase = match mode {
            MODE_PASSPHRASE => Some(passphrase.filter(|p| !p.is_empty()).ok_or(StoreError::NeedPassphrase)?),
            _ => None,
        };
        let key = self.derive_key(passphrase, &salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key[..]).map_err(|_| StoreError::Format)?;
        let nonce = Nonce::try_from(&data[HEADER_LEN..HEADER_LEN + NONCE_LEN]).map_err(|_| StoreError::Format)?;
        let plain = cipher
            .decrypt(&nonce, Payload { msg: &data[HEADER_LEN + NONCE_LEN..], aad: &data[..HEADER_LEN] })
            .map_err(|_| StoreError::Decrypt)?;
        let plain = Zeroizing::new(plain);
        serde_json::from_slice(&plain).map(Some).map_err(|_| StoreError::Format)
    }

    pub fn delete(&self) -> Result<(), StoreError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn derive_key(&self, passphrase: Option<&str>, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, StoreError> {
        // Argon2id, 19 MiB, 2 passes, 1 lane (OWASP baseline).
        let params = Params::new(19 * 1024, 2, 1, Some(32)).map_err(|e| StoreError::Kdf(e.to_string()))?;
        let argon = Argon2::new_with_secret(&self.machine_secret, Algorithm::Argon2id, Version::V0x13, params)
            .map_err(|e| StoreError::Kdf(e.to_string()))?;
        let password = passphrase.map(str::as_bytes).unwrap_or(DEFAULT_PASSWORD);
        let mut key = Zeroizing::new([0u8; 32]);
        argon.hash_password_into(password, salt, &mut *key).map_err(|e| StoreError::Kdf(e.to_string()))?;
        Ok(key)
    }
}

fn parse_header(data: &[u8]) -> Result<([u8; SALT_LEN], u8), StoreError> {
    if data.len() < HEADER_LEN || &data[..4] != MAGIC || data[4] != VERSION {
        return Err(StoreError::Format);
    }
    let mode = data[5];
    if mode != MODE_MACHINE && mode != MODE_PASSPHRASE {
        return Err(StoreError::Format);
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&data[6..HEADER_LEN]);
    Ok((salt, mode))
}

/// A secret that ties the file to this machine: the OS machine id, or (if the OS has none) a
/// random key generated once and kept next to the credentials with owner-only permissions.
fn machine_secret(dir: &Path) -> Result<Zeroizing<Vec<u8>>, StoreError> {
    let mut secret = b"NeteaseMusicDownloader/v1|".to_vec();
    match machine_uid::get() {
        Ok(id) if !id.trim().is_empty() => secret.extend_from_slice(id.trim().as_bytes()),
        _ => {
            let path = dir.join(MACHINE_KEY_FILE);
            let key = match fs::read(&path) {
                Ok(k) if k.len() == 32 => k,
                _ => {
                    let mut k = vec![0u8; 32];
                    rand::rng().fill_bytes(&mut k);
                    write_atomic(&path, &k)?;
                    k
                }
            };
            secret.extend_from_slice(&key);
        }
    }
    Ok(Zeroizing::new(secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ncm-secure-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn sample() -> Session {
        let mut s = Session::new();
        s.cookies.insert("MUSIC_U".into(), "super-secret-token".into());
        s
    }

    #[test]
    fn roundtrip_without_passphrase_and_no_plaintext_on_disk() {
        let dir = temp_dir("plain");
        let store = SecureStore::open(&dir).unwrap();
        assert!(store.load(None).unwrap().is_none());
        let original = sample();
        store.save(&original, None).unwrap();
        assert!(!store.needs_passphrase().unwrap());
        let raw = fs::read(store.path()).unwrap();
        assert!(!raw.windows(18).any(|w| w == b"super-secret-token"));
        let back = store.load(None).unwrap().unwrap();
        assert_eq!(back.music_u(), Some("super-secret-token"));
        assert_eq!(back.device.device_id, original.device.device_id);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn passphrase_mode_requires_the_right_passphrase() {
        let dir = temp_dir("pass");
        let store = SecureStore::open(&dir).unwrap();
        store.save(&sample(), Some("correct horse")).unwrap();
        assert!(store.needs_passphrase().unwrap());
        assert!(matches!(store.load(None), Err(StoreError::NeedPassphrase)));
        assert!(matches!(store.load(Some("wrong")), Err(StoreError::Decrypt)));
        assert!(store.load(Some("correct horse")).unwrap().is_some());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tampering_is_detected() {
        let dir = temp_dir("tamper");
        let store = SecureStore::open(&dir).unwrap();
        store.save(&sample(), None).unwrap();
        let mut raw = fs::read(store.path()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        fs::write(store.path(), &raw).unwrap();
        assert!(matches!(store.load(None), Err(StoreError::Decrypt)));
        // Flipping a header byte (authenticated as AAD) is caught too.
        store.save(&sample(), None).unwrap();
        let mut raw = fs::read(store.path()).unwrap();
        raw[8] ^= 0x01;
        fs::write(store.path(), &raw).unwrap();
        assert!(matches!(store.load(None), Err(StoreError::Decrypt)));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn layout_is_aes_gcm_and_every_save_is_fresh() {
        let dir = temp_dir("layout");
        let store = SecureStore::open(&dir).unwrap();
        let session = sample();
        let plain_len = serde_json::to_vec(&session).unwrap().len();

        store.save(&session, None).unwrap();
        let first = fs::read(store.path()).unwrap();
        assert_eq!(&first[..4], MAGIC);
        assert_eq!(first.len(), HEADER_LEN + NONCE_LEN + plain_len + TAG_LEN);

        store.save(&session, None).unwrap();
        let second = fs::read(store.path()).unwrap();
        assert_eq!(first.len(), second.len());
        assert_ne!(first[6..HEADER_LEN], second[6..HEADER_LEN], "salt must change on every save");
        assert_ne!(
            first[HEADER_LEN..HEADER_LEN + NONCE_LEN],
            second[HEADER_LEN..HEADER_LEN + NONCE_LEN],
            "nonce must change on every save"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn garbage_and_delete() {
        let dir = temp_dir("garbage");
        let store = SecureStore::open(&dir).unwrap();
        fs::write(store.path(), b"not a credentials file").unwrap();
        assert!(matches!(store.load(None), Err(StoreError::Format)));
        store.delete().unwrap();
        assert!(!store.exists());
        store.delete().unwrap();
        fs::remove_dir_all(&dir).ok();
    }
}
