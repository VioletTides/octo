//! getArtistTopSongs, getTopChart and the standard getTopSongs, plus getCharts.

use octo_core::models::domain::Song;
use serde_json::{Map, Value, json};

use super::{
    Fields, SUBSONIC_NAMESPACE, SUBSONIC_VERSION, SubsonicReply, SubsonicResponseBuilder, envelope,
    subsonic_element,
};
use crate::xml::XElement;

/// What a ranked list says about itself. `chart`, `name` and `country` are set on a chart only,
/// so an artist's list goes out exactly as before charts had them.
#[derive(Debug, Clone, Default)]
pub struct TopSongsHeader<'a> {
    pub artist: Option<&'a str>,
    pub source: &'a str,
    pub chart: Option<&'a str>,
    pub name: Option<&'a str>,
    pub country: Option<&'a str>,
}

/// One ranked row: the song, its counts, and the caller's own library copy when there is one.
#[derive(Debug, Clone)]
pub struct RankedRow<'a> {
    pub rank: usize,
    pub song: &'a Song,
    pub plays: Option<i64>,
    pub listeners: Option<i64>,
    pub library: Option<&'a Value>,
}

/// One row of the standard getTopSongs: the caller's own library song as Navidrome described it
/// to them, or a song from outside the library.
#[derive(Debug, Clone)]
pub enum TopSongRow {
    Library(Value),
    Outside(Song),
}

/// One chart of getCharts.
#[derive(Debug, Clone)]
pub struct ChartChoiceRow {
    pub id: String,
    pub name: String,
    pub label: String,
    pub kind: &'static str,
    pub playlist: Option<String>,
    pub on: bool,
}

impl SubsonicResponseBuilder {
    /// getArtistTopSongs and getTopChart: a ranked list, each entry a song with its rank, the
    /// Last.fm counts when there are any, and whether the caller's library has it. A library song
    /// is sent as Navidrome described it to the caller, so it plays from the library and keeps
    /// its heart and rating; an outside one in the shape every outside song has. Always JSON. The
    /// field names are a contract with the Octo apps.
    pub fn create_top_songs_response(
        &self,
        header: &TopSongsHeader<'_>,
        rows: &[RankedRow<'_>],
    ) -> SubsonicReply {
        let entries: Vec<Value> = rows
            .iter()
            .map(|row| {
                let song = match row.library {
                    Some(owned) => self
                        .convert_subsonic_json_element(owned, true)
                        .unwrap_or(Value::Null),
                    None => Value::Object(self.convert_song_to_json(row.song)),
                };
                json!({
                    "rank": row.rank,
                    "plays": row.plays,
                    "listeners": row.listeners,
                    "inLibrary": row.library.is_some(),
                    "song": song,
                })
            })
            .collect();
        let mut top = Map::new();
        top.insert(
            "artist".into(),
            header.artist.map_or(Value::Null, |a| Value::String(a.into())),
        );
        top.insert("source".into(), Value::String(header.source.into()));
        if let Some(chart) = header.chart {
            top.insert("chart".into(), Value::String(chart.into()));
            top.insert(
                "name".into(),
                header.name.map_or(Value::Null, |n| Value::String(n.into())),
            );
            top.insert(
                "country".into(),
                header.country.map_or(Value::Null, |c| Value::String(c.into())),
            );
        }
        top.insert("entry".into(), Value::Array(entries));
        self.create_json_response(json!({
            "status": "ok",
            "version": SUBSONIC_VERSION,
            "type": "octo",
            "openSubsonic": true,
            "topSongs": Value::Object(top),
        }))
    }

    /// getCharts: every chart the server's country has, Popular right now first, then Best New
    /// Songs and Trending Songs, then the genres in Apple's order; `playlist` is the chart's
    /// playlist for this listener while it is one. Always JSON.
    pub fn create_charts_response(&self, country: Option<&str>, charts: &[ChartChoiceRow]) -> SubsonicReply {
        let rows: Vec<Value> = charts
            .iter()
            .map(|chart| {
                let mut row = Map::new();
                row.insert("id".into(), Value::String(chart.id.clone()));
                row.insert("name".into(), Value::String(chart.name.clone()));
                row.insert("label".into(), Value::String(chart.label.clone()));
                row.insert("kind".into(), Value::String(chart.kind.into()));
                if let Some(playlist) = &chart.playlist {
                    row.insert("playlist".into(), Value::String(playlist.clone()));
                }
                row.insert("on".into(), Value::Bool(chart.on));
                Value::Object(row)
            })
            .collect();
        self.create_json_response(json!({
            "status": "ok",
            "version": SUBSONIC_VERSION,
            "type": "octo",
            "openSubsonic": true,
            "charts": {
                "country": country,
                "chart": rows,
            },
        }))
    }

    /// The standard Subsonic getTopSongs, answered by Octo: `topSongs` holding one `song` per
    /// row, in rank order, in the format asked for. A library song goes out as Navidrome sent it
    /// (in XML its fields as attributes and its lists as child elements, the shape Navidrome's
    /// own XML has), an outside song in the shape every outside song has.
    pub fn create_standard_top_songs_response(&self, format: &str, rows: &[TopSongRow]) -> SubsonicReply {
        if format == "json" {
            let songs: Vec<Value> = rows
                .iter()
                .map(|row| match row {
                    TopSongRow::Library(library) => self
                        .convert_subsonic_json_element(library, true)
                        .unwrap_or(Value::Null),
                    TopSongRow::Outside(song) => Value::Object(self.convert_song_to_json(song)),
                })
                .collect();
            return self.create_json_response(json!({
                "status": "ok",
                "version": SUBSONIC_VERSION,
                "openSubsonic": true,
                "topSongs": { "song": songs },
            }));
        }
        let mut top = subsonic_element("topSongs");
        for row in rows {
            top.push(match row {
                TopSongRow::Library(library) => library_song_xml(library),
                TopSongRow::Outside(song) => self.convert_song_to_xml(song, Some(SUBSONIC_NAMESPACE)),
            });
        }
        SubsonicReply::xml(&envelope("ok").attr("openSubsonic", "true").child(top))
    }
}

/// What "Popular right now" says it is, in apps that show a playlist's comment.
pub const POPULAR_COMMENT: &str =
    "The most played songs right now, made by Octo. Songs you have play from your library.";

/// What a chart list says it is: Popular right now's words, and the same for the others.
/// `kind` is `overall`, `new`, `trending` or `genre`; `genre` names the genre.
pub fn chart_playlist_comment(kind: &str, genre: &str) -> String {
    const TAIL: &str = ", made by Octo. Songs you have play from your library.";
    match kind {
        "new" => format!("The best new songs of the week{TAIL}"),
        "trending" => format!("The songs catching on this week{TAIL}"),
        "genre" => format!("The most played {} songs right now{TAIL}", genre.trim()),
        _ => POPULAR_COMMENT.to_string(),
    }
}

impl SubsonicResponseBuilder {
    /// A chart list with its songs: library songs as Navidrome described them to the listener,
    /// outside songs in the shape every outside song has. In XML each song is an `entry` with its
    /// fields as attributes and its lists as child elements.
    pub fn create_chart_playlist_response(
        &self,
        format: &str,
        mut fields: Fields,
        entries: &[Value],
    ) -> SubsonicReply {
        fields.insert("songCount".into(), entries.len().into());
        let duration: i64 = entries
            .iter()
            .map(|entry| entry.get("duration").and_then(Value::as_i64).unwrap_or(0))
            .sum();
        fields.insert("duration".into(), duration.into());
        if format.eq_ignore_ascii_case("json") {
            let mut playlist = fields;
            playlist.insert("entry".into(), Value::Array(entries.to_vec()));
            return self.create_json_response(json!({
                "status": "ok",
                "version": SUBSONIC_VERSION,
                "playlist": Value::Object(playlist),
            }));
        }
        let mut playlist = subsonic_element("playlist");
        super::attributes(&mut playlist, &fields);
        for entry in entries {
            playlist.push(json_as_xml("entry", entry));
        }
        SubsonicReply::xml(&envelope("ok").child(playlist))
    }
}

/// A library song Navidrome described in JSON, as the `song` element its XML would have held,
/// marked as the library's like every library row Octo passes on.
pub fn library_song_xml(song: &Value) -> XElement {
    let mut element = json_as_xml("song", song);
    element.set_attr("isExternal", "false");
    element
}

/// A Subsonic JSON object as the XML element the API writes for it: plain values as attributes,
/// an object as a child element, a list of objects as one child element each (`genres`,
/// `artists`), and a list of plain values as one child element each holding the value as text
/// (`isrc`).
pub fn json_as_xml(name: &str, value: &Value) -> XElement {
    let mut element = subsonic_element(name);
    let Value::Object(map) = value else {
        return element;
    };
    for (key, child) in map {
        match child {
            Value::Object(_) => element.push(json_as_xml(key, child)),
            Value::Array(items) => {
                for item in items {
                    if item.is_object() {
                        element.push(json_as_xml(key, item));
                    } else if let Some(text) = scalar(item) {
                        element.push(subsonic_element(key.as_str()).text(text));
                    }
                }
            }
            other => {
                if let Some(text) = scalar(other) {
                    element.set_attr(key.as_str(), text);
                }
            }
        }
    }
    element
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(true) => Some("true".into()),
        Value::Bool(false) => Some("false".into()),
        _ => None,
    }
}

#[cfg(test)]
#[path = "top_songs_tests.rs"]
mod tests;
