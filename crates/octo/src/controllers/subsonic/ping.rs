//! `ping` and `getRandomSongs`.

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use octo_core::common::dotnet;
use octo_subsonic::SubsonicReply;

use octo_subsonic::SubsonicCredential;
use octo_subsonic::xml::XElement;

use super::action_helpers::SubsonicCall;
use super::media::fetch_library_cover;
use crate::app::AppState;
use crate::http::error::AppError;
use crate::services::subsonic::subsonic_proxy_service::is_absolute_uri;

/// How many of a shuffle's first songs have their covers fetched ahead of the player.
const WARM_FIRST_COVERS: usize = 4;

// -------------------------------------------------------------------------
// ping: the first call every Subsonic client makes. We make it the moment a
// broken setup explains itself, instead of relaying blindly and returning an
// opaque error when the Navidrome URL is missing or unreachable.
// -------------------------------------------------------------------------
pub async fn ping(State(state): State<AppState>, req: Request) -> Response {
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(response) => return response,
    };
    let url = state.settings.current().subsonic.url.clone().unwrap_or_default();

    if dotnet::is_blank(&url) || !is_absolute_uri(&url) {
        return state
            .subsonic_response_builder
            .create_error(
                &call.format,
                0,
                &format!(
                    "Octo isn't configured yet. Open {}://{}/admin and set your Navidrome URL \
                     (SUBSONIC_URL), then point this client at Octo instead of Navidrome.",
                    call.scheme(),
                    call.host()
                ),
            )
            .into_response();
    }

    // Relay to Navidrome so real credentials are validated there. A connection
    // failure means Octo can't reach the configured URL; pass a successful
    // (or auth-failed) Navidrome envelope straight through otherwise.
    match call.proxy.relay_safe("rest/ping.view", &call.parameters).await {
        Some(relay) => call.file(&relay.body, relay.content_type.as_deref()),
        None => state
            .subsonic_response_builder
            .create_error(
                &call.format,
                0,
                &format!(
                    "Octo can't reach Navidrome at {url}. Check the URL is correct and reachable from \
                     the Octo container (use a LAN IP or service name, not localhost)."
                ),
            )
            .into_response(),
    }
}

// ---------------------------------------------------------------------
// getRandomSongs: pure shuffle. Pass straight through to Navidrome.
// The actual "radio from this song" feature is getSimilarSongs2.
// ---------------------------------------------------------------------
pub async fn get_random_songs(State(state): State<AppState>, req: Request) -> Result<Response, AppError> {
    let call = match SubsonicCall::read(&state, req).await {
        Ok(call) => call,
        Err(response) => return Ok(response),
    };
    // No catch: a relay failure is the global handler's (502/503 JSON).
    let passthrough = call.proxy.relay("rest/getRandomSongs", &call.parameters).await?;
    warm_first_covers(&state, &call, &passthrough.body);
    Ok(SubsonicReply::content(
        String::from_utf8_lossy(&passthrough.body).into_owned(),
        passthrough.content_type.as_deref().unwrap_or("application/json"),
    )
    .into_response())
}

/// Fetches the covers of a shuffle's first songs into the cover cache in the background, as
/// the caller, so the player finds them kept when it asks.
fn warm_first_covers(state: &AppState, call: &SubsonicCall, body: &[u8]) {
    let cache = &state.local_cover_cache;
    if !cache.is_on() {
        return;
    }
    let Some(credential) = SubsonicCredential::from(&call.parameters) else {
        return;
    };
    for id in first_cover_ids(body, WARM_FIRST_COVERS) {
        let proxy = state.subsonic_proxy.clone();
        let parameters = credential.parameters(&[("id", &id)]);
        cache.warm(id, move |side| fetch_library_cover(proxy, parameters, side));
    }
}

/// The first `count` distinct cover ids of a song list answer, JSON or XML.
fn first_cover_ids(body: &[u8], count: usize) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut add = |id: &str| {
        if !id.is_empty() && ids.len() < count && !ids.iter().any(|known| known == id) {
            ids.push(id.to_string());
        }
    };
    if let Ok(document) = serde_json::from_slice::<serde_json::Value>(body) {
        let songs = document
            .get("subsonic-response")
            .and_then(|response| response.get("randomSongs"))
            .and_then(|list| list.get("song"))
            .and_then(serde_json::Value::as_array);
        for song in songs.into_iter().flatten() {
            if let Some(id) = song.get("coverArt").and_then(serde_json::Value::as_str) {
                add(id);
            }
        }
    } else if let Ok(root) = XElement::parse(&String::from_utf8_lossy(body)) {
        let songs = root
            .elements()
            .filter(|list| list.name == "randomSongs")
            .flat_map(|list| list.elements().filter(|song| song.name == "song"));
        for song in songs {
            if let Some(id) = song.attribute("coverArt") {
                add(id);
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::first_cover_ids;

    #[test]
    fn a_shuffles_first_covers_are_read_from_json_and_xml() {
        let json = br#"{"subsonic-response":{"status":"ok","randomSongs":{"song":[
            {"id":"1","coverArt":"mf-1_a"},{"id":"2"},{"id":"3","coverArt":"mf-1_a"},
            {"id":"4","coverArt":"al-4_b"},{"id":"5","coverArt":"mf-5_c"}]}}}"#;
        assert_eq!(first_cover_ids(json, 2), ["mf-1_a", "al-4_b"]);

        let xml = br#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok"><randomSongs><song id="1" coverArt="mf-1_a"/><song id="2" coverArt="mf-2_b"/></randomSongs></subsonic-response>"#;
        assert_eq!(first_cover_ids(xml, 4), ["mf-1_a", "mf-2_b"]);

        assert!(first_cover_ids(b"not an answer", 4).is_empty());
        assert!(first_cover_ids(br#"{"subsonic-response":{"status":"failed"}}"#, 4).is_empty());
    }
}
