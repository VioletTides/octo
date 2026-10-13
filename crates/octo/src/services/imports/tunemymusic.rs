//! The services a person can bring their music from, through TuneMyMusic's free export to a
//! file: each one's page on tunemymusic.com, and the words on the tile to pick there, since the
//! page does not pick the service by itself. The file it makes comes back through a file import.

use serde::Serialize;

/// One service TuneMyMusic exports from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportService {
    /// TuneMyMusic's name for it in its addresses.
    pub id: &'static str,
    /// The name people know it by.
    pub name: &'static str,
    /// The words on TuneMyMusic's "Select the source" tile.
    pub tile: &'static str,
}

/// Every service, in the order the apps and pages list them.
pub const SERVICES: &[ExportService] = &[
    ExportService {
        id: "spotify",
        name: "Spotify",
        tile: "Spotify",
    },
    ExportService {
        id: "apple-music",
        name: "Apple Music",
        tile: "Apple Music",
    },
    ExportService {
        id: "youtube-music",
        name: "YouTube Music",
        tile: "YouTube Music",
    },
    ExportService {
        id: "amazon-music",
        name: "Amazon Music",
        tile: "Amazon Music",
    },
    ExportService {
        id: "tidal",
        name: "TIDAL",
        tile: "TIDAL",
    },
    ExportService {
        id: "deezer",
        name: "Deezer",
        tile: "Deezer",
    },
    ExportService {
        id: "qobuz",
        name: "Qobuz",
        tile: "Qobuz",
    },
    ExportService {
        id: "soundcloud",
        name: "SoundCloud",
        tile: "SoundCloud",
    },
    ExportService {
        id: "pandora",
        name: "Pandora",
        tile: "Pandora",
    },
    ExportService {
        id: "youtube",
        name: "YouTube",
        tile: "YouTube",
    },
];

impl ExportService {
    /// TuneMyMusic's page that exports this service's music to a file.
    pub fn export_url(&self) -> String {
        format!("https://www.tunemymusic.com/transfer/{}-to-file", self.id)
    }
}

/// One service as the apps and pages read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    pub id: &'static str,
    pub name: &'static str,
    pub export_url: String,
    pub tile: &'static str,
}

/// Where a person can bring their music from: every export service, and whether the Spotify
/// sign-in that keeps a list up to date is set up on this server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportServices {
    pub services: Vec<ServiceView>,
    pub spotify_connect: bool,
}

impl ImportServices {
    /// The list, with `spotify_connect` true when a Spotify client ID is set.
    pub fn new(spotify_client_id: &str) -> Self {
        ImportServices {
            services: SERVICES
                .iter()
                .map(|service| ServiceView {
                    id: service.id,
                    name: service.name,
                    export_url: service.export_url(),
                    tile: service.tile,
                })
                .collect(),
            spotify_connect: !spotify_client_id.trim().is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_in_order_with_its_export_page() {
        let services = ImportServices::new("");
        let ids: Vec<&str> = services.services.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            [
                "spotify",
                "apple-music",
                "youtube-music",
                "amazon-music",
                "tidal",
                "deezer",
                "qobuz",
                "soundcloud",
                "pandora",
                "youtube"
            ]
        );
        for service in &services.services {
            assert_eq!(
                service.export_url,
                format!("https://www.tunemymusic.com/transfer/{}-to-file", service.id)
            );
            assert!(!service.name.is_empty() && !service.tile.is_empty());
        }
        assert!(!services.spotify_connect);
        assert!(ImportServices::new(" client-1 ").spotify_connect);
        assert!(!ImportServices::new("   ").spotify_connect);
    }

    #[test]
    fn the_answer_reads_as_the_contract_says() {
        let json = serde_json::to_value(ImportServices::new("client-1")).expect("json");
        assert_eq!(
            json["services"][1],
            serde_json::json!({
                "id": "apple-music",
                "name": "Apple Music",
                "exportUrl": "https://www.tunemymusic.com/transfer/apple-music-to-file",
                "tile": "Apple Music"
            })
        );
        assert_eq!(json["spotifyConnect"], true);
    }
}
