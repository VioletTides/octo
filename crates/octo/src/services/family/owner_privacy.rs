//! The owner's privacy. With `Family:OwnerSeesMemberLibraries` off (the default), the owner's
//! answers leave out the music in members' own libraries: their songs, albums whose every song
//! is a member's, artists whose every song is a member's, the libraries themselves in
//! `getMusicFolders`, and their songs and albums in `getGenres` counts. With `Family:OwnerSeesMemberPlaylists` off, the owner's `getPlaylists`
//! leaves out playlists members own.
//!
//! Navidrome shows an admin every library, and `getArtist` and `getAlbum` take no folder, so
//! this works on the answers, from the [`LibraryIndex`]. A song the index does not know yet
//! counts as shared: the owner never loses sight of something just downloaded.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use octo_core::family::abilities::Role;
use octo_core::settings::FamilySettings;
use parking_lot::Mutex;

use octo_subsonic::xml::XNode;
use serde_json::Value;

use super::caller::FamilyCaller;
use super::library_index::LibraryIndex;
use super::list_rules::{self, NodeKind, listed};

/// The endpoints whose answers to the owner leave out members' own music.
pub const LIBRARY_ENDPOINTS: &[&str] = &[
    "search",
    "getGenres",
    "search2",
    "search3",
    "getAlbum",
    "getAlbumList",
    "getAlbumList2",
    "getArtist",
    "getArtists",
    "getIndexes",
    "getMusicDirectory",
    "getMusicFolders",
    "getPlaylist",
    "getRandomSongs",
    "getSongsByGenre",
    "getSimilarSongs",
    "getSimilarSongs2",
    "getTopSongs",
    "getStarred",
    "getStarred2",
];

/// Which of the owner's two privacy rules apply to one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerRules {
    /// Leave out members' own music.
    pub libraries: bool,
    /// Leave out members' playlists.
    pub playlists: bool,
}

/// The owner's rules for an answer from `endpoint`, or None when neither applies.
pub fn applies(caller: &FamilyCaller, endpoint: &str, family: &FamilySettings) -> Option<OwnerRules> {
    if caller.managed || caller.role != Role::Owner {
        return None;
    }
    let rules = OwnerRules {
        libraries: !family.owner_sees_member_libraries && listed(LIBRARY_ENDPOINTS, endpoint),
        playlists: !family.owner_sees_member_playlists && endpoint.eq_ignore_ascii_case("getPlaylists"),
    };
    (rules.libraries || rules.playlists).then_some(rules)
}

/// Whether a playlist row is a member's (its owner is a member), given the members' names in
/// lower case. Family playlists are owned by "Family" and stay.
pub fn hides_playlist(members: &HashSet<String>, owner: Option<&str>) -> bool {
    owner.is_some_and(|owner| members.contains(&owner.to_lowercase()))
}

/// What is a member's own, worked out once per read of the index.
#[derive(Debug, Default)]
pub struct PrivateView {
    libraries: HashSet<i64>,
    albums: HashSet<String>,
    artists: HashSet<String>,
    /// Per genre (lower case): the members' own songs, and the albums that are all members'.
    genres: HashMap<String, (i64, i64)>,
}

impl PrivateView {
    /// Reads the index: an album or artist is a member's when every song of it the index knows
    /// is in a member's own library.
    pub fn build(index: &LibraryIndex, personal_libraries: &[i64]) -> Self {
        let libraries: HashSet<i64> = personal_libraries.iter().copied().collect();
        // Songs per album and per artist: (in members' libraries, elsewhere).
        let mut albums: HashMap<String, (u32, u32)> = HashMap::new();
        let mut artists: HashMap<String, (u32, u32)> = HashMap::new();
        let mut genres: HashMap<String, (i64, i64)> = HashMap::new();
        // The genres of the members' own songs, per album.
        let mut album_genres: HashMap<String, HashSet<String>> = HashMap::new();
        index.each_song(|song| {
            let personal = song
                .library_id
                .is_some_and(|library| libraries.contains(&library));
            let tally = |counts: &mut (u32, u32)| {
                if personal {
                    counts.0 += 1;
                } else {
                    counts.1 += 1;
                }
            };
            if !song.album_id.is_empty() {
                tally(albums.entry(song.album_id.clone()).or_default());
            }
            for artist in &song.artist_ids {
                tally(artists.entry(artist.clone()).or_default());
            }
            if personal {
                for genre in &song.genres {
                    let key = genre.to_lowercase();
                    genres.entry(key.clone()).or_default().0 += 1;
                    if !song.album_id.is_empty() {
                        album_genres.entry(song.album_id.clone()).or_default().insert(key);
                    }
                }
            }
        });
        let own = |counts: HashMap<String, (u32, u32)>| -> HashSet<String> {
            counts
                .into_iter()
                .filter(|(_, (personal, shared))| *personal > 0 && *shared == 0)
                .map(|(id, _)| id)
                .collect()
        };
        let albums = own(albums);
        for album in &albums {
            for genre in album_genres.get(album).into_iter().flatten() {
                genres.entry(genre.clone()).or_default().1 += 1;
            }
        }
        PrivateView {
            libraries,
            albums,
            artists: own(artists),
            genres,
        }
    }

    /// How many of a genre's songs, and of its albums, are members' own.
    pub fn genre_counts(&self, genre: &str) -> (i64, i64) {
        self.genres
            .get(&genre.trim().to_lowercase())
            .copied()
            .unwrap_or_default()
    }

    /// Whether a song in `library` is a member's own (an unknown library is shared).
    pub fn hides_song(&self, library: Option<i64>) -> bool {
        library.is_some_and(|library| self.libraries.contains(&library))
    }

    /// Whether a node other than a song is a member's own.
    pub fn hides(&self, kind: NodeKind, id: &str) -> bool {
        match kind {
            NodeKind::Album => self.albums.contains(id),
            NodeKind::Artist => self.artists.contains(id),
            NodeKind::Folder => self.albums.contains(id) || self.artists.contains(id),
            NodeKind::MusicFolder => id
                .trim()
                .parse::<i64>()
                .is_ok_and(|library| self.libraries.contains(&library)),
            NodeKind::Song | NodeKind::Playlist => false,
        }
    }
}

/// A `getGenres` answer without the members' own music: each genre's `songCount` and
/// `albumCount` lose what is in members' own libraries, and a genre left with nothing goes.
/// Unchanged without reading it when no genre holds members' music.
pub fn genres_answer(body: &[u8], view: &PrivateView) -> list_rules::Edit {
    if view.genres.is_empty() {
        return list_rules::Edit::Unchanged;
    }
    // The counts a genre keeps, or None when it has nothing left.
    let lowered = |genre: &str, songs: i64, albums: i64| -> Option<(i64, i64, bool)> {
        let (own_songs, own_albums) = view.genre_counts(genre);
        if own_songs == 0 && own_albums == 0 {
            return Some((songs, albums, false));
        }
        let (songs, albums) = ((songs - own_songs).max(0), (albums - own_albums).max(0));
        (songs > 0 || albums > 0).then_some((songs, albums, true))
    };
    let number = |value: Option<&Value>| match value {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    };
    list_rules::edit(
        body,
        |response| {
            let Some(Value::Array(list)) = response
                .get_mut("genres")
                .and_then(Value::as_object_mut)
                .and_then(|genres| genres.get_mut("genre"))
            else {
                return false;
            };
            let mut changed = false;
            list.retain_mut(|item| {
                let Value::Object(genre) = item else {
                    return true;
                };
                let name = genre
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let songs = number(genre.get("songCount"));
                let albums = number(genre.get("albumCount"));
                match lowered(&name, songs, albums) {
                    None => {
                        changed = true;
                        false
                    }
                    Some((_, _, false)) => true,
                    Some((songs, albums, true)) => {
                        genre.insert("songCount".into(), Value::from(songs));
                        genre.insert("albumCount".into(), Value::from(albums));
                        changed = true;
                        true
                    }
                }
            });
            changed
        },
        |root| {
            let mut changed = false;
            for genres in root.elements_mut().filter(|e| e.name == "genres") {
                let content = std::mem::take(&mut genres.content);
                let mut kept = Vec::with_capacity(content.len());
                for node in content {
                    let XNode::Element(mut genre) = node else {
                        kept.push(node);
                        continue;
                    };
                    if genre.name != "genre" {
                        kept.push(XNode::Element(genre));
                        continue;
                    }
                    let parse = |name: &str| {
                        genre
                            .attribute(name)
                            .and_then(|v| v.trim().parse::<i64>().ok())
                            .unwrap_or(0)
                    };
                    let (songs, albums) = (parse("songCount"), parse("albumCount"));
                    // Subsonic writes the name as the element's text; some servers as `value`.
                    let name = genre
                        .attribute("value")
                        .map(str::to_string)
                        .unwrap_or_else(|| genre.value());
                    match lowered(&name, songs, albums) {
                        None => changed = true,
                        Some((_, _, false)) => kept.push(XNode::Element(genre)),
                        Some((songs, albums, true)) => {
                            genre.set_attr("songCount", songs.to_string());
                            genre.set_attr("albumCount", albums.to_string());
                            changed = true;
                            kept.push(XNode::Element(genre));
                        }
                    }
                }
                genres.content = kept;
            }
            changed
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ViewKey {
    refreshed: Option<DateTime<Utc>>,
    songs: usize,
    libraries: Vec<i64>,
}

/// The last [`PrivateView`], kept until the index is read again or the members' libraries
/// change.
#[derive(Default)]
pub struct PrivacyCache {
    cached: Mutex<Option<(ViewKey, Arc<PrivateView>)>>,
}

impl PrivacyCache {
    pub fn view(&self, index: &LibraryIndex, mut personal_libraries: Vec<i64>) -> Arc<PrivateView> {
        personal_libraries.sort_unstable();
        personal_libraries.dedup();
        let key = ViewKey {
            refreshed: index.refreshed_at(),
            songs: index.len(),
            libraries: personal_libraries,
        };
        self.view_for(key, |libraries| PrivateView::build(index, libraries))
    }

    /// The cached view for `key`, or one `build` makes. The lock is held only to look and to
    /// keep: a view is built (a walk of every song) with the lock free, so other requests read
    /// the cache meanwhile. Two requests that miss at once may both build; either view is right.
    fn view_for(&self, key: ViewKey, build: impl FnOnce(&[i64]) -> PrivateView) -> Arc<PrivateView> {
        if let Some((held, view)) = self.cached.lock().as_ref()
            && *held == key
        {
            return Arc::clone(view);
        }
        let view = Arc::new(build(&key.libraries));
        *self.cached.lock() = Some((key, Arc::clone(&view)));
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octo_core::family::abilities::{Abilities, Preset};

    #[test]
    fn only_the_owner_gets_privacy() {
        let family = FamilySettings::default();
        let owner = FamilyCaller::unmanaged("winters", true);
        let other = FamilyCaller::unmanaged("guest", false);
        let member = FamilyCaller::member("alex", Preset::CoAdmin, Abilities::preset(Preset::CoAdmin));
        let rules = applies(&owner, "getArtist", &family);
        assert_eq!(
            rules,
            (!family.owner_sees_member_libraries).then_some(OwnerRules {
                libraries: true,
                playlists: false
            })
        );
        assert!(applies(&other, "getArtist", &family).is_none());
        assert!(applies(&member, "getArtist", &family).is_none());
        assert!(applies(&owner, "getNowPlaying", &family).is_none());
        let sees_all = FamilySettings {
            owner_sees_member_libraries: true,
            owner_sees_member_playlists: true,
            ..FamilySettings::default()
        };
        assert!(applies(&owner, "getArtist", &sees_all).is_none());
        assert!(applies(&owner, "getPlaylists", &sees_all).is_none());
    }

    #[test]
    fn member_playlists_are_told_by_owner() {
        let members: HashSet<String> = ["alex".to_string()].into();
        assert!(hides_playlist(&members, Some("Alex")));
        assert!(!hides_playlist(&members, Some("winters")));
        assert!(!hides_playlist(&members, Some("Family")));
        assert!(!hides_playlist(&members, None));
    }

    #[test]
    fn genre_counts_lose_members_songs_with_the_name_as_text() {
        let view = PrivateView {
            genres: [("rock".to_string(), (2, 1)), ("pop".to_string(), (1, 1))].into(),
            ..PrivateView::default()
        };
        let xml = br#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok"><genres><genre songCount="5" albumCount="3">Rock</genre><genre songCount="1" albumCount="1">Pop</genre><genre songCount="2" albumCount="1">Jazz</genre></genres></subsonic-response>"#;
        let list_rules::Edit::Changed(out) = genres_answer(xml, &view) else {
            panic!("changed");
        };
        let out = String::from_utf8(out).expect("text");
        assert!(
            out.contains(r#"<genre songCount="3" albumCount="2">Rock</genre>"#),
            "{out}"
        );
        assert!(!out.contains("Pop"), "{out}");
        assert!(out.contains(">Jazz</genre>"), "{out}");
        let untouched = br#"<subsonic-response status="ok"><genres><genre songCount="2" albumCount="1">Jazz</genre></genres></subsonic-response>"#;
        assert_eq!(genres_answer(untouched, &view), list_rules::Edit::Unchanged);
        assert_eq!(genres_answer(b"<genres", &view), list_rules::Edit::Unreadable);
    }

    #[test]
    fn the_privacy_view_is_built_with_the_cache_free() {
        let cache = PrivacyCache::default();
        let key = ViewKey {
            refreshed: None,
            songs: 3,
            libraries: vec![3],
        };
        let built = cache.view_for(key.clone(), |libraries| {
            assert!(
                cache.cached.try_lock().is_some(),
                "the cache is free while a view is built"
            );
            PrivateView {
                libraries: libraries.iter().copied().collect(),
                ..PrivateView::default()
            }
        });
        assert!(built.hides_song(Some(3)));
        let again = cache.view_for(key, |_| panic!("kept, not built again"));
        assert!(Arc::ptr_eq(&built, &again));
    }

    #[test]
    fn music_folders_are_libraries() {
        let view = PrivateView {
            libraries: [3].into(),
            ..PrivateView::default()
        };
        assert!(view.hides(NodeKind::MusicFolder, "3"));
        assert!(!view.hides(NodeKind::MusicFolder, "1"));
        assert!(view.hides_song(Some(3)));
        assert!(!view.hides_song(None), "an unknown song counts as shared");
    }
}
