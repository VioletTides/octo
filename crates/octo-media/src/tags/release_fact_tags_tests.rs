//! A peer's file carries the facts of the release it was ripped from. Once Octo files the song,
//! only the facts of the album it is written under may stay, each under one name, and the values
//! the library server groups an album by never come from a peer. Checked on real MP3, FLAC and
//! MP4 files, read back the way the library server reads them. The advisory decisions are in
//! `octo_core::tagging::explicit_advisory`, `filed_elsewhere_is_another_album_title` in
//! `octo_core::tagging::release_fact_tags`.

use std::path::{Path, PathBuf};

use octo_core::tagging::AlbumGrouping;

use super::*;
use crate::audio::test_support::{require_ffmpeg, run_ffmpeg};
use crate::tags::kept_identity::KeptIdentityTags;
use crate::tags::tag_writer_extras as extras;
use crate::tags::test_support::{flac, mp3, write};

const PEER_ALBUM_ID: &str = "0b1c2d3e-4f50-4617-8899-aabbccddeeff";
const PEER_TRACK_ID: &str = "11112222-3333-4444-8555-666677778888";

fn open(path: &Path) -> TagFile {
    TagFile::open(path).expect("opens")
}

fn mp4(dir: &Path, name: &str) -> PathBuf {
    run_ffmpeg(
        dir,
        &format!("-f lavfi -i sine=frequency=440:duration=1 -c:a aac -b:a 128k {name}"),
    );
    dir.join(name)
}

/// A FLAC as a peer tagged it for "The Party Never Ends 2.0", track 8 of 16.
fn peer_flac(dir: &Path, year_only: bool) -> PathBuf {
    let path = write(dir, "peer.flac", &flac());
    let mut file = open(&path);
    {
        let xiph = file.xiph.as_mut().expect("a Vorbis comment");
        let mut set = |key: &str, value: &str| xiph.set_field(key, &[value]);
        set("TITLE", "Love Letter");
        set("ARTIST", "Juice WRLD");
        set("ALBUM", "The Party Never Ends 2.0");
        set("ALBUMARTIST", "Juice WRLD");
        if year_only {
            set("YEAR", "2024");
        } else {
            set("DATE", "2024-11-29");
            set("RELEASEDATE", "2024-11-29");
        }
        set("MUSICBRAINZ_ALBUMID", PEER_ALBUM_ID);
        set("MUSICBRAINZ_RELEASETRACKID", PEER_TRACK_ID);
        set("MUSICBRAINZ_RELEASEGROUPID", "group-tpne");
        set("UPC", "0602475682233");
        set("LABEL", "Grade A Productions - Interscope Records");
        set("ORGANIZATION", "Interscope");
        set("CATALOGNUMBER", "B0042");
        set("RELEASETYPE", "album");
        set("ORIGINALDATE", "2024-11-29");
        set("TRACKNUMBER", "8");
        set("TRACKTOTAL", "16");
        set("DISCNUMBER", "1");
        set("REPLAYGAIN_ALBUM_GAIN", "-8.10 dB");
        set("REPLAYGAIN_TRACK_GAIN", "-7.20 dB");
        set("ISRC", "USUG12407311");
        set("COPYRIGHT", "2024 Grade A Productions, LLC");
    }
    file.save().expect("saved");
    path
}

fn vorbis(path: &Path, field: &str) -> Option<String> {
    open(path).xiph.as_ref().and_then(|xiph| xiph.first_field(field))
}

fn tidy_file(path: &Path, filed_elsewhere: bool, joins: Option<&AlbumGrouping>) -> Vec<String> {
    let mut file = open(path);
    let removed = tidy(&mut file, filed_elsewhere, joins);
    file.save().expect("saved");
    removed
}

// ---- the grouping values never stay ------------------------------------------------------

#[test]
fn same_album_loses_only_the_grouping_values() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = peer_flac(dir.path(), false);

    let removed = tidy_file(&path, false, None);

    assert_eq!(vorbis(&path, "MUSICBRAINZ_ALBUMID"), None);
    assert_eq!(vorbis(&path, "RELEASEDATE"), None);
    assert!(
        removed
            .iter()
            .any(|name| name.eq_ignore_ascii_case("MUSICBRAINZ_ALBUMID")),
        "{removed:?}"
    );
    // The rest describes this very album and stays.
    assert_eq!(vorbis(&path, "UPC").as_deref(), Some("0602475682233"));
    assert_eq!(vorbis(&path, "CATALOGNUMBER").as_deref(), Some("B0042"));
    assert_eq!(
        vorbis(&path, "MUSICBRAINZ_RELEASETRACKID").as_deref(),
        Some(PEER_TRACK_ID)
    );
    assert_eq!(vorbis(&path, "TRACKNUMBER").as_deref(), Some("8"));
    assert_eq!(vorbis(&path, "DATE").as_deref(), Some("2024-11-29"));
    // What the library server groups the album by is now the name and the album artist alone.
    let identity = KeptIdentityTags::read(&path, None).expect("reads");
    assert_eq!(identity.album_id, None);
    assert_eq!(identity.release_date, None);
    assert!(!KeptIdentityTags::navidrome_view(&open(&path)).contains_key("musicbrainz_albumid"));
}

#[test]
fn a_year_only_in_year_stays_as_the_date() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = peer_flac(dir.path(), true);

    tidy_file(&path, false, None);

    assert_eq!(vorbis(&path, "YEAR"), None);
    assert_eq!(vorbis(&path, "DATE").as_deref(), Some("2024"));
    assert_eq!(open(&path).year(), 2024);
}

#[test]
fn joining_an_album_copies_its_grouping_values_exactly() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = peer_flac(dir.path(), false);
    let theirs = "99999999-8888-4777-8666-555555555555";

    tidy_file(
        &path,
        false,
        Some(&AlbumGrouping::new(
            Some(theirs.into()),
            Some("2024-11-29".into()),
            None,
        )),
    );

    let identity = KeptIdentityTags::read(&path, None).expect("reads");
    assert_eq!(
        (
            identity.album_id.as_deref(),
            identity.release_date.as_deref(),
            identity.album_version.as_deref()
        ),
        (Some(theirs), Some("2024-11-29"), None)
    );
}

// ---- filed under another album: that album's facts go ------------------------------------

#[test]
fn filed_elsewhere_flac_keeps_only_what_is_about_the_recording() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = peer_flac(dir.path(), false);

    let removed = tidy_file(&path, true, None);

    for field in [
        "MUSICBRAINZ_ALBUMID",
        "RELEASEDATE",
        "MUSICBRAINZ_RELEASETRACKID",
        "MUSICBRAINZ_RELEASEGROUPID",
        "UPC",
        "LABEL",
        "ORGANIZATION",
        "CATALOGNUMBER",
        "RELEASETYPE",
        "ORIGINALDATE",
        "TRACKNUMBER",
        "TRACKTOTAL",
        "DISCNUMBER",
        "REPLAYGAIN_ALBUM_GAIN",
    ] {
        assert_eq!(vorbis(&path, field), None, "{field} should have gone");
    }
    // The recording's own facts stay: its code, its gain, its words and its year.
    assert_eq!(vorbis(&path, "ISRC").as_deref(), Some("USUG12407311"));
    assert_eq!(
        vorbis(&path, "REPLAYGAIN_TRACK_GAIN").as_deref(),
        Some("-7.20 dB")
    );
    assert_eq!(
        vorbis(&path, "COPYRIGHT").as_deref(),
        Some("2024 Grade A Productions, LLC")
    );
    assert_eq!(vorbis(&path, "TITLE").as_deref(), Some("Love Letter"));
    assert_eq!(vorbis(&path, "DATE").as_deref(), Some("2024-11-29"));
    assert!(
        removed.contains(&"track and disc numbers".to_string()),
        "{removed:?}"
    );
}

#[test]
fn filed_elsewhere_mp3_loses_the_other_releases_frames() {
    for version in [3u8, 4] {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = write(dir.path(), "peer.mp3", &mp3());
        {
            let mut file = open(&path);
            extras::id3_for(&mut file).expect("an ID3v2 tag").version = version;
            file.set_title(Some("Love Letter"));
            file.set_album(Some("The Party Never Ends 2.0"));
            file.set_track(8);
            file.set_track_count(16);
            let id3 = file.id3v2.as_mut().expect("an ID3v2 tag");
            id3.set_text_frame("TPUB", &["Interscope"]);
            id3.set_text_frame("TDRL", &["2024-11-29"]);
            id3.set_text_frame("TDOR", &["2024-11-29"]);
            id3.set_text_frame("TSRC", &["USUG12407311"]);
            for (description, value) in [
                ("MusicBrainz Album Id", PEER_ALBUM_ID),
                ("MusicBrainz Release Track Id", PEER_TRACK_ID),
                ("BARCODE", "0602475682233"),
                ("CATALOGNUMBER", "B0042"),
            ] {
                id3.set_user_text(description, &[value.to_string()], true);
            }
            file.save().expect("saved");
        }

        tidy_file(&path, true, None);

        let after = open(&path);
        let tag = after.id3v2.as_ref().expect("an ID3v2 tag");
        for frame in ["TPUB", "TDRL", "TDOR", "TORY", "TRCK"] {
            assert!(!tag.has_frames(frame), "{frame} on version {version}");
        }
        for description in [
            "MusicBrainz Album Id",
            "MusicBrainz Release Track Id",
            "BARCODE",
            "CATALOGNUMBER",
        ] {
            assert_eq!(tag.user_text(description, false), None, "{description}");
        }
        assert_eq!(tag.text_values("TSRC"), ["USUG12407311"]);
    }
}

#[test]
fn filed_elsewhere_mp4_loses_the_other_releases_freeform_atoms() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = mp4(dir.path(), "peer.m4a");
    {
        let mut file = open(&path);
        file.set_album(Some("The Party Never Ends 2.0"));
        let apple = file.apple.as_mut().expect("an item list");
        apple.set_dash_box(APPLE_MEAN, "MusicBrainz Album Id", Some(PEER_ALBUM_ID));
        apple.set_dash_box(APPLE_MEAN, "BARCODE", Some("0602475682233"));
        apple.set_dash_box(APPLE_MEAN, "LABEL", Some("Interscope"));
        apple.set_dash_box(APPLE_MEAN, "ISRC", Some("USUG12407311"));
        file.save().expect("saved");
    }

    tidy_file(&path, true, None);

    let after = open(&path);
    let apple = after.apple.as_ref().expect("an item list");
    assert_eq!(apple.dash_box(APPLE_MEAN, "MusicBrainz Album Id"), None);
    assert_eq!(apple.dash_box(APPLE_MEAN, "BARCODE"), None);
    assert_eq!(apple.dash_box(APPLE_MEAN, "LABEL"), None);
    assert_eq!(
        apple.dash_box(APPLE_MEAN, "ISRC").as_deref(),
        Some("USUG12407311")
    );
}

// ---- one value per field -----------------------------------------------------------------

#[test]
fn a_label_octo_wrote_is_the_only_label() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = peer_flac(dir.path(), false);
    {
        let mut file = open(&path);
        extras::set_text(&mut file, TagFields::LABEL, Some("G59 Records"));
        extras::set_text(&mut file, TagFields::BARCODE, Some("195497822577"));
        drop_other_names(&mut file, TagFields::LABEL);
        drop_other_names(&mut file, TagFields::BARCODE);
        file.save().expect("saved");
    }

    let view = KeptIdentityTags::navidrome_view(&open(&path));
    assert_eq!(view["label"], ["G59 Records"]);
    assert!(!view.contains_key("organization"));
    assert_eq!(view["barcode"], ["195497822577"]);
    assert!(!view.contains_key("upc"));
}

#[test]
fn a_label_octo_wrote_on_id3_leaves_no_txxx_label_beside_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = write(dir.path(), "song.mp3", &mp3());
    {
        let mut file = open(&path);
        let id3 = extras::id3_for(&mut file).expect("an ID3v2 tag");
        id3.set_user_text("LABEL", &["G*59 Records".to_string()], true);
        id3.set_user_text("RELEASETYPE", &["compilation".to_string()], true);
        extras::set_text(&mut file, TagFields::LABEL, Some("G59 Records"));
        extras::set_multi(&mut file, TagFields::RELEASE_TYPE, &["album".to_string()]);
        drop_other_names(&mut file, TagFields::LABEL);
        drop_other_names(&mut file, TagFields::RELEASE_TYPE);
        file.save().expect("saved");
    }

    let after = open(&path);
    let tag = after.id3v2.as_ref().expect("an ID3v2 tag");
    assert_eq!(tag.all_text_values("TPUB"), ["G59 Records"]);
    assert_eq!(tag.user_text("LABEL", false), None);
    assert_eq!(tag.user_text("RELEASETYPE", false), None);
    assert_eq!(
        tag.user_text("MusicBrainz Album Type", false),
        Some(vec!["album".to_string()])
    );
}

#[test]
fn totals_under_their_second_name_go_once_octo_wrote_them() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = peer_flac(dir.path(), false);
    {
        let mut file = open(&path);
        let xiph = file.xiph.as_mut().expect("a Vorbis comment");
        xiph.set_field("TOTALTRACKS", &["40"]);
        xiph.set_field("TOTALDISCS", &["2"]);
        file.set_track_count(11);
        file.set_disc_count(1);
        drop_other_totals(&mut file);
        file.save().expect("saved");
    }

    assert_eq!(vorbis(&path, "TRACKTOTAL").as_deref(), Some("11"));
    assert_eq!(vorbis(&path, "DISCTOTAL").as_deref(), Some("1"));
    assert_eq!(vorbis(&path, "TOTALTRACKS"), None);
    assert_eq!(vorbis(&path, "TOTALDISCS"), None);
}

// ---- the advisory ------------------------------------------------------------------------

#[test]
fn advisory_lands_where_the_library_server_reads_it() {
    for (extension, value) in [(".mp3", 1), (".flac", 2), (".mp3", 0)] {
        let dir = tempfile::tempdir().expect("a temp dir");
        let bytes = if extension == ".mp3" { mp3() } else { flac() };
        let path = write(dir.path(), &format!("song{extension}"), &bytes);
        {
            let mut file = open(&path);
            extras::set_advisory(&mut file, Some(value));
            file.save().expect("saved");
        }

        let after = open(&path);
        assert_eq!(extras::read_advisory(&after), Some(value), "{extension} {value}");
        assert_eq!(
            KeptIdentityTags::navidrome_view(&after)["itunesadvisory"],
            [value.to_string()],
            "{extension} {value}"
        );
    }
}

#[test]
fn advisory_out_of_range_or_unknown_leaves_the_file_alone() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = write(dir.path(), "song.flac", &flac());
    {
        let mut file = open(&path);
        extras::set_advisory(&mut file, Some(1));
        extras::set_advisory(&mut file, None);
        extras::set_advisory(&mut file, Some(7));
        file.save().expect("saved");
    }
    assert_eq!(vorbis(&path, "ITUNESADVISORY").as_deref(), Some("1"));
}

#[test]
fn advisory_on_mp4_is_the_one_byte_rating_atom() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = mp4(dir.path(), "song.m4a");
    {
        let mut file = open(&path);
        file.apple
            .as_mut()
            .expect("an item list")
            .set_dash_box(APPLE_MEAN, "ITUNESADVISORY", Some("2"));
        extras::set_advisory(&mut file, Some(1));
        file.save().expect("saved");
    }

    let after = open(&path);
    let apple = after.apple.as_ref().expect("an item list");
    let data = apple
        .first_data(super::super::apple::names::RTNG)
        .expect("a rating atom");
    assert_eq!(data.data, [1]);
    assert_eq!(data.flags, super::super::apple::flags::FOR_TEMPO);
    assert_eq!(apple.dash_box(APPLE_MEAN, "ITUNESADVISORY"), None);
    assert_eq!(extras::read_advisory(&after), Some(1));
}

/// `extras::clear_advisory`, the undo of the explicit marking, takes the advisory out of every
/// frame `extras::set_advisory` writes, and leaves the rest of the file alone.
#[test]
fn clear_advisory_takes_it_out_of_every_frame() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut paths = vec![
        write(dir.path(), "song.mp3", &mp3()),
        write(dir.path(), "song.flac", &flac()),
    ];
    if crate::audio::test_support::ffmpeg_available() {
        paths.push(mp4(dir.path(), "song.m4a"));
    } else {
        eprintln!("the MP4 case is skipped: ffmpeg is not on the PATH");
    }
    for path in paths {
        {
            let mut file = open(&path);
            file.set_title(Some("Kept"));
            extras::set_advisory(&mut file, Some(1));
            if let Some(apple) = file.apple.as_mut() {
                apple.set_dash_box(APPLE_MEAN, "ITUNESADVISORY", Some("1"));
            }
            file.save().expect("saved");
        }
        {
            let mut file = open(&path);
            extras::clear_advisory(&mut file);
            file.save().expect("saved");
        }

        let after = open(&path);
        assert_eq!(extras::read_advisory(&after), None, "{}", path.display());
        assert!(
            !KeptIdentityTags::navidrome_view(&after).contains_key("itunesadvisory"),
            "{}",
            path.display()
        );
        if let Some(apple) = after.apple.as_ref() {
            assert!(apple.first_data(super::super::apple::names::RTNG).is_none());
            assert_eq!(apple.dash_box(APPLE_MEAN, "ITUNESADVISORY"), None);
        }
        assert_eq!(after.title().as_deref(), Some("Kept"), "{}", path.display());
    }
}

// ---- What a tidy reports ----------------------------------------------------------------

/// What a tidy reports: each name once, as the file spelled it, the frames by their ids.
#[test]
fn a_tidy_reports_each_name_once() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = write(dir.path(), "song.mp3", &mp3());
    {
        let mut file = open(&path);
        let id3 = extras::id3_for(&mut file).expect("an ID3v2 tag");
        id3.set_user_text("MusicBrainz Album Id", &[PEER_ALBUM_ID.to_string()], true);
        id3.set_user_text("musicbrainz album id", &[PEER_ALBUM_ID.to_string()], true);
        id3.set_text_frame("TDRL", &["2024-11-29"]);
        id3.set_text_frame("TPUB", &["Interscope"]);
        file.save().expect("saved");
    }
    assert_eq!(tidy_file(&path, false, None), ["MusicBrainz Album Id", "TDRL"]);
    assert_eq!(tidy_file(&path, true, None), ["TPUB"]);
    assert!(tidy_file(&path, true, None).is_empty());
}
