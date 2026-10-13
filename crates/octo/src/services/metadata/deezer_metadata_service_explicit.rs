//! The Deezer catalog lookups behind the library's explicit marking, on the background lane.

use async_trait::async_trait;
use octo_core::common::SongIdentity;
use octo_core::common::dotnet::{escape_data_string, is_blank};
use octo_core::json::element::{ElementResult, get_int64, str_prop, try_get_property};
use octo_core::metadata::deezer_metadata_service::{CatalogTrackAnswer, CatalogTrackFacts};
use octo_core::tagging::explicit_lookup::IExplicitCatalog;
use serde_json::Value;
use tracing::debug;

use super::{DeezerMetadataService, TRACK_SEARCHES, all_matches, catalog_explicit, int};

/// How many hits a search for the explicit lookup reads: enough for the explicit original and
/// its clean edit to both be seen, which five often is not.
const EXPLICIT_SEARCH_HITS: usize = 15;

impl DeezerMetadataService {
    /// The catalog's track for this ISRC, or none when it has none. In the background lane,
    /// for a library-wide job that must never crowd out a search someone is waiting on.
    pub async fn track_by_isrc(&self, isrc: &str) -> CatalogTrackAnswer {
        let Some(code) = SongIdentity::normalize_isrc(isrc) else {
            return CatalogTrackAnswer::default();
        };
        let r = self
            .get_json(
                &format!("{}/track/isrc:{}", self.base(), escape_data_string(&code)),
                true,
            )
            .await;
        if r.transient {
            return CatalogTrackAnswer::busy();
        }
        let Some(root @ Value::Object(_)) = r.doc.as_ref() else {
            return CatalogTrackAnswer::default();
        };
        match facts(root) {
            Ok(Some(track)) => CatalogTrackAnswer::found(vec![track]),
            Ok(None) => CatalogTrackAnswer::default(),
            Err(e) => {
                debug!("deezer track by ISRC {code} could not be read: {e}");
                CatalogTrackAnswer::default()
            }
        }
    }

    /// Every track of a search that names this song (the guard the tagger's lookups use: a
    /// positive match on artist or title and a contradiction on neither), from the first way of
    /// writing it that finds any. The caller judges them strictly. In the background lane.
    pub async fn search_track_facts(&self, artist: Option<&str>, title: Option<&str>) -> CatalogTrackAnswer {
        let (artist, title) = (artist.unwrap_or(""), title.unwrap_or(""));
        if is_blank(artist) && is_blank(title) {
            return CatalogTrackAnswer::default();
        }
        for variant in SongIdentity::query_variants(title, artist)
            .into_iter()
            .take(TRACK_SEARCHES)
        {
            let url = format!(
                "{}/search?q={}&limit={EXPLICIT_SEARCH_HITS}",
                self.base(),
                escape_data_string(&variant.text())
            );
            let r = self.get_json(&url, true).await;
            if r.transient {
                return CatalogTrackAnswer::busy();
            }
            let hits = all_matches(r.doc.as_ref(), artist, title, None).and_then(|hits| {
                hits.iter()
                    .map(facts)
                    .collect::<ElementResult<Vec<_>>>()
                    .map(|found| found.into_iter().flatten().collect::<Vec<_>>())
            });
            match hits {
                Ok(hits) if !hits.is_empty() => return CatalogTrackAnswer::found(hits),
                Ok(_) => {}
                Err(e) => debug!("deezer explicit search '{artist} - {title}' could not be read: {e}"),
            }
        }
        CatalogTrackAnswer::default()
    }
}

/// One catalog track's facts, or None without an id or a title.
fn facts(t: &Value) -> ElementResult<Option<CatalogTrackFacts>> {
    let id = match try_get_property(t, "id")? {
        Some(tid @ Value::Number(_)) => Some(get_int64(tid)?.to_string()),
        _ => None,
    };
    let title = str_prop(t, "title")?;
    let (Some(id), Some(title)) = (id, title.filter(|title| !is_blank(title))) else {
        return Ok(None);
    };
    let artist = match try_get_property(t, "artist")? {
        Some(a @ Value::Object(_)) => str_prop(a, "name")?,
        _ => None,
    };
    let album = match try_get_property(t, "album")? {
        Some(al @ Value::Object(_)) => str_prop(al, "title")?,
        _ => None,
    };
    Ok(Some(CatalogTrackFacts {
        track_id: id,
        title: title.to_string(),
        artist: artist.unwrap_or("").to_string(),
        album: album.map(str::to_string),
        duration: int(t, "duration")?,
        isrc: str_prop(t, "isrc")?.map(str::to_string),
        explicit_content: catalog_explicit(t)?,
    }))
}

/// The explicit lookup's catalog in the app: Deezer.
pub struct DeezerExplicitCatalog {
    deezer: std::sync::Arc<DeezerMetadataService>,
}

impl DeezerExplicitCatalog {
    pub fn new(deezer: std::sync::Arc<DeezerMetadataService>) -> Self {
        Self { deezer }
    }
}

#[async_trait]
impl IExplicitCatalog for DeezerExplicitCatalog {
    async fn by_isrc(&self, isrc: &str) -> CatalogTrackAnswer {
        self.deezer.track_by_isrc(isrc).await
    }

    async fn search(&self, artist: Option<&str>, title: Option<&str>) -> CatalogTrackAnswer {
        self.deezer.search_track_facts(artist, title).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use octo_core::settings::{AppSettings, SettingsStore};
    use wiremock::matchers::{path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::services::metadata::{DeezerRateLimitHandler, DeezerRateLimiter};

    async fn service(server: &MockServer) -> DeezerMetadataService {
        let settings = Arc::new(SettingsStore::for_tests(AppSettings::default()));
        let http = Arc::new(DeezerRateLimitHandler::new(Arc::new(DeezerRateLimiter::new())));
        DeezerMetadataService::with_base_url(http, settings, server.uri())
    }

    #[tokio::test]
    async fn the_isrc_lookup_reads_the_tracks_explicit_word() {
        let server = MockServer::start().await;
        Mock::given(path("/track/isrc:USUM71702791"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":9,"title":"AfricAryaN","duration":200,"isrc":"USUM71702791","explicit_content_lyrics":1,
                    "artist":{"name":"Logic"},"album":{"title":"Everybody"}}"#,
            ))
            .mount(&server)
            .await;

        let answer = service(&server).await.track_by_isrc("usum71702791").await;

        assert!(!answer.did_not_answer);
        assert_eq!(
            answer.tracks,
            [CatalogTrackFacts {
                track_id: "9".into(),
                title: "AfricAryaN".into(),
                artist: "Logic".into(),
                album: Some("Everybody".into()),
                duration: Some(200),
                isrc: Some("USUM71702791".into()),
                explicit_content: Some(1),
            }]
        );
    }

    #[tokio::test]
    async fn the_search_keeps_every_hit_of_the_song_and_reads_fifteen() {
        let server = MockServer::start().await;
        Mock::given(path("/search"))
            .and(query_param("limit", "15"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"data":[
                    {"id":4,"title":"squabble up","duration":157,"explicit_content_lyrics":1,"artist":{"name":"Kendrick Lamar"}},
                    {"id":5,"title":"squabble up","duration":157,"explicit_content_lyrics":3,"artist":{"name":"Kendrick Lamar"}},
                    {"id":6,"title":"Other","duration":100,"artist":{"name":"Someone"}}]}"#,
            ))
            .mount(&server)
            .await;

        let answer = service(&server)
            .await
            .search_track_facts(Some("Kendrick Lamar"), Some("squabble up"))
            .await;

        let ids: Vec<(&str, Option<i32>)> = answer
            .tracks
            .iter()
            .map(|track| (track.track_id.as_str(), track.explicit_content))
            .collect();
        assert_eq!(ids, [("4", Some(1)), ("5", Some(3))]);
    }
}
