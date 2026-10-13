//! Who a request is from, as the family gate worked it out. The gate puts a [`FamilyCaller`]
//! on every request it identifies while Family is on; handlers read it with
//! [`family_caller`]. With Family off there is never one. A member's caller is always checked;
//! anyone else's names who the request says it is from, and the gate's reading of the request
//! is what checks it (`GateReading::verify`).

use std::net::IpAddr;
use std::sync::Arc;

use octo_core::family::abilities::{Abilities, Preset, Role};
use octo_core::family::device::NetworkPlace;
use octo_core::family::quality::{DeviceQualityMode, QualityPreference};

/// The person behind one request.
#[derive(Debug, Clone, PartialEq)]
pub struct FamilyCaller {
    /// The Navidrome username, spelled as Octo keeps it.
    pub user: String,
    pub role: Role,
    /// What this person may do. The owner and accounts outside the family get the owner's
    /// abilities, and no rule applies to them (`managed` is false).
    pub abilities: Arc<Abilities>,
    /// The signed-in device of a member, or the detected device of anyone else. None when no
    /// device could be told apart (an API key, a radio stream).
    pub device_id: Option<String>,
    pub place: NetworkPlace,
    /// Whether Octo manages this account (a member): family rules apply only then.
    pub managed: bool,
    /// The client address the place was read from.
    pub address: IpAddr,
    /// The audio quality the account chose, for home and away.
    pub quality: QualityPreference,
    /// Whether this device follows the account's quality or lets its app decide.
    pub device_quality: DeviceQualityMode,
}

impl FamilyCaller {
    /// The owner, or an account outside the family: everything as it is today.
    pub fn unmanaged(user: &str, owner: bool) -> Self {
        FamilyCaller {
            user: user.to_string(),
            role: if owner { Role::Owner } else { Role::Unmanaged },
            abilities: Arc::new(Abilities::preset(Preset::Owner)),
            device_id: None,
            place: NetworkPlace::Home,
            managed: false,
            address: IpAddr::from([0, 0, 0, 0]),
            quality: QualityPreference::default(),
            device_quality: DeviceQualityMode::Account,
        }
    }

    /// A member Octo manages, with their abilities.
    pub fn member(user: &str, preset: Preset, abilities: Abilities) -> Self {
        FamilyCaller {
            role: preset.into(),
            abilities: Arc::new(abilities),
            managed: true,
            ..FamilyCaller::unmanaged(user, false)
        }
    }

    /// The account's quality for the place this request comes from, when the device follows
    /// the account; None when the app decides.
    pub fn account_quality(&self) -> Option<octo_core::family::quality::StreamQuality> {
        match self.device_quality {
            DeviceQualityMode::Account => Some(self.quality.for_place(self.place)),
            DeviceQualityMode::App => None,
        }
    }
}

/// The caller the family gate found for this request, if any.
pub fn family_caller(extensions: &http::Extensions) -> Option<&FamilyCaller> {
    extensions.get::<FamilyCaller>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use octo_core::family::quality::StreamQuality;

    #[test]
    fn the_caller_is_read_from_the_request() {
        let mut request = http::Request::new(());
        assert!(family_caller(request.extensions()).is_none());
        let mut caller = FamilyCaller::member("alex", Preset::Listener, Abilities::preset(Preset::Listener));
        caller.place = NetworkPlace::Away;
        caller.quality = QualityPreference {
            home: StreamQuality::Original,
            away: StreamQuality::Standard,
        };
        request.extensions_mut().insert(caller.clone());
        let found = family_caller(request.extensions()).expect("there");
        assert_eq!(found.role, Role::Listener);
        assert!(found.managed);
        assert_eq!(found.account_quality(), Some(StreamQuality::Standard));
        caller.device_quality = DeviceQualityMode::App;
        assert_eq!(caller.account_quality(), None);
        let owner = FamilyCaller::unmanaged("winters", true);
        assert_eq!(owner.role, Role::Owner);
        assert!(!owner.managed);
    }
}
