use super::*;
use crate::cover::cover_book::CoverBook;

const GLYPHS: [CoverGlyph; 3] = [CoverGlyph::Heart, CoverGlyph::Magnifier, CoverGlyph::Copies];

#[test]
fn every_glyph_sits_inside_its_centred_box() {
    let numbers = &CoverBook::shipped().glyphs;
    for glyph in GLYPHS {
        let drawn = drawing(glyph, 600, numbers);
        let [left, top, size] = drawn.box_;
        assert!((left - (600.0 - size) / 2.0).abs() < 0.01, "{glyph:?} is centred");
        assert!((size - 600.0 * numbers.box_ as f32).abs() < 0.01);
        assert!(!drawn.steps.is_empty());
        for step in &drawn.steps {
            let [x0, y0, x1, y1] = step.shape.bounds();
            assert!(x0 >= left - 0.5 && y0 >= top - 0.5, "{glyph:?} {step:?}");
            assert!(
                x1 <= left + size + 0.5 && y1 <= top + size + 0.5,
                "{glyph:?} {step:?}"
            );
            assert!(step.shape.path(0.0).is_some(), "{glyph:?} {step:?} draws");
        }
    }
}

#[test]
fn every_glyph_leaves_ink_in_the_middle_of_the_cover_and_none_at_its_corners() {
    let numbers = &CoverBook::shipped().glyphs;
    for glyph in GLYPHS {
        let drawn = drawing(glyph, 600, numbers);
        let layer = layer(&drawn, 600, 600, 0.0).expect("a layer");
        let inked = layer.pixels().iter().filter(|p| p.alpha() > 128).count();
        assert!(inked > 600 * 600 / 50, "{glyph:?} has ink: {inked}");
        assert_eq!(layer.pixel(5, 5).map(|p| p.alpha()), Some(0));
        assert_eq!(layer.pixel(594, 594).map(|p| p.alpha()), Some(0));
    }
}

#[test]
fn the_magnifier_ring_is_hollow_and_the_copies_keep_a_gap() {
    let numbers = &CoverBook::shipped().glyphs;
    let magnifier = drawing(CoverGlyph::Magnifier, 600, numbers);
    let ring = layer(&magnifier, 600, 600, 0.0).expect("a layer");
    let [left, top, size] = magnifier.box_;
    let centre = |v: f64, at: f32| (at + v as f32 * size) as u32;
    let (cx, cy) = (
        centre(numbers.magnifier.center[0], left),
        centre(numbers.magnifier.center[1], top),
    );
    assert_eq!(
        ring.pixel(cx, cy).map(|p| p.alpha()),
        Some(0),
        "the lens is clear"
    );

    let copies = drawing(CoverGlyph::Copies, 600, numbers);
    let squares = layer(&copies, 600, 600, 0.0).expect("a layer");
    let c = &numbers.copies;
    let [left, top, size] = copies.box_;
    // On the back square's bottom stroke: cut away just past the front square's right edge,
    // inked further along.
    let at = |share: f64, origin: f32| (origin + share as f32 * size) as u32;
    let stroke_y = at(c.back[1] + c.side, top);
    let in_gap = at(c.front[0] + c.side + c.gap / 2.0, left);
    let past_gap = at(c.front[0] + c.side + c.gap * 2.0, left);
    assert_eq!(
        squares.pixel(in_gap, stroke_y).map(|p| p.alpha()),
        Some(0),
        "the gap is clear"
    );
    assert_eq!(
        squares.pixel(past_gap, stroke_y).map(|p| p.alpha()),
        Some(255),
        "the back square's stroke goes on past the gap"
    );
    let front_mid = (
        (left + (c.front[0] + c.side / 2.0) as f32 * size) as u32,
        (top + (c.front[1] + c.side / 2.0) as f32 * size) as u32,
    );
    assert_eq!(
        squares.pixel(front_mid.0, front_mid.1).map(|p| p.alpha()),
        Some(255),
        "the front square is filled"
    );
}

#[test]
fn the_shadow_offsets_are_shares_of_the_side() {
    let numbers = &CoverBook::shipped().glyphs;
    let drawn = drawing(CoverGlyph::Heart, 1000, numbers);
    let expected: Vec<f32> = numbers
        .shadow
        .offsets
        .iter()
        .map(|o| *o as f32 * 1000.0)
        .collect();
    assert_eq!(drawn.shadow_offsets, expected);
    assert!((drawn.shadow_alpha - numbers.shadow.alpha as f32).abs() < 1e-6);
}
