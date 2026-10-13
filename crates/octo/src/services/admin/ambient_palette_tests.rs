//! The dashboard's Afterglow light, a fetched song's cover read with the station covers' color
//! rules, and the OKLCH round trip it is drawn with.

use octo_core::models::download::DownloadHistoryEntry;
use octo_media::cover::CoverArtService;
use octo_media::cover::CoverMusic;
use octo_media::cover::cover_colors::{self, Lch, Swatch};

use super::from_swatches;
use crate::controllers::admin::ambient::newest_per_cover;

fn hex(text: &str) -> i32 {
    cover_colors::hex(text).expect("a hex color")
}

fn s(text: &str, share: f32) -> Swatch {
    Swatch {
        argb: hex(text),
        share,
    }
}

#[test]
fn from_lch_undoes_to_lch() {
    for case in ["#d9552b", "#1d3f8c", "#7fb35a", "#ffffff", "#000000"] {
        let argb = hex(case);
        let back = cover_colors::from_lch(cover_colors::to_lch(argb));
        assert!(
            (cover_colors::r(back) - cover_colors::r(argb)).abs() <= 1,
            "{case}"
        );
        assert!(
            (cover_colors::g(back) - cover_colors::g(argb)).abs() <= 1,
            "{case}"
        );
        assert!(
            (cover_colors::b(back) - cover_colors::b(argb)).abs() <= 1,
            "{case}"
        );
    }
}

/// A color no screen shows keeps its hue and loses chroma, never wraps to another color.
#[test]
fn from_lch_out_of_gamut_keeps_hue() {
    let wanted = Lch {
        l: 0.7,
        c: 0.4,
        h: 145.0,
    };
    let got = cover_colors::to_lch(cover_colors::from_lch(wanted));
    assert!(cover_colors::hue_distance(got.h, wanted.h) < 3.0, "hue {}", got.h);
    assert!((0.68..=0.72).contains(&got.l), "lightness {}", got.l);
    assert!(got.c < wanted.c);
}

/// The first light and the accent are the color the station covers call the music's.
#[test]
fn palette_leads_with_the_color_cover_music_picks() {
    let swatches = vec![s("#2b1a12", 0.55), s("#d9552b", 0.30), s("#1d3f8c", 0.15)];
    let music = CoverMusic::from_covers(std::slice::from_ref(&swatches)).expect("a color");
    let palette = from_swatches(Some(&swatches)).expect("a palette");

    assert_eq!(palette.colors.len(), 3);
    let lead = cover_colors::to_lch(hex(&palette.colors[0]));
    let accent = cover_colors::to_lch(hex(&palette.accent));
    let hue = f64::from(music.hue);
    assert!(
        cover_colors::hue_distance(lead.h, hue) < 4.0,
        "lead {} vs music {hue}",
        lead.h
    );
    assert!(
        cover_colors::hue_distance(accent.h, hue) < 4.0,
        "accent {} vs music {hue}",
        accent.h
    );
}

#[test]
fn palette_lights_are_distinct_hues() {
    let palette = from_swatches(Some(&[
        s("#d9552b", 0.4),
        s("#e0662f", 0.2),
        s("#1d3f8c", 0.2),
        s("#7fb35a", 0.2),
    ]))
    .expect("a palette");
    let hues: Vec<f64> = palette
        .colors
        .iter()
        .map(|c| cover_colors::to_lch(hex(c)).h)
        .collect();
    for i in 0..hues.len() {
        for j in i + 1..hues.len() {
            assert!(
                cover_colors::hue_distance(hues[i], hues[j]) >= 30.0,
                "{} vs {}",
                palette.colors[i],
                palette.colors[j]
            );
        }
    }
}

/// One color still gives three lights: its neighbors on the hue circle.
#[test]
fn palette_one_color_cover_fills_with_neighbors() {
    let palette = from_swatches(Some(&[s("#1d3f8c", 1.0)])).expect("a palette");
    let mut distinct = palette.colors.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 3);
}

/// A grey, black and white cover has no color, so the dashboard keeps its own.
#[test]
fn palette_gray_cover_is_null() {
    assert_eq!(
        from_swatches(Some(&[s("#111111", 0.6), s("#f0f0f0", 0.3), s("#808080", 0.1)])),
        None
    );
    assert_eq!(from_swatches(Some(&[])), None);
    assert_eq!(from_swatches(None), None);
}

/// A night sky that is nearly black but teal (Drake's NOKIA cover, read live) is grey to the
/// station covers, yet still lights the page: quietly, in its own teal.
#[test]
fn palette_faintly_tinted_cover_is_muted_in_its_own_hue() {
    let swatches = vec![
        s("#0b1f1e", 0.17),
        s("#e4efef", 0.17),
        s("#0a1515", 0.14),
        s("#2d322a", 0.12),
        s("#010709", 0.12),
    ];
    assert_eq!(CoverMusic::from_covers(std::slice::from_ref(&swatches)), None);
    let palette = from_swatches(Some(&swatches)).expect("a palette");
    let lead = cover_colors::to_lch(hex(&palette.colors[0]));
    assert!(cover_colors::hue_distance(lead.h, 191.0) < 12.0, "hue {}", lead.h);
    assert!((0.03..=0.065).contains(&lead.c), "chroma {}", lead.c);
    let mut distinct = palette.colors.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 3);
    let accent = hex(&palette.accent);
    assert!(cover_colors::contrast_ratio(accent, hex("#14080e")) >= 7.0);
}

/// The accent carries dark words on it and reads as words on the dark page.
#[test]
fn accent_is_readable_both_ways() {
    for cover in ["#d9552b", "#1d3f8c", "#f2d100", "#5a2a82"] {
        let accent = hex(&from_swatches(Some(&[s(cover, 1.0)])).expect("a palette").accent);
        assert!(
            cover_colors::contrast_ratio(accent, hex("#14080e")) >= 7.0,
            "dark words on the accent of {cover}"
        );
        assert!(
            cover_colors::contrast_ratio(accent, hex("#0a0a0f")) >= 7.0,
            "the accent on the page of {cover}"
        );
    }
}

/// An album fetched whole is one cover to choose from, and a song without a cover counts by its
/// album.
#[test]
fn newest_per_cover_one_entry_per_cover_newest_first() {
    fn e(title: &str, cover: Option<&str>, album: &str) -> DownloadHistoryEntry {
        DownloadHistoryEntry {
            artist: "Drake".to_string(),
            title: title.to_string(),
            album: Some(album.to_string()),
            cover_art_url: cover.map(str::to_string),
            ..Default::default()
        }
    }
    let log = vec![
        e("NOKIA", Some("https://x/nokia.jpg"), "A"),
        e("Make Them Know", Some("https://x/iceman.jpg"), "A"),
        e("Firm Friends", Some("https://x/iceman.jpg"), "A"),
        e("Digital Love", Some("https://x/discovery.jpg"), "A"),
        e("Don't Worry", Some("https://x/iceman.jpg"), "A"),
        e("No Art 1", None, "Loose"),
        e("No Art 2", None, "Loose"),
        e("Old", Some("https://x/old.jpg"), "A"),
    ];
    let titles =
        |picked: Vec<DownloadHistoryEntry>| -> Vec<String> { picked.into_iter().map(|e| e.title).collect() };
    assert_eq!(
        titles(newest_per_cover(log.clone(), 6)),
        ["NOKIA", "Make Them Know", "Digital Love", "No Art 1", "Old"]
    );
    assert_eq!(titles(newest_per_cover(log, 2)), ["NOKIA", "Make Them Know"]);
}

/// End to end from a real picture, through the station covers' reading of it.
#[test]
fn palette_from_a_picture() {
    let image = image::RgbaImage::from_fn(64, 64, |x, _| {
        if x < 40 {
            image::Rgba([0xd9, 0x55, 0x2b, 0xff])
        } else {
            image::Rgba([0x1d, 0x3f, 0x8c, 0xff])
        }
    });
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).expect("a PNG");
    let swatches = CoverArtService::swatches_of(png.get_ref());
    let palette = from_swatches(swatches.as_deref()).expect("a palette");
    let lead = cover_colors::to_lch(hex(&palette.colors[0]));
    assert!(cover_colors::hue_distance(lead.h, cover_colors::to_lch(hex("#d9552b")).h) < 4.0);
}

/// The stub cover (`parity/stubs/bodies/cover.jpg`) lights the page as the reference recording
/// shows it (`18-dashboard/ambient-seeded`).
#[test]
fn palette_of_the_stub_cover_matches_the_reference() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../parity/stubs/bodies/cover.jpg");
    let bytes = std::fs::read(path).expect("the parity cover");
    let swatches = CoverArtService::swatches_of(&bytes);
    let palette = from_swatches(swatches.as_deref()).expect("a palette");
    assert_eq!(palette.colors, ["#a967be", "#c85c87", "#727bdb"]);
    assert_eq!(palette.accent, "#e49ffa");
}
