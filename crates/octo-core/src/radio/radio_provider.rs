//! Where a radio suggestion came from. The keys are stored with station tracks, so they never
//! change; the display names are what the apps show after "Suggested by".

pub const LAST_FM: &str = "lastfm";
pub const YOU_TUBE_MUSIC: &str = "youtube-music";
pub const LISTEN_BRAINZ: &str = "listenbrainz";
pub const SOUNDS_ALIKE: &str = "sounds-alike";
pub const LIBRARY: &str = "library";
/// A song from the listener's own plays, which no source suggested.
pub const HISTORY: &str = "history";

/// The sources that know songs beyond the library.
pub fn is_catalog(provider: &str) -> bool {
    matches!(provider, LAST_FM | YOU_TUBE_MUSIC | LISTEN_BRAINZ)
}

/// A provider's own key, as a station's source lists are prefixed with.
pub fn is_known(key: &str) -> bool {
    matches!(
        key,
        LAST_FM | YOU_TUBE_MUSIC | LISTEN_BRAINZ | SOUNDS_ALIKE | LIBRARY | HISTORY
    )
}

/// What the apps show; None for a song the listener played themselves.
pub fn display_name(provider: Option<&str>) -> Option<&'static str> {
    match provider? {
        LAST_FM => Some("Last.fm"),
        YOU_TUBE_MUSIC => Some("YouTube Music"),
        LISTEN_BRAINZ => Some("ListenBrainz"),
        SOUNDS_ALIKE => Some("Sounds alike"),
        LIBRARY => Some("Your library"),
        _ => None,
    }
}

/// The key behind a display name, for a song that carries only the name.
pub fn from_display_name(name: Option<&str>) -> Option<&'static str> {
    [LAST_FM, YOU_TUBE_MUSIC, LISTEN_BRAINZ, SOUNDS_ALIKE, LIBRARY]
        .into_iter()
        .find(|key| display_name(Some(key)) == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_keys_go_both_ways() {
        for key in [LAST_FM, YOU_TUBE_MUSIC, LISTEN_BRAINZ, SOUNDS_ALIKE, LIBRARY] {
            assert_eq!(from_display_name(display_name(Some(key))), Some(key), "{key}");
            assert!(is_known(key));
        }
        assert_eq!(display_name(Some(HISTORY)), None);
        assert_eq!(display_name(None), None);
        // A song with no name (the listener's own) has no key either: None matches no name.
        assert_eq!(from_display_name(None), None);
        assert!(is_known(HISTORY));
        assert!(!is_known("tag"));
        assert!(is_catalog(LISTEN_BRAINZ) && !is_catalog(SOUNDS_ALIKE) && !is_catalog(LIBRARY));
    }
}
