//! How long and how wide one Soulseek search looks.

use crate::settings::SoulseekSettings;

/// How long and how wide one Soulseek search looks. slskd ends a search at whichever comes
/// first: SearchTimeoutMs with no new answer, ResponseLimit peers, or FileLimit files. The file
/// limit counts every format. Octo asked for 150 files, so a popular song's first wave of MP3s
/// ended the search before the FLAC answers arrived (#70). CeilingSeconds is Octo's own limit:
/// a search still running then is canceled, and slskd still hands over what it gathered.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SearchProfile {
    pub name: String,
    pub ceiling_seconds: i32,
    pub search_timeout_ms: i32,
    pub response_limit: i32,
    pub file_limit: i32,
}

/// How wide every song search reads, a star's and Better quality's alike, so the first
/// download sees what Better quality would. A star once asked for 500 files: "Drake Take Care"
/// filled that with 33 peers' answers in one second, none of them a usable FLAC, and the song
/// came from YouTube as an MP3; Better quality read 2,000 files from 117 peers and found the
/// FLAC at once. Reading 2,000 took the same second.
pub const SONG_RESPONSE_LIMIT: i32 = 500;

/// See [`SONG_RESPONSE_LIMIT`].
pub const SONG_FILE_LIMIT: i32 = 2_000;

impl SearchProfile {
    pub fn new(
        name: impl Into<String>,
        ceiling_seconds: i32,
        search_timeout_ms: i32,
        response_limit: i32,
        file_limit: i32,
    ) -> Self {
        SearchProfile {
            name: name.into(),
            ceiling_seconds,
            search_timeout_ms,
            response_limit,
            file_limit,
        }
    }

    /// A star or a play. Searched as wide as Better quality; somebody may be waiting, so the
    /// configured ceiling holds and a quiet search ends after 15 s without a new answer.
    pub fn interactive(s: &SoulseekSettings) -> Self {
        Self::new(
            "interactive",
            s.search_wait_seconds,
            15_000,
            SONG_RESPONSE_LIMIT,
            SONG_FILE_LIMIT,
        )
    }

    /// Better quality and the weekly upgrade. Nobody is waiting, so it waits longer for
    /// slow peers: the same width as a star, with more patience.
    pub fn upgrade(s: &SoulseekSettings) -> Self {
        Self::new(
            "upgrade",
            s.effective_upgrade_search_wait_seconds(),
            30_000,
            SONG_RESPONSE_LIMIT,
            SONG_FILE_LIMIT,
        )
    }

    /// An album heart's one search for the whole record. Each peer answers with many
    /// files, so the file limit is wide; nobody is waiting on one song, so it may look a little
    /// longer than a star does.
    pub fn album(s: &SoulseekSettings) -> Self {
        Self::new("album", s.search_wait_seconds.max(45), 20_000, 500, 3_000)
    }

    /// The same search asked again, wider, once a search filled its limit and nothing in it was
    /// usable: the first, fastest answers to a very popular song can all be lossy, and the lossless
    /// ones were still on their way when slskd stopped counting.
    pub fn wider(&self) -> Self {
        SearchProfile {
            name: format!("{}, wider", self.name),
            response_limit: self.response_limit * 2,
            file_limit: self.file_limit * 4,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each profile carries its own limits; that the search payload carries them is tested with
    /// `soulseek_client::search_payload`.
    #[test]
    fn each_profile_carries_its_limits() {
        let s = SoulseekSettings::default();
        let limits = |p: &SearchProfile| (p.search_timeout_ms, p.response_limit, p.file_limit);
        assert_eq!(limits(&SearchProfile::interactive(&s)), (15_000, 500, 2_000));
        assert_eq!(limits(&SearchProfile::upgrade(&s)), (30_000, 500, 2_000));
        assert_eq!(limits(&SearchProfile::album(&s)), (20_000, 500, 3_000));
        assert_eq!(
            (
                SearchProfile::interactive(&s).ceiling_seconds,
                SearchProfile::upgrade(&s).ceiling_seconds
            ),
            (30, 90)
        );
        let wide = SoulseekSettings {
            upgrade_search_wait_seconds: 9999,
            ..Default::default()
        };
        assert_eq!(SearchProfile::upgrade(&wide).ceiling_seconds, 300);
    }

    #[test]
    fn an_album_search_waits_at_least_45_seconds() {
        let short = SoulseekSettings {
            search_wait_seconds: 10,
            ..Default::default()
        };
        assert_eq!(SearchProfile::album(&short).ceiling_seconds, 45);
        let long = SoulseekSettings {
            search_wait_seconds: 60,
            ..Default::default()
        };
        assert_eq!(SearchProfile::album(&long).ceiling_seconds, 60);
        assert_eq!(SearchProfile::album(&long).name, "album");
    }
}
