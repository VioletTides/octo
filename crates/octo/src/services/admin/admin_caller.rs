//! Who a caller of the admin API is, and how they got past the sign-in gate.

/// How a caller got past the sign-in gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminCallerKind {
    /// A browser or script signed in with a Navidrome admin account.
    Dashboard,
    /// Signed in with the recovery code: settings yes, library files no.
    Recovery,
    /// The Octo app with a Navidrome admin's own Subsonic sign-in.
    NavidromeAdmin,
    /// The Octo app with a listener's Subsonic sign-in: their own stations only.
    Listener,
    /// ADMIN_SIGN_IN is off, so nobody was asked.
    SignInOff,
    /// Signed in on the Family page as a family member.
    FamilyMember,
    /// Signed in on the Family page as someone who manages the family.
    FamilyManager,
}

/// Who the gate let in, kept on the request (as an extension) for the endpoints that answer
/// differently. A request the gate let through without a caller (an open endpoint, signed out)
/// carries none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminCaller {
    pub user: Option<String>,
    pub kind: AdminCallerKind,
}

impl AdminCaller {
    pub fn new(user: Option<String>, kind: AdminCallerKind) -> Self {
        AdminCaller { user, kind }
    }

    /// Everyone but a listener and the Family page's sign-ins may see and change everything
    /// the dashboard shows.
    pub fn is_admin(&self) -> bool {
        !matches!(
            self.kind,
            AdminCallerKind::Listener | AdminCallerKind::FamilyMember | AdminCallerKind::FamilyManager
        )
    }

    /// Who made a change, in words for the log: the audit trail of who saved the settings or
    /// restarted Octo.
    pub fn describe(caller: Option<&AdminCaller>) -> String {
        match caller {
            Some(AdminCaller {
                kind: AdminCallerKind::Recovery,
                ..
            }) => "someone with the recovery code".to_string(),
            Some(AdminCaller {
                kind: AdminCallerKind::SignInOff,
                ..
            }) => "someone (the dashboard sign-in is off)".to_string(),
            Some(AdminCaller {
                kind: AdminCallerKind::NavidromeAdmin,
                user: Some(user),
            }) => format!("{user} (from the Octo app)"),
            Some(AdminCaller { user: Some(user), .. }) => user.clone(),
            _ => "someone".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_names_who_made_a_change() {
        let caller = |user: Option<&str>, kind| AdminCaller::new(user.map(str::to_string), kind);
        let cases = [
            (
                Some(caller(Some("winters"), AdminCallerKind::Dashboard)),
                "winters",
            ),
            (
                Some(caller(Some("(recovery code)"), AdminCallerKind::Recovery)),
                "someone with the recovery code",
            ),
            (
                Some(caller(None, AdminCallerKind::SignInOff)),
                "someone (the dashboard sign-in is off)",
            ),
            (
                Some(caller(Some("winters"), AdminCallerKind::NavidromeAdmin)),
                "winters (from the Octo app)",
            ),
            (Some(caller(Some("bob"), AdminCallerKind::Listener)), "bob"),
            (None, "someone"),
        ];
        for (caller, expected) in cases {
            assert_eq!(AdminCaller::describe(caller.as_ref()), expected);
        }
        assert!(!caller(Some("bob"), AdminCallerKind::Listener).is_admin());
        assert!(caller(None, AdminCallerKind::SignInOff).is_admin());
        assert!(!caller(Some("alex"), AdminCallerKind::FamilyMember).is_admin());
        assert!(!caller(Some("sam"), AdminCallerKind::FamilyManager).is_admin());
    }
}
