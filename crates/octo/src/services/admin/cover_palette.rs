//! The colors the dashboard's Afterglow look takes from a cover: three lights behind the glass
//! and an accent for the controls.
//!
//! The color logic is the station covers' own. A cover's main colors come from
//! [`cover_colors::swatches`], grey ones are left out by the same rule, and the first light and
//! the accent are the color [`CoverMusic::from_covers`](octo_media::cover::CoverMusic::from_covers)
//! would call the music's: so the dashboard, a station's cover and the apps agree on what color
//! an album is. Only the lightness and chroma are the dashboard's, chosen for dark glass and dark
//! words.
//!
//! Where the station covers would call a cover grey, the dashboard still takes a faint lean (a
//! teal night, a sepia photo) as quiet light in that hue, so every chosen song changes the page;
//! only a truly black, white or grey cover leaves the dashboard's own colors.

use octo_media::cover::cover_colors::{self, Lch, Swatch};

/// Three lights, strongest first, and the accent, all as `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub colors: Vec<String>,
    pub accent: String,
}

// CoverMusic's line between a color and a grey, and the lightness it trusts.
const COLORLESS: f64 = 0.035;
// Two lights at least this far apart on the hue circle, so the three are three colors.
const MIN_HUE_GAP: f64 = 35.0;
// A cover of one color still lights the page from three sides, from its neighbors.
const NEIGHBOR_TURN: f64 = 40.0;

// Lights: mid and vivid, so they read as color through dark glass.
const LIGHT_L: f64 = 0.62;
const LIGHT_C_MIN: f64 = 0.10;
const LIGHT_C_MAX: f64 = 0.20;
// Accent: light, so dark words sit on it and it reads as words on the dark page.
const ACCENT_L: f64 = 0.80;
const ACCENT_C_MIN: f64 = 0.08;
const ACCENT_C_MAX: f64 = 0.16;

/// The palette of a cover's swatches: None for no swatches, or for a cover with no color and no
/// lean.
pub fn from_swatches(swatches: Option<&[Swatch]>) -> Option<Palette> {
    let swatches = swatches.filter(|s| !s.is_empty())?;
    let seen: Vec<(Lch, f32)> = swatches
        .iter()
        .map(|s| (cover_colors::to_lch(s.argb), s.share))
        .collect();
    let mut ranked: Vec<(Lch, f64)> = seen
        .iter()
        .filter(|(lch, _)| lch.c >= COLORLESS && (0.15..=0.97).contains(&lch.l))
        // CoverMusic's measure of the strongest color: how much of the cover, how vivid.
        .map(|&(lch, share)| (lch, f64::from(share).sqrt() * (0.3 + lch.c * 5.0)))
        .collect();
    if ranked.is_empty() {
        return muted(&seen);
    }
    // A stable sort, highest score first: equal scores keep their order.
    ranked.sort_by(|x, y| y.1.total_cmp(&x.1));

    let mut picks: Vec<Lch> = Vec::new();
    for &(lch, _) in &ranked {
        if picks
            .iter()
            .any(|p| cover_colors::hue_distance(p.h, lch.h) < MIN_HUE_GAP)
        {
            continue;
        }
        picks.push(lch);
        if picks.len() == 3 {
            break;
        }
    }
    let first = picks[0];
    while picks.len() < 3 {
        let turn = if picks.len() == 1 {
            NEIGHBOR_TURN
        } else {
            -NEIGHBOR_TURN
        };
        picks.push(Lch {
            h: (first.h + turn + 360.0) % 360.0,
            ..first
        });
    }

    let colors = picks
        .iter()
        .map(|p| {
            hex(Lch {
                l: LIGHT_L,
                c: p.c.clamp(LIGHT_C_MIN, LIGHT_C_MAX),
                h: p.h,
            })
        })
        .collect();
    let accent = hex(Lch {
        l: ACCENT_L,
        c: first.c.clamp(ACCENT_C_MIN, ACCENT_C_MAX),
        h: first.h,
    });
    Some(Palette { colors, accent })
}

// Under CoverMusic's line a cover can still lean one way: a night sky that is nearly black but
// teal. Such a cover lights the page quietly in that hue, so choosing it still changes the
// page. Below this it is truly black, white or grey, and the dashboard keeps its own colors.
const TINTED: f64 = 0.012;
const MUTED_L: f64 = 0.55;
const MUTED_C_MIN: f64 = 0.035;
const MUTED_C_MAX: f64 = 0.06;
const MUTED_ACCENT_C: f64 = 0.06;

/// A grey cover's quiet palette from its own lean, or None for a cover with none.
fn muted(seen: &[(Lch, f32)]) -> Option<Palette> {
    // The first of the highest scores, or None when nothing qualifies.
    let mut lean: Option<(Lch, f64)> = None;
    for &(lch, share) in seen {
        if lch.c < TINTED || !(0.08..=0.97).contains(&lch.l) {
            continue;
        }
        let score = f64::from(share).sqrt() * lch.c;
        if lean.is_none_or(|(_, best)| score > best) {
            lean = Some((lch, score));
        }
    }
    let (first, _) = lean?;
    let c = (first.c * 1.6).clamp(MUTED_C_MIN, MUTED_C_MAX);
    let colors = [0.0, NEIGHBOR_TURN, -NEIGHBOR_TURN]
        .iter()
        .map(|turn| {
            hex(Lch {
                l: MUTED_L,
                c,
                h: (first.h + turn + 360.0) % 360.0,
            })
        })
        .collect();
    Some(Palette {
        colors,
        accent: hex(Lch {
            l: ACCENT_L,
            c: MUTED_ACCENT_C,
            h: first.h,
        }),
    })
}

fn hex(lch: Lch) -> String {
    let argb = cover_colors::from_lch(lch);
    format!(
        "#{:02x}{:02x}{:02x}",
        cover_colors::r(argb),
        cover_colors::g(argb),
        cover_colors::b(argb)
    )
}

#[cfg(test)]
#[path = "ambient_palette_tests.rs"]
mod tests;
