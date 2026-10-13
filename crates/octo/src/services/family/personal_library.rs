//! A member's own library as Octo sees it: its folder (`<members>/<user>`), where their songs
//! land, how much room they take, and taking one out again. Creating and deleting the member's
//! Navidrome account and library is the family service's job; it calls
//! [`PersonalLibrary::ensure_folder`] before it makes the library.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octo_core::settings::SettingsStore;
use tracing::info;

use super::family_store::{FamilyStore, Member};
use super::library_index::{IndexedSong, LibraryIndex};
use super::pool::FamilyPool;

/// Where a member's songs go: their folder and their Navidrome library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryDestination {
    /// The member's name as Octo keeps it.
    pub user: String,
    /// `<members>/<user>`.
    pub root: PathBuf,
    /// Their Navidrome library, once it has been made.
    pub library_id: Option<i64>,
}

/// Why a song could not be taken out of a member's library, with the Subsonic error code the
/// app gets (50 not allowed, 70 not found) and the words the member reads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LibraryError {
    pub code: i32,
    pub message: String,
}

impl LibraryError {
    fn new(code: i32, message: &str) -> Self {
        LibraryError {
            code,
            message: message.to_string(),
        }
    }
}

/// Members' own libraries. See the module comment.
pub struct PersonalLibrary {
    settings: Arc<SettingsStore>,
    family: Arc<FamilyStore>,
    index: Arc<LibraryIndex>,
    pool: Arc<FamilyPool>,
}

impl PersonalLibrary {
    pub fn new(
        settings: Arc<SettingsStore>,
        family: Arc<FamilyStore>,
        index: Arc<LibraryIndex>,
        pool: Arc<FamilyPool>,
    ) -> Self {
        PersonalLibrary {
            settings,
            family,
            index,
            pool,
        }
    }

    pub fn pool(&self) -> &Arc<FamilyPool> {
        &self.pool
    }

    /// A family member, while Family is on.
    fn member(&self, user: &str) -> Option<Member> {
        if !self.settings.current().family.enabled {
            return None;
        }
        self.family.member(user.trim())
    }

    /// `<members>/<user>`.
    pub fn folder_of(&self, user: &str) -> PathBuf {
        PathBuf::from(self.pool.member_root(user))
    }

    /// Makes the member's folder (group writable, so Navidrome can read it whatever user it
    /// runs as) and answers its path.
    pub fn ensure_folder(&self, user: &str) -> io::Result<PathBuf> {
        let folder = self.folder_of(user);
        if !folder.is_dir() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o775);
            }
            builder.create(&folder)?;
            info!("FAMILY made {}", folder.display());
        }
        Ok(folder)
    }

    /// The member's own Navidrome library.
    pub fn library_id(&self, user: &str) -> Option<i64> {
        self.member(user).and_then(|m| m.personal_library_id)
    }

    /// Where `user`'s songs go; None when they are not a member (or Family is off).
    pub fn destination(&self, user: &str) -> Option<LibraryDestination> {
        let member = self.member(user)?;
        Some(LibraryDestination {
            root: self.folder_of(&member.username),
            library_id: member.personal_library_id,
            user: member.username,
        })
    }

    /// Whether library song `song_id` is in `user`'s own library.
    pub fn owns_song(&self, user: &str, song_id: &str) -> bool {
        let Some(library) = self.library_id(user) else {
            return false;
        };
        self.index.library_of(song_id) == Some(library)
    }

    /// The songs in `user`'s own library, as the index knows them.
    pub fn songs(&self, user: &str) -> Vec<IndexedSong> {
        self.library_id(user)
            .map(|library| self.index.songs_in_library(library))
            .unwrap_or_default()
    }

    /// How many bytes `user`'s own library holds: every song at full size, linked or not (the
    /// index's sizes plus files placed since it was last read). 0 for anyone who is not a
    /// member.
    pub fn storage_used(&self, user: &str) -> u64 {
        let Some(member) = self.member(user) else {
            return 0;
        };
        let indexed = member
            .personal_library_id
            .map(|library| self.index.bytes_in_library(library))
            .unwrap_or(0);
        // Before the index is first read (just after a restart) the pool's record still knows.
        indexed.max(self.pool.store().bytes_linked_by(&member.username))
    }

    /// Takes song `song_id` out of `user`'s own library: their link goes, and the file goes to
    /// the trash once nobody else has it. Only their own songs: a shared song is refused (50).
    pub async fn remove_from_my_library(&self, user: &str, song_id: &str) -> Result<(), LibraryError> {
        let Some(member) = self.member(user) else {
            return Err(LibraryError::new(
                50,
                "Only family members have a library of their own.",
            ));
        };
        let Some(song) = self.index.song(song_id) else {
            return Err(LibraryError::new(70, "That song was not found."));
        };
        if member.personal_library_id.is_none() || song.library_id != member.personal_library_id {
            return Err(LibraryError::new(
                50,
                "This song is in the shared library, so it cannot be removed from yours.",
            ));
        }
        let Some(pool_path) = self.pool.pool_path_of_link(&member.username, &song.path) else {
            return Err(LibraryError::new(70, "That song was not found."));
        };
        if !Path::new(&song.path).exists() && self.pool.store().get(&pool_path).is_none() {
            return Err(LibraryError::new(70, "That song was not found."));
        }
        self.pool
            .unlink(&pool_path, &member.username)
            .await
            .map_err(|e| LibraryError::new(0, &e.to_string()))?;
        info!(
            "FAMILY {} removed {} from their library",
            member.username, song.path
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::family::test_support::{World, navidrome_with_songs, song_row};
    use serde_json::json;

    const SONG: &str = "Artist/Album/01 Song.flac";

    fn personal(world: &World) -> PersonalLibrary {
        PersonalLibrary::new(
            world.settings.clone(),
            world.family.clone(),
            world.index.clone(),
            world.pool.clone(),
        )
    }

    #[tokio::test]
    async fn usage_counts_linked_songs_at_full_size() {
        let world = World::new(None);
        let library = personal(&world);
        let pooled = world.pool_file(SONG, 30_000);
        world.pool.link(&pooled, "alex").await.expect("linked");
        world.pool.link(&pooled, "sam").await.expect("linked");
        // One file on disk, and each member's library holds all of it.
        assert_eq!(library.storage_used("alex"), 30_000);
        assert_eq!(library.storage_used("SAM"), 30_000);
        assert_eq!(
            library.storage_used("winters"),
            0,
            "the owner has no library of their own"
        );

        // Read from the index once Navidrome has scanned it, still at full size.
        let members = world.members();
        let navidrome = navidrome_with_songs(json!([
            song_row("a1", 3, &format!("{members}/alex"), SONG, 30_000),
            song_row("a2", 3, &format!("{members}/alex"), "Other/Album/02.mp3", 5_000),
        ]))
        .await;
        world.point_at(&navidrome.uri());
        world.index.refresh().await.expect("read");
        assert_eq!(library.storage_used("alex"), 35_000);
    }

    #[tokio::test]
    async fn folders_and_destinations() {
        let world = World::new(None);
        let library = personal(&world);
        let folder = library.ensure_folder("alex").expect("made");
        assert!(folder.is_dir());
        assert_eq!(folder, PathBuf::from(format!("{}/alex", world.members())));
        let destination = library.destination("ALEX").expect("a member");
        assert_eq!(destination.user, "alex");
        assert_eq!(destination.library_id, Some(3));
        assert!(library.destination("winters").is_none());
    }

    #[tokio::test]
    async fn remove_from_my_library_takes_only_their_own_songs() {
        let world = World::new(None);
        let library = personal(&world);
        let pooled = world.pool_file(SONG, 10);
        world.pool.link(&pooled, "alex").await.expect("linked");
        world.pool.link(&pooled, "sam").await.expect("linked");
        let members = world.members();
        let music = world.music();
        let navidrome = navidrome_with_songs(json!([
            song_row("alex-1", 3, &format!("{members}/alex"), SONG, 10),
            song_row("sam-1", 4, &format!("{members}/sam"), SONG, 10),
            song_row("shared-1", 1, &music, "Shared/Album/01.flac", 10),
        ]))
        .await;
        world.point_at(&navidrome.uri());
        world.index.refresh().await.expect("read");
        assert!(library.owns_song("alex", "alex-1"));
        assert!(!library.owns_song("alex", "sam-1"));

        let shared = library.remove_from_my_library("alex", "shared-1").await;
        assert_eq!(shared.expect_err("refused").code, 50);
        let others = library.remove_from_my_library("alex", "sam-1").await;
        assert_eq!(others.expect_err("refused").code, 50);
        let missing = library.remove_from_my_library("alex", "nothing").await;
        assert_eq!(missing.expect_err("refused").code, 70);
        let owner = library.remove_from_my_library("winters", "shared-1").await;
        assert_eq!(owner.expect_err("refused").code, 50);

        library
            .remove_from_my_library("alex", "alex-1")
            .await
            .expect("removed");
        assert!(!Path::new(&format!("{members}/alex/{SONG}")).exists());
        assert!(Path::new(&pooled).is_file(), "Sam still has it");
        library
            .remove_from_my_library("sam", "sam-1")
            .await
            .expect("removed");
        assert!(!Path::new(&pooled).exists(), "nobody has it now");
    }
}
