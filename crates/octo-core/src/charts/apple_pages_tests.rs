use super::*;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/rust-migration/fixtures/apple-music/",
            $name,
            ".html"
        ))
    };
}

fn room(id: &str, title: &str) -> SongRoom {
    SongRoom {
        room_id: id.into(),
        title: title.into(),
    }
}

#[test]
fn parses_the_hip_hop_chart() {
    let page = parse_songs(fixture!("us_18")).unwrap();
    assert_eq!(page.country, "us");
    assert_eq!(page.genre_id.as_deref(), Some("18"));
    assert_eq!(page.title, "Top Hip-Hop/Rap Songs");
    assert_eq!(page.songs.len(), 5);
    let first = &page.songs[0];
    assert_eq!(first.rank, 1);
    assert_eq!(first.title, "Janice STFU");
    assert_eq!(first.artists, vec!["Drake"]);
    assert_eq!(first.album.as_deref(), Some("ICEMAN"));
    assert_eq!(first.song_id.as_deref(), Some("6769568596"));
    assert_eq!(first.album_id.as_deref(), Some("6769568449"));
    // 237,344 ms
    assert_eq!(first.duration_secs, Some(237));
    assert!(first.explicit);
    assert!(first.artwork.as_deref().unwrap().contains("{w}x{h}bb.{f}"));
    assert_eq!(page.songs[4].rank, 5);
}

#[test]
fn the_overall_chart_is_genre_34() {
    let page = parse_songs(fixture!("us_all")).unwrap();
    assert_eq!(page.genre_id.as_deref(), Some("34"));
    assert_eq!(page.title, "Top Songs");
    assert_eq!(page.songs[0].title, "Solar Eclipse");
    assert!(!page.songs[2].explicit, "Cold Shoulder carries no badge");
}

#[test]
fn reads_every_artist_main_first() {
    let page = parse_songs(fixture!("us_all")).unwrap();
    assert_eq!(page.songs[0].artists, vec!["Drake", "Don Toliver"]);
    assert_eq!(page.songs[2].artists, vec!["Drake", "Don Toliver", "Yebba"]);
}

#[test]
fn reads_the_countrys_genre_menu() {
    let us = parse_songs(fixture!("us_18")).unwrap();
    assert_eq!(us.genres.len(), 22);
    assert_eq!(
        us.genres[0],
        ChartGenre {
            id: "34".into(),
            name: "All Genres".into()
        }
    );
    assert!(us.genres.iter().any(|g| g.id == "18" && g.name == "Hip-Hop/Rap"));
    assert!(us.genres.iter().any(|g| g.id == "12"));

    let gb = parse_songs(fixture!("gb_18")).unwrap();
    assert_eq!(gb.country, "gb");
    assert_eq!(gb.genres.len(), 19);
    assert!(
        !gb.genres.iter().any(|g| g.id == "12"),
        "the UK has no Latin chart"
    );
}

#[test]
fn a_japanese_menu_keeps_apples_words() {
    let jp = parse_songs(fixture!("jp_all")).unwrap();
    assert_eq!(jp.country, "jp");
    assert_eq!(jp.title, "トップソング");
    assert!(jp.genres.iter().any(|g| g.id == "27" && g.name == "J-Pop"));
    assert!(jp.genres.iter().any(|g| g.id == "29" && g.name == "アニメ"));
}

#[test]
fn reads_a_room_of_songs() {
    let page = parse_songs(fixture!("us_room_best_new")).unwrap();
    assert_eq!(page.genre_id, None);
    assert_eq!(page.title, "Best New Songs");
    assert!(page.genres.is_empty());
    let ranks: Vec<u32> = page.songs.iter().map(|s| s.rank).collect();
    assert_eq!(ranks, vec![1, 2, 3, 4, 5]);
    assert_eq!(page.songs[1].title, "Chelsea Boots (Bonus Track)");

    let trending = parse_songs(fixture!("us_room_trending")).unwrap();
    assert_eq!(trending.title, "Trending Songs");
    assert_eq!(trending.songs[2].artists, vec!["Madonna", "Charli xcx"]);
}

#[test]
fn the_new_page_names_this_weeks_song_rooms() {
    let us = parse_song_rooms(fixture!("us_new")).unwrap();
    assert_eq!(
        us,
        vec![
            room("6818396128", "Best New Songs"),
            room("6818395867", "Trending Songs")
        ]
    );
    let jp = parse_song_rooms(fixture!("jp_new_en")).unwrap();
    assert_eq!(jp[0], room("6818340711", "Best New Songs"));
}

#[test]
fn the_shelf_title_is_read_from_its_title_link() {
    let jp = parse_song_rooms(fixture!("jp_new")).unwrap();
    assert_eq!(jp[0].title, "ベストニューソング");
    assert_eq!(jp[1].room_id, "6818349088");
}

#[test]
fn room_for_goes_by_title_then_by_position() {
    let swapped = vec![room("2", "Trending Songs"), room("1", "Best New Songs")];
    assert_eq!(room_for(&swapped, RoomKind::BestNewSongs).unwrap().room_id, "1");
    assert_eq!(room_for(&swapped, RoomKind::TrendingSongs).unwrap().room_id, "2");

    let unnamed = vec![room("1", "ベストニューソング"), room("2", "注目ソング")];
    assert_eq!(room_for(&unnamed, RoomKind::BestNewSongs).unwrap().room_id, "1");
    assert_eq!(room_for(&unnamed, RoomKind::TrendingSongs).unwrap().room_id, "2");

    let one_known = vec![room("1", "Best New Songs"), room("2", "Something Else")];
    assert_eq!(room_for(&one_known, RoomKind::TrendingSongs), None);

    let only_one = vec![room("1", "ベストニューソング")];
    assert_eq!(room_for(&only_one, RoomKind::TrendingSongs), None);
}

#[test]
fn parse_countries_reads_the_switcher() {
    let countries = parse_countries(fixture!("us_all"));
    assert_eq!(countries.len(), 172);
    for code in ["us", "gb", "jp", "zw"] {
        assert!(countries.iter().any(|c| c == code), "{code}");
    }
    let mut sorted = countries.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(countries, sorted);
}

#[test]
fn a_page_without_a_switcher_has_no_countries() {
    assert!(parse_countries(fixture!("us_18")).is_empty());
    assert!(
        parse_countries(r#"<a href="/us/new">US</a><a href="?l=es" data-testid="region-list-link">"#)
            .is_empty()
    );
}

#[test]
fn a_page_without_the_data_is_no_data() {
    assert_eq!(parse_songs(fixture!("no_script")), Err(ApplePageError::NoData));
    assert_eq!(
        parse_song_rooms(fixture!("no_script")),
        Err(ApplePageError::NoData)
    );
}

#[test]
fn a_changed_shape_is_no_songs() {
    assert_eq!(
        parse_songs(fixture!("shape_changed")),
        Err(ApplePageError::NoSongs)
    );
}

#[test]
fn broken_json_is_not_json() {
    let html = r#"<script type="application/json" id="serialized-server-data">{"data":[</script>"#;
    assert_eq!(parse_songs(html), Err(ApplePageError::NotJson));
}

#[test]
fn a_new_page_without_song_shelves_is_no_songs() {
    assert_eq!(
        parse_song_rooms(fixture!("us_18").replace("trackLockup", "squareLockup").as_str()),
        Err(ApplePageError::NoSongs)
    );
}

#[test]
fn a_big_new_page_parses() {
    let page = fixture!("us_new");
    let filler = r#"{"id":"square-lockup-swoosh - 1","itemKind":"squareLockup","header":{"item":{"titleLink":{"title":"Filler"}}},"items":[]},"#;
    let mut big = page.to_string();
    while big.len() < 2_500_000 {
        big = big.replacen(
            r#""sections":["#,
            &format!(r#""sections":[{}"#, filler.repeat(2_000)),
            1,
        );
    }
    let rooms = parse_song_rooms(&big).unwrap();
    assert_eq!(rooms.len(), 2);
}

#[test]
fn is_country_code_takes_two_lowercase_letters_only() {
    assert!(is_country_code("us"));
    for bad in ["US", "usa", "u/", "..", "", "u"] {
        assert!(!is_country_code(bad), "{bad}");
    }
}

#[test]
fn artwork_at_fills_the_template() {
    assert_eq!(
        artwork_at("https://is1-ssl.mzstatic.com/a/b.jpg/{w}x{h}bb.{f}", 1200),
        "https://is1-ssl.mzstatic.com/a/b.jpg/1200x1200bb.jpg"
    );
}
