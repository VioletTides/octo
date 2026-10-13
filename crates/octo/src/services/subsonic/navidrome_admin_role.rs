use octo_subsonic::SubsonicCredential;
use serde_json::Value;

use super::SubsonicProxyService;

/// Whether a Subsonic sign-in belongs to one of Navidrome's admins, asked with that sign-in.
/// False whenever Navidrome cannot say, so an outage never grants anything. Asked every time, so
/// a role taken away counts at once.
pub async fn is_admin(credential: &SubsonicCredential, username: &str, relay: &SubsonicProxyService) -> bool {
    let parameters = credential.parameters(&[("username", username)]);
    let Some(answer) = relay.relay_safe("rest/getUser", &parameters).await else {
        return false;
    };
    serde_json::from_slice::<Value>(&answer.body).is_ok_and(|document| {
        document
            .get("subsonic-response")
            .and_then(|root| root.get("user"))
            .and_then(|user| user.get("adminRole"))
            == Some(&Value::Bool(true))
    })
}
