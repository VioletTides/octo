//! Album ownership and the library snapshot, against a real "drake" search: the library and the
//! catalog as Navidrome and Deezer gave them, what search listed as not in the library, and what
//! the library really held.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use octo_core::common::clock::Clock;
use octo_core::models::domain::Album;
use parking_lot::Mutex;

use super::*;
use crate::services::library::library_snapshot::LibrarySnapshot;
use crate::services::library::quality_upgrade_worker::LibrarySongRow;

/// The library and the catalog of the "drake" search, shared with the search tests.
pub(crate) mod drake_search {
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    pub const FOMO_ID: &str = "al-fomo";
    pub const SEXY_DRAKE_PART_ID: &str = "3jmLzEVlwtrlq8XfVjei9k";
    pub const SEXY_PND_PART_ID: &str = "al-sss-pnd";
    pub const WATTBA_ID: &str = "al-wattba";
    pub const TAKE_CARE_ID: &str = "al-takecare";

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn row(
        album_id: &str,
        album: &str,
        album_artist: &str,
        artist: &str,
        title: &str,
        seconds: i32,
    ) -> LibrarySongRow {
        let next = NEXT.fetch_add(1, Ordering::SeqCst) + 1;
        LibrarySongRow {
            id: format!("s-{album_id}-{next}"),
            path: format!("{album_artist}/{album}/{title}.flac"),
            library_path: None,
            size: 1,
            suffix: "flac".into(),
            bit_rate: 900,
            title: title.into(),
            artist: artist.into(),
            duration: Some(seconds),
            album: Some(album.into()),
            album_id: Some(album_id.into()),
            album_artist: Some(album_artist.into()),
            isrcs: Vec::new(),
            ..Default::default()
        }
    }

    /// The library: HABIBTI's expanded edition, the split "$ome $exy $ongs 4 U" (five songs
    /// credited to Drake apart from the rest, and one Drake song the library credits to
    /// PARTYNEXTDOOR), two songs of What A Time To Be Alive's explicit version, and Take Care. No
    /// ISRCs, as few library files carry one.
    pub static LIBRARY: LazyLock<Vec<LibrarySongRow>> = LazyLock::new(|| {
        let mut rows = Vec::new();
        for (title, seconds) in [
            ("Rusty Intro", 62),
            ("WNBA", 179),
            ("Slap The City", 202),
            ("High Fives", 256),
            ("Hurrr Nor Thurrr", 188),
            ("I'm Spent", 144),
            ("Classic", 179),
            ("Gen 5", 217),
            ("White Bone", 297),
            ("Fortworth", 231),
            ("Prioritizing", 233),
            ("Solar Eclipse", 218),
            ("Quebec", 130),
            ("Cold Shoulder", 234),
            ("Classic PT2", 179),
        ] {
            rows.push(row(FOMO_ID, "HABIBTI (FOMO)", "Drake", "Drake", title, seconds));
        }
        for (title, seconds) in [
            ("CRYING IN CHANEL", 200),
            ("SMALL TOWN FAME", 149),
            ("BRIAN STEEL", 111),
            ("RAINING IN HOUSTON", 245),
            ("NOKIA", 241),
        ] {
            rows.push(row(
                SEXY_DRAKE_PART_ID,
                "$ome $exy $ongs 4 U",
                "Drake",
                "Drake",
                title,
                seconds,
            ));
        }
        for (title, seconds) in [
            ("CN TOWER", 242),
            ("MOTH BALLS", 213),
            ("SOMETHING ABOUT YOU", 219),
            ("SPIDER-MAN SUPERMAN", 204),
            ("DEEPER", 172),
            ("GIMME A HUG", 193),
            ("LASERS", 199),
            ("MEET YOUR PADRE", 271),
            ("DIE TRYING", 195),
            ("SOMEBODY LOVES ME", 183),
            ("CELIBACY", 236),
            ("OMW", 233),
            ("GLORIOUS", 205),
            ("WHEN HE'S GONE", 210),
            ("GREEDY", 386),
        ] {
            rows.push(row(
                SEXY_PND_PART_ID,
                "$ome $exy $ongs 4 U",
                "PARTYNEXTDOOR",
                "PARTYNEXTDOOR",
                title,
                seconds,
            ));
        }
        let wattba = "What A Time To Be Alive (Explicit Version)";
        rows.push(row(
            WATTBA_ID,
            wattba,
            "Drake & Future",
            "Drake & Future",
            "Jumpman",
            206,
        ));
        rows.push(row(
            WATTBA_ID,
            wattba,
            "Drake & Future",
            "Drake & Future",
            "Live From The Gutter",
            211,
        ));
        rows.push(row(
            TAKE_CARE_ID,
            "Take Care",
            "Drake",
            "Drake",
            "Over My Dead Body",
            272,
        ));
        rows.push(row(
            TAKE_CARE_ID,
            "Take Care",
            "Drake",
            "Drake",
            "Shot For Me",
            224,
        ));
        rows
    });

    pub fn t(title: &str, artist: &str, seconds: i32, isrc: &str) -> CatalogTrack {
        CatalogTrack::new(title, Some(artist), Some(seconds), Some(isrc))
    }

    /// Deezer's tracklists, ISRCs and all.
    pub fn habibti() -> Vec<CatalogTrack> {
        vec![
            t("Rusty Intro", "Drake", 62, "USUG12602381"),
            t("WNBA", "Drake", 178, "USUG12602382"),
            t("Slap The City", "Drake", 202, "USUG12602383"),
            t("High Fives", "Drake", 256, "USUG12602384"),
            t("Hurrr Nor Thurrr", "Drake", 188, "USUG12602385"),
            t("I\u{2019}m Spent", "Drake", 144, "USUG12602440"),
            t("Classic", "Drake", 179, "USUG12602441"),
            t("Gen 5", "Drake", 217, "USUG12602442"),
            t("White Bone", "Drake", 297, "USUG12602443"),
            t("Fortworth", "Drake", 231, "USUG12602444"),
            t("Prioritizing", "Drake", 233, "USUG12602445"),
        ]
    }

    pub fn habibti_fomo() -> Vec<CatalogTrack> {
        let mut tracks = habibti();
        tracks.extend([
            t("Solar Eclipse", "Drake", 218, "USUG12604795"),
            t("Quebec", "Drake", 130, "USUG12604801"),
            t("Cold Shoulder", "Drake", 234, "USUG12604803"),
            t("Classic PT2", "Drake", 179, "USUG12604805"),
        ]);
        tracks
    }

    pub fn some_sexy_songs() -> Vec<CatalogTrack> {
        vec![
            t("CN TOWER", "PARTYNEXTDOOR", 242, "USLD91772019"),
            t("MOTH BALLS", "PARTYNEXTDOOR", 213, "USLD91772020"),
            t("SOMETHING ABOUT YOU", "PARTYNEXTDOOR", 219, "USLD91772021"),
            t("CRYING IN CHANEL", "Drake", 200, "USLD91772022"),
            t("SPIDER-MAN SUPERMAN", "PARTYNEXTDOOR", 204, "USLD91772023"),
            t("DEEPER", "PARTYNEXTDOOR", 172, "USLD91772024"),
            t("SMALL TOWN FAME", "Drake", 149, "USLD91772025"),
            t("PIMMIE'S DILEMMA", "Pimmie", 118, "USLD91772026"),
            t("BRIAN STEEL", "Drake", 111, "USLD91772027"),
            t("GIMME A HUG", "Drake", 193, "USLD91772028"),
            t("RAINING IN HOUSTON", "Drake", 245, "USLD91772029"),
            t("LASERS", "PARTYNEXTDOOR", 199, "USLD91772030"),
            t("MEET YOUR PADRE", "PARTYNEXTDOOR", 271, "USLD91772031"),
            t("NOKIA", "Drake", 241, "USLD91772032"),
            t("DIE TRYING", "PARTYNEXTDOOR", 195, "USLD91772033"),
            t("SOMEBODY LOVES ME", "PARTYNEXTDOOR", 183, "USLD91772034"),
            t("CELIBACY", "PARTYNEXTDOOR", 236, "USLD91772035"),
            t("OMW", "PARTYNEXTDOOR", 233, "USLD91772036"),
            t("GLORIOUS", "PARTYNEXTDOOR", 205, "USLD91772037"),
            t("WHEN HE'S GONE", "PARTYNEXTDOOR", 210, "USLD91772038"),
            t("GREEDY", "PARTYNEXTDOOR", 386, "USLD91772039"),
        ]
    }

    pub fn what_a_time() -> Vec<CatalogTrack> {
        vec![
            t("Digital Dash", "Drake", 231, "USCM51500292"),
            t("Big Rings", "Drake", 217, "USCM51500293"),
            t("Live From The Gutter", "Drake", 211, "USCM51500295"),
            t("Diamonds Dancing", "Drake", 314, "USCM51500294"),
            t("Scholarships", "Drake", 209, "USCM51500296"),
            t("Plastic Bag", "Drake", 202, "USCM51500297"),
            t("I'm The Plug", "Drake", 180, "USCM51500298"),
            t("Change Locations", "Drake", 220, "USCM51500299"),
            t("Jumpman", "Drake", 205, "USCM51500300"),
            t("Jersey", "Future", 188, "USCM51500301"),
            t("30 for 30 Freestyle", "Drake", 253, "USCM51500302"),
        ]
    }

    pub fn maid_of_honour() -> Vec<CatalogTrack> {
        vec![
            t("Hoe Phase", "Drake", 203, "USUG12602459"),
            t("Road Trips", "Drake", 243, "USUG12602460"),
            t("Outside Tweaking", "Drake", 190, "USUG12602461"),
            t("Cheetah Print", "Drake", 202, "USUG12602462"),
            t("Which One", "Drake", 169, "USUG12506621"),
            t("Amazing Shape", "Drake", 176, "USUG12602463"),
            t("BBW", "Drake", 211, "USUG12602464"),
            t("True Bestie", "Drake", 148, "USUG12602465"),
            t("Where\u{2019}s Your Stuff Interlude", "Drake", 52, "USUG12602466"),
            t("New Bestie", "Drake", 259, "USUG12602467"),
            t("Q&A", "Drake", 223, "USUG12602468"),
            t("Stuck", "Drake", 177, "USUG12602469"),
            t("Goose and The Juice", "Drake", 263, "USUG12602470"),
            t("Princess", "Drake", 193, "USUG12602471"),
        ]
    }

    pub fn outside(title: &str, artist: &str, songs: i32) -> Album {
        Album {
            id: format!("ext-{title}"),
            title: title.into(),
            artist: artist.into(),
            song_count: Some(songs),
            is_local: false,
            ..Default::default()
        }
    }
}

use drake_search as drake;

fn index() -> Arc<LibraryAlbumIndex> {
    static INDEX: std::sync::LazyLock<Arc<LibraryAlbumIndex>> =
        std::sync::LazyLock::new(|| Arc::new(LibraryAlbumIndex::new(drake::LIBRARY.clone())));
    Arc::clone(&INDEX)
}

fn ids(albums: &[Arc<LibraryAlbum>]) -> Vec<&str> {
    albums.iter().map(|album| album.id.as_str()).collect()
}

fn judge(title: &str, artist: &str, songs: i32, tracks: &[CatalogTrack]) -> Standing {
    AlbumOwnership::judge_album(&drake::outside(title, artist, songs), tracks, &index())
}

#[test]
fn habibti_is_held_whole_inside_the_expanded_edition() {
    let standing = judge("HABIBTI", "Drake", 11, &drake::habibti());

    assert_eq!(standing.owned, Some(11));
    assert!(standing.fully_owned());
    assert_eq!(ids(&standing.holders), [drake::FOMO_ID]);
    assert_eq!(ids(standing.stand_ins()), [drake::FOMO_ID]);
}

#[test]
fn habibti_fomo_is_the_librarys_album_by_its_very_name_and_artist() {
    assert_eq!(
        ids(&index().same_album(Some("HABIBTI (FOMO)"), Some("Drake"))),
        [drake::FOMO_ID]
    );
    // Its songs say the same, the four extra ones too.
    assert!(judge("HABIBTI (FOMO)", "Drake", 15, &drake::habibti_fomo()).fully_owned());
}

#[test]
fn some_sexy_songs_is_held_but_one_across_the_split_album_whatever_the_credit() {
    let standing = judge(
        "$ome $exy $ongs 4 U",
        "PARTYNEXTDOOR",
        21,
        &drake::some_sexy_songs(),
    );

    // All but "PIMMIE'S DILEMMA": "GIMME A HUG" is Drake's on Deezer and PARTYNEXTDOOR's in the
    // library, and is still found, on the album of the same name at the same length.
    assert_eq!(standing.owned, Some(20));
    assert!(!standing.fully_owned());
    assert_eq!(
        ids(&standing.holders),
        [drake::SEXY_PND_PART_ID, drake::SEXY_DRAKE_PART_ID]
    );
}

#[test]
fn some_sexy_songs_has_the_librarys_name_and_artist_dollar_signs_and_all() {
    let index = index();
    assert!(
        ids(&index.same_album(Some("$ome $exy $ongs 4 U"), Some("PARTYNEXTDOOR")))
            .contains(&drake::SEXY_PND_PART_ID)
    );
    assert!(
        ids(&index.same_album(Some("$ome $exy $ongs 4 U"), Some("Drake")))
            .contains(&drake::SEXY_DRAKE_PART_ID)
    );
    assert!(
        index
            .same_album(Some("Some Sexy Songs 4 U"), Some("Pimmie"))
            .is_empty()
    );
}

#[test]
fn what_a_time_to_be_alive_is_held_in_part() {
    let standing = judge("What A Time To Be Alive", "Drake", 11, &drake::what_a_time());

    assert_eq!(standing.owned, Some(2));
    assert_eq!(standing.total, 11);
    assert!(!standing.fully_owned());
    assert!(standing.stand_ins().is_empty());
}

#[test]
fn maid_of_honour_is_not_held_at_all() {
    let standing = judge("MAID OF HONOUR", "Drake", 14, &drake::maid_of_honour());

    assert_eq!(standing.owned, Some(0));
    assert!(standing.holders.is_empty());
}

#[test]
fn a_deluxe_edition_is_not_held_because_the_standard_one_is() {
    let deluxe = [
        drake::t("Over My Dead Body", "Drake", 272, "USCM51100001"),
        drake::t("Shot For Me", "Drake", 224, "USCM51100002"),
        drake::t("The Motto", "Drake", 181, "USCM51100003"),
    ];
    let standing = judge("Take Care (Deluxe)", "Drake", 3, &deluxe);

    assert_eq!(standing.owned, Some(2));
    assert!(!standing.fully_owned());
    assert!(index().likely(Some("Take Care (Deluxe)"), Some("Drake")));
    assert!(
        index()
            .same_album(Some("Take Care (Deluxe)"), Some("Drake"))
            .is_empty()
    );
}

#[test]
fn a_version_is_another_song() {
    let live = [drake::t("Jumpman (Live)", "Drake", 205, "USCM51599999")];
    assert_eq!(judge("Jumpman (Live)", "Drake", 1, &live).owned, Some(0));
}

#[test]
fn another_artists_song_of_the_same_title_is_not_owned() {
    let other = [drake::t("Classic", "Someone Else", 179, "GBXXX2600001")];
    assert_eq!(judge("Elsewhere", "Someone Else", 1, &other).owned, Some(0));
}

#[test]
fn base_key_sets_the_edition_aside() {
    for (title, key) in [
        ("HABIBTI (FOMO)", "habibti"),
        ("What A Time To Be Alive (Explicit Version)", "whatatimetobealive"),
        ("Take Care (Deluxe) [Explicit]", "takecare"),
        ("NOKIA - Single", "nokia"),
        ("$ome $exy $ongs 4 U", "omeexyongs4u"),
        ("(What's the Story) Morning Glory?", "whatsthestorymorningglory"),
        ("(Untitled)", "untitled"),
    ] {
        assert_eq!(AlbumOwnership::base_key(Some(title)), key, "{title}");
    }
}

#[test]
fn likely_goes_by_names_and_an_artist_in_common() {
    let index = index();
    assert!(index.likely(Some("HABIBTI"), Some("Drake")));
    assert!(index.likely(Some("What A Time To Be Alive"), Some("Drake")));
    // A single named as a library song.
    assert!(index.likely(Some("NOKIA"), Some("Drake")));
    assert!(!index.likely(Some("MAID OF HONOUR"), Some("Drake")));
    // Another artist's album of a name the library has.
    assert!(!index.likely(Some("HABIBTI"), Some("Nick Drake")));
    assert!(index.knows_artist(Some("Drake")));
    assert!(index.knows_artist(Some("PARTYNEXTDOOR & Drake")));
    assert!(!index.knows_artist(Some("Nick Drake")));
}

type Asked = Arc<Mutex<Vec<(String, bool)>>>;

fn service(
    tracklist: impl Fn(&Album) -> futures::future::BoxFuture<'static, Option<Vec<CatalogTrack>>>
    + Send
    + Sync
    + 'static,
    asked: &Asked,
) -> AlbumOwnership {
    let asked = Arc::clone(asked);
    let tracklist = Arc::new(tracklist);
    AlbumOwnership::new(
        Arc::new(|_| Box::pin(async { Some(index()) })),
        Arc::new(move |album: Album, background| {
            asked.lock().push((album.title.clone(), background));
            tracklist(&album)
        }),
        Arc::new(|_| None),
    )
}

fn tracks(title: &str) -> Option<Vec<CatalogTrack>> {
    match title {
        "HABIBTI" => Some(drake::habibti()),
        "HABIBTI (FOMO)" => Some(drake::habibti_fomo()),
        "$ome $exy $ongs 4 U" => Some(drake::some_sexy_songs()),
        "What A Time To Be Alive" => Some(drake::what_a_time()),
        "MAID OF HONOUR" => Some(drake::maid_of_honour()),
        _ => None,
    }
}

fn ready(
    tracks: Option<Vec<CatalogTrack>>,
) -> futures::future::BoxFuture<'static, Option<Vec<CatalogTrack>>> {
    Box::pin(async move { tracks })
}

#[tokio::test]
async fn judge_reads_likely_albums_at_once_known_artists_in_the_background_and_no_others() {
    let asked: Asked = Arc::default();
    let service = service(|album| ready(tracks(&album.title)), &asked);
    let albums = [
        drake::outside("HABIBTI", "Drake", 11),
        drake::outside("HABIBTI (FOMO)", "Drake", 15),
        drake::outside("$ome $exy $ongs 4 U", "PARTYNEXTDOOR", 21),
        drake::outside("What A Time To Be Alive", "Drake", 11),
        drake::outside("MAID OF HONOUR", "Drake", 14),
        drake::outside("Five Leaves Left", "Nick Drake", 10),
    ];

    let standings = service.judge(&albums, Duration::from_secs(5)).await;
    let by_title = |title: &str| {
        standings
            .iter()
            .find(|standing| standing.album.title == title)
            .expect("judged")
    };

    // The namesakes need no tracklist; Nick Drake is not in the library at all.
    let mut seen = asked.lock().clone();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("HABIBTI".to_string(), false),
            ("MAID OF HONOUR".to_string(), true),
            ("What A Time To Be Alive".to_string(), false),
        ]
    );
    assert!(by_title("HABIBTI").fully_owned());
    assert_eq!(ids(by_title("HABIBTI (FOMO)").stand_ins()), [drake::FOMO_ID]);
    assert!(ids(by_title("$ome $exy $ongs 4 U").stand_ins()).contains(&drake::SEXY_PND_PART_ID));
    assert_eq!(by_title("What A Time To Be Alive").owned, Some(2));
    assert_eq!(by_title("MAID OF HONOUR").owned, Some(0));
    assert_eq!(by_title("Five Leaves Left").owned, None);
    assert_eq!(by_title("Five Leaves Left").index, 5);
}

#[tokio::test]
async fn judge_asks_for_a_few_tracklists_at_a_time_and_always_counts_cached_ones() {
    let asked: Asked = Arc::default();
    let mut service = service(|_| ready(tracks("What A Time To Be Alive")), &asked);
    let cached_title = "Take Care (Edition 3)";
    service.cached = Arc::new(move |album: &Album| (album.title == cached_title).then(drake::what_a_time));
    // Thirteen albums named as one the library has, and six more by an artist it knows.
    let likely: Vec<Album> = (1..=13)
        .map(|i| drake::outside(&format!("Take Care (Edition {i})"), "Drake", 11))
        .collect();
    let known: Vec<Album> = (1..=6)
        .map(|i| drake::outside(&format!("Other Record {i}"), "Drake", 11))
        .collect();
    for album in &likely {
        assert!(
            index().likely(Some(&album.title), Some(&album.artist)),
            "{}",
            album.title
        );
    }
    let all: Vec<Album> = likely.into_iter().chain(known).collect();

    let standings = service.judge(&all, Duration::from_secs(5)).await;

    let asked = asked.lock().clone();
    assert_eq!(
        asked.iter().filter(|(_, background)| !background).count(),
        AlbumOwnership::MAX_READS
    );
    assert_eq!(
        asked.iter().filter(|(_, background)| *background).count(),
        AlbumOwnership::MAX_BACKGROUND_READS
    );
    assert!(!asked.iter().any(|(title, _)| title == cached_title));
    assert_eq!(
        standings
            .iter()
            .find(|standing| standing.album.title == cached_title)
            .expect("judged")
            .owned,
        Some(2)
    );
    assert_eq!(
        standings
            .iter()
            .filter(|standing| standing.owned.is_some())
            .count(),
        1 + AlbumOwnership::MAX_READS + AlbumOwnership::MAX_BACKGROUND_READS
    );
}

#[tokio::test]
async fn judge_sends_what_is_not_read_in_time_uncounted() {
    let asked: Asked = Arc::default();
    let service = service(
        |album| {
            if album.title == "HABIBTI" {
                ready(tracks("HABIBTI"))
            } else {
                Box::pin(futures::future::pending())
            }
        },
        &asked,
    );

    let standings = service
        .judge(
            &[
                drake::outside("HABIBTI", "Drake", 11),
                drake::outside("What A Time To Be Alive", "Drake", 11),
            ],
            Duration::from_millis(200),
        )
        .await;

    assert_eq!(standings[0].owned, Some(11));
    assert_eq!(standings[1].owned, None);
}

/// A request whose own albums are all cached waits only a short grace for the reads it starts
/// for the next request (albums by an artist the library knows), and answers them uncounted.
#[tokio::test]
async fn judge_never_waits_for_reads_meant_for_the_next_request() {
    let asked: Asked = Arc::default();
    let service = service(|_| Box::pin(futures::future::pending()), &asked);

    let started = std::time::Instant::now();
    let standings = service
        .judge(
            &[drake::outside("MAID OF HONOUR", "Drake", 14)],
            Duration::from_secs(5),
        )
        .await;

    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(asked.lock().clone(), [("MAID OF HONOUR".to_string(), true)]);
    assert_eq!(standings[0].owned, None);
}

#[tokio::test]
async fn judge_without_the_library_counts_nothing() {
    let service = AlbumOwnership::empty();
    assert!(
        service
            .judge(&[drake::outside("HABIBTI", "Drake", 11)], Duration::from_secs(1))
            .await
            .is_empty()
    );
}

/// A library album passes through unjudged, and a library that is not read in time counts
/// nothing.
#[tokio::test]
async fn judge_leaves_library_albums_alone_and_gives_up_on_a_slow_library() {
    let asked: Asked = Arc::default();
    let service = service(|album| ready(tracks(&album.title)), &asked);
    let mut local = drake::outside("HABIBTI", "Drake", 11);
    local.is_local = true;
    assert!(service.judge(&[local], Duration::from_secs(1)).await.is_empty());

    let slow = AlbumOwnership::new(
        Arc::new(|_| Box::pin(futures::future::pending())),
        Arc::new(|_, _| Box::pin(async { None })),
        Arc::new(|_| None),
    );
    assert!(
        slow.judge(
            &[drake::outside("HABIBTI", "Drake", 11)],
            Duration::from_millis(50)
        )
        .await
        .is_empty()
    );
    assert!(slow.library(Duration::from_millis(50)).await.is_none());
}

// ---- The library snapshot ----------------------------------------------------------------

#[derive(Default)]
struct Pages {
    reads: AtomicUsize,
    hold: Mutex<Option<Arc<tokio::sync::Notify>>>,
    fail: AtomicBool,
}

impl Pages {
    fn read_page(self: &Arc<Self>) -> crate::services::library::library_snapshot::ReadPage {
        let pages = Arc::clone(self);
        Arc::new(move |start, count| {
            let pages = Arc::clone(&pages);
            Box::pin(async move {
                pages.reads.fetch_add(1, Ordering::SeqCst);
                let hold = pages.hold.lock().clone();
                if let Some(hold) = hold {
                    hold.notified().await;
                }
                if pages.fail.load(Ordering::SeqCst) {
                    return None;
                }
                let rows: Vec<LibrarySongRow> = drake::LIBRARY
                    .iter()
                    .skip(start as usize)
                    .take(count as usize)
                    .cloned()
                    .collect();
                let len = rows.len();
                Some((rows, len))
            })
        })
    }
}

fn snapshot(pages: &Arc<Pages>, now: &Arc<Mutex<DateTime<Utc>>>) -> LibrarySnapshot {
    let now = Arc::clone(now);
    LibrarySnapshot::with(pages.read_page(), Clock::new(move || *now.lock()))
}

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0)
        .single()
        .expect("a time")
}

#[tokio::test]
async fn an_old_copy_is_answered_with_while_a_new_one_is_read() {
    let now = Arc::new(Mutex::new(start()));
    let pages = Arc::new(Pages::default());
    let snapshot = snapshot(&pages, &now);

    let first = snapshot.current().await.expect("read");
    assert!(Arc::ptr_eq(&first, &snapshot.current().await.expect("kept")));
    assert_eq!(pages.reads.load(Ordering::SeqCst), 1);

    *now.lock() += LibrarySnapshot::FRESH_FOR + TimeDelta::seconds(1);
    let hold = Arc::new(tokio::sync::Notify::new());
    *pages.hold.lock() = Some(Arc::clone(&hold));
    // Stale: the old copy at once, and one read under way however many ask.
    assert!(Arc::ptr_eq(&first, &snapshot.current().await.expect("kept")));
    assert!(Arc::ptr_eq(&first, &snapshot.current().await.expect("kept")));
    *pages.hold.lock() = None;
    hold.notify_waiters();
    hold.notify_one();
    // The read finishes in the background; a loaded machine can take a while.
    for _ in 0..500 {
        if !Arc::ptr_eq(&first, &snapshot.current().await.expect("a copy")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(pages.reads.load(Ordering::SeqCst), 2);
    assert!(!Arc::ptr_eq(&first, &snapshot.current().await.expect("a copy")));
}

#[tokio::test]
async fn a_failed_read_is_not_tried_again_at_once() {
    let now = Arc::new(Mutex::new(start()));
    let pages = Arc::new(Pages {
        fail: AtomicBool::new(true),
        ..Default::default()
    });
    let snapshot = snapshot(&pages, &now);

    assert!(snapshot.current().await.is_none());
    assert!(snapshot.current().await.is_none());
    assert_eq!(pages.reads.load(Ordering::SeqCst), 1);

    *now.lock() += LibrarySnapshot::RETRY_AFTER;
    pages.fail.store(false, Ordering::SeqCst);
    assert!(snapshot.current().await.is_some());
}

fn plain_row(id: &str, artist: &str, title: &str, seconds: i32) -> LibrarySongRow {
    LibrarySongRow {
        id: id.into(),
        path: format!("{artist}/{title}.flac"),
        suffix: "flac".into(),
        bit_rate: 900,
        title: title.into(),
        artist: artist.into(),
        duration: Some(seconds),
        ..Default::default()
    }
}

#[test]
fn a_catalog_credit_written_last_name_first_finds_the_librarys_song() {
    let index = LibraryAlbumIndex::new([plain_row("1", "Luis Fonsi", "Despacito", 229)]);
    let found = |artist: &str, seconds: i32| {
        index
            .find("Despacito", Some(artist), Some(seconds), &[], None)
            .map(|row| row.id.clone())
    };
    assert_eq!(found("Fonsi, Luis", 230).as_deref(), Some("1"));
    assert_eq!(found("Luis Fonsi Tribute Band", 229), None);
}

#[test]
fn a_catalog_title_a_typo_away_is_not_the_librarys_song() {
    let index = LibraryAlbumIndex::new([plain_row("1", "Imagine Dragons", "Believer", 204)]);
    assert!(
        index
            .find("Beleiver", Some("Imagine Dragons"), Some(204), &[], None)
            .is_none()
    );
}
