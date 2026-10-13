//! Members' account passwords: the one login a member uses everywhere (any Subsonic app, the
//! Octo apps, the `/family/` page and Navidrome's web player). Subsonic's token sign-in needs
//! a value equal to the password, so it is kept sealed with the family key, as Navidrome keeps
//! its own, and never stored or logged in clear.
//!
//! A member from before one login has an older hash here instead, which opens to nothing: they
//! have no password until a manager resets it.

use octo_core::family::codes::constant_time_eq;

use super::vault::{FamilyVault, SEALED_PREFIX};

/// The shortest account password Octo accepts.
pub const MIN_LENGTH: usize = 8;
/// The longest password Octo checks or keeps, in bytes. Anything longer is wrong without being
/// compared.
pub const MAX_BYTES: usize = 1024;

/// Seals a new account password with the family key; None when `family.key` is missing.
pub fn seal(vault: &FamilyVault, password: &str) -> Option<String> {
    vault.seal(password)
}

/// The account password kept in `stored`, opened: None when nothing is kept the sealed way
/// (never set, or a hash from before one login), or `family.key` cannot open it.
pub fn open(vault: &FamilyVault, stored: &str) -> Option<String> {
    if !stored.starts_with(SEALED_PREFIX) {
        return None;
    }
    vault.open_sealed(stored)
}

/// Whether `typed` is the account password kept in `stored`, in time that does not depend on
/// where they differ. A password over [`MAX_BYTES`] matches nothing.
pub fn matches(vault: &FamilyVault, stored: &str, typed: &str) -> bool {
    if typed.len() > MAX_BYTES {
        return false;
    }
    open(vault, stored).is_some_and(|password| constant_time_eq(password.as_bytes(), typed.as_bytes()))
}

/// Whether a new account password is long enough, with the words the member reads when not.
pub fn acceptable(password: &str) -> Result<(), &'static str> {
    if password.chars().count() < MIN_LENGTH {
        return Err("Use at least 8 characters.");
    }
    if password.len() > MAX_BYTES {
        return Err("Use a shorter password: at most 1024 characters.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> ((), FamilyVault) {
        ((), FamilyVault::in_memory())
    }

    #[test]
    fn a_sealed_password_matches_only_itself() {
        let (_dir, vault) = vault();
        let sealed = seal(&vault, "correct horse").expect("sealed");
        assert!(sealed.starts_with(SEALED_PREFIX));
        assert!(!sealed.contains("correct horse"));
        assert!(matches(&vault, &sealed, "correct horse"));
        assert!(!matches(&vault, &sealed, "Correct horse"));
        assert!(!matches(&vault, &sealed, ""));
        assert_eq!(open(&vault, &sealed).as_deref(), Some("correct horse"));
    }

    #[test]
    fn a_hash_from_before_one_login_opens_to_nothing() {
        let (_dir, vault) = vault();
        let old = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA";
        assert_eq!(open(&vault, old), None);
        assert!(!matches(&vault, old, "correct horse"));
        assert!(!matches(&vault, "", "correct horse"));
    }

    #[test]
    fn short_password_is_refused() {
        assert!(acceptable("1234567").is_err());
        assert!(acceptable("12345678").is_ok());
        assert!(acceptable("").is_err());
    }

    #[test]
    fn a_password_over_the_cap_is_refused_and_never_matches() {
        let (_dir, vault) = vault();
        let long = "x".repeat(MAX_BYTES + 1);
        assert!(acceptable(&long).is_err());
        assert!(acceptable(&"x".repeat(MAX_BYTES)).is_ok());
        // Sealed directly, the long password still never matches.
        let sealed = seal(&vault, &long).expect("sealed");
        assert!(!matches(&vault, &sealed, &long));
    }
}
