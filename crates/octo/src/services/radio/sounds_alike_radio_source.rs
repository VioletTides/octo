//! Sounds alike as a radio source: library songs that sound like the seed, from octo-sonic's
//! analysis. Only for library seeds.

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use octo_core::models::domain::Song;
use octo_core::radio::radio_provider::SOUNDS_ALIKE;
use octo_core::radio::{RadioAnswer, RadioMatch, RadioSeed};
use octo_core::settings::SettingsStore;

use super::radio_source::IRadioSource;
use crate::services::sonic::SonicStore;

pub struct SoundsAlikeRadioSource {
    store: Arc<SonicStore>,
    /// The radio source settings.
    settings: Arc<SettingsStore>,
}

impl SoundsAlikeRadioSource {
    pub fn new(store: Arc<SonicStore>, settings: Arc<SettingsStore>) -> Self {
        SoundsAlikeRadioSource { store, settings }
    }
}

#[async_trait]
impl IRadioSource for SoundsAlikeRadioSource {
    fn provider(&self) -> &'static str {
        SOUNDS_ALIKE
    }

    fn available(&self) -> bool {
        self.settings.current().radio_sources.sounds_alike
    }

    async fn songs_like(
        &self,
        seed: &RadioSeed,
        count: i32,
        _auth: &IndexMap<String, String>,
    ) -> anyhow::Result<RadioAnswer> {
        let Some(song) = seed.library_song.as_ref().filter(|song| !song.id.is_empty()) else {
            return Ok(RadioAnswer::nothing(SOUNDS_ALIKE));
        };
        if seed.is_artist() {
            return Ok(RadioAnswer::nothing(SOUNDS_ALIKE));
        }
        let near = self.store.nearest(&song.id, count.max(0) as usize);
        // Stubs: SongRadioService reads each picked song again with the listener's own sign-in.
        let songs: Vec<Song> = near
            .into_iter()
            .map(|(id, song, _)| Song {
                id,
                title: song.title,
                artist: song.artist,
                album: song.album.unwrap_or_default(),
                duration: song.duration,
                is_local: true,
                ..Default::default()
            })
            .collect();
        let r#match = if songs.is_empty() {
            RadioMatch::None
        } else {
            RadioMatch::Song
        };
        let mut answer = RadioAnswer::new(SOUNDS_ALIKE, r#match, Vec::new(), songs);
        answer.library_songs_complete = false;
        Ok(answer)
    }
}
