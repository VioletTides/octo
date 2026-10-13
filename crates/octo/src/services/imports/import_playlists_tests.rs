//! "Keep as playlist": the playlist an import keeps, and what a failure leaves behind.

use super::super::ImportTrack;
use super::super::test_support::FakeNavidrome;
use super::*;

fn list(owner: &str, library_ids: &[Option<&str>]) -> ImportList {
    ImportList {
        id: "spotify-p1".into(),
        owner: owner.into(),
        name: "Road trip".into(),
        source: import_sources::SPOTIFY_PLAYLIST.into(),
        tracks: library_ids
            .iter()
            .enumerate()
            .map(|(i, id)| ImportTrack {
                key: format!("k{i}"),
                title: format!("S{i}"),
                artist: "A".into(),
                library_id: id.map(str::to_string),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn writer(navidrome: &Arc<FakeNavidrome>) -> ImportPlaylists {
    ImportPlaylists::new(navidrome.clone() as Arc<dyn PlaylistNavidrome>)
}

#[tokio::test]
async fn a_playlist_is_made_for_its_owner_with_the_songs_the_library_has() {
    let navidrome = Arc::new(FakeNavidrome::default());
    let result = writer(&navidrome)
        .sync(&list("alice", &[Some("a"), None, Some("c")]))
        .await;
    assert!(result.ok);
    assert_eq!(result.playlist_id.as_deref(), Some("pl1"));
    assert_eq!(result.note, None);
    assert_eq!(navidrome.playlist("pl1"), ["a", "c"]);
    assert_eq!(
        navidrome.calls(),
        ["create Road trip", "give pl1 alice", "add a,c"]
    );
}

#[tokio::test]
async fn a_playlist_navidrome_would_not_hand_on_says_whose_it_is() {
    let navidrome = Arc::new(FakeNavidrome::default());
    *navidrome.give_works.lock() = false;
    let result = writer(&navidrome).sync(&list("alice", &[Some("a")])).await;
    assert!(result.note.as_deref().is_some_and(|n| n.contains("under admin")));
    let own = Arc::new(FakeNavidrome::default());
    writer(&own).sync(&list("admin", &[Some("a")])).await;
    assert_eq!(*own.given_to.lock(), None);
}

#[tokio::test]
async fn only_what_differs_is_written_keeping_the_songs_before_it() {
    let navidrome = Arc::new(FakeNavidrome::default());
    navidrome
        .playlists
        .lock()
        .insert("pl1".into(), vec!["a".into(), "c".into(), "d".into()]);
    let mut kept = list("alice", &[Some("a"), Some("b"), Some("c"), Some("d")]);
    kept.playlist_id = Some("pl1".into());
    let writer = writer(&navidrome);
    writer.sync(&kept).await;
    assert_eq!(navidrome.playlist("pl1"), ["a", "b", "c", "d"]);
    assert_eq!(navidrome.calls(), ["remove 2,3", "add b,c,d"]);

    navidrome.calls.lock().clear();
    let again = writer.sync(&kept).await;
    assert!(again.ok);
    assert!(navidrome.calls().is_empty());
}

#[tokio::test]
async fn a_long_tail_is_removed_from_the_end_in_chunks() {
    let navidrome = Arc::new(FakeNavidrome::default());
    navidrome
        .playlists
        .lock()
        .insert("pl1".into(), (0..250).map(|i| format!("old{i}")).collect());
    let mut kept = list("alice", &[Some("new")]);
    kept.playlist_id = Some("pl1".into());
    writer(&navidrome).sync(&kept).await;
    assert_eq!(navidrome.playlist("pl1"), ["new"]);
    let removals: Vec<String> = navidrome
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("remove"))
        .collect();
    assert_eq!(removals.len(), 3);
    assert!(removals[0].starts_with("remove 151,"), "{}", removals[0]);
}

#[tokio::test]
async fn a_playlist_the_person_deleted_stays_deleted() {
    let navidrome = Arc::new(FakeNavidrome::default());
    let mut kept = list("alice", &[Some("a")]);
    kept.playlist_id = Some("gone".into());
    let result = writer(&navidrome).sync(&kept).await;
    assert!(result.deleted);
    assert!(navidrome.calls().is_empty());
}

/// A playlist this call made and could not fill is deleted again, so a failure leaves nothing
/// empty behind, and nothing is kept that the next try would find gone.
#[tokio::test]
async fn a_playlist_that_could_not_be_filled_is_not_left_behind() {
    let navidrome = Arc::new(FakeNavidrome::default());
    *navidrome.fail_add.lock() = true;
    let result = writer(&navidrome).sync(&list("alice", &[Some("a")])).await;
    assert!(!result.ok);
    assert_eq!(result.playlist_id, None);
    assert!(navidrome.playlists.lock().is_empty(), "no empty playlist is left");
    assert_eq!(
        navidrome.calls(),
        ["create Road trip", "give pl1 alice", "add a", "delete pl1"]
    );
}

/// When Navidrome will not delete it either, the next try fills that playlist rather than
/// making another.
#[tokio::test]
async fn a_playlist_that_would_not_go_is_filled_next_time_not_made_again() {
    let navidrome = Arc::new(FakeNavidrome::default());
    *navidrome.fail_add.lock() = true;
    *navidrome.fail_delete.lock() = true;
    let writer = writer(&navidrome);
    let mut kept = list("alice", &[Some("a")]);
    let first = writer.sync(&kept).await;
    assert!(!first.ok);
    assert_eq!(first.playlist_id.as_deref(), Some("pl1"));

    *navidrome.fail_add.lock() = false;
    kept.playlist_id = first.playlist_id;
    let second = writer.sync(&kept).await;
    assert!(second.ok);
    assert_eq!(navidrome.playlists.lock().len(), 1, "still one playlist");
    assert_eq!(navidrome.playlist("pl1"), ["a"]);
}
