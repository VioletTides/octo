//! Reads a public Spotify playlist or album from its link.

use std::sync::LazyLock;
use std::time::Duration;

use octo_core::common::dotnet::{is_blank, to_lower_invariant};
use regex::Regex;
use serde_json::Value;
use tracing::info;

use super::ImportTrack;
use super::spotify_web_api::{ImportKeys, SpotifyException, seconds_of, str_of};

/// What a public link held: its name, picture and songs, and whether that was all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedList {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub image_url: Option<String>,
    pub tracks: Vec<ImportTrack>,
    pub capped: bool,
}

static LINK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:open\.spotify\.com/(?:intl-[a-z-]+/|embed/)?|spotify:)(playlist|album)[/:]([A-Za-z0-9]{10,40})")
        .expect("the link pattern compiles")
});

static NEXT_DATA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<script id="__NEXT_DATA__" type="application/json">(.*?)</script>"#)
        .expect("the page pattern compiles")
});

/// Reads a public Spotify playlist or album from its link, with no sign-in, through the page
/// Spotify serves for embedding it in other sites. That page carries the first 100 songs, with
/// their title, artists and length (no album for a playlist), which is the most a link can give
/// without an account. Larger lists say so, and the account sign-in or an exported file reads
/// them whole.
pub struct SpotifyLinkReader {
    /// The named "spotify-embed" client (20 s, a browser-like user agent).
    http: reqwest::Client,
    base: String,
}

impl SpotifyLinkReader {
    pub const CLIENT_NAME: &str = "spotify-embed";
    /// What the embed page holds at most; a list this long may be longer.
    pub const EMBED_CAP: usize = 100;

    pub fn new(http: reqwest::Client) -> Self {
        Self::with_base(http, "https://open.spotify.com")
    }

    /// The same, against another address: tests point it at a local server.
    pub fn with_base(http: reqwest::Client, base: &str) -> Self {
        SpotifyLinkReader {
            http,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    /// The HTTP client: a 20 second timeout, `Mozilla/5.0 (compatible; Octo)`.
    pub fn client() -> reqwest::Client {
        crate::services::http_client_factory::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("Mozilla/5.0 (compatible; Octo)")
            .build()
            .expect("the spotify-embed client builds")
    }

    /// The kind (playlist or album) and id a link or URI names, or None.
    pub fn parse(link: Option<&str>) -> Option<(String, String)> {
        let link = link.filter(|l| !is_blank(l))?.trim();
        let found = LINK_PATTERN.captures(link)?;
        Some((to_lower_invariant(&found[1]), found[2].to_string()))
    }

    pub async fn read(&self, link: &str) -> anyhow::Result<LinkedList> {
        let Some((kind, id)) = Self::parse(Some(link)) else {
            return Err(SpotifyException::new("That is not a link to a Spotify playlist or album.").into());
        };
        let response = self
            .http
            .get(format!("{}/embed/{kind}/{id}", self.base))
            .send()
            .await?;
        let status = response.status().as_u16();
        if status == 404 {
            return Err(SpotifyException::with_status(
                format!(
                    "Spotify has no public {kind} at that link. A private playlist needs the Spotify sign-in."
                ),
                status,
            )
            .into());
        }
        if !response.status().is_success() {
            return Err(SpotifyException::with_status(
                format!("Spotify answered {status} for that link."),
                status,
            )
            .into());
        }
        let page = response.text().await?;
        let read = Self::parse_page(&kind, &id, &page).ok_or_else(|| {
            SpotifyException::new(format!(
                "Octo could not read the songs on that {kind}'s page. Spotify may have changed it."
            ))
        })?;
        info!(
            "Read {} songs from Spotify {kind} {id} by its link",
            read.tracks.len()
        );
        Ok(read)
    }

    /// The list the embed page describes, or None when it is not the page Octo knows.
    pub fn parse_page(kind: &str, id: &str, page: &str) -> Option<LinkedList> {
        let found = NEXT_DATA.captures(page)?;
        let root: Value = serde_json::from_str(&found[1]).ok()?;
        let entity = ["props", "pageProps", "state", "data", "entity"]
            .iter()
            .try_fold(&root, |at, step| at.as_object()?.get(*step))?;
        if !entity.is_object() {
            return None;
        }
        let name = str_of(entity, "name")
            .or_else(|| str_of(entity, "title"))
            .unwrap_or_else(|| format!("Spotify {kind}"));
        let image = entity
            .get("coverArt")
            .and_then(|cover| cover.get("sources"))
            .and_then(Value::as_array)
            .and_then(|sources| sources.iter().find_map(|source| str_of(source, "url")));
        let album = (kind == "album").then(|| name.clone());
        let mut tracks = Vec::new();
        if let Some(list) = entity.get("trackList").and_then(Value::as_array) {
            for row in list {
                let Some(title) = str_of(row, "title").filter(|t| !is_blank(t)) else {
                    continue;
                };
                if str_of(row, "entityType").is_some_and(|t| t != "track") {
                    continue;
                }
                // The subtitle is the credit, artists joined by commas (with a no-break space).
                let artist = str_of(row, "subtitle")
                    .unwrap_or_default()
                    .replace('\u{a0}', " ")
                    .trim()
                    .to_string();
                tracks.push(ImportTrack {
                    key: ImportKeys::for_spotify(str_of(row, "uri").as_deref())
                        .unwrap_or_else(|| ImportKeys::for_words(Some(&artist), Some(&title))),
                    title: title.trim().to_string(),
                    artist,
                    album: album.clone(),
                    seconds: seconds_of(row.get("duration")),
                    isrc: None,
                    library_id: None,
                });
            }
        }
        let capped = tracks.len() >= Self::EMBED_CAP;
        Some(LinkedList {
            kind: kind.to_string(),
            id: id.to_string(),
            name,
            image_url: image,
            tracks,
            capped,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn links_and_uris_are_read() {
        let cases = [
            (
                "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=abc",
                "playlist",
                "37i9dQZF1DXcBWIGoYBM5M",
            ),
            (
                "https://open.spotify.com/intl-de/album/4LH4d3cOWNNsVw41Gqt2kv",
                "album",
                "4LH4d3cOWNNsVw41Gqt2kv",
            ),
            (
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
                "playlist",
                "37i9dQZF1DXcBWIGoYBM5M",
            ),
            (
                "https://open.spotify.com/embed/playlist/37i9dQZF1DXcBWIGoYBM5M",
                "playlist",
                "37i9dQZF1DXcBWIGoYBM5M",
            ),
        ];
        for (link, kind, id) in cases {
            assert_eq!(
                SpotifyLinkReader::parse(Some(link)),
                Some((kind.to_string(), id.to_string())),
                "case {link}"
            );
        }
    }

    #[test]
    fn other_links_are_not_taken() {
        for link in [
            "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC",
            "https://example.com/playlist/37i9dQZF1DXcBWIGoYBM5M",
            "",
        ] {
            assert_eq!(SpotifyLinkReader::parse(Some(link)), None, "case {link}");
        }
    }

    /// An embed page of `count` songs, for tests.
    pub(crate) fn embed_page(count: usize, name: &str) -> String {
        let rows = (1..=count)
            .map(|i| {
                format!(
                    r#"{{"uri":"spotify:track:{i:022}","title":"Song {i}","subtitle":"Artist {i}, Guest","duration":200000,"entityType":"track"}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(
            r#"{{"props":{{"pageProps":{{"state":{{"data":{{"entity":{{"type":"playlist","name":"{name}","coverArt":{{"sources":[{{"url":"https://i.scdn.co/image/x"}}]}},"trackList":[{rows}]}}}}}}}}}}}}"#
        );
        format!(
            r#"<html><body><script id="__NEXT_DATA__" type="application/json">{json}</script></body></html>"#
        )
    }

    #[test]
    fn an_embed_page_gives_titles_artists_and_lengths() {
        let list = SpotifyLinkReader::parse_page("playlist", "p", &embed_page(3, "Road trip")).expect("read");
        assert_eq!(list.name, "Road trip");
        assert_eq!(list.image_url.as_deref(), Some("https://i.scdn.co/image/x"));
        assert_eq!(list.tracks.len(), 3);
        assert_eq!(list.tracks[0].artist, "Artist 1, Guest");
        assert_eq!(list.tracks[0].seconds, Some(200));
        assert_eq!(list.tracks[0].key, format!("spotify:{:022}", 1));
        assert!(!list.capped);
        assert!(
            SpotifyLinkReader::parse_page("playlist", "p", &embed_page(100, "Road trip"))
                .expect("read")
                .capped
        );
        assert!(SpotifyLinkReader::parse_page("playlist", "p", "<html>changed</html>").is_none());
    }

    #[tokio::test]
    async fn a_private_playlists_link_says_it_needs_the_sign_in() {
        let server = MockServer::builder().start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let reader = SpotifyLinkReader::with_base(SpotifyLinkReader::client(), &server.uri());
        let error = reader
            .read("https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M")
            .await
            .expect_err("refused");
        assert!(error.to_string().contains("sign-in"), "{error}");
        let calls = server.received_requests().await.expect("recording");
        assert_eq!(calls[0].url.path(), "/embed/playlist/37i9dQZF1DXcBWIGoYBM5M");
    }
}
