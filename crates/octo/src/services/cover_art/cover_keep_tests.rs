//! The cover keep judgment: the cover a song arrives with is judged before the chain replaces
//! it. The same art at 1000 px or more stays, a small, blown-up, wrong or off-square one goes,
//! and a smaller cover of the same art stays unless the chain finds one much larger. The
//! resolver's side of it is tested in `download_cover_resolver_tests.rs`.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{Rgb, RgbImage};

use super::*;

/// A small seeded generator, so the same seed draws the same picture (only "same seed, same
/// picture" matters here).
struct Seeded(u64);

impl Seeded {
    fn new(seed: u64) -> Self {
        // splitmix64 of the seed, never zero.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Seeded((z ^ (z >> 31)) | 1)
    }

    fn next_double(&mut self) -> f64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn byte(&mut self) -> u8 {
        (self.next_double() * 256.0) as u8
    }
}

fn fill(image: &mut RgbImage, x: f64, y: f64, w: f64, h: f64, color: [u8; 3]) {
    let (width, height) = (image.width(), image.height());
    let x0 = x.max(0.0).round() as u32;
    let y0 = y.max(0.0).round() as u32;
    let x1 = ((x + w).round().max(0.0) as u32).min(width);
    let y1 = ((y + h).round().max(0.0) as u32).min(height);
    for py in y0..y1 {
        for px in x0..x1 {
            image.put_pixel(px, py, Rgb(color));
        }
    }
}

fn encode(image: &RgbImage) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut out, 75)
        .encode_image(image)
        .expect("encodes");
    out.into_inner()
}

fn draw(seed: u64, side: u32, height: Option<u32>) -> RgbImage {
    let height = height.unwrap_or(side);
    let (w, h) = (f64::from(side), f64::from(height));
    let mut random = Seeded::new(seed);
    let mut image = RgbImage::from_pixel(side, height, Rgb([20, 20, 20]));
    for _ in 0..60 {
        let x = random.next_double() * w;
        let y = random.next_double() * h;
        let rw = random.next_double() * w / 3.0 + 4.0;
        let rh = random.next_double() * h / 3.0 + 4.0;
        let color = [random.byte(), random.byte(), random.byte()];
        fill(&mut image, x, y, rw, rh, color);
    }
    // Fine lines, the lettering a real cover has and a blown-up copy loses.
    for _ in 0..400 {
        let x = random.next_double() * w;
        let y = random.next_double() * h;
        fill(&mut image, x, y, w / 300.0 + 1.0, w / 60.0, [255, 255, 255]);
    }
    image
}

/// Artwork with real edges, drawn natively at `side`: the same seed is the same picture at any
/// size, another seed another picture.
pub(crate) fn art(seed: u64, side: u32) -> Vec<u8> {
    encode(&draw(seed, side, None))
}

pub(crate) fn art_sized(seed: u64, width: u32, height: u32) -> Vec<u8> {
    encode(&draw(seed, width, Some(height)))
}

/// The picture drawn small, then stretched: what a peer's upscaled thumbnail is.
pub(crate) fn blown_up_art(seed: u64, from: u32, to: u32) -> Vec<u8> {
    let small = draw(seed, from, None);
    encode(&image::imageops::resize(&small, to, to, FilterType::CatmullRom))
}

fn catalog() -> Vec<u8> {
    art(1, 1000)
}

fn judge_plain(own: &[u8], reference: Option<&[u8]>, require_square: bool) -> OwnCoverJudgement {
    judge(own, reference, require_square, false, false)
}

// ---- The judgment ----------------------------------------------------------------------

#[test]
fn judge_the_same_art_at1400_is_kept() {
    let verdict = judge_plain(&art(1, 1400), Some(&catalog()), true);

    assert_eq!(verdict.verdict, OwnCoverVerdict::Keep);
    assert_eq!(verdict.same_art, Some(true));
    assert_eq!(verdict.side(), 1400);
}

#[test]
fn judge_the_same_art_under1000_is_weighed() {
    let verdict = judge_plain(&art(1, 600), Some(&catalog()), true);

    assert_eq!(verdict.verdict, OwnCoverVerdict::Weigh);
    assert_eq!(verdict.same_art, Some(true));
}

#[test]
fn judge_a_different_picture_is_replaced_however_large() {
    let verdict = judge_plain(&art(2, 1400), Some(&catalog()), true);

    assert_eq!(verdict.verdict, OwnCoverVerdict::Replace);
    assert_eq!(verdict.same_art, Some(false));
    assert!(verdict.why.contains("different picture"), "{}", verdict.why);
}

#[test]
fn judge_a_small_picture_blown_up_is_replaced() {
    let verdict = judge_plain(&blown_up_art(1, 200, 1400), Some(&catalog()), true);

    assert_eq!(verdict.verdict, OwnCoverVerdict::Replace);
    assert!(verdict.why.contains("blown up"), "{}", verdict.why);
}

#[test]
fn detail_a_blown_up_copy_holds_far_less_than_the_real_art_at_the_same_size() {
    let real = cover_image::detail(&art(1, 1400), 800).expect("measured");
    let blown = cover_image::detail(&blown_up_art(1, 200, 1400), 800).expect("measured");

    assert!(
        blown < real * BLOWN_UP_RATIO,
        "blown up {blown:.2}, real {real:.2}"
    );
    assert!(!blown_up(&art(1, 1400), 1400, &catalog()));
}

#[test]
fn judge_not_square_is_replaced_only_while_video_covers_are_replaced() {
    let wide = art_sized(1, 1600, 900);

    assert_eq!(
        judge_plain(&wide, Some(&catalog()), true).verdict,
        OwnCoverVerdict::Replace
    );
    assert_ne!(judge_plain(&wide, None, false).verdict, OwnCoverVerdict::Replace);
}

#[test]
fn judge_the_art_of_the_album_it_was_filed_away_from_is_replaced() {
    let verdict = judge(&art(1, 1400), Some(&catalog()), true, true, false);

    assert_eq!(verdict.verdict, OwnCoverVerdict::Replace);
    assert!(verdict.why.contains("filed under before"), "{}", verdict.why);
}

#[test]
fn judge_the_library_copys_own_picture_is_kept_even_when_it_is_another_picture() {
    let verdict = judge(&art(2, 1400), Some(&catalog()), true, false, true);

    assert_eq!(verdict.verdict, OwnCoverVerdict::Keep);
}

#[test]
fn judge_with_nothing_to_compare_with_is_weighed() {
    let verdict = judge_plain(&art(1, 1400), None, true);

    assert_eq!(verdict.verdict, OwnCoverVerdict::Weigh);
    assert_eq!(verdict.same_art, None);
}

#[test]
fn judge_a_tiny_thumbnail_is_replaced() {
    assert_eq!(
        judge_plain(&art(1, 120), Some(&catalog()), true).verdict,
        OwnCoverVerdict::Replace
    );
}

#[test]
fn is_much_larger_needs_a_quarter_more_side() {
    for (found, own, expected) in [(1000, 800, true), (1000, 900, false), (3000, 600, true)] {
        assert_eq!(is_much_larger(found, own), expected, "found {found}, own {own}");
    }
}
