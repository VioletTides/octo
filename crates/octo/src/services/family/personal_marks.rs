//! Members' own songs, marked. In every answer to a member, a song from that member's own
//! library carries `"octoPersonal": true` (XML `octoPersonal="true"`), so apps can offer
//! "Remove from my library" on it alone. Shared songs carry nothing, and so does every answer
//! to the owner and to accounts outside the family.

use super::caller::FamilyCaller;
use super::family_store::FamilyStore;

/// The caller's own library, when the caller is a member who has one.
pub fn personal_library(store: &FamilyStore, caller: &FamilyCaller) -> Option<i64> {
    if !caller.managed {
        return None;
    }
    store.member(&caller.user)?.personal_library_id
}

/// Whether a song in `library` is in the caller's own library `personal`.
pub fn is_personal(library: Option<i64>, personal: Option<i64>) -> bool {
    personal.is_some() && library == personal
}

#[cfg(test)]
mod tests {
    use super::*;
    use octo_core::family::abilities::{Abilities, Preset};

    #[test]
    fn only_a_members_own_library_is_personal() {
        let store = FamilyStore::in_memory();
        let mut alex = crate::services::family::member_credentials::tests::member("alex", String::new());
        alex.personal_library_id = Some(3);
        store.upsert_member(alex);
        store.set_owner("winters");
        let member = FamilyCaller::member("alex", Preset::Member, Abilities::preset(Preset::Member));
        let owner = FamilyCaller::unmanaged("winters", true);
        assert_eq!(personal_library(&store, &member), Some(3));
        assert_eq!(personal_library(&store, &owner), None);
        assert!(is_personal(Some(3), Some(3)));
        assert!(!is_personal(Some(1), Some(3)));
        assert!(!is_personal(None, Some(3)));
        assert!(!is_personal(None, None));
    }
}
