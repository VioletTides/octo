//! Where the library's artists are from: the lookup's order (MusicBrainz id, full credit,
//! first artist), what it keeps, the budget, and the shared walk.

use std::sync::Arc;

use octo_core::settings::{AppSettings, SettingsStore, SubsonicSettings};
use parking_lot::Mutex;
use tempfile::TempDir;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;

/// MusicBrainz as a table: an answer per id or name, None for "could not be asked".
#[derive(Default)]
struct FakeMusicBrainz {
    by_id: HashMap<String, Option<String>>,
    by_name: HashMap<String, Option<String>>,
    asked: Mutex<Vec<String>>,
}

impl FakeMusicBrainz {
    fn with(ids: &[(&str, Option<&str>)], names: &[(&str, Option<&str>)]) -> Self {
        let table = |rows: &[(&str, Option<&str>)]| {
            rows.iter()
                .map(|(key, answer)| (key.to_string(), answer.map(str::to_string)))
                .collect()
        };
        FakeMusicBrainz {
            by_id: table(ids),
            by_name: table(names),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().clone()
    }
}

#[async_trait::async_trait]
impl CountrySource for FakeMusicBrainz {
    async fn by_mbid(&self, mbid: &str) -> Option<String> {
        self.asked.lock().push(format!("id {mbid}"));
        self.by_id.get(mbid).cloned().unwrap_or(Some(String::new()))
    }

    async fn by_name(&self, name: &str) -> Option<String> {
        self.asked.lock().push(format!("name {name}"));
        self.by_name.get(name).cloned().unwrap_or(Some(String::new()))
    }
}

fn lookup(source: &Arc<FakeMusicBrainz>, path: Option<PathBuf>) -> ArtistCountryLookup {
    ArtistCountryLookup::new(Arc::clone(source) as Arc<dyn CountrySource>, path)
}

#[tokio::test]
async fn an_artist_with_a_musicbrainz_id_is_asked_by_it_and_never_by_name() {
    let source = Arc::new(FakeMusicBrainz::with(&[("mb-rosalia", Some("ES"))], &[]));
    let lookup = lookup(&source, None);
    let mut asks = 10;

    let country = lookup.country("Rosalía", Some(" mb-rosalia "), &mut asks).await;

    assert_eq!(country.as_deref(), Some("ES"));
    assert_eq!(source.asked(), ["id mb-rosalia"]);
    assert_eq!(
        lookup.known("Someone Else", Some("MB-ROSALIA")).as_deref(),
        Some("ES")
    );
    assert_eq!(asks, 9);
}

#[tokio::test]
async fn a_credit_is_asked_whole_then_by_its_first_artist() {
    let source = Arc::new(FakeMusicBrainz::with(
        &[],
        &[
            ("Bad Bunny feat. Jhay Cortez", Some("")),
            ("Bad Bunny", Some("PR")),
        ],
    ));
    let lookup = lookup(&source, None);
    let mut asks = 10;

    let country = lookup
        .country("Bad Bunny feat. Jhay Cortez", None, &mut asks)
        .await;

    assert_eq!(country.as_deref(), Some("PR"));
    assert_eq!(
        source.asked(),
        ["name Bad Bunny feat. Jhay Cortez", "name Bad Bunny"]
    );
    assert_eq!(
        lookup.known("Bad Bunny feat. Jhay Cortez", None).as_deref(),
        Some("PR")
    );
    // A whole credit that is known is never split.
    let mut more = 10;
    assert_eq!(
        lookup.country("Bad Bunny", None, &mut more).await.as_deref(),
        Some("PR")
    );
    assert_eq!(more, 10);
}

#[tokio::test]
async fn an_unknown_answer_is_kept_and_a_failure_is_asked_again() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(STORE_FILE);
    let source = Arc::new(FakeMusicBrainz::with(
        &[("mb-down", None)],
        &[("Nobody Knows", Some("")), ("Down", None)],
    ));
    let first = lookup(&source, Some(path.clone()));
    let mut asks = 10;

    assert_eq!(
        first.country("Nobody Knows", None, &mut asks).await.as_deref(),
        Some("")
    );
    assert_eq!(first.country("Down", Some("mb-down"), &mut asks).await, None);
    assert_eq!(first.known("Down", Some("mb-down")), None);
    first.save();

    let kept: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept.get("name:nobodyknows").map(String::as_str), Some(""));

    // After a restart the unknown answer stands and the failure is asked again.
    let again = lookup(&source, Some(path));
    let mut asks = 10;
    assert_eq!(again.known("Nobody Knows", None).as_deref(), Some(""));
    assert_eq!(again.country("Down", Some("mb-down"), &mut asks).await, None);
    assert_eq!(
        source.asked(),
        [
            "name Nobody Knows",
            "id mb-down",
            "name Down",
            "id mb-down",
            "name Down"
        ]
    );
}

#[tokio::test]
async fn nothing_is_asked_once_the_budget_is_spent() {
    let source = Arc::new(FakeMusicBrainz::with(
        &[],
        &[("A feat. B", Some("")), ("A", Some("SE"))],
    ));
    let lookup = lookup(&source, None);
    let mut asks = 1;

    assert_eq!(lookup.country("A feat. B", None, &mut asks).await, None);
    assert_eq!(asks, 0);
    assert_eq!(lookup.country("C", None, &mut asks).await, None);
    assert_eq!(source.asked(), ["name A feat. B"]);
}

fn song(id: &str, artist: &str, artist_id: Option<&str>) -> Node {
    let artist_id = artist_id.map_or(String::new(), |id| format!(r#","artistId":"{id}""#));
    Node::parse(&format!(
        r#"{{"id":"{id}","title":"Song {id}","artist":"{artist}"{artist_id},"playCount":7,"starred":"2026-01-01T00:00:00Z","userRating":5,"genre":"Pop"}}"#
    ))
    .unwrap()
}

#[test]
fn the_walk_groups_songs_by_artist_and_takes_the_admins_own_marks_off() {
    let mbids: HashMap<String, String> = [("ar1".to_string(), "mb-1".to_string())].into_iter().collect();
    let walked = WalkedLibrary::new(
        vec![
            song("s1", "Rosalía", Some("ar1")),
            song("s2", "Rosalía", Some("ar1")),
            song("s3", "Nadie", None),
            song("s4", "nadie", None),
            song("s5", "Bad Bunny", Some("ar2")),
        ],
        &mbids,
    );

    assert_eq!(
        walked.artists,
        [
            LibraryArtist {
                name: "Rosalía".into(),
                mbid: Some("mb-1".into()),
                songs: 2
            },
            LibraryArtist {
                name: "Nadie".into(),
                mbid: None,
                songs: 2
            },
            LibraryArtist {
                name: "Bad Bunny".into(),
                mbid: None,
                songs: 1
            },
        ]
    );
    assert_eq!(walked.artist_of_song, [0, 0, 1, 1, 2]);
    for song in &walked.songs {
        for field in PERSONAL_FIELDS {
            assert!(song.get(field).is_none(), "{field}");
        }
        assert_eq!(text(song, "genre"), Some("Pop"));
    }
}

#[tokio::test]
async fn the_walk_counts_and_draws_by_what_is_known() {
    let source = Arc::new(FakeMusicBrainz::with(
        &[("mb-1", Some("ES"))],
        &[("Bad Bunny", Some("PR"))],
    ));
    let lookup = lookup(&source, None);
    let mbids: HashMap<String, String> = [("ar1".to_string(), "mb-1".to_string())].into_iter().collect();
    let walked = WalkedLibrary::new(
        vec![
            song("s1", "Rosalía", Some("ar1")),
            song("s2", "Rosalía", Some("ar1")),
            song("s3", "Bad Bunny", Some("ar2")),
            song("s4", "Nadie", None),
        ],
        &mbids,
    );
    assert!(walked.counts(&lookup, &[]).is_empty());

    let mut asks = 10;
    for artist in &walked.artists {
        lookup
            .country(&artist.name, artist.mbid.as_deref(), &mut asks)
            .await;
    }

    let counts = walked.counts(&lookup, &[]);
    assert_eq!(
        counts.into_iter().collect::<Vec<_>>(),
        [("country:ES".to_string(), 2), ("country:PR".to_string(), 1)]
    );
    assert_eq!(walked.counts(&lookup, &["PR"]).len(), 1);
    let spain = walked.songs_from(&lookup, "ES");
    assert_eq!(
        spain
            .iter()
            .filter_map(|song| text(song, "id"))
            .collect::<Vec<_>>(),
        ["s1", "s2"]
    );
}

/// Navidrome as the admin sees it: a library of three artists, one with a MusicBrainz id.
#[derive(Clone, Default)]
struct AdminNavidrome {
    calls: Arc<Mutex<Vec<String>>>,
}

impl Respond for AdminNavidrome {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let path = request.url.path().trim_matches('/').to_string();
        self.calls
            .lock()
            .push(format!("{path}?{}", request.url.query().unwrap_or_default()));
        let songs: Vec<String> = (0..12)
            .map(|i| {
                let (artist, id) = match i % 4 {
                    0 | 1 => ("Rosalía", "ar1"),
                    2 => ("Bad Bunny", "ar2"),
                    _ => ("Unheard Of", "ar3"),
                };
                format!(r#"{{"id":"s{i}","title":"Song {i}","artist":"{artist}","artistId":"{id}"}}"#)
            })
            .collect();
        let body = match path.as_str() {
            "rest/getArtists" => r#"{"subsonic-response":{"status":"ok","artists":{"index":[{"name":"B","artist":[{"id":"ar2","name":"Bad Bunny","musicBrainzId":""}]},{"name":"R","artist":[{"id":"ar1","name":"Rosalía","musicBrainzId":"mb-1"}]}]}}}"#.to_string(),
            "rest/search3" => format!(
                r#"{{"subsonic-response":{{"status":"ok","searchResult3":{{"song":[{}]}}}}}}"#,
                songs.join(",")
            ),
            _ => r#"{"subsonic-response":{"status":"ok"}}"#.to_string(),
        };
        ResponseTemplate::new(200)
            .insert_header("Content-Type", "application/json")
            .set_body_string(body)
    }
}

fn admin_auth() -> AdminAuth {
    Arc::new(|| {
        Some(
            [
                ("u", "admin"),
                ("t", "token"),
                ("s", "salt"),
                ("v", "1.16.1"),
                ("c", "octo"),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        )
    })
}

#[tokio::test]
async fn a_pass_walks_the_library_as_the_admin_and_asks_the_biggest_artists_first_within_its_budget() {
    let navidrome = AdminNavidrome::default();
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(navidrome.clone())
        .mount(&server)
        .await;
    let store = Arc::new(SettingsStore::for_tests(AppSettings {
        subsonic: SubsonicSettings {
            url: Some(server.uri()),
            ..Default::default()
        },
        ..Default::default()
    }));
    let source = Arc::new(FakeMusicBrainz::with(
        &[("mb-1", Some("ES"))],
        &[("Bad Bunny", Some("PR"))],
    ));
    let library = Arc::new(CountryLibrary::new(
        Arc::new(lookup(&source, None)),
        SubsonicProxyService::new(store),
        admin_auth(),
    ));
    assert!(library.pass_due());

    assert!(library.run_pass(2).await);

    // Rosalía (6 songs) by her id, then the next biggest by name; the third waits.
    assert_eq!(source.asked(), ["id mb-1", "name Bad Bunny"]);
    let walked = library.last_walk().expect("walked");
    assert_eq!(walked.songs.len(), 12);
    assert_eq!(
        walked
            .counts(library.lookup(), &[])
            .into_iter()
            .collect::<Vec<_>>(),
        [("country:ES".to_string(), 6), ("country:PR".to_string(), 3)]
    );
    let calls = navidrome.calls.lock().clone();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("rest/getArtists"))
            .count(),
        1
    );
    assert!(calls.iter().all(|call| call.contains("u=admin")));
    // The next pass waits its turn, and reuses the walk.
    assert!(!library.pass_due());
    assert!(Arc::ptr_eq(&library.walk().await.expect("walked"), &walked));
}

#[tokio::test]
async fn without_the_admin_sign_in_nothing_is_walked_and_the_pass_is_tried_again_soon() {
    let store = Arc::new(SettingsStore::for_tests(AppSettings::default()));
    let source = Arc::new(FakeMusicBrainz::default());
    let library = Arc::new(CountryLibrary::new(
        Arc::new(lookup(&source, None)),
        SubsonicProxyService::new(store),
        Arc::new(|| None),
    ));

    assert!(!library.run_pass(200).await);

    assert!(library.last_walk().is_none());
    assert!(source.asked().is_empty());
    let retried_at = library.pass.lock().started.unwrap() + RETRY_AFTER_FAILURE;
    assert!(library.pass.lock().failed && retried_at > Utc::now());
}

#[tokio::test]
async fn an_id_that_cannot_be_asked_falls_back_to_the_name_search() {
    let source = Arc::new(FakeMusicBrainz::with(
        &[("mb-down", None)],
        &[("Rosalía", Some("ES"))],
    ));
    let lookup = lookup(&source, None);
    let mut asks = 10;

    let country = lookup.country("Rosalía", Some("mb-down"), &mut asks).await;

    assert_eq!(country.as_deref(), Some("ES"));
    assert_eq!(source.asked(), ["id mb-down", "name Rosalía"]);
    assert_eq!(lookup.known("Rosalía", Some("mb-down")).as_deref(), Some("ES"));
}

#[tokio::test]
async fn a_failed_lookup_is_not_asked_again_until_its_back_off_is_over() {
    let source = Arc::new(FakeMusicBrainz::with(&[], &[("Down", None)]));
    let lookup = lookup(&source, None);
    let mut asks = 10;

    assert_eq!(lookup.country("Down", None, &mut asks).await, None);
    assert_eq!(lookup.country("Down", None, &mut asks).await, None);

    assert_eq!(source.asked(), ["name Down"]);
    assert_eq!(asks, 9, "a lookup backing off costs no ask");
}

#[test]
fn the_back_off_doubles_from_an_hour_up_to_a_day() {
    assert_eq!(back_off(1), Duration::from_secs(3600));
    assert_eq!(back_off(2), Duration::from_secs(2 * 3600));
    assert_eq!(back_off(4), Duration::from_secs(8 * 3600));
    assert_eq!(back_off(6), Duration::from_secs(24 * 3600));
    assert_eq!(back_off(60), Duration::from_secs(24 * 3600));
}
