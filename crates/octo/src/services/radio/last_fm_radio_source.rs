//! Last.fm as a radio source: Last.fm's songs like a song, or the top songs of artists like an
//! artist.

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use octo_core::last_fm::SimilarTrack;
use octo_core::radio::radio_provider::LAST_FM;
use octo_core::radio::{RadioAnswer, RadioMatch, RadioSeed};

use super::radio_source::IRadioSource;
use crate::services::last_fm::LastFmService;
use crate::services::last_fm::last_fm_service::SimilarSource;

pub struct LastFmRadioSource {
    last_fm: Arc<LastFmService>,
}

impl LastFmRadioSource {
    pub fn new(last_fm: Arc<LastFmService>) -> Self {
        LastFmRadioSource { last_fm }
    }

    fn mark(&self, tracks: Vec<SimilarTrack>) -> Vec<SimilarTrack> {
        tracks
            .into_iter()
            .map(|track| track.with_provider(Some(LAST_FM.to_string())))
            .collect()
    }
}

#[async_trait]
impl IRadioSource for LastFmRadioSource {
    fn provider(&self) -> &'static str {
        LAST_FM
    }

    fn available(&self) -> bool {
        self.last_fm.has_api_key()
    }

    async fn songs_like(
        &self,
        seed: &RadioSeed,
        count: i32,
        _auth: &IndexMap<String, String>,
    ) -> anyhow::Result<RadioAnswer> {
        if seed.is_artist() {
            let tracks = self.last_fm.get_similar_artist_tracks(&seed.artist, count).await;
            return Ok(RadioAnswer::new(
                LAST_FM,
                RadioMatch::ArtistsTrusted,
                self.mark(tracks),
                Vec::new(),
            ));
        }
        let answer = self
            .last_fm
            .find_similar_tracks(&seed.artist, &seed.title, count)
            .await;
        let r#match = match answer.source {
            SimilarSource::Song => RadioMatch::Song,
            SimilarSource::Original => RadioMatch::Original,
            SimilarSource::SimilarArtists if answer.artist_trusted => RadioMatch::ArtistsTrusted,
            SimilarSource::SimilarArtists => RadioMatch::ArtistsGuessed,
            SimilarSource::None => RadioMatch::None,
        };
        Ok(RadioAnswer::new(
            LAST_FM,
            r#match,
            self.mark(answer.tracks),
            Vec::new(),
        ))
    }

    async fn tag(&self, tag: &str, count: i32) -> anyhow::Result<RadioAnswer> {
        let tracks = self.last_fm.get_tag_top_tracks(tag, count).await?;
        Ok(RadioAnswer::new(
            LAST_FM,
            RadioMatch::Genre,
            self.mark(tracks),
            Vec::new(),
        ))
    }
}
