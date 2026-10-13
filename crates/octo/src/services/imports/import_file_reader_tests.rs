//! Imports from exported files: the files, their lengths and their limits.

use std::io::Write;

use super::*;

fn zip_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut bytes);
        let options =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in files {
            zip.start_file(name.as_str(), options).expect("an entry");
            zip.write_all(content).expect("written");
        }
        zip.finish().expect("finished");
    }
    bytes.into_inner()
}

#[test]
fn an_exportify_csv_is_read_with_quotes_commas_and_line_breaks() {
    let csv = "\u{feff}Track URI,Track Name,Artist Name(s),Album Name,Track Duration (ms),ISRC\r\n"
        .to_string()
        + "spotify:track:4uLU6hMCjMI75M1A2tKUQC,Never Gonna Give You Up,Rick Astley,Whenever You Need Somebody,213573,GBARL9300135\r\n"
        + "spotify:track:7ouMYWpwJ422jRcDASZB7P,\"Teardrop, \"\"Live\"\"\",\"Massive Attack, Elizabeth Fraser\",\"Mezzanine\nDeluxe\",330000,\r\n";
    let lists = ImportFileReader::read("Road trip.csv", csv.as_bytes()).expect("read");
    assert_eq!(lists.len(), 1);
    let list = &lists[0];
    assert_eq!(list.name, "Road trip");
    assert_eq!(list.tracks.len(), 2);
    assert_eq!(list.tracks[0].key, "spotify:4uLU6hMCjMI75M1A2tKUQC");
    assert_eq!(list.tracks[0].seconds, Some(214));
    assert_eq!(list.tracks[0].isrc.as_deref(), Some("GBARL9300135"));
    assert_eq!(list.tracks[1].title, "Teardrop, \"Live\"");
    assert_eq!(list.tracks[1].album.as_deref(), Some("Mezzanine\nDeluxe"));
}

#[test]
fn a_csv_with_a_playlist_column_becomes_one_list_each() {
    let csv = "Track name;Artist name;Album;Playlist name;Duration\n\
        Song A;Artist;Album;First;3:20\nSong B;Artist;Album;Second;200\nSong C;Artist;;First;\n";
    let lists = ImportFileReader::read("export.csv", csv.as_bytes()).expect("read");
    assert_eq!(
        lists.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["First", "Second"]
    );
    assert_eq!(lists[0].tracks[0].seconds, Some(200));
    assert_eq!(lists[1].tracks[0].seconds, Some(200));
    assert_eq!(lists[0].tracks.len(), 2);
    assert!(lists[0].tracks[0].key.starts_with("song:"));
}

#[test]
fn a_csv_without_title_or_artist_says_what_it_needs() {
    let error = ImportFileReader::read("x.csv", b"Foo,Bar\n1,2\n").expect_err("refused");
    assert!(matches!(&error, FileReadError::Format(m) if m.contains("title column and an artist column")));
}

const YOUR_LIBRARY: &str = r#"{"tracks":[{"artist":"Rick Astley","album":"Whenever You Need Somebody","track":"Never Gonna Give You Up","uri":"spotify:track:4uLU6hMCjMI75M1A2tKUQC"}],
 "albums":[],"shows":[],"episodes":[],"bannedTracks":[],"artists":[],"bannedArtists":[],"other":[]}"#;

const PLAYLIST1: &str = r#"{"playlists":[{"name":"Gym","lastModifiedDate":"2026-01-02","description":null,"numberOfFollowers":0,
  "items":[{"track":{"trackName":"Teardrop","artistName":"Massive Attack","albumName":"Mezzanine","trackUri":"spotify:track:7ouMYWpwJ422jRcDASZB7P"},
            "episode":null,"localTrack":null,"addedDate":"2026-01-01"},
           {"track":null,"episode":{"episodeName":"A podcast"},"localTrack":null}]}]}"#;

#[test]
fn spotifys_own_data_export_is_read_as_json_or_its_zip() {
    let liked = ImportFileReader::read("YourLibrary.json", YOUR_LIBRARY.as_bytes()).expect("read");
    assert_eq!(liked.len(), 1);
    assert_eq!(liked[0].name, "Liked Songs");

    let zip = zip_of(&[
        (
            "Spotify Account Data/YourLibrary.json".to_string(),
            YOUR_LIBRARY.as_bytes().to_vec(),
        ),
        (
            "Spotify Account Data/Playlist1.json".to_string(),
            PLAYLIST1.as_bytes().to_vec(),
        ),
        (
            "Spotify Account Data/StreamingHistory_music_0.json".to_string(),
            b"[]".to_vec(),
        ),
    ]);
    let lists = ImportFileReader::read("my_spotify_data.zip", &zip).expect("read");
    let mut names: Vec<&str> = lists.iter().map(|l| l.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Gym", "Liked Songs"]);
    let gym = lists.iter().find(|l| l.name == "Gym").expect("gym");
    assert_eq!(gym.tracks.len(), 1);
    assert_eq!(gym.tracks[0].key, "spotify:7ouMYWpwJ422jRcDASZB7P");
}

#[test]
fn lengths_are_read_in_every_usual_form() {
    let cases: [(&str, bool, Option<i32>); 7] = [
        ("213573", true, Some(214)),
        ("3:20", false, Some(200)),
        ("1:02:03", false, Some(3723)),
        ("245", false, Some(245)),
        ("245000", false, Some(245)),
        ("", false, None),
        ("soon", false, None),
    ];
    for (value, ms, seconds) in cases {
        assert_eq!(
            ImportFileReader::seconds(Some(value), ms),
            seconds,
            "case {value}"
        );
    }
}

// ---- Limits on what a file may hold ---------------------------------------------------------

#[test]
fn a_file_of_more_than_50000_songs_is_refused_in_plain_words() {
    let mut csv = String::from("Track Name,Artist Name(s)\n");
    for i in 0..=ImportFileReader::MAX_SONGS {
        csv.push_str(&format!("Song {i},Someone\n"));
    }
    let refused = ImportFileReader::read("Everything.csv", csv.as_bytes()).expect_err("refused");
    assert_eq!(
        refused,
        FileReadError::Format("It holds more than 50,000 songs. Split it into smaller files.".to_string())
    );
}

#[test]
fn a_zip_of_too_many_files_or_too_much_unpacked_is_refused() {
    let many = zip_of(
        &(0..=ImportFileReader::MAX_ZIP_ENTRIES)
            .map(|i| (format!("junk{i}.txt"), Vec::new()))
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        ImportFileReader::read("export.zip", &many).expect_err("refused"),
        FileReadError::Format(
            "The zip holds more than 200 files, more than any export Octo reads.".to_string()
        )
    );

    // Five CSVs of 60 MB of blank padding each: small packed, far too big unpacked.
    let padding = " ".repeat(60 * 1024 * 1024);
    let text = format!("Track Name,Artist Name(s)\nTeardrop,Massive Attack{padding}");
    let big = zip_of(
        &(0..5)
            .map(|i| (format!("Playlist{i}.csv"), text.as_bytes().to_vec()))
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        ImportFileReader::read("export.zip", &big).expect_err("refused"),
        FileReadError::Format(
            "The zip unpacks to more than 256 MB, more than any export Octo reads.".to_string()
        )
    );
}

/// The exact words a broken file is refused with: the parser's and the zip reader's.
#[test]
fn a_broken_file_is_refused_in_the_parsers_words() {
    let error = ImportFileReader::read("broken.json", br#"{"tracks": [oops"#).expect_err("refused");
    assert_eq!(
        octo_core::common::acquisition_tracker::user_safe(Some(&error.to_string())).as_deref(),
        Some("'o' is an invalid start of a value")
    );
    assert_eq!(
        ImportFileReader::read("export.zip", b"this is not a zip file").expect_err("refused"),
        FileReadError::InvalidData("End of Central Directory record could not be found.".to_string())
    );
    // A JSON array and an object of another shape hold no lists.
    assert!(
        ImportFileReader::read("list.json", b"[1, 2, 3]")
            .expect("read")
            .is_empty()
    );
    assert!(
        ImportFileReader::read("Other.json", br#"{"songs": [{"title": "x"}]}"#)
            .expect("read")
            .is_empty()
    );
}

#[test]
fn a_table_saved_as_text_is_still_read_as_a_table() {
    let lists = ImportFileReader::read(
        "songs.txt",
        "Song,Artists\nLjós í myrkri,Bjørn Ålund\nUnknown Tune,Nobody Parity\n".as_bytes(),
    )
    .expect("read");
    assert_eq!(lists.len(), 1);
    let tracks: Vec<(&str, &str)> = lists[0]
        .tracks
        .iter()
        .map(|t| (t.artist.as_str(), t.title.as_str()))
        .collect();
    assert_eq!(
        tracks,
        [
            ("Bjørn Ålund", "Ljós í myrkri"),
            ("Nobody Parity", "Unknown Tune")
        ]
    );
    // A plain list in a text file stays a plain list, commas and all.
    let plain =
        ImportFileReader::read("mine.txt", "Earth, Wind & Fire - September\n".as_bytes()).expect("read");
    assert_eq!(plain[0].tracks[0].artist, "Earth, Wind & Fire");
}
