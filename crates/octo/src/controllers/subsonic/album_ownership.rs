//! Outside albums the library already holds. A search, an artist page and an outside album's own
//! page used to judge an outside album by its id or its exact name only, so "HABIBTI", whose
//! eleven songs all sit in the library's "HABIBTI (FOMO)", was listed as not in the library, and
//! its page showed none of them owned. See `services::library::AlbumOwnership`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use axum::response::{IntoResponse, Response};
use indexmap::IndexMap;
use octo_core::models::domain::Album;
use octo_subsonic::subsonic_model_mapper::Row;
use octo_subsonic::subsonic_request_parser::Parameters;
use octo_subsonic::subsonic_response_builder::SubsonicResponseBuilder;
use octo_subsonic::xml::XElement;
use serde_json::Value;
use tracing::debug;

use super::action_helpers::is_failed_subsonic_body;
use crate::app::AppState;
use crate::services::family::{FamilyCaller, LibraryScope};
use crate::services::library::{AlbumOwnership, CatalogTrack, Standing};
use crate::services::subsonic::SubsonicProxyService;

/// How long a request waits for the library and the tracklists it needs. A search started the
/// wait when its albums came back, alongside its songs, so it rarely adds anything; what is not
/// ready by then goes out uncounted, as before, and is ready for the next request.
pub(crate) const OWNERSHIP_WAIT: Duration = Duration::from_millis(1500);

/// At most this many library albums stand in for one outside album held whole.
const MAX_STAND_INS: usize = 3;

/// The library a caller sees: a family member's own songs with the shared ones, anyone else the
/// shared library.
pub(crate) fn scope_of(caller: Option<&FamilyCaller>) -> LibraryScope {
    match caller {
        Some(caller) if caller.managed => LibraryScope::Viewer(caller.user.clone()),
        _ => LibraryScope::Shared,
    }
}

/// What the library `scope` sees holds of each outside album, once they are found; empty when
/// it cannot be told.
pub(crate) async fn judge_albums(state: &AppState, albums: &[Album], scope: LibraryScope) -> Vec<Standing> {
    if albums.is_empty() {
        return Vec::new();
    }
    state
        .album_ownership
        .judge_in(albums, OWNERSHIP_WAIT, scope)
        .await
}

/// A search's outside albums settled against the library: each carries how many of its songs
/// the library holds; one held whole, or one with a library album's very name and artist, is
/// left out and the library album that holds it listed instead (fetched as the caller when the
/// search did not already list it). An outside album whose library album cannot be listed
/// stays, with its count, so nothing disappears.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn settle_search_albums(
    builder: &SubsonicResponseBuilder,
    proxy: &SubsonicProxyService,
    parameters: &Parameters,
    outside: Vec<Album>,
    standings: &[Standing],
    local_albums: &[Row],
    local_content_type: Option<&str>,
    can_add: bool,
) -> (Vec<Album>, Vec<Row>) {
    if standings.is_empty() {
        return (outside, Vec::new());
    }
    let by_album: HashMap<usize, &Standing> = standings
        .iter()
        .map(|standing| (standing.index, standing))
        .collect();
    let mut listed: HashSet<String> = local_albums.iter().filter_map(album_id_of).collect();

    let mut wanted: Vec<String> = Vec::new();
    for standing in standings {
        if !can_add {
            break;
        }
        for stand_in in standing.stand_ins().iter().take(MAX_STAND_INS) {
            if !listed.contains(&stand_in.id) && !wanted.contains(&stand_in.id) {
                wanted.push(stand_in.id.clone());
            }
        }
    }

    let json = local_content_type.is_some_and(|content_type| content_type.contains("json"));
    let fetched = futures::future::join_all(
        wanted
            .iter()
            .map(|id| library_album_row(builder, proxy, parameters, id, json)),
    )
    .await;
    let mut added: Vec<Row> = Vec::new();
    for row in fetched.into_iter().flatten() {
        let Some(id) = album_id_of(&row) else { continue };
        if !listed.insert(id) {
            continue;
        }
        added.push(row);
    }

    let mut kept: Vec<Album> = Vec::new();
    for (at, mut album) in outside.into_iter().enumerate() {
        let Some(standing) = by_album.get(&at) else {
            kept.push(album);
            continue;
        };
        album.owned_count = standing.owned;
        if standing
            .stand_ins()
            .iter()
            .take(MAX_STAND_INS)
            .any(|stand_in| listed.contains(&stand_in.id))
        {
            continue;
        }
        kept.push(album);
    }
    (kept, added)
}

/// An artist page's outside albums settled against the library: each carries its count, and one
/// held whole by an album the page already lists, or sharing a listed album's name, is left out.
/// Nothing is added: an artist page lists that artist's albums.
pub(crate) async fn settle_artist_albums(
    state: &AppState,
    outside: Vec<Album>,
    local_albums: &[Value],
    scope: LibraryScope,
) -> Vec<Album> {
    let standings = judge_albums(state, &outside, scope).await;
    if standings.is_empty() {
        return outside;
    }
    let by_album: HashMap<usize, &Standing> = standings
        .iter()
        .map(|standing| (standing.index, standing))
        .collect();
    let listed: HashSet<String> = local_albums.iter().filter_map(json_id_of).collect();
    let mut kept = Vec::new();
    for (at, mut album) in outside.into_iter().enumerate() {
        if let Some(standing) = by_album.get(&at) {
            album.owned_count = standing.owned;
            if standing
                .stand_ins()
                .iter()
                .any(|stand_in| listed.contains(&stand_in.id))
            {
                continue;
            }
        }
        kept.push(album);
    }
    kept
}

/// An outside album's page with the songs the library holds given as the library's own copies,
/// in the album's order and numbered as the album numbers them, so they play from the library
/// and show as owned; the rest stay outside songs. None when the library holds none of them or
/// cannot be read in time, and the page goes out as before (with the count, once it is known).
pub(crate) async fn outside_album_with_owned_songs(
    state: &AppState,
    proxy: &SubsonicProxyService,
    parameters: &Parameters,
    album: &mut Album,
    format: &str,
    scope: LibraryScope,
) -> Option<Response> {
    if album.songs.is_empty() {
        return None;
    }
    let clock = Instant::now();
    let index = state.album_ownership.library_in(OWNERSHIP_WAIT, scope).await?;

    let rows: Vec<_> = album
        .songs
        .iter()
        .map(|song| {
            AlbumOwnership::find(
                &index,
                album,
                &CatalogTrack {
                    title: song.title.clone(),
                    artist: Some(song.artist.clone()),
                    duration: song.duration,
                    isrc: song.isrc.clone(),
                },
            )
        })
        .collect();
    let found = rows.iter().filter(|row| row.is_some()).count() as i32;
    album.owned_count = Some(found);
    if found == 0 {
        return None;
    }

    // The library's songs as the caller's Navidrome describes them, one call per library album.
    let mut holders: Vec<String> = Vec::new();
    for album_id in rows.iter().flatten().filter_map(|row| row.album_id.clone()) {
        if !holders.contains(&album_id) {
            holders.push(album_id);
        }
    }
    let builder = &state.subsonic_response_builder;
    let mut copies: HashMap<String, serde_json::Map<String, Value>> = HashMap::new();
    for songs in futures::future::join_all(
        holders
            .iter()
            .map(|id| library_album_songs(builder, proxy, parameters, id)),
    )
    .await
    {
        for (id, song) in songs {
            copies.entry(id).or_insert(song);
        }
    }

    let mut merged: Vec<Value> = Vec::new();
    let mut owned = 0;
    for (song, row) in album.songs.iter().zip(&rows) {
        if let Some(copy) = row.as_ref().and_then(|row| copies.get(&row.id)) {
            let mut placed = copy.clone();
            if let Some(track) = song.track {
                placed.insert("track".into(), track.into());
            }
            if let Some(disc) = song.disc_number {
                placed.insert("discNumber".into(), disc.into());
            }
            merged.push(Value::Object(placed));
            owned += 1;
        } else {
            merged.push(Value::Object(builder.convert_song_to_json(song)));
        }
    }
    album.owned_count = Some(owned);
    if owned == 0 {
        return None;
    }

    let mut fields = builder.album_detail_fields(album);
    fields.insert("song".into(), Value::Array(merged));
    debug!(
        "getAlbum '{} - {}': {owned} of {} songs given as the library's copies in {} ms",
        album.artist,
        album.title,
        album.songs.len(),
        clock.elapsed().as_millis()
    );
    Some(
        builder
            .create_merged_response(format, "album", &Value::Object(fields))
            .into_response(),
    )
}

/// A library album as the caller's search would list it, without its songs, in the search's own
/// format; None when Navidrome does not answer.
async fn library_album_row(
    builder: &SubsonicResponseBuilder,
    proxy: &SubsonicProxyService,
    parameters: &Parameters,
    id: &str,
    json: bool,
) -> Option<Row> {
    let mut asked = library_album_parameters(id, parameters);
    asked.insert("f".into(), if json { "json" } else { "xml" }.into());
    let result = proxy.relay_safe("rest/getAlbum", &asked).await?;
    let content_type = result.content_type.as_deref();
    if result.body.is_empty() || is_failed_subsonic_body(Some(&result.body), content_type) {
        return None;
    }
    let read = || -> Result<Option<Row>, String> {
        if content_type.is_some_and(|content_type| content_type.contains("json")) {
            let document: Value = serde_json::from_slice(&result.body).map_err(|e| e.to_string())?;
            let Some(album_element) = document
                .get("subsonic-response")
                .and_then(|response| response.get("album"))
            else {
                return Ok(None);
            };
            let Some(Value::Object(mut row)) = builder.convert_subsonic_json_element(album_element, true)
            else {
                return Ok(None);
            };
            row.shift_remove("song");
            return Ok(Some(Row::Json(Value::Object(row))));
        }
        let root = XElement::parse(&String::from_utf8_lossy(&result.body)).map_err(|e| e.to_string())?;
        let ns = root.namespace.clone();
        let Some(element) = root.elements_named(ns.as_deref(), "album").next() else {
            return Ok(None);
        };
        let mut album = builder.convert_subsonic_xml_element(element, "album");
        album.content.retain(|node| {
            !matches!(node, octo_subsonic::xml::XNode::Element(child) if child.is(ns.as_deref(), "song"))
        });
        Ok(Some(Row::Xml(album)))
    };
    match read() {
        Ok(row) => row,
        Err(message) => {
            debug!("library album {id} could not be listed: {message}");
            None
        }
    }
}

/// A library album's songs as the caller's Navidrome describes them, by id.
async fn library_album_songs(
    builder: &SubsonicResponseBuilder,
    proxy: &SubsonicProxyService,
    parameters: &Parameters,
    id: &str,
) -> Vec<(String, serde_json::Map<String, Value>)> {
    let mut asked = library_album_parameters(id, parameters);
    asked.insert("f".into(), "json".into());
    let mut found = Vec::new();
    let Some(result) = proxy.relay_safe("rest/getAlbum", &asked).await else {
        return found;
    };
    if result.body.is_empty() {
        return found;
    }
    let document: Value = match serde_json::from_slice(&result.body) {
        Ok(document) => document,
        Err(error) => {
            debug!("library album {id} songs could not be read: {error}");
            return found;
        }
    };
    let Some(Value::Array(songs)) = document
        .get("subsonic-response")
        .and_then(|response| response.get("album"))
        .and_then(|album| album.get("song"))
    else {
        return found;
    };
    for song in songs {
        if let Some(Value::Object(dict)) = builder.convert_subsonic_json_element(song, true)
            && let Some(key) = dict.get("id").and_then(id_text).filter(|key| !key.is_empty())
        {
            found.push((key, dict));
        }
    }
    found
}

/// The caller's own credentials and client, asking for one album.
fn library_album_parameters(id: &str, parameters: &Parameters) -> IndexMap<String, String> {
    let mut asked = IndexMap::new();
    for name in ["u", "p", "t", "s", "apiKey", "v", "c"] {
        if let Some(value) = parameters.get(name) {
            asked.insert(name.to_string(), value.clone());
        }
    }
    asked.insert("id".into(), id.into());
    asked
}

/// A listed album's id, from its JSON row or its XML element.
pub(crate) fn album_id_of(row: &Row) -> Option<String> {
    match row {
        Row::Json(value) => json_id_of(value),
        Row::Xml(element) => element.attribute("id").map(str::to_string),
    }
}

fn json_id_of(value: &Value) -> Option<String> {
    value.as_object()?.get("id").and_then(id_text)
}

/// `object?.ToString()` of an id value.
fn id_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(if *flag { "True" } else { "False" }.into()),
        _ => None,
    }
}
