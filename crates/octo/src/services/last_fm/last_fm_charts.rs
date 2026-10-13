//! Last.fm's play counts, an artist's most played songs and the site's chart. Uncached here; the
//! caller keeps what it builds from them, and must know a call that did not answer apart from an
//! empty one.

use octo_core::charts::top_songs::CountedTrack;
use octo_core::common::dotnet::is_blank;
use serde_json::Value;

use super::{LastFmService, text, text_in, try_array};

impl LastFmService {
    /// The artist's most played songs, most played first. Last.fm's own spelling of the name is
    /// used (autocorrect), so "daft punk" finds Daft Punk. None when there is no key or Last.fm
    /// did not answer.
    pub async fn get_artist_chart(&self, artist: &str, limit: i32) -> Option<Vec<CountedTrack>> {
        if is_blank(artist) {
            return Some(Vec::new());
        }
        let limit = limit.to_string();
        let doc = self
            .get_document(
                "artist.gettoptracks",
                &[
                    ("artist", artist),
                    ("limit", limit.as_str()),
                    ("autocorrect", "1"),
                ],
            )
            .await?;
        read_chart(&doc, "toptracks", Some(artist))
    }

    /// Last.fm's chart of the moment, most played first. None when there is no key or Last.fm
    /// did not answer.
    pub async fn get_chart(&self, limit: i32) -> Option<Vec<CountedTrack>> {
        let limit = limit.to_string();
        let doc = self
            .get_document("chart.gettoptracks", &[("limit", limit.as_str())])
            .await?;
        read_chart(&doc, "tracks", None)
    }
}

/// None for an error document; the list (maybe empty) otherwise.
fn read_chart(doc: &Value, container: &str, artist: Option<&str>) -> Option<Vec<CountedTrack>> {
    if doc.get("error").is_some() {
        return None;
    }
    let Ok(Some(values)) = try_array(doc, container, "track") else {
        return Some(Vec::new());
    };
    Some(
        values
            .iter()
            .filter_map(|item| {
                let named = text_in(item, "artist", "name").unwrap_or_default();
                let artist = if named.is_empty() {
                    artist.unwrap_or("").to_string()
                } else {
                    named
                };
                let title = text(item, "name").unwrap_or_default();
                (!artist.is_empty() && !title.is_empty()).then(|| CountedTrack {
                    artist,
                    title,
                    plays: count(item, "playcount"),
                    listeners: count(item, "listeners"),
                })
            })
            .collect(),
    )
}

/// A count, from its text or number, kept when it is a whole number and not negative.
fn count(item: &Value, property: &str) -> Option<i64> {
    text(item, property)
        .ok()
        .and_then(|t| t.trim().parse::<i64>().ok())
        .filter(|n| *n >= 0)
}

#[cfg(test)]
#[path = "last_fm_charts_tests.rs"]
mod tests;
