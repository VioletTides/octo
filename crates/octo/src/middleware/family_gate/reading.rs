//! What the family gate read from one request, kept on it as an extension so every handler
//! works from the same reading: the parameters, which sign-in keys were there, and who the
//! request is from. A [`Verified`] caller is only ever made here: a member's or an API key's
//! when the gate checked it, anyone else's once Navidrome takes exactly the sign-in the gate
//! read.

use std::sync::Arc;

use indexmap::IndexMap;
use octo_core::common::dotnet::eq_ignore_case;
use octo_subsonic::SubsonicCredential;
use octo_subsonic::subsonic_request_parser::{Parameters, RequestReading, StringValuesCollection};
use tokio::sync::OnceCell;

use crate::app::AppState;
use crate::services::family::member_bridge::SIGN_IN_KEYS;
use crate::services::family::{FamilyCaller, Sighting};
use crate::services::subsonic::CredentialVerdict;

/// A caller whose sign-in was checked. Only the family gate makes one.
#[derive(Debug, Clone)]
pub struct Verified {
    caller: FamilyCaller,
}

impl Verified {
    pub(super) fn new(caller: FamilyCaller) -> Self {
        Verified { caller }
    }

    /// Who the checked sign-in is.
    pub fn caller(&self) -> &FamilyCaller {
        &self.caller
    }
}

/// Why a request's caller could not be checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unverified {
    /// The request named nobody.
    Missing,
    /// The sign-in is wrong, or names more than one person.
    Refused,
    /// Navidrome did not answer the check.
    Unreachable,
}

/// The family gate's reading of a request.
#[derive(Debug, Clone)]
pub struct GateReading(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    reading: RequestReading,
    parameters: Parameters,
    keys: Vec<&'static str>,
    caller: Option<FamilyCaller>,
    /// The sign-in exactly as read, for Navidrome to check an unchecked caller.
    credential: Option<SubsonicCredential>,
    seen: Sighting,
    checked: OnceCell<Result<Verified, Unverified>>,
}

impl GateReading {
    /// A reading whose caller (if any) is not checked yet. `credential` is the sign-in that
    /// checks it.
    pub(super) fn unchecked(
        reading: RequestReading,
        parameters: Parameters,
        caller: Option<FamilyCaller>,
        credential: Option<SubsonicCredential>,
        seen: Sighting,
    ) -> Self {
        GateReading(Arc::new(Inner {
            keys: keys_in(&reading),
            reading,
            parameters,
            caller,
            credential,
            seen,
            checked: OnceCell::new(),
        }))
    }

    /// A reading whose caller the gate checked itself.
    pub(super) fn checked(reading: RequestReading, parameters: Parameters, caller: FamilyCaller) -> Self {
        GateReading(Arc::new(Inner {
            keys: keys_in(&reading),
            reading,
            parameters,
            caller: Some(caller.clone()),
            credential: None,
            seen: Sighting::default(),
            checked: OnceCell::new_with(Some(Ok(Verified::new(caller)))),
        }))
    }

    /// The gate's reading on this request, if the gate read it.
    pub fn of(extensions: &axum::http::Extensions) -> Option<&GateReading> {
        extensions.get::<GateReading>()
    }

    /// The parameters every handler reads: the query, then the form or JSON body, after the
    /// gate's rewrites.
    pub fn parameters(&self) -> &Parameters {
        &self.0.parameters
    }

    /// The query's values.
    pub fn query(&self) -> &StringValuesCollection {
        &self.0.reading.query
    }

    /// The form's fields, when the body is a form.
    pub fn form(&self) -> Option<&StringValuesCollection> {
        self.0.reading.form()
    }

    /// Every occurrence of a parameter from the query and then a form, blank ones skipped.
    pub fn parameter_values(&self, name: &str) -> Vec<String> {
        self.0.reading.parameter_values(name)
    }

    /// The sign-in keys the request carried, in the spelling Subsonic gives them.
    pub fn sign_in_keys(&self) -> &[&'static str] {
        &self.0.keys
    }

    /// Who the request says it is from, checked or not.
    pub fn caller(&self) -> Option<&FamilyCaller> {
        self.0.caller.as_ref()
    }

    /// Whether the caller is checked already.
    pub fn is_checked(&self) -> bool {
        matches!(self.0.checked.get(), Some(Ok(_)))
    }

    /// The checked caller. A caller the gate did not check is checked now, once, by Navidrome
    /// with exactly the sign-in the gate read (a cached ping), and their device is noted when
    /// it checks out.
    pub async fn verify(&self, state: &AppState) -> Result<Verified, Unverified> {
        self.0.checked.get_or_init(|| self.check(state)).await.clone()
    }

    async fn check(&self, state: &AppState) -> Result<Verified, Unverified> {
        let Some(caller) = self.0.caller.as_ref() else {
            return Err(if self.0.keys.contains(&"u") {
                Unverified::Refused
            } else {
                Unverified::Missing
            });
        };
        let Some(credential) = self.0.credential.as_ref() else {
            return Err(Unverified::Refused);
        };
        match state
            .credential_check
            .check(Some(credential), &state.subsonic_proxy)
            .await
        {
            Ok(CredentialVerdict::Accepted) => {
                state.family_devices.seen(caller, &self.0.seen);
                Ok(Verified::new(caller.clone()))
            }
            Ok(CredentialVerdict::Refused) => Err(Unverified::Refused),
            Ok(CredentialVerdict::Unreachable) | Err(_) => Err(Unverified::Unreachable),
        }
    }
}

/// The sign-in keys a reading carries anywhere, in any case.
fn keys_in(reading: &RequestReading) -> Vec<&'static str> {
    let body = reading.body_values();
    SIGN_IN_KEYS
        .iter()
        .copied()
        .filter(|key| {
            reading.query.get(key).is_some() || body.as_ref().is_some_and(|values| values.get(key).is_some())
        })
        .collect()
}

/// The sign-in exactly as read, for Navidrome to check: each sign-in key with its one value
/// from the query or the body, and `v` and `c`. It is the sign-in Navidrome then reads on the
/// call itself, or None:
///
/// - when a key carries two different values, or an empty value beside a real one (Navidrome
///   reads a key's first value, which could be either);
/// - when a key is first written in another spelling than Subsonic's (`U`, `P`) in the query
///   or the body: the relay sends a key's values in the spelling it was first sent with, and
///   Navidrome reads keys in their own spelling only;
/// - when `u` is not exactly `user` (or is there at all when `user` is None).
pub fn credential_of(
    reading: &RequestReading,
    parameters: &Parameters,
    user: Option<&str>,
) -> Option<SubsonicCredential> {
    let body = reading.body_values();
    let mut values: IndexMap<String, String> = IndexMap::new();
    for key in SIGN_IN_KEYS {
        let mut found: Vec<&String> = Vec::new();
        let mut empty = false;
        let sources = [Some(&reading.query), body.as_ref()];
        for source in sources.into_iter().flatten() {
            let Some((spelling, sent)) = source.iter().find(|(name, _)| eq_ignore_case(name, key)) else {
                continue;
            };
            if spelling != key {
                return None;
            }
            for value in sent {
                if value.is_empty() {
                    empty = true;
                } else if !found.contains(&value) {
                    found.push(value);
                }
            }
        }
        match found.as_slice() {
            [] => {}
            [_] if empty => return None,
            [value] => {
                values.insert(key.to_string(), (*value).clone());
            }
            _ => return None,
        }
    }
    match user {
        Some(user) if values.get("u").map(String::as_str) != Some(user) => return None,
        None if values.contains_key("u") => return None,
        _ => {}
    }
    for key in ["v", "c"] {
        if let Some((_, value)) = parameters.iter().find(|(name, _)| eq_ignore_case(name, key)) {
            values.insert(key.to_string(), value.clone());
        }
    }
    SubsonicCredential::from(&values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use octo_subsonic::subsonic_request_parser::{RequestParts, read_request};

    fn read(query: &str, content_type: Option<&str>, body: &[u8]) -> (RequestReading, Parameters) {
        let reading = read_request(&RequestParts {
            query: Some(query),
            content_type,
            content_length: Some(body.len() as u64),
            body,
        });
        let parameters = reading.parameters();
        (reading, parameters)
    }

    #[test]
    fn the_credential_carries_exactly_the_sign_in_that_was_read() {
        let (reading, parameters) = read("u=winters&p=pw&v=1.16.1&c=App&id=1", None, b"");
        let credential = credential_of(&reading, &parameters, Some("winters")).expect("one sign-in");
        let sent = credential.parameters(&[]);
        assert_eq!(sent.get("u").map(String::as_str), Some("winters"));
        assert_eq!(sent.get("p").map(String::as_str), Some("pw"));
        assert_eq!(sent.get("c").map(String::as_str), Some("App"));
        assert!(!sent.contains_key("id"));
    }

    #[test]
    fn a_sign_in_split_between_query_and_body_is_read_whole() {
        let form = Some("application/x-www-form-urlencoded");
        let (reading, parameters) = read("u=winters", form, b"p=pw");
        let credential = credential_of(&reading, &parameters, Some("winters")).expect("one sign-in");
        assert_eq!(
            credential.parameters(&[]).get("p").map(String::as_str),
            Some("pw")
        );
        assert_eq!(keys_in(&reading), ["u", "p"]);
    }

    #[test]
    fn a_sign_in_navidrome_would_read_differently_checks_nothing() {
        let form = Some("application/x-www-form-urlencoded");
        let json = Some("application/json");
        // Another spelling first, in the query or the body: Navidrome reads no `u` or `p`.
        for (query, content_type, body) in [
            ("U=winters&p=pw", None, &b""[..]),
            ("u=winters&P=pw", None, b""),
            ("U=winters&u=winters&p=pw", None, b""),
            ("u=winters&p=pw", form, b"P=pw"),
            ("", json, br#"{"U":"winters","p":"pw"}"#),
            // An empty value beside a real one: Navidrome may read the empty one.
            ("u=&u=winters&p=pw", None, b""),
            ("u=winters&p=&p=pw", None, b""),
            ("u=winters&p=pw&p=", None, b""),
            ("u=winters&p=pw", form, b"p="),
        ] {
            let (reading, parameters) = read(query, content_type, body);
            assert!(
                credential_of(&reading, &parameters, Some("winters")).is_none(),
                "{query} {:?}",
                String::from_utf8_lossy(body)
            );
        }
        // The same value twice in Subsonic's spelling, or a key only ever empty, is read alike.
        for (query, content_type, body) in [
            ("u=winters&p=pw&p=pw", None, &b""[..]),
            ("u=winters&u=winters&p=pw", None, b""),
            ("u=winters&p=pw", form, b"p=pw"),
            ("u=winters&p=pw&U=winters", None, b""),
            ("u=winters&p=pw&t=", None, b""),
            ("%75=winters&%70=pw", None, b""),
            ("", json, br#"{"u":"winters","p":"pw"}"#),
        ] {
            let (reading, parameters) = read(query, content_type, body);
            assert!(
                credential_of(&reading, &parameters, Some("winters")).is_some(),
                "{query} {:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn two_different_values_or_another_name_check_nothing() {
        let form = Some("application/x-www-form-urlencoded");
        let (reading, parameters) = read("u=winters&p=pw", form, b"p=other");
        assert!(credential_of(&reading, &parameters, Some("winters")).is_none());
        let (reading, parameters) = read("U=winters&u=guest&p=pw", None, b"");
        assert!(credential_of(&reading, &parameters, Some("winters")).is_none());
        let (reading, parameters) = read("u=guest&p=pw", None, b"");
        assert!(credential_of(&reading, &parameters, Some("winters")).is_none());
        let (reading, parameters) = read("u=%20winters&p=pw", None, b"");
        assert!(credential_of(&reading, &parameters, Some("winters")).is_none());
        // A token alone names nobody, and must not carry a `u` either.
        let (reading, parameters) = read("jwt=abc", None, b"");
        assert!(credential_of(&reading, &parameters, None).is_some());
        let (reading, parameters) = read("jwt=abc&u=guest", None, b"");
        assert!(credential_of(&reading, &parameters, None).is_none());
    }
}
