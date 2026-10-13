use super::*;

fn genre(id: &str, name: &str) -> ChartGenre {
    ChartGenre {
        id: id.into(),
        name: name.into(),
    }
}

#[test]
fn parse_charts_keeps_order_drops_repeats_and_junk() {
    assert_eq!(
        parse_charts("new, 18,trending,18,x,,6"),
        vec!["new", "18", "trending", "6"]
    );
    assert_eq!(parse_charts(""), Vec::<String>::new());
    assert_eq!(parse_charts(" , ,"), Vec::<String>::new());
}

#[test]
fn parse_charts_never_takes_the_overall() {
    assert_eq!(parse_charts("34,new"), vec!["new"]);
}

#[test]
fn parse_charts_stops_at_fourteen() {
    let many: Vec<String> = (1..=20).map(|n| n.to_string()).collect();
    assert_eq!(parse_charts(&many.join(",")).len(), MAX_LISTS);
}

#[test]
fn parse_charts_reads_any_case() {
    assert_eq!(parse_charts("NEW,Trending"), vec!["new", "trending"]);
}

#[test]
fn kind_table() {
    let cases = [
        ("34", Some(ChartKind::Overall)),
        ("new", Some(ChartKind::NewSongs)),
        ("trending", Some(ChartKind::Trending)),
        ("18", Some(ChartKind::Genre)),
        ("1153", Some(ChartKind::Genre)),
        ("", None),
        ("pop", None),
        ("-1", None),
        ("12345678901", None),
    ];
    for (id, want) in cases {
        assert_eq!(kind(id), want, "{id}");
    }
}

#[test]
fn parse_country_auto_and_fixed() {
    assert_eq!(parse_country("auto"), ChartCountry::Auto);
    assert_eq!(parse_country(""), ChartCountry::Auto);
    assert_eq!(parse_country("US"), ChartCountry::Fixed("us".into()));
    assert_eq!(parse_country(" gb "), ChartCountry::Fixed("gb".into()));
    assert_eq!(parse_country("usa"), ChartCountry::Auto);
    assert_eq!(parse_country("u1"), ChartCountry::Auto);
}

#[test]
fn is_valid_country_table() {
    for ok in ["auto", "AUTO", "us", "GB", " jp "] {
        assert!(is_valid_country(ok), "{ok}");
    }
    for bad in ["", "usa", "u", "u1", "../"] {
        assert!(!is_valid_country(bad), "{bad}");
    }
}

#[test]
fn is_valid_charts_table() {
    for ok in ["", "new,trending,18,14,6", " NEW , 34 ", "18,,6"] {
        assert!(is_valid_charts(ok), "{ok}");
    }
    for bad in ["pop", "new;18", "18,x", "-3"] {
        assert!(!is_valid_charts(bad), "{bad}");
    }
}

#[test]
fn the_overall_keeps_its_old_key_and_name() {
    assert_eq!(playlist_key(OVERALL), "chart:popular");
    assert_eq!(playlist_name(OVERALL, "All Genres"), "Popular right now");
}

#[test]
fn names_and_keys_of_new_and_trending() {
    assert_eq!(playlist_key("new"), "chart:new");
    assert_eq!(playlist_key("trending"), "chart:trending");
    assert_eq!(playlist_name("new", ""), "Best New Songs");
    assert_eq!(playlist_name("trending", ""), "Trending Songs");
}

#[test]
fn a_playlist_key_names_its_chart_again() {
    for id in ["34", "new", "trending", "18", "1153"] {
        assert_eq!(chart_from_key(&playlist_key(id)).as_deref(), Some(id), "{id}");
    }
    assert_eq!(chart_from_key("genre:Rock"), None);
    assert_eq!(chart_from_key("chart:genre:pop"), None);
}

#[test]
fn kind_words() {
    assert_eq!(kind_word("34"), Some("overall"));
    assert_eq!(kind_word("new"), Some("new"));
    assert_eq!(kind_word("trending"), Some("trending"));
    assert_eq!(kind_word("6"), Some("genre"));
    assert_eq!(kind_word("x"), None);
}

#[test]
fn a_genre_is_top_its_name() {
    assert_eq!(playlist_key("18"), "chart:genre:18");
    assert_eq!(playlist_name("18", "Hip-Hop/Rap"), "Top Hip-Hop/Rap");
}

#[test]
fn a_known_genre_is_named_in_english_an_unknown_one_by_apple() {
    let menu = vec![
        genre("18", "ヒップホップ／ラップ"),
        genre("29", "アニメ"),
        genre("27", "J-Pop"),
    ];
    assert_eq!(genre_name(&menu, "18").as_deref(), Some("Hip-Hop/Rap"));
    assert_eq!(genre_name(&menu, "29").as_deref(), Some("アニメ"));
    assert_eq!(genre_name(&menu, "27").as_deref(), Some("J-Pop"));
    assert_eq!(genre_name(&menu, "999"), None);
}

#[test]
fn max_songs_50_for_charts_100_for_rooms() {
    assert_eq!(max_songs(OVERALL), 50);
    assert_eq!(max_songs("18"), 50);
    assert_eq!(max_songs("new"), 100);
    assert_eq!(max_songs("trending"), 100);
}

#[test]
fn english_menu_starts_with_all_genres() {
    let menu = english_menu();
    assert_eq!(menu.len(), 22);
    assert_eq!(menu[0], genre("34", "All Genres"));
}

#[test]
fn apple_countries_are_sorted_two_letter_codes() {
    assert_eq!(APPLE_COUNTRIES.len(), 172);
    assert!(APPLE_COUNTRIES.windows(2).all(|w| w[0] < w[1]));
    assert!(
        APPLE_COUNTRIES
            .iter()
            .all(|c| super::super::apple_pages::is_country_code(c))
    );
}
