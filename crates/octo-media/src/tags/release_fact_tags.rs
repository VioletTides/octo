//! The release facts a file arrived with, and which of them may stay. A peer tags a file for
//! the release it ripped; when Octo files the song under another album, or keeps none of that
//! release's grouping values, what the peer wrote would describe the wrong album, and the
//! library server reads it all the same. Names are lowercase and matched against ID3 TXXX
//! descriptions, Vorbis field names and MP4 freeform names alike, covering every alias the
//! library server reads for the field. The pure half (`AlbumGrouping`, `filed_elsewhere`) is
//! `octo_core::tagging::release_fact_tags`.

use octo_core::common::dotnet::{eq_ignore_case, is_null_or_white_space};
use octo_core::tagging::AlbumGrouping;

pub use octo_core::tagging::release_fact_tags::filed_elsewhere;

use super::tag_file::TagFile;
use super::tag_writer_extras::{self as extras, APPLE_MEAN, TagField, TagFields};

/// What the library server groups an album by before its name: the release id, then the album's
/// version and release date. Octo writes none of them for a new download (one track carrying
/// them beside another without them splits the album), so a peer's never stay. A Vorbis YEAR is
/// a release date to the library server too; DATE holds the year.
pub const GROUPING: &[&str] = &[
    "musicbrainz album id",
    "musicbrainz_albumid",
    "releasedate",
    "year",
    "albumversion",
    "musicbrainz_albumcomment",
    "musicbrainz album comment",
];

const GROUPING_FRAMES: [&str; 1] = ["TDRL"];

/// The rest of what one release is. They go when the song is filed under another album.
pub const OTHER_RELEASE: &[&str] = &[
    "musicbrainz release group id",
    "musicbrainz_releasegroupid",
    "musicbrainz release track id",
    "musicbrainz_releasetrackid",
    "musicbrainz album artist id",
    "musicbrainz_albumartistid",
    "musicbrainz disc id",
    "musicbrainz_discid",
    "musicbrainz album type",
    "musicbrainz_albumtype",
    "releasetype",
    "musicbrainz album status",
    "musicbrainz_albumstatus",
    "releasestatus",
    "musicbrainz album release country",
    "musicbrainz_albumreleasecountry",
    "releasecountry",
    "barcode",
    "upc",
    "ean",
    "catalognumber",
    "label",
    "organization",
    "publisher",
    "originaldate",
    "originalyear",
    "albumartists",
    "album artists",
    "asin",
    "media",
    "discsubtitle",
    "setsubtitle",
    "tracktotal",
    "totaltracks",
    "disctotal",
    "totaldiscs",
    "replaygain_album_gain",
    "replaygain_album_peak",
];

const OTHER_RELEASE_FRAMES: [&str; 5] = ["TPUB", "TDOR", "TORY", "TSST", "TMED"];

/// Every name a field goes by, for the fields Octo writes under one of them. Once Octo has
/// written a value, the other names are a second value the library server reads beside it: "G59
/// Records" in LABEL and "G*59 Records" in ORGANIZATION are two labels.
const ALIASES: [(TagField, [&str; 3]); 5] = [
    (TagFields::BARCODE, ["barcode", "upc", "ean"]),
    (TagFields::LABEL, ["label", "organization", "publisher"]),
    (
        TagFields::RELEASE_TYPE,
        ["releasetype", "musicbrainz_albumtype", "musicbrainz album type"],
    ),
    (
        TagFields::RELEASE_STATUS,
        [
            "releasestatus",
            "musicbrainz_albumstatus",
            "musicbrainz album status",
        ],
    ),
    (
        TagFields::RELEASE_COUNTRY,
        [
            "releasecountry",
            "musicbrainz_albumreleasecountry",
            "musicbrainz album release country",
        ],
    ),
];

/// Take off what the file arrived with that does not describe the album it is written under,
/// before Octo writes its own values. The grouping values always go, unless the song joins an
/// album that carries them, which they are then copied from; when the song is filed elsewhere
/// the rest of the other release goes too, track and disc numbers with it. Returns the names
/// that went, for the report.
pub fn tidy(file: &mut TagFile, filed_elsewhere: bool, joins: Option<&AlbumGrouping>) -> Vec<String> {
    let mut removed: Vec<String> = Vec::new();
    keep_the_year(file);
    removed.extend(remove(file, GROUPING, &GROUPING_FRAMES));
    if filed_elsewhere {
        removed.extend(remove(file, OTHER_RELEASE, &OTHER_RELEASE_FRAMES));
        if file.track() > 0 || file.track_count() > 0 || file.disc() > 0 || file.disc_count() > 0 {
            removed.push("track and disc numbers".to_string());
        }
        file.set_track(0);
        file.set_track_count(0);
        file.set_disc(0);
        file.set_disc_count(0);
    }
    if let Some(joins) = joins {
        extras::set_text(file, TagFields::ALBUM_ID, joins.album_id.as_deref());
        extras::set_text(file, TagFields::RELEASE_DATE, joins.release_date.as_deref());
        extras::set_text(file, TagFields::ALBUM_VERSION, joins.album_version.as_deref());
    }
    // Distinct, ignoring case, the first spelling kept.
    let mut distinct: Vec<String> = Vec::new();
    for name in removed {
        if !distinct.iter().any(|seen| eq_ignore_case(seen, &name)) {
            distinct.push(name);
        }
    }
    distinct
}

/// After Octo wrote a field, its other names go, so the library server reads one value.
pub fn drop_other_names(file: &mut TagFile, field: TagField) {
    for (aliased, names) in ALIASES {
        if aliased == field {
            remove_except(file, &names, field);
        }
    }
}

/// Totals under the second name a Vorbis comment can give them. TagLib writes TRACKTOTAL and
/// DISCTOTAL, so once those hold Octo's totals a peer's TOTALTRACKS or TOTALDISCS is a second
/// value.
pub fn drop_other_totals(file: &mut TagFile) {
    let Some(xiph) = file.xiph.as_mut() else {
        return;
    };
    if xiph
        .first_field("TRACKTOTAL")
        .is_some_and(|total| !total.is_empty())
    {
        xiph.remove_field("TOTALTRACKS");
    }
    if xiph
        .first_field("DISCTOTAL")
        .is_some_and(|total| !total.is_empty())
    {
        xiph.remove_field("TOTALDISCS");
    }
}

/// A Vorbis comment whose only year is in YEAR keeps it as DATE, the recording date, before YEAR
/// goes as a release date.
fn keep_the_year(file: &mut TagFile) {
    let Some(xiph) = file.xiph.as_mut() else {
        return;
    };
    if !is_null_or_white_space(xiph.first_field("DATE").as_deref()) {
        return;
    }
    if let Some(year) = xiph.first_field("YEAR")
        && !year.is_empty()
        && !is_null_or_white_space(Some(&year))
    {
        xiph.set_field("DATE", &[year.trim()]);
    }
}

fn wanted(names: &[&str], name: &str) -> bool {
    names.iter().any(|wanted| eq_ignore_case(wanted, name))
}

fn remove(file: &mut TagFile, names: &[&str], frames: &[&str]) -> Vec<String> {
    let mut removed: Vec<String> = Vec::new();

    if let Some(id3) = file.id3v2.as_mut() {
        for (description, _) in id3.user_texts() {
            if wanted(names, &description) {
                removed.push(description);
            }
        }
        id3.retain_user_texts(|description| !wanted(names, description));
        for id in frames {
            if id3.has_frames(id) {
                removed.push((*id).to_string());
                id3.remove_frames(id);
            }
        }
    }
    if let Some(xiph) = file.xiph.as_mut() {
        for name in xiph.fields.keys() {
            if wanted(names, &name) {
                xiph.remove_field(&name);
                removed.push(name);
            }
        }
    }
    if let Some(apple) = file.apple.as_mut() {
        for name in freeform_names(apple) {
            if wanted(names, &name) {
                apple.set_dash_box(APPLE_MEAN, &name, None);
                removed.push(name);
            }
        }
    }
    removed
}

/// Every name in `names` goes from each container but the one Octo writes the field under
/// there. On ID3 a field with a frame of its own keeps no TXXX at all.
fn remove_except(file: &mut TagFile, names: &[&str], keep: TagField) {
    if let Some(id3) = file.id3v2.as_mut() {
        id3.retain_user_texts(|description| {
            !(wanted(names, description)
                && (keep.id3_frame.is_some()
                    || !keep
                        .id3_description
                        .is_some_and(|kept| eq_ignore_case(description, kept))))
        });
    }
    if let Some(xiph) = file.xiph.as_mut() {
        for name in xiph.fields.keys() {
            if wanted(names, &name) && !eq_ignore_case(&name, keep.vorbis) {
                xiph.remove_field(&name);
            }
        }
    }
    if let Some(apple) = file.apple.as_mut() {
        for name in freeform_names(apple) {
            if wanted(names, &name) && !eq_ignore_case(&name, keep.mp4) {
                apple.set_dash_box(APPLE_MEAN, &name, None);
            }
        }
    }
}

/// The names of the iTunes freeform atoms a file carries, each once.
fn freeform_names(apple: &super::apple::AppleTag) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (name, _) in apple.freeform(APPLE_MEAN) {
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
#[path = "release_fact_tags_tests.rs"]
mod tests;
