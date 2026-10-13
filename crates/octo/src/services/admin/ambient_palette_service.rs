//! `AmbientPaletteService`: the palettes of the covers in the fetched-songs log, for the
//! dashboard's light. A cover is read once and its palette kept for as long as Octo runs, so the
//! page asks as often as it likes.
//!
//! The browser cannot read these colors itself: the covers come from Deezer's servers, and a page
//! may only read the pixels of an image its own server sent. Octo fetches the cover, keeps only
//! the few colors, and never passes the image on.

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt;
use octo_media::cover::CoverArtService;
use parking_lot::Mutex;
use tracing::debug;

use super::cover_palette::{self, Palette};

pub struct AmbientPaletteService {
    http: reqwest::Client,
    // None is remembered too: a cover that failed or has no color is not fetched again.
    palettes: Mutex<HashMap<String, Option<Palette>>>,
}

impl AmbientPaletteService {
    /// The cover client's timeout.
    pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(6);

    // A cover is a few hundred kilobytes; anything far past that is not one.
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    const MAX_REMEMBERED: usize = 512;

    /// The production service, on its own 6 s client.
    pub fn new() -> Self {
        // The client does not decompress responses.
        let http = reqwest::Client::builder()
            .timeout(Self::CLIENT_TIMEOUT)
            .no_gzip()
            .no_deflate()
            .build()
            .expect("a plain HTTP client builds");
        Self::with_client(http)
    }

    pub fn with_client(http: reqwest::Client) -> Self {
        AmbientPaletteService {
            http,
            palettes: Mutex::new(HashMap::new()),
        }
    }

    /// The palette of the cover at `cover_url`, None for a cover that cannot be read or has no
    /// color. A caller that gives up drops the future.
    pub async fn palette_for(&self, cover_url: Option<&str>) -> Option<Palette> {
        let cover_url = cover_url.filter(|u| !u.trim().is_empty())?;
        if let Some(known) = self.palettes.lock().get(cover_url) {
            return known.clone();
        }
        // Only an absolute URL with an http or https scheme.
        let uri = url::Url::parse(cover_url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))?;

        let palette = match self.read(uri).await {
            Ok(bytes) if !bytes.is_empty() => {
                // The station covers' reading of a picture: its main colors from a small copy.
                let swatches = CoverArtService::swatches_of(&bytes);
                cover_palette::from_swatches(swatches.as_deref())
            }
            Ok(_) => None,
            Err(e) => {
                // A cover that cannot be read only costs the page its color; the dashboard keeps
                // its own.
                debug!("Could not read the colors of a fetched song's cover: {e}");
                None
            }
        };

        let mut palettes = self.palettes.lock();
        if palettes.len() >= Self::MAX_REMEMBERED {
            palettes.clear();
        }
        palettes.insert(cover_url.to_string(), palette.clone());
        palette
    }

    /// The picture's bytes, or none when the answer was not a success, said it was too large,
    /// or ran past the limit while it was read.
    async fn read(&self, uri: url::Url) -> Result<Vec<u8>, reqwest::Error> {
        let response = self.http.get(uri).send().await?;
        if !response.status().is_success()
            || response.content_length().unwrap_or(0) as usize > Self::MAX_BYTES
        {
            return Ok(Vec::new());
        }
        let mut limited = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if limited.len() + chunk.len() > Self::MAX_BYTES {
                limited.clear();
                break;
            }
            limited.extend_from_slice(&chunk);
        }
        Ok(limited)
    }
}

impl Default for AmbientPaletteService {
    fn default() -> Self {
        Self::new()
    }
}
