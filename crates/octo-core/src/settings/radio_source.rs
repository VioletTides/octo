//! `Octo.Models.Settings.RadioSourceSettings` (the `RadioSources` section).

use serde::{Deserialize, Serialize};

/// Where radio's suggestions come from besides Last.fm, and how much each counts. Song radio
/// (getSimilarSongs) and the stations ask every source that is on, and blend their answers.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct RadioSourceSettings {
    /// YouTube Music's radio for the song, through the yt-dlp shim. RADIO_YOUTUBE_MUSIC.
    #[serde(rename = "YouTubeMusic")]
    pub you_tube_music: bool,

    /// ListenBrainz's similar recordings, and LB Radio when Octo has a ListenBrainz token.
    /// RADIO_LISTENBRAINZ.
    pub listen_brainz: bool,

    /// Library songs that sound like the seed, from octo-sonic's analysis of every file. Off stops
    /// reading how songs sound; their transition profiles are still made. RADIO_SOUNDS_ALIKE.
    pub sounds_alike: bool,

    /// The octo-sonic sidecar. RADIO_SONIC_URL.
    pub sonic_url: String,

    /// Seconds between two songs in the analysis pass, so a library on a network mount is read
    /// gently. RADIO_SONIC_PAUSE_SECONDS.
    pub sonic_pause_seconds: i32,

    /// The ListenBrainz similarity dataset, as Navidrome uses. RADIO_LISTENBRAINZ_ALGORITHM.
    pub listen_brainz_algorithm: String,

    /// How far LB Radio reaches for artist and genre stations: "easy" keeps to the artist's
    /// closest neighbours and popular songs, "medium" and "hard" go further out.
    /// RADIO_LISTENBRAINZ_RADIO_MODE.
    pub listen_brainz_radio_mode: String,

    /// How much each source counts in the blend before learning, 0 to 3. 0 leaves a source out
    /// of the blend. Dashboard only. Reasoned starting points: YouTube Music has the most
    /// listening data and knows uploads; Last.fm is strong for songs it knows; ListenBrainz is
    /// precise but has fewer listeners; how a song sounds says tempo and timbre, not genre or
    /// mood, so alone it counts least.
    pub last_fm_weight: f64,
    #[serde(rename = "YouTubeMusicWeight")]
    pub you_tube_music_weight: f64,
    pub listen_brainz_weight: f64,
    pub sounds_alike_weight: f64,

    /// Let each listener's plays and skips move these weights (0.3 to 1.5).
    /// RADIO_LEARN_FROM_LISTENING.
    pub learn_from_listening: bool,
}

impl Default for RadioSourceSettings {
    fn default() -> Self {
        Self {
            you_tube_music: true,
            listen_brainz: true,
            sounds_alike: true,
            sonic_url: "http://octo-sonic:8080".to_string(),
            sonic_pause_seconds: 2,
            listen_brainz_algorithm:
                "session_based_days_9000_session_300_contribution_5_threshold_15_limit_50_skip_30".to_string(),
            listen_brainz_radio_mode: "easy".to_string(),
            last_fm_weight: 0.9,
            you_tube_music_weight: 1.0,
            listen_brainz_weight: 0.7,
            sounds_alike_weight: 0.4,
            learn_from_listening: true,
        }
    }
}

impl RadioSourceSettings {
    /// LB Radio's mode as it is asked: easy, medium or hard, any other value easy.
    pub fn effective_listen_brainz_radio_mode(&self) -> &'static str {
        let mode = self.listen_brainz_radio_mode.trim();
        ["medium", "hard"]
            .into_iter()
            .find(|known| known.eq_ignore_ascii_case(mode))
            .unwrap_or("easy")
    }

    pub fn effective_sonic_pause_seconds(&self) -> i32 {
        self.sonic_pause_seconds.clamp(0, 600)
    }

    /// A weight as the blend uses it: 0 to 3, and 1 for a value that is not a number.
    pub fn effective_weight(weight: f64) -> f64 {
        if weight.is_finite() {
            weight.clamp(0.0, 3.0)
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_are_clamped_and_a_broken_one_counts_as_one() {
        for (weight, expected) in [
            (0.9, 0.9),
            (-1.0, 0.0),
            (7.5, 3.0),
            (f64::NAN, 1.0),
            (f64::INFINITY, 1.0),
        ] {
            assert_eq!(
                RadioSourceSettings::effective_weight(weight),
                expected,
                "{weight}"
            );
        }
        let s = RadioSourceSettings {
            sonic_pause_seconds: 9999,
            ..Default::default()
        };
        assert_eq!(s.effective_sonic_pause_seconds(), 600);
        assert_eq!(RadioSourceSettings::default().effective_sonic_pause_seconds(), 2);
    }

    #[test]
    fn lb_radio_mode_is_easy_unless_medium_or_hard_is_chosen() {
        let mode = |value: &str| {
            RadioSourceSettings {
                listen_brainz_radio_mode: value.into(),
                ..Default::default()
            }
            .effective_listen_brainz_radio_mode()
        };
        assert_eq!(
            RadioSourceSettings::default().effective_listen_brainz_radio_mode(),
            "easy"
        );
        assert_eq!(mode(" Hard "), "hard");
        assert_eq!(mode("medium"), "medium");
        assert_eq!(mode("extreme"), "easy");
        assert_eq!(mode(""), "easy");
    }

    /// The config binder matches names ignoring case and underscores, so the PascalCase names
    /// (with the capital T of YouTube) bind.
    #[test]
    fn the_section_binds_by_its_pascal_case_names() {
        let mut tree = crate::config::ConfigTree::new();
        tree.set("RadioSources:YouTubeMusic", Some("false".into()));
        tree.set("RadioSources:youtubemusicweight", Some("2.5".into()));
        tree.set("RadioSources:SonicUrl", Some("http://sonic:9000".into()));
        let (bound, warnings) = crate::config::bind::<RadioSourceSettings>(&tree.section("RadioSources"));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!bound.you_tube_music);
        assert_eq!(bound.you_tube_music_weight, 2.5);
        assert_eq!(bound.sonic_url, "http://sonic:9000");
    }
}
