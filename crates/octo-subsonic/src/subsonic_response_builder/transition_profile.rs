//! getTransitionProfile (octoTransitions): a song's transition profile for the Octo apps, or
//! that there is none yet. Always JSON.

use serde_json::{Map, Value, json};

use super::{SUBSONIC_VERSION, SubsonicReply, SubsonicResponseBuilder};

impl SubsonicResponseBuilder {
    /// `transitionProfile` holds the song id asked for and `ready`. When ready, the profile's own
    /// fields follow (`version`, `durationMs`, `bodyDb`, `tempo`, `head`, `tail`); when not,
    /// `version` is the analysis version the server makes now, when it knows, so a client that
    /// cannot read that version stops asking.
    pub fn create_transition_profile_response(
        &self,
        id: &str,
        profile: Option<&Value>,
        version: Option<i32>,
    ) -> SubsonicReply {
        let mut answer = Map::new();
        answer.insert("id".into(), json!(id));
        match profile.and_then(Value::as_object) {
            Some(fields) => {
                answer.insert("ready".into(), json!(true));
                for (key, value) in fields {
                    if key != "id" && key != "ready" {
                        answer.insert(key.clone(), value.clone());
                    }
                }
            }
            None => {
                answer.insert("ready".into(), json!(false));
                if let Some(version) = version {
                    answer.insert("version".into(), json!(version));
                }
            }
        }
        self.create_json_response(json!({
            "status": "ok",
            "version": SUBSONIC_VERSION,
            "type": "octo",
            "openSubsonic": true,
            "transitionProfile": Value::Object(answer),
        }))
    }
}
