use super::*;

#[test]
fn versions_of_one_song_come_off_the_end() {
    for variant in [
        "Hello - Live",
        "Hello (Live at the Royal Albert Hall)",
        "Hello (2011 Remaster)",
        "Hello - Remastered 2011",
        "Hello [Mono Version]",
        "Hello (Radio Edit)",
        "Hello (Stereo)",
        "Hello (Demo)",
        "Hello (Acoustic)",
        "Hello (Live) (Remastered)",
        "Hello – Live",
    ] {
        assert_eq!(strip_versions(variant), "Hello", "{variant}");
    }
}

#[test]
fn remixes_mixes_and_features_keep_their_whole_title() {
    for other in [
        "Hello (Skrillex Remix)",
        "Hello - Skrillex Remix",
        "Hello (Extended Mix)",
        "Hello (feat. Someone)",
        "Hello (Live Remix)",
        "Hello (Sped Up)",
        "Hello (Instrumental)",
    ] {
        assert_eq!(strip_versions(other), other, "{other}");
    }
}

#[test]
fn a_marker_that_is_the_whole_title_stays() {
    assert_eq!(strip_versions("(Live)"), "(Live)");
    assert_eq!(strip_versions("  Live Forever  "), "Live Forever");
    assert_eq!(strip_versions("Bury a Friend (acoustic)"), "Bury a Friend");
}

#[test]
fn a_bracket_in_the_middle_is_left_alone() {
    assert_eq!(strip_versions("Hello (Live) Again"), "Hello (Live) Again");
}
