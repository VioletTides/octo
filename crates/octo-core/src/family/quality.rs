//! Personal audio quality: what each account asks for at home and away, and whether a device
//! follows that choice or lets its app decide. Every account has one, the owner and accounts
//! outside the family included; the family limits in [`super::abilities`] are separate and
//! only ever lower it.

use serde::{Deserialize, Serialize};

use super::device::NetworkPlace;

/// The format and bitrate Octo asks Navidrome for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StreamQuality {
    /// The file as it is: nothing is asked for.
    #[default]
    Original,
    /// Opus at 256 kbps.
    High,
    /// Opus at 160 kbps.
    Standard,
    /// Opus at 96 kbps.
    DataSaver,
}

impl StreamQuality {
    /// The Opus bitrate this quality asks for, or None for the original.
    pub fn kbps(self) -> Option<u32> {
        match self {
            StreamQuality::Original => None,
            StreamQuality::High => Some(256),
            StreamQuality::Standard => Some(160),
            StreamQuality::DataSaver => Some(96),
        }
    }

    /// The name as the API and `family.json` write it.
    pub fn label(self) -> &'static str {
        match self {
            StreamQuality::Original => "Original",
            StreamQuality::High => "High",
            StreamQuality::Standard => "Standard",
            StreamQuality::DataSaver => "DataSaver",
        }
    }

    /// A quality by name, ignoring case and surrounding spaces.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        [
            StreamQuality::Original,
            StreamQuality::High,
            StreamQuality::Standard,
            StreamQuality::DataSaver,
        ]
        .into_iter()
        .find(|q| q.label().eq_ignore_ascii_case(text))
    }
}

/// An account's own choice of quality at home and away. Original for both until the person
/// picks something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct QualityPreference {
    pub home: StreamQuality,
    pub away: StreamQuality,
}

impl QualityPreference {
    /// The quality chosen for `place`.
    pub fn for_place(&self, place: NetworkPlace) -> StreamQuality {
        match place {
            NetworkPlace::Home => self.home,
            NetworkPlace::Away => self.away,
        }
    }
}

/// How one device picks its quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DeviceQualityMode {
    /// Octo applies the account's choice for the place the device is in.
    #[default]
    Account,
    /// The app decides; Octo applies only the family limit.
    App,
}

impl DeviceQualityMode {
    /// Whether this is the default, so `family.json` leaves it out.
    pub fn is_account(&self) -> bool {
        *self == DeviceQualityMode::Account
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualities_ask_for_their_bitrates() {
        assert_eq!(StreamQuality::Original.kbps(), None);
        assert_eq!(StreamQuality::High.kbps(), Some(256));
        assert_eq!(StreamQuality::Standard.kbps(), Some(160));
        assert_eq!(StreamQuality::DataSaver.kbps(), Some(96));
        assert_eq!(
            StreamQuality::parse(" datasaver "),
            Some(StreamQuality::DataSaver)
        );
        assert_eq!(StreamQuality::parse("lossless"), None);
    }

    #[test]
    fn preferences_default_to_original_and_read_by_place() {
        let preference = QualityPreference::default();
        assert_eq!(preference.home, StreamQuality::Original);
        assert_eq!(preference.away, StreamQuality::Original);
        let chosen = QualityPreference {
            home: StreamQuality::High,
            away: StreamQuality::Standard,
        };
        assert_eq!(chosen.for_place(NetworkPlace::Away), StreamQuality::Standard);
        assert_eq!(
            serde_json::to_string(&chosen).expect("writes"),
            r#"{"Home":"High","Away":"Standard"}"#
        );
        let half: QualityPreference = serde_json::from_str(r#"{"Away":"DataSaver"}"#).expect("reads");
        assert_eq!(half.home, StreamQuality::Original);
        assert_eq!(DeviceQualityMode::default(), DeviceQualityMode::Account);
        assert!(DeviceQualityMode::Account.is_account());
        assert!(!DeviceQualityMode::App.is_account());
    }
}
