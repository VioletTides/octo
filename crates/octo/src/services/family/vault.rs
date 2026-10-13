//! `family.key` and the secrets sealed with it. The key file is 64 random bytes: the first 32
//! seal device secrets and members' hidden Navidrome passwords (ChaCha20-Poly1305), the second
//! 32 sign the tokens Octo gives members' browsers. A copy of `family.json` alone (a backup, a
//! support bundle) opens nothing.

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, Generate, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use parking_lot::RwLock;
use tracing::warn;

/// What every sealed value starts with.
pub const SEALED_PREFIX: &str = "sealed:";

/// The key file's name in the config folder.
pub const KEY_FILE: &str = "family.key";

const NONCE_LEN: usize = 12;

struct Keys {
    seal: ChaCha20Poly1305,
    jwt: [u8; 32],
}

/// Seals and opens family secrets under `family.key`. Without the key file nothing seals or
/// opens; the key is made only when Family is turned on.
pub struct FamilyVault {
    path: Option<PathBuf>,
    keys: RwLock<Option<Keys>>,
}

impl FamilyVault {
    /// A vault over `<config_dir>/family.key`, loaded when the file is there. Never creates it.
    pub fn open(config_dir: &Path) -> Self {
        let path = config_dir.join(KEY_FILE);
        let keys = match std::fs::read(&path) {
            Ok(bytes) => match keys_from(&bytes) {
                Some(keys) => Some(keys),
                None => {
                    warn!(
                        "FAMILY {} is not a Family key file ({} bytes, not 64); nothing sealed opens",
                        path.display(),
                        bytes.len()
                    );
                    None
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                warn!("FAMILY {} could not be read: {e}", path.display());
                None
            }
        };
        FamilyVault {
            path: Some(path),
            keys: RwLock::new(keys),
        }
    }

    /// A vault with a key that lives in memory only, for tests.
    pub fn in_memory() -> Self {
        FamilyVault {
            path: None,
            keys: RwLock::new(keys_from(&new_key_bytes())),
        }
    }

    /// A vault with no key at all, for tests.
    pub fn without_key() -> Self {
        FamilyVault {
            path: None,
            keys: RwLock::new(None),
        }
    }

    /// Whether a key is loaded.
    pub fn has_key(&self) -> bool {
        self.keys.read().is_some()
    }

    /// Makes `family.key` (readable by its owner only) when there is none yet. Answers whether
    /// a key is loaded afterwards; an existing key file is never replaced.
    pub fn create_key_if_missing(&self) -> io::Result<bool> {
        let mut keys = self.keys.write();
        if keys.is_some() {
            return Ok(true);
        }
        let bytes = new_key_bytes();
        if let Some(path) = &self.path {
            if path.exists() {
                // A file that did not load is a key someone may still need: never overwrite it.
                return Ok(false);
            }
            write_private(path, &bytes)?;
        }
        *keys = keys_from(&bytes);
        Ok(keys.is_some())
    }

    /// `sealed:` + base64(nonce ‖ ciphertext), or None without a key.
    pub fn seal(&self, plain: &str) -> Option<String> {
        let keys = self.keys.read();
        let keys = keys.as_ref()?;
        let nonce = Nonce::generate();
        let sealed = keys.seal.encrypt(&nonce, plain.as_bytes()).ok()?;
        let mut bytes = Vec::with_capacity(NONCE_LEN + sealed.len());
        bytes.extend_from_slice(nonce.as_slice());
        bytes.extend_from_slice(&sealed);
        Some(format!("{SEALED_PREFIX}{}", STANDARD.encode(bytes)))
    }

    /// The plain text of a sealed value; None without a key, for anything changed, or for
    /// anything that is not a sealed value.
    pub fn open_sealed(&self, sealed: &str) -> Option<String> {
        let keys = self.keys.read();
        let keys = keys.as_ref()?;
        let bytes = STANDARD.decode(sealed.strip_prefix(SEALED_PREFIX)?).ok()?;
        if bytes.len() < NONCE_LEN {
            return None;
        }
        let (nonce, text) = bytes.split_at(NONCE_LEN);
        let nonce = Nonce::try_from(nonce).ok()?;
        let plain = keys.seal.decrypt(&nonce, text).ok()?;
        String::from_utf8(plain).ok()
    }

    /// The key that signs members' browser tokens (the key file's second half), or None
    /// without a key.
    pub fn jwt_key(&self) -> Option<[u8; 32]> {
        self.keys.read().as_ref().map(|keys| keys.jwt)
    }
}

/// 64 fresh random bytes from the operating system.
fn new_key_bytes() -> [u8; 64] {
    let mut bytes = [0u8; 64];
    bytes[..32].copy_from_slice(Key::generate().as_slice());
    bytes[32..].copy_from_slice(Key::generate().as_slice());
    bytes
}

fn keys_from(bytes: &[u8]) -> Option<Keys> {
    if bytes.len() != 64 {
        return None;
    }
    let seal = ChaCha20Poly1305::new_from_slice(&bytes[..32]).ok()?;
    let mut jwt = [0u8; 32];
    jwt.copy_from_slice(&bytes[32..]);
    Some(Keys { seal, jwt })
}

/// Writes `<path>.tmp` readable by its owner only, then renames it over `path`.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let temp = crate::services::state_file::temp_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        // The mode above only applies to a new file; a leftover temp keeps its own.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_round_trips() {
        let vault = FamilyVault::in_memory();
        let sealed = vault.seal("hidden password").expect("a key");
        assert!(sealed.starts_with(SEALED_PREFIX));
        assert!(!sealed.contains("hidden"));
        assert_eq!(vault.open_sealed(&sealed).as_deref(), Some("hidden password"));
        // A fresh nonce every time.
        assert_ne!(
            vault.seal("same").expect("a key"),
            vault.seal("same").expect("a key")
        );
    }

    #[test]
    fn tampered_text_does_not_open() {
        let vault = FamilyVault::in_memory();
        let sealed = vault.seal("secret").expect("a key");
        let mut bytes = STANDARD
            .decode(sealed.strip_prefix(SEALED_PREFIX).expect("prefix"))
            .expect("base64");
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let tampered = format!("{SEALED_PREFIX}{}", STANDARD.encode(bytes));
        assert_eq!(vault.open_sealed(&tampered), None);
        assert_eq!(vault.open_sealed("secret"), None);
        assert_eq!(vault.open_sealed("sealed:!!!"), None);
        assert_eq!(vault.open_sealed("sealed:AAAA"), None);
        // Another key opens nothing.
        assert_eq!(FamilyVault::in_memory().open_sealed(&sealed), None);
    }

    #[test]
    fn missing_key_opens_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let vault = FamilyVault::open(dir.path());
        assert!(!vault.has_key());
        assert_eq!(vault.seal("x"), None);
        assert_eq!(vault.jwt_key(), None);
        let sealed = FamilyVault::in_memory().seal("x").expect("a key");
        assert_eq!(vault.open_sealed(&sealed), None);
        assert!(
            !dir.path().join(KEY_FILE).exists(),
            "opening never creates the key"
        );
        assert_eq!(FamilyVault::without_key().seal("x"), None);
    }

    #[test]
    fn the_key_survives_a_restart_and_has_two_halves() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let vault = FamilyVault::open(dir.path());
        assert!(vault.create_key_if_missing().expect("creates"));
        assert_eq!(std::fs::read(dir.path().join(KEY_FILE)).expect("reads").len(), 64);
        let sealed = vault.seal("hidden").expect("a key");
        let jwt = vault.jwt_key().expect("a jwt key");

        let again = FamilyVault::open(dir.path());
        assert_eq!(again.open_sealed(&sealed).as_deref(), Some("hidden"));
        assert_eq!(again.jwt_key(), Some(jwt));
        // The signing half is not the sealing half.
        let bytes = std::fs::read(dir.path().join(KEY_FILE)).expect("reads");
        assert_eq!(&bytes[32..], &jwt[..]);
        assert_ne!(&bytes[..32], &jwt[..]);
        // Creating again keeps the key.
        assert!(again.create_key_if_missing().expect("keeps"));
        assert_eq!(std::fs::read(dir.path().join(KEY_FILE)).expect("reads"), bytes);
    }

    #[test]
    fn a_bad_key_file_is_never_replaced() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join(KEY_FILE), b"short").expect("writes");
        let vault = FamilyVault::open(dir.path());
        assert!(!vault.has_key());
        assert!(!vault.create_key_if_missing().expect("refuses quietly"));
        assert_eq!(std::fs::read(dir.path().join(KEY_FILE)).expect("reads"), b"short");
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temp dir");
        let vault = FamilyVault::open(dir.path());
        vault.create_key_if_missing().expect("creates");
        let mode = std::fs::metadata(dir.path().join(KEY_FILE))
            .expect("exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
