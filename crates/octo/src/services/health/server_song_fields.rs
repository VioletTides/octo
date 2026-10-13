//! Library health's view of Navidrome's own song list, read the way the Octo app reads the same
//! songs through Subsonic, so both find the same things: the length in whole seconds (Subsonic
//! cuts the fraction off), an m4a lossless only with a bit depth, and the release a part came out
//! on taken for its whole album (Navidrome keeps one per album, the value most of its songs
//! carry).

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use octo_core::common::dotnet::{is_blank, to_lower_invariant};

use super::library_health::IHealthFields;
use crate::services::library::LibrarySongRow;
use crate::services::library::quality_upgrade_worker::compare_ordinal;

const LOSSLESS_FORMATS: [&str; 13] = [
    "flac", "alac", "wav", "wave", "aiff", "aif", "aifc", "pcm", "ape", "wv", "dsf", "dff", "tta",
];

#[derive(Debug, Clone, Default)]
struct AlbumFacts {
    release: Option<String>,
    group: Option<String>,
    barcode: Option<String>,
    labels: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ServerSongFields {
    albums: HashMap<String, AlbumFacts>,
}

impl ServerSongFields {
    pub fn new<'a>(rows: impl IntoIterator<Item = &'a LibrarySongRow>) -> Self {
        let mut by_album: IndexMap<&str, Vec<&LibrarySongRow>> = IndexMap::new();
        for row in rows {
            if let Some(id) = row.album_id.as_deref().filter(|id| !id.is_empty()) {
                by_album.entry(id).or_default().push(row);
            }
        }
        let albums = by_album
            .into_iter()
            .map(|(id, album)| {
                let mut labels: Vec<String> = Vec::new();
                for label in album.iter().flat_map(|row| row.labels.iter()) {
                    if !labels.contains(label) {
                        labels.push(label.clone());
                    }
                }
                let facts = AlbumFacts {
                    release: most_of(&album, |row| row.release_id.as_deref()),
                    group: most_of(&album, |row| row.release_group_id.as_deref()),
                    barcode: most_of(&album, |row| row.barcode.as_deref()),
                    labels,
                };
                (id.to_string(), facts)
            })
            .collect();
        ServerSongFields { albums }
    }

    pub fn is_lossless(format: Option<&str>, bit_depth: Option<i32>) -> bool {
        let Some(kind) = format.map(to_lower_invariant) else {
            return false;
        };
        if kind == "m4a" || kind == "mp4" {
            return bit_depth.unwrap_or(0) > 0;
        }
        LOSSLESS_FORMATS.contains(&kind.as_str())
    }

    fn facts_of(&self, song: &LibrarySongRow) -> Option<&AlbumFacts> {
        song.album_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .and_then(|id| self.albums.get(id))
    }
}

/// The value most of an album's songs carry; between equals, the first in plain order.
fn most_of(songs: &[&LibrarySongRow], of: impl Fn(&LibrarySongRow) -> Option<&str>) -> Option<String> {
    let mut counts: IndexMap<&str, usize> = IndexMap::new();
    for value in songs
        .iter()
        .filter_map(|song| of(song))
        .filter(|value| !is_blank(value))
    {
        *counts.entry(value).or_default() += 1;
    }
    counts
        .into_iter()
        .min_by(|(a, count_a), (b, count_b)| count_b.cmp(count_a).then_with(|| compare_ordinal(a, b)))
        .map(|(value, _)| value.to_string())
}

fn text(value: &str) -> Option<&str> {
    (!is_blank(value)).then_some(value)
}

fn text_of(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !is_blank(value))
}

/// The fields of one row, for a row and for a row shared by the library snapshot alike.
macro_rules! server_fields {
    ($song:ty) => {
        impl IHealthFields<$song> for ServerSongFields {
            fn id<'a>(&'a self, song: &'a $song) -> &'a str {
                &song.id
            }

            fn title<'a>(&'a self, song: &'a $song) -> &'a str {
                &song.title
            }

            fn artist<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                text(&song.artist)
            }

            fn album<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                song.album.as_deref()
            }

            fn album_id<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                song.album_id.as_deref()
            }

            fn album_artist<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                text_of(song.album_artist.as_deref()).or(song.album_artists.first().map(String::as_str))
            }

            fn genres<'a>(&'a self, song: &'a $song) -> Vec<&'a str> {
                song.genres.iter().map(String::as_str).collect()
            }

            fn year(&self, song: &$song) -> Option<i32> {
                song.year
            }

            fn disc(&self, song: &$song) -> Option<i32> {
                song.disc
            }

            fn track(&self, song: &$song) -> Option<i32> {
                song.track
            }

            fn seconds(&self, song: &$song) -> i32 {
                song.seconds
                    .or(song.duration.map(f64::from))
                    .unwrap_or(0.0)
                    .floor() as i32
            }

            fn format(&self, song: &$song) -> Option<String> {
                text(&song.suffix).map(to_lower_invariant)
            }

            fn lossless(&self, song: &$song) -> bool {
                ServerSongFields::is_lossless(Some(&song.suffix), song.bit_depth)
            }

            fn bit_rate(&self, song: &$song) -> Option<i32> {
                Some(song.bit_rate)
            }

            fn bit_depth(&self, song: &$song) -> Option<i32> {
                song.bit_depth
            }

            fn sample_rate(&self, song: &$song) -> Option<i32> {
                song.sample_rate
            }

            fn isrcs<'a>(&'a self, song: &'a $song) -> Vec<&'a str> {
                song.isrcs.iter().map(String::as_str).collect()
            }

            fn recording_id<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                song.recording_id.as_deref()
            }

            /// A picture inside the file. Navidrome's Subsonic answer always names a cover (the
            /// album's, when the file has none), so the apps cannot tell; the native list can.
            fn cover<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                song.has_cover.then_some("embedded")
            }

            fn artists<'a>(&'a self, song: &'a $song) -> Vec<&'a str> {
                let named: Vec<&str> = song
                    .artists
                    .iter()
                    .map(String::as_str)
                    .filter(|name| !is_blank(name))
                    .collect();
                if named.is_empty() {
                    self.artist(song).into_iter().collect()
                } else {
                    named
                }
            }

            fn album_artists<'a>(&'a self, song: &'a $song) -> Vec<&'a str> {
                let named: Vec<&str> = song
                    .album_artists
                    .iter()
                    .map(String::as_str)
                    .filter(|name| !is_blank(name))
                    .collect();
                if named.is_empty() {
                    self.album_artist(song).into_iter().collect()
                } else {
                    named
                }
            }

            fn release_id<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                self.facts_of(song)?.release.as_deref()
            }

            fn release_group_id<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                self.facts_of(song)?.group.as_deref()
            }

            fn barcode<'a>(&'a self, song: &'a $song) -> Option<&'a str> {
                self.facts_of(song)?.barcode.as_deref()
            }

            fn labels<'a>(&'a self, song: &'a $song) -> Vec<&'a str> {
                self.facts_of(song)
                    .map(|facts| facts.labels.iter().map(String::as_str).collect())
                    .unwrap_or_default()
            }
        }
    };
}

server_fields!(LibrarySongRow);
server_fields!(Arc<LibrarySongRow>);
