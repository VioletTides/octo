//! Plain song lists, read alone and through the file reader.

use super::super::LibraryIndex;
use super::super::test_support::row;
use super::*;

fn pairs(list: &FileList) -> Vec<(&str, &str)> {
    list.tracks
        .iter()
        .map(|t| (t.artist.as_str(), t.title.as_str()))
        .collect()
}

fn read(name: &str, text: &str) -> Vec<FileList> {
    ImportFileReader::read(name, text.as_bytes()).expect("read")
}

#[test]
fn artist_dash_title_lines_are_read_as_one_list_named_after_the_file() {
    let lists = read(
        "Road trip.txt",
        "Rick Astley - Never Gonna Give You Up\nMassive Attack - Teardrop\n",
    );
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "Road trip");
    assert_eq!(
        pairs(&lists[0]),
        [
            ("Rick Astley", "Never Gonna Give You Up"),
            ("Massive Attack", "Teardrop")
        ]
    );
    assert_eq!(
        lists[0].tracks[0].key,
        ImportKeys::for_words(Some("Rick Astley"), Some("Never Gonna Give You Up"))
    );
    assert_eq!(lists[0].tracks[0].seconds, None);
    assert_eq!(lists[0].tracks[0].album, None);
}

#[test]
fn track_numbers_in_front_are_dropped() {
    let lists = read(
        "list.txt",
        "1. Queen - Bohemian Rhapsody\n02) Prince - 1999\n3.Toto - Africa\n",
    );
    assert_eq!(
        pairs(&lists[0]),
        [
            ("Queen", "Bohemian Rhapsody"),
            ("Prince", "1999"),
            ("Toto", "Africa")
        ]
    );
}

#[test]
fn en_and_em_dashes_and_tabs_split_too_and_only_the_first_separator_counts() {
    let lists = read(
        "list.txt",
        "Daft Punk \u{2013} One More Time\nAdele \u{2014} Hello\nBeyonc\u{e9}\tHalo\nThe Beatles - Let It Be - Remastered 2009\n",
    );
    assert_eq!(
        pairs(&lists[0]),
        [
            ("Daft Punk", "One More Time"),
            ("Adele", "Hello"),
            ("Beyonc\u{e9}", "Halo"),
            ("The Beatles", "Let It Be - Remastered 2009"),
        ]
    );
}

#[test]
fn a_dash_inside_a_word_does_not_split() {
    let lists = read("list.txt", "Jay-Z - Run-DMC Tribute\n");
    assert_eq!(pairs(&lists[0]), [("Jay-Z", "Run-DMC Tribute")]);
}

#[test]
fn playlist_lines_start_new_lists() {
    let lists = read(
        "export.txt",
        "Loose - Song\n#playlist: Gym\nA - One\nB - Two\n#Playlist:   Chill  \nC - Three\n#playlist: Gym\nD - Four\n",
    );
    assert_eq!(
        lists.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["export", "Gym", "Chill"]
    );
    assert_eq!(pairs(&lists[0]), [("Loose", "Song")]);
    assert_eq!(pairs(&lists[1]), [("A", "One"), ("B", "Two"), ("D", "Four")]);
    assert_eq!(pairs(&lists[2]), [("C", "Three")]);
}

#[test]
fn a_list_with_only_playlist_lines_drops_the_empty_file_list() {
    let lists = read("export.txt", "#playlist: Gym\nA - One\n");
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "Gym");
}

#[test]
fn crlf_line_ends_a_bom_blank_lines_and_comments_are_all_ignored() {
    let text = "\u{feff}# my list\r\n\r\nRick Astley - Never Gonna Give You Up\r\n   \r\n# another comment\r\nToto - Africa\r\n";
    let lists = read("list.txt", text);
    assert_eq!(
        pairs(&lists[0]),
        [("Rick Astley", "Never Gonna Give You Up"), ("Toto", "Africa")]
    );
    // The BOM as bytes, as a file from Windows carries it.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"Toto - Africa\r\n");
    let lists = ImportFileReader::read("list.txt", &bytes).expect("read");
    assert_eq!(pairs(&lists[0]), [("Toto", "Africa")]);
}

#[test]
fn a_line_without_a_separator_is_a_title_alone() {
    let lists = read("list.txt", "Toto - Africa\nBohemian Rhapsody\n");
    assert_eq!(pairs(&lists[0]), [("Toto", "Africa"), ("", "Bohemian Rhapsody")]);
    let alone = &lists[0].tracks[1];
    assert_eq!(
        alone.key,
        ImportKeys::for_words(Some(""), Some("Bohemian Rhapsody"))
    );

    // The matcher looks for it without falling over.
    let index = LibraryIndex::new([
        row("1", "Queen", "Bohemian Rhapsody", Some(354), None, "flac", 900),
        row("2", "Toto", "Africa", Some(295), None, "flac", 900),
    ]);
    let _ = index.find(alone);
    assert_eq!(index.find(&lists[0].tracks[0]).map(|r| r.id.as_str()), Some("2"));
}

#[test]
fn a_dash_with_one_side_empty_keeps_the_other_as_a_title() {
    let lists = read("list.txt", "\tAfrica\n");
    assert_eq!(pairs(&lists[0]), [("", "Africa")]);
}

#[test]
fn by_in_a_title_is_never_read_as_a_credit() {
    let lists = read("list.txt", "Stand by Me\nBen E. King - Stand by Me\n");
    assert_eq!(
        pairs(&lists[0]),
        [("", "Stand by Me"), ("Ben E. King", "Stand by Me")]
    );
}

#[test]
fn a_first_line_naming_the_columns_is_a_header() {
    let lists = read("list.txt", "Artist - Title\nToto - Africa\n");
    assert_eq!(pairs(&lists[0]), [("Toto", "Africa")]);
    let lists = read("list.txt", "Track name\tArtist name\nAfrica\tToto\n");
    assert_eq!(pairs(&lists[0]), [("Africa", "Toto")]);
}

#[test]
fn a_list_with_no_song_line_is_refused_in_plain_words() {
    let refused = ImportFileReader::read("list.txt", b"# only a comment\n\n").expect_err("refused");
    assert_eq!(
        refused,
        FileReadError::Format("No songs found. Each line should read Artist - Title.".to_string())
    );
}

#[test]
fn a_comma_in_an_artist_does_not_make_the_text_a_table() {
    let lists = read(
        "pasted",
        "Earth, Wind & Fire - September\nCrosby, Stills & Nash - Helplessly Hoping\n",
    );
    assert_eq!(
        pairs(&lists[0]),
        [
            ("Earth, Wind & Fire", "September"),
            ("Crosby, Stills & Nash", "Helplessly Hoping")
        ]
    );
}

#[test]
fn a_real_table_with_its_header_is_still_read_as_csv() {
    let csv =
        "Track name,Artist name,Album,Playlist name\nSeptember,\"Earth, Wind & Fire\",The Best Of,Party\n";
    for name in ["export", "export.csv"] {
        let lists = read(name, csv);
        assert_eq!(lists.len(), 1, "{name}");
        assert_eq!(lists[0].name, "Party");
        assert_eq!(pairs(&lists[0]), [("Earth, Wind & Fire", "September")]);
        assert_eq!(lists[0].tracks[0].album.as_deref(), Some("The Best Of"));
    }
}

#[test]
fn a_csv_file_with_an_unknown_header_names_both_ways_to_write_it() {
    let error = ImportFileReader::read("x.csv", b"Foo,Bar\n1,2\n").expect_err("refused");
    assert_eq!(
        error,
        FileReadError::Format(
            "The file needs a title column and an artist column, named in its first row, or a plain list with one Artist - Title per line."
                .to_string()
        )
    );
}

#[test]
fn a_list_of_more_than_50000_songs_is_refused_in_plain_words() {
    let mut text = String::new();
    for i in 0..=ImportFileReader::MAX_SONGS {
        text.push_str(&format!("Someone - Song {i}\n"));
    }
    for name in ["Everything.txt", "Everything"] {
        let refused = ImportFileReader::read(name, text.as_bytes()).expect_err("refused");
        assert_eq!(
            refused,
            FileReadError::Format(
                "It holds more than 50,000 songs. Split it into smaller files.".to_string()
            )
        );
    }
    // Exactly the limit is fine.
    let at_limit: String = text.lines().skip(1).map(|l| format!("{l}\n")).collect();
    let lists = read("Everything.txt", &at_limit);
    assert_eq!(lists[0].tracks.len(), ImportFileReader::MAX_SONGS);
}

// ---- Which half is the artist ---------------------------------------------------------------

fn lines_of(text: &str) -> Vec<TextLine> {
    read_lines("list", text).expect("read").remove(0).lines
}

/// A tiny library: these artists and titles are known.
fn known(artist: &str, title: &str) -> bool {
    matches!(
        (artist, title),
        ("Toto", "Africa") | ("Queen", "Bohemian Rhapsody") | ("Adele", "Hello")
    )
}

#[test]
fn orientation_keeps_artist_title_when_that_matches_more() {
    let lines = lines_of("Toto - Africa\nQueen - Bohemian Rhapsody\nNobody - Nothing\n");
    assert_eq!(choose_orientation(&lines, known), Orientation::ArtistTitle);
}

#[test]
fn orientation_turns_round_when_title_artist_matches_more() {
    let lines = lines_of("Africa - Toto\nBohemian Rhapsody - Queen\nToto - Africa\nTitle only\n");
    assert_eq!(choose_orientation(&lines, known), Orientation::TitleArtist);
    let lists = to_file_lists(
        vec![TextList {
            name: "list".into(),
            lines,
        }],
        Orientation::TitleArtist,
    );
    assert_eq!(
        pairs(&lists[0]),
        [
            ("Toto", "Africa"),
            ("Queen", "Bohemian Rhapsody"),
            ("Africa", "Toto"),
            ("", "Title only")
        ]
    );
}

#[test]
fn orientation_on_a_tie_or_with_nothing_known_is_artist_title() {
    let lines = lines_of("Africa - Toto\nAdele - Hello\n");
    assert_eq!(choose_orientation(&lines, known), Orientation::ArtistTitle);
    let lines = lines_of("A - B\nC - D\n");
    assert_eq!(choose_orientation(&lines, |_, _| false), Orientation::ArtistTitle);
}

#[test]
fn orientation_looks_only_at_the_first_fifty_lines_with_both_halves() {
    let mut text = String::new();
    for _ in 0..10 {
        text.push_str("Just a title\n");
    }
    for _ in 0..ORIENTATION_SAMPLE {
        text.push_str("Toto - Africa\n");
    }
    for _ in 0..(ORIENTATION_SAMPLE + 10) {
        text.push_str("Africa - Toto\n");
    }
    let lines = lines_of(&text);
    assert_eq!(choose_orientation(&lines, known), Orientation::ArtistTitle);
    let asked = std::cell::Cell::new(0);
    choose_orientation(&lines, |_, _| {
        asked.set(asked.get() + 1);
        false
    });
    assert_eq!(asked.get(), ORIENTATION_SAMPLE * 2);
}

// ---- Real exports ----------------------------------------------------------------------------

const TUNEMYMUSIC_TXT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/imports/tunemymusic-freetext.txt"
));

const TUNEMYMUSIC_CSV: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/imports/tunemymusic-freetext.csv"
));

#[test]
fn a_tunemymusic_txt_export_is_read_artist_first() {
    let lists = read("My Free text Library.txt", TUNEMYMUSIC_TXT);
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "My Free text Library");
    assert_eq!(
        pairs(&lists[0]),
        [
            ("The Beatles", "Hey Jude"),
            ("Beyonce", "Halo"),
            ("Earth, Wind & Fire", "September"),
            ("Drake", "Too Good (feat. Rihanna)"),
            ("Adele", "Hello"),
            ("Queen", "Bohemian Rhapsody - Remastered 2011"),
            ("Massive Attack", "Teardrop"),
            ("Ben E. King", "Stand by Me"),
            ("Luis Fonsi, Daddy Yankee", "Despacito"),
            ("Kendrick Lamar", "HUMBLE."),
        ]
    );
}

#[test]
fn a_tunemymusic_csv_from_free_text_splits_its_title_column() {
    let lists = read("My Free text Library.csv", TUNEMYMUSIC_CSV);
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "My Playlist");
    assert_eq!(
        pairs(&lists[0]),
        [
            ("The Beatles", "Hey Jude"),
            ("Beyonce", "Halo"),
            ("Earth, Wind & Fire", "September"),
            ("Drake", "Too Good (feat. Rihanna)"),
            ("Queen", "Bohemian Rhapsody - Remastered 2011"),
            ("Ben E. King", "Stand by Me"),
            ("Luis Fonsi, Daddy Yankee", "Despacito"),
            ("Monetochka", "Kazhdyy raz"),
            ("Beyonc\u{e9}", "Formation"),
            ("Kendrick Lamar", "HUMBLE."),
        ]
    );
    assert_eq!(
        lists[0].tracks[2].key,
        ImportKeys::for_words(Some("Earth, Wind & Fire"), Some("September"))
    );
}

#[test]
fn a_csv_row_with_its_own_artist_keeps_its_title_whole() {
    let csv = "Track name,Artist name\n\"Bohemian Rhapsody - Remastered 2011\",\"Queen\"\n\"Lonely - Song\",\"\"\nNo dash here,\n";
    let lists = read("export.csv", csv);
    assert_eq!(
        pairs(&lists[0]),
        [
            ("Queen", "Bohemian Rhapsody - Remastered 2011"),
            ("Lonely", "Song")
        ]
    );
}
