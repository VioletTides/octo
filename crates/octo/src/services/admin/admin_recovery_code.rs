//! The dashboard's recovery code, kept in `admin-recovery-code`.

use std::path::{Path, PathBuf};

use octo_core::common::dotnet::{is_blank, is_letter_or_digit, to_upper_char};
use parking_lot::Mutex;
use tracing::warn;

/// The way into the dashboard when Navidrome cannot vouch for anyone: its URL is wrong or not set
/// yet, or it is down and no browser is still signed in. A random code in a file beside the
/// settings, readable only by whoever can open the Octo host or container, which is the person who
/// owns Octo anyway. Each code works once; a new one is written as soon as it is used.
///
/// The file holds the code and a line end (`ABCDE-FGHJK-LMNPQ-RSTUV\n`), written through a
/// temporary file and made readable by its owner alone where the platform allows.
pub struct AdminRecoveryCode {
    path: PathBuf,
    lock: Mutex<()>,
}

/// The letters a code is made of: no I, O, 0 or 1, which read alike.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

impl AdminRecoveryCode {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        AdminRecoveryCode {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn file_path(&self) -> &Path {
        &self.path
    }

    /// The code in the file, written first if there is none. None when it cannot be written.
    pub fn current(&self) -> Option<String> {
        let _held = self.lock.lock();
        self.current_locked()
    }

    fn current_locked(&self) -> Option<String> {
        let read = match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        };
        let outcome = read.and_then(|saved| match saved.as_deref().map(str::trim) {
            Some(saved) if !saved.is_empty() => Ok(saved.to_string()),
            _ => self.write(&new_code()),
        });
        match outcome {
            Ok(code) => Some(code),
            Err(e) => {
                warn!(
                    "The recovery code could not be read or written at {}: {e}",
                    self.path.display()
                );
                None
            }
        }
    }

    /// True when the typed code is the current one, which is then replaced.
    pub fn try_use(&self, typed: Option<&str>) -> bool {
        let _held = self.lock.lock();
        let Some(typed) = typed.filter(|t| !is_blank(t)) else {
            return false;
        };
        let Some(current) = self.current_locked() else {
            return false;
        };
        if !fixed_time_equals(normal(&current).as_bytes(), normal(typed).as_bytes()) {
            return false;
        }
        if let Err(e) = self.write(&new_code()) {
            warn!("A used recovery code could not be replaced: {e}");
        }
        true
    }

    fn write(&self, code: &str) -> std::io::Result<String> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut temp = self.path.clone().into_os_string();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, format!("{code}\n"))?;
        // Best effort: a bind mount that refuses chmod must not make the way back in disappear.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600)) {
                warn!("The recovery code file could not be made private: {e}");
            }
        }
        std::fs::rename(&temp, &self.path)?;
        Ok(code.to_string())
    }
}

/// Typed by hand, so spaces, dashes and case do not matter.
fn normal(code: &str) -> String {
    code.chars()
        .filter(|c| is_letter_or_digit(*c))
        .map(to_upper_char)
        .collect()
}

/// Four groups of five letters from [`ALPHABET`], joined with dashes, drawn by the operating
/// system's random source.
fn new_code() -> String {
    let raw: Vec<char> = (0..20)
        .map(|_| char::from(ALPHABET[rand::random_range(0..ALPHABET.len())]))
        .collect();
    raw.chunks(5)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// A constant-time compare: false for different lengths, otherwise a compare whose time does not
/// depend on where the bytes differ.
fn fixed_time_equals(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("config").join("admin-recovery-code");
        (dir, path)
    }

    #[test]
    fn a_code_is_written_once_and_read_back() {
        let (_dir, path) = temp();
        let recovery = AdminRecoveryCode::new(&path);
        let code = recovery.current().expect("a code");
        assert_eq!(code.len(), 23);
        assert_eq!(code.split('-').count(), 4);
        assert!(code.chars().all(|c| c == '-' || ALPHABET.contains(&(c as u8))));
        assert_eq!(
            std::fs::read_to_string(&path).expect("reads"),
            format!("{code}\n")
        );
        assert_eq!(recovery.current().as_deref(), Some(code.as_str()));
        // A second store over the same file reads the same code.
        assert_eq!(AdminRecoveryCode::new(&path).current(), Some(code));
    }

    #[test]
    fn the_code_works_once_however_it_is_typed() {
        let (_dir, path) = temp();
        let recovery = AdminRecoveryCode::new(&path);
        let code = recovery.current().expect("a code");
        let loose = code.to_lowercase().replace('-', " ");

        assert!(!recovery.try_use(None));
        assert!(!recovery.try_use(Some("   ")));
        assert!(!recovery.try_use(Some("AAAAA-AAAAA")));
        assert!(recovery.try_use(Some(&loose)));
        assert_ne!(recovery.current().as_deref(), Some(code.as_str()));
        assert!(!recovery.try_use(Some(&code)));
    }

    #[test]
    fn a_code_written_by_hand_is_used_as_it_is() {
        let (_dir, path) = temp();
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("creates");
        std::fs::write(&path, "  my-own-code \n").expect("writes");
        let recovery = AdminRecoveryCode::new(&path);
        assert_eq!(recovery.current().as_deref(), Some("my-own-code"));
        assert!(recovery.try_use(Some("MYOWNCODE")));
    }

    /// The fixture is the file as written: the code and a line end.
    #[test]
    fn the_fixture_reads_as_its_code() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/rust-migration/fixtures/state/admin-recovery-code"
        );
        let (_dir, path) = temp();
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("creates");
        std::fs::copy(fixture, &path).expect("copies");
        let recovery = AdminRecoveryCode::new(&path);
        assert_eq!(recovery.current().as_deref(), Some("ABCDE-FGHJK-LMNPQ-RSTUV"));
        assert!(recovery.try_use(Some("abcde fghjk lmnpq rstuv")));
        let written = std::fs::read_to_string(&path).expect("reads");
        assert_eq!(written.len(), 24, "a new code and a line end: {written:?}");
        assert!(written.ends_with('\n'));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_readable_by_its_owner_alone() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = temp();
        AdminRecoveryCode::new(&path).current().expect("a code");
        let mode = std::fs::metadata(&path).expect("exists").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_folder_that_cannot_hold_the_file_gives_no_code() {
        let (_dir, path) = temp();
        // A folder where the file should be.
        std::fs::create_dir_all(&path).expect("creates");
        let recovery = AdminRecoveryCode::new(&path);
        assert_eq!(recovery.current(), None);
        assert!(!recovery.try_use(Some("anything")));
    }
}
