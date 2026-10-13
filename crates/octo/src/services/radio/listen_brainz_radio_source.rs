//! ListenBrainz as a radio source: ListenBrainz's similar recordings for a song, found by the
//! recording's MusicBrainz id; LB Radio for an artist or a tag when Octo holds a ListenBrainz
//! token.

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use octo_core::last_fm::SimilarTrack;
use octo_core::radio::radio_provider::LISTEN_BRAINZ;
use octo_core::radio::{RadioAnswer, RadioMatch, RadioSeed};
use octo_core::settings::SettingsStore;

use super::radio_source::IRadioSource;
use crate::services::listen_brainz::listen_brainz_radio_client::{
    ListenBrainzRadioClient, ListenBrainzTrack,
};

pub struct ListenBrainzRadioSource {
    client: Arc<ListenBrainzRadioClient>,
    /// The radio source settings.
    settings: Arc<SettingsStore>,
}

impl ListenBrainzRadioSource {
    pub fn new(client: Arc<ListenBrainzRadioClient>, settings: Arc<SettingsStore>) -> Self {
        ListenBrainzRadioSource { client, settings }
    }
}

fn tracks(tracks: Vec<ListenBrainzTrack>) -> Vec<SimilarTrack> {
    tracks
        .into_iter()
        .map(|track| {
            SimilarTrack::new(track.artist, track.title, track.score)
                .with_duration(track.duration_seconds)
                .with_provider(Some(LISTEN_BRAINZ.to_string()))
        })
        .collect()
}

#[async_trait]
impl IRadioSource for ListenBrainzRadioSource {
    fn provider(&self) -> &'static str {
        LISTEN_BRAINZ
    }

    fn available(&self) -> bool {
        self.settings.current().radio_sources.listen_brainz
    }

    async fn songs_like(
        &self,
        seed: &RadioSeed,
        count: i32,
        _auth: &IndexMap<String, String>,
    ) -> anyhow::Result<RadioAnswer> {
        if seed.is_artist() {
            let mbid = seed
                .library_song
                .as_ref()
                .and_then(|song| song.music_brainz_artist_ids.first().cloned());
            let radio = self
                .client
                .lb_radio(
                    &format!("artist:({})", mbid.as_deref().unwrap_or(&seed.artist)),
                    count,
                )
                .await?;
            let r#match = if radio.is_empty() {
                RadioMatch::None
            } else {
                RadioMatch::ArtistsTrusted
            };
            return Ok(RadioAnswer::new(
                LISTEN_BRAINZ,
                r#match,
                tracks(radio),
                Vec::new(),
            ));
        }
        let tagged = seed
            .library_song
            .as_ref()
            .and_then(|song| song.music_brainz_recording_id.clone())
            .filter(|id| !id.is_empty());
        let recording = match tagged {
            Some(tagged) => Some(tagged),
            None => self.client.recording_mbid(&seed.artist, &seed.title).await,
        };
        let Some(recording) = recording else {
            return Ok(RadioAnswer::nothing(LISTEN_BRAINZ));
        };
        let algorithm = self
            .settings
            .current()
            .radio_sources
            .listen_brainz_algorithm
            .clone();
        let similar = self
            .client
            .similar_recordings(&recording, &algorithm, count)
            .await;
        let r#match = if similar.is_empty() {
            RadioMatch::None
        } else {
            RadioMatch::Song
        };
        Ok(RadioAnswer::new(
            LISTEN_BRAINZ,
            r#match,
            tracks(similar),
            Vec::new(),
        ))
    }

    async fn tag(&self, tag: &str, count: i32) -> anyhow::Result<RadioAnswer> {
        let radio = self.client.lb_radio(&format!("tag:({tag})"), count).await?;
        let r#match = if radio.is_empty() {
            RadioMatch::None
        } else {
            RadioMatch::Genre
        };
        Ok(RadioAnswer::new(
            LISTEN_BRAINZ,
            r#match,
            tracks(radio),
            Vec::new(),
        ))
    }
}

#[cfg(test)]
#[path = "listen_brainz_radio_source_tests.rs"]
mod tests;
