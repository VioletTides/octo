//! The MusicBrainz client: its cache and escaping. Reading release details and candidate
//! sources is tested with the tagging code; the query builders are tested in
//! `octo_core::fingerprint::music_brainz_client`.

use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const NIGHT_AT_THE_OPERA: &str = r#"
{
  "id": "r-nato", "title": "A Night at the Opera", "status": "Official", "date": "1975-11-21", "country": "GB",
  "barcode": "077778949224", "quality": "normal",
  "label-info": [{"catalog-number": "EMTC 103", "label": {"id": "l-emi", "name": "EMI"}}],
  "release-group": {"id": "g-nato", "title": "A Night at the Opera", "primary-type": "Album",
    "secondary-types": [], "first-release-date": "1975-11-21"},
  "artist-credit": [{"name": "Queen", "joinphrase": "", "artist": {"id": "a-queen", "name": "Queen"}}],
  "media": [{"position": 1, "format": "12\" Vinyl", "track-count": 12,
    "tracks": [
      {"id": "t-1", "position": 1, "number": "A1", "title": "Death on Two Legs", "length": 223000,
       "recording": {"id": "rec-dotl", "title": "Death on Two Legs (Dedicated to...)", "length": 223000, "isrcs": ["GBUM71029604"]}},
      {"id": "t-11", "position": 11, "number": "B5", "title": "Bohemian Rhapsody", "length": 355000,
       "recording": {"id": "rec-br", "title": "Bohemian Rhapsody", "length": 355000, "isrcs": ["GBUM71029604", "GBUM71029605"]}}
    ]}],
  "genres": [{"name": "rock", "count": 12}, {"name": "progressive rock", "count": 5}, {"name": "glam rock", "count": 1}]
}
"#;

async fn client(answer: ResponseTemplate) -> (MusicBrainzClient, MockServer) {
    let server = MockServer::builder().start().await;
    Mock::given(any()).respond_with(answer).mount(&server).await;
    let base = Url::parse(&format!("{}/ws/2/", server.uri())).expect("parses");
    (MusicBrainzClient::with_base_url(base), server)
}

async fn calls(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.url.to_string())
        .collect()
}

#[tokio::test]
async fn lookup_release_asks_once_then_answers_from_memory() {
    let (client, server) = client(ResponseTemplate::new(200).set_body_string(NIGHT_AT_THE_OPERA)).await;

    let first = client
        .lookup_release("r-nato")
        .await
        .expect("no timeout")
        .expect("found");
    let second = client
        .lookup_release("r-nato")
        .await
        .expect("no timeout")
        .expect("found");

    assert!(Arc::ptr_eq(&first, &second));
    let calls = calls(&server).await;
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains(
        "release/r-nato?inc=labels+release-groups+artist-credits+recordings+isrcs+genres&fmt=json"
    ));
    assert!(calls[0].contains("/ws/2/release/"));
    // MusicBrainz wants a User-Agent naming the application.
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests[0]
            .headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok()),
        Some(octo_user_agent::value())
    );
}

#[tokio::test]
async fn lookup_release_server_error_is_null_and_not_remembered() {
    let (client, server) = client(ResponseTemplate::new(503)).await;

    assert!(
        client
            .lookup_release("r-nato")
            .await
            .expect("no timeout")
            .is_none()
    );
    assert!(
        client
            .lookup_release("r-nato")
            .await
            .expect("no timeout")
            .is_none()
    );
    assert_eq!(calls(&server).await.len(), 2);
}

#[tokio::test]
async fn search_recordings_is_remembered_for_the_same_song() {
    let (client, server) = client(ResponseTemplate::new(200).set_body_string(r#"{"recordings": []}"#)).await;

    let first = client
        .search_recordings("Portishead", "Glory Box (Live)", 300)
        .await
        .expect("no timeout");
    let second = client
        .search_recordings("Portishead", "Glory Box (Live)", 300)
        .await
        .expect("no timeout");

    assert!(first.is_some());
    assert!(second.is_some());
    assert_eq!(calls(&server).await.len(), 1);
}

/// The second request waits out the one-a-second gap.
#[tokio::test]
async fn requests_are_spaced_a_second_apart() {
    let (client, _server) =
        client(ResponseTemplate::new(200).set_body_string(r#"{"isrcs": ["GBUM71029604"]}"#)).await;

    let start = Instant::now();
    assert_eq!(
        client.fetch_isrcs("rec-1").await.expect("no timeout"),
        Some(vec!["GBUM71029604".to_string()])
    );
    client.fetch_isrcs("rec-2").await.expect("no timeout");
    assert!(Instant::now() - start >= Duration::from_millis(1100));
}

// ---- Where an artist is from, on the background lane ----------------------------------------

const ROSALIA: &str =
    r#"{"id":"a-rosalia","name":"Rosalía","country":"ES","area":{"name":"Spain","iso-3166-1-codes":["ES"]}}"#;

#[tokio::test]
async fn an_artist_lookup_reads_the_country_and_remembers_it() {
    let (client, server) = client(ResponseTemplate::new(200).set_body_string(ROSALIA)).await;

    let first = client.artist_country("A-Rosalia").await.expect("no timeout");
    let second = client.artist_country("a-rosalia").await.expect("no timeout");

    assert_eq!(first.as_deref(), Some("ES"));
    assert_eq!(second.as_deref(), Some("ES"));
    let calls = calls(&server).await;
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].ends_with("/ws/2/artist/A-Rosalia?fmt=json"),
        "{}",
        calls[0]
    );
}

#[tokio::test]
async fn an_artist_search_that_could_not_be_asked_is_none_and_not_remembered() {
    let (client, server) = client(ResponseTemplate::new(503)).await;

    assert_eq!(
        client.search_artist_country("Rosalía").await.expect("no timeout"),
        None
    );
    assert_eq!(
        client.search_artist_country("Rosalía").await.expect("no timeout"),
        None
    );

    let calls = calls(&server).await;
    assert_eq!(calls.len(), 2);
    assert!(
        calls[0].contains("/ws/2/artist/?query=artist%3A%22Rosal"),
        "{}",
        calls[0]
    );
}

#[tokio::test]
async fn background_lookups_step_aside_so_a_download_check_waits_one_gap_at_most() {
    let (client, _server) = client(ResponseTemplate::new(200).set_body_string(ROSALIA)).await;
    let client = Arc::new(client);
    let background = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            for i in 0..20 {
                let _ = client.artist_country(&format!("artist-{i}")).await;
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    // A download being verified asks three questions in a row.
    let started = std::time::Instant::now();
    client.fetch_isrcs("rec-1").await.expect("no timeout");
    let first = started.elapsed();
    client.fetch_isrcs("rec-2").await.expect("no timeout");
    client.fetch_isrcs("rec-3").await.expect("no timeout");
    let all = started.elapsed();
    background.abort();

    let gap = MusicBrainzClient::MINIMUM_GAP;
    assert!(first < gap + Duration::from_millis(400), "first {first:?}");
    // One gap behind the background's last call, then one gap each: never behind its queue.
    assert!(all < gap * 3 + Duration::from_millis(900), "all {all:?}");
}

// ---- artists by name, and recording credits ------------------------------------------------

#[tokio::test]
async fn find_artists_named_asks_once_and_remembers_the_answer() {
    let body = r#"{"artists":[{"id":"a1","name":"Earth, Wind & Fire"},{"id":"a2","name":"Earth, Wind & Fire Experience"}]}"#;
    let (client, server) = client(ResponseTemplate::new(200).set_body_string(body)).await;

    assert!(client.known_artists_named("Earth, Wind & Fire").is_none());
    let first = client
        .find_artists_named("Earth, Wind & Fire")
        .await
        .expect("no timeout")
        .expect("asked");
    let second = client
        .find_artists_named("EARTH, WIND & FIRE")
        .await
        .expect("no timeout")
        .expect("asked");

    assert_eq!(first.len(), 1);
    assert_eq!(second, first);
    assert_eq!(calls(&server).await.len(), 1);
    assert!(client.known_artists_named("earth, wind & fire").is_some());
}

/// A failed request is "could not ask", never "no such artist", and is not remembered.
#[tokio::test]
async fn find_artists_named_failure_is_none_and_not_remembered() {
    let (client, _server) = client(ResponseTemplate::new(503)).await;

    assert!(
        client
            .find_artists_named("Bizarrap")
            .await
            .expect("no timeout")
            .is_none()
    );
    assert!(client.known_artists_named("Bizarrap").is_none());
}

#[tokio::test]
async fn recording_credits_answer_every_id_in_one_search() {
    let body = r#"{"recordings":[{"id":"11111111-2222-3333-4444-555555555555","artist-credit":[{"name":"Bizarrap","joinphrase":", "},{"name":"Rauw Alejandro"}]}]}"#;
    let (client, server) = client(ResponseTemplate::new(200).set_body_string(body)).await;
    let ids = vec![
        "11111111-2222-3333-4444-555555555555".to_string(),
        "99999999-2222-3333-4444-555555555555".to_string(),
    ];

    let credits = client
        .recording_credits(&ids)
        .await
        .expect("no timeout")
        .expect("asked");
    let again = client
        .recording_credits(&ids)
        .await
        .expect("no timeout")
        .expect("asked");

    assert_eq!(credits[&ids[0]].len(), 2);
    assert_eq!(credits[&ids[0]][1].join_phrase, "");
    assert!(credits[&ids[1]].is_empty());
    assert_eq!(again, credits);
    assert_eq!(calls(&server).await.len(), 1);
}

#[test]
fn the_kept_answers_are_dropped_before_they_outgrow_their_bound() {
    let mut map: HashMap<String, u32> = (0..3).map(|i| (i.to_string(), i)).collect();
    make_room(&mut map, 1, 4);
    assert_eq!(map.len(), 3, "room for one more");
    make_room(&mut map, 2, 4);
    assert!(map.is_empty(), "no room for two: the old answers go");
    assert_eq!(KNOWN_MAX, 10_000);
}
