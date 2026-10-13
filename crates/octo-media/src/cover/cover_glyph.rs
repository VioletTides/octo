//! The glyph a marked list's cover carries in place of words: a heart for Liked Songs, a
//! magnifier for Review, two overlapping squares for Duplicates. The shapes are the numbers in
//! cover-design.json "glyphs", the same ones the Octo apps draw from, laid out as shapes that are
//! added to the glyph or cut out of what is there so far.

use serde::Deserialize;

/// Which glyph a cover carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CoverGlyph {
    Heart,
    Magnifier,
    Copies,
}

impl CoverGlyph {
    /// A short name, for cache keys.
    pub fn name(self) -> &'static str {
        match self {
            CoverGlyph::Heart => "heart",
            CoverGlyph::Magnifier => "magnifier",
            CoverGlyph::Copies => "copies",
        }
    }
}

/// cover-design.json "glyphs". Points and widths are shares of the glyph's box, which is `box_`
/// of the cover's side, centred; the shadow's offsets are shares of the side.
#[derive(Clone, Debug, Deserialize)]
pub struct GlyphNumbers {
    #[serde(rename = "box")]
    pub box_: f64,
    pub veil_grow: f64,
    pub shadow: GlyphShadow,
    pub heart: HeartNumbers,
    pub magnifier: MagnifierNumbers,
    pub copies: CopiesNumbers,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GlyphShadow {
    pub alpha: f64,
    pub offsets: Vec<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HeartNumbers {
    pub from: [f64; 2],
    /// The cubics up the left side from `from`, each two control points and its end; the last
    /// ends at the top of the dip between the lobes.
    pub left: Vec<[[f64; 2]; 3]>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MagnifierNumbers {
    pub center: [f64; 2],
    pub radius: f64,
    pub stroke: f64,
    pub handle: [[f64; 2]; 2],
    pub handle_width: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CopiesNumbers {
    pub side: f64,
    pub radius: f64,
    pub back: [f64; 2],
    pub front: [f64; 2],
    pub stroke: f64,
    pub gap: f64,
}

/// One shape of a glyph, in the cover's pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum GlyphShape {
    Circle {
        x: f32,
        y: f32,
        radius: f32,
    },
    RoundSquare {
        left: f32,
        top: f32,
        side: f32,
        radius: f32,
    },
    /// A line `width` wide with round ends.
    Capsule {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        width: f32,
    },
    /// A closed outline from (x, y) through cubics: two control points and the end each.
    Curves {
        x: f32,
        y: f32,
        cubics: Vec<[f32; 6]>,
    },
}

impl GlyphShape {
    /// Left, top, right, bottom.
    pub fn bounds(&self) -> [f32; 4] {
        match self {
            GlyphShape::Circle { x, y, radius } => [x - radius, y - radius, x + radius, y + radius],
            GlyphShape::RoundSquare { left, top, side, .. } => [*left, *top, left + side, top + side],
            GlyphShape::Capsule {
                x0,
                y0,
                x1,
                y1,
                width,
            } => {
                let r = width / 2.0;
                [x0.min(*x1) - r, y0.min(*y1) - r, x0.max(*x1) + r, y0.max(*y1) + r]
            }
            GlyphShape::Curves { x, y, cubics } => {
                let xs = std::iter::once(*x).chain(cubics.iter().flat_map(|c| [c[0], c[2], c[4]]));
                let ys = std::iter::once(*y).chain(cubics.iter().flat_map(|c| [c[1], c[3], c[5]]));
                let (mut out, mut first) = ([0.0; 4], true);
                for (px, py) in xs.zip(ys) {
                    if first {
                        out = [px, py, px, py];
                        first = false;
                    }
                    out = [out[0].min(px), out[1].min(py), out[2].max(px), out[3].max(py)];
                }
                out
            }
        }
    }

    /// The shape as a path, moved down by `dy` pixels.
    pub fn path(&self, dy: f32) -> Option<tiny_skia::Path> {
        let mut builder = tiny_skia::PathBuilder::new();
        match self {
            GlyphShape::Circle { x, y, radius } => builder.push_circle(*x, y + dy, *radius),
            GlyphShape::RoundSquare {
                left,
                top,
                side,
                radius,
            } => round_square(&mut builder, *left, top + dy, *side, *radius),
            GlyphShape::Capsule {
                x0,
                y0,
                x1,
                y1,
                width,
            } => {
                let mut line = tiny_skia::PathBuilder::new();
                line.move_to(*x0, y0 + dy);
                line.line_to(*x1, y1 + dy);
                let stroke = tiny_skia::Stroke {
                    width: *width,
                    line_cap: tiny_skia::LineCap::Round,
                    ..Default::default()
                };
                return line.finish()?.stroke(&stroke, 1.0);
            }
            GlyphShape::Curves { x, y, cubics } => {
                builder.move_to(*x, y + dy);
                for c in cubics {
                    builder.cubic_to(c[0], c[1] + dy, c[2], c[3] + dy, c[4], c[5] + dy);
                }
                builder.close();
            }
        }
        builder.finish()
    }
}

/// A rounded square, its corners quarter circles drawn as cubics.
fn round_square(builder: &mut tiny_skia::PathBuilder, left: f32, top: f32, side: f32, radius: f32) {
    let r = radius.clamp(0.0, side / 2.0);
    let (right, bottom) = (left + side, top + side);
    // How far a cubic's control point sits from the corner's ends to draw a quarter circle.
    let k = r * (1.0 - 0.552_284_8);
    builder.move_to(left + r, top);
    builder.line_to(right - r, top);
    builder.cubic_to(right - k, top, right, top + k, right, top + r);
    builder.line_to(right, bottom - r);
    builder.cubic_to(right, bottom - k, right - k, bottom, right - r, bottom);
    builder.line_to(left + r, bottom);
    builder.cubic_to(left + k, bottom, left, bottom - k, left, bottom - r);
    builder.line_to(left, top + r);
    builder.cubic_to(left, top + k, left + k, top, left + r, top);
    builder.close();
}

/// One shape laid on the glyph: added to it, or cut out of what is there so far.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphStep {
    pub shape: GlyphShape,
    pub add: bool,
}

/// A glyph ready to paint: its steps, its box (left, top, side), and its shadow's offsets in
/// pixels and opacity.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphDrawing {
    pub glyph: CoverGlyph,
    pub steps: Vec<GlyphStep>,
    pub box_: [f32; 3],
    pub shadow_offsets: Vec<f32>,
    pub shadow_alpha: f32,
}

/// Where a glyph's shapes go on a cover `side` pixels square.
pub fn drawing(glyph: CoverGlyph, side: i32, numbers: &GlyphNumbers) -> GlyphDrawing {
    let s = side as f32;
    let size = s * numbers.box_ as f32;
    let left = (s - size) / 2.0;
    let top = (s - size) / 2.0;
    let x = |v: f64| left + v as f32 * size;
    let y = |v: f64| top + v as f32 * size;
    let d = |v: f64| v as f32 * size;
    let steps = match glyph {
        CoverGlyph::Heart => {
            let h = &numbers.heart;
            // Up the left side, then each cubic mirrored and run backwards down the right side:
            // a reversed cubic swaps its control points and ends where it began.
            let starts: Vec<[f64; 2]> = std::iter::once(h.from)
                .chain(h.left.iter().map(|c| c[2]))
                .collect();
            let up = h
                .left
                .iter()
                .map(|[c1, c2, end]| [x(c1[0]), y(c1[1]), x(c2[0]), y(c2[1]), x(end[0]), y(end[1])]);
            let down = h.left.iter().zip(&starts).rev().map(|([c1, c2, _], start)| {
                [
                    x(1.0 - c2[0]),
                    y(c2[1]),
                    x(1.0 - c1[0]),
                    y(c1[1]),
                    x(1.0 - start[0]),
                    y(start[1]),
                ]
            });
            vec![GlyphStep {
                shape: GlyphShape::Curves {
                    x: x(h.from[0]),
                    y: y(h.from[1]),
                    cubics: up.chain(down).collect(),
                },
                add: true,
            }]
        }
        CoverGlyph::Magnifier => {
            let m = &numbers.magnifier;
            let (cx, cy) = (x(m.center[0]), y(m.center[1]));
            vec![
                GlyphStep {
                    shape: GlyphShape::Circle {
                        x: cx,
                        y: cy,
                        radius: d(m.radius + m.stroke / 2.0),
                    },
                    add: true,
                },
                GlyphStep {
                    shape: GlyphShape::Circle {
                        x: cx,
                        y: cy,
                        radius: d(m.radius - m.stroke / 2.0),
                    },
                    add: false,
                },
                GlyphStep {
                    shape: GlyphShape::Capsule {
                        x0: x(m.handle[0][0]),
                        y0: y(m.handle[0][1]),
                        x1: x(m.handle[1][0]),
                        y1: y(m.handle[1][1]),
                        width: d(m.handle_width),
                    },
                    add: true,
                },
            ]
        }
        CoverGlyph::Copies => {
            let c = &numbers.copies;
            let half = c.stroke / 2.0;
            let square = |at: [f64; 2], grow: f64| GlyphShape::RoundSquare {
                left: x(at[0] - grow),
                top: y(at[1] - grow),
                side: d(c.side + 2.0 * grow),
                radius: d((c.radius + grow).max(0.0)),
            };
            vec![
                GlyphStep {
                    shape: square(c.back, half),
                    add: true,
                },
                GlyphStep {
                    shape: square(c.back, -half),
                    add: false,
                },
                GlyphStep {
                    shape: square(c.front, c.gap),
                    add: false,
                },
                GlyphStep {
                    shape: square(c.front, 0.0),
                    add: true,
                },
            ]
        }
    };
    GlyphDrawing {
        glyph,
        steps,
        box_: [left, top, size],
        shadow_offsets: numbers.shadow.offsets.iter().map(|o| *o as f32 * s).collect(),
        shadow_alpha: numbers.shadow.alpha as f32,
    }
}

/// The glyph's coverage on a clear layer `width` by `height`, moved down by `dy` pixels: added
/// shapes in white, cut shapes cleared.
pub fn layer(drawing: &GlyphDrawing, width: u32, height: u32, dy: f32) -> Option<tiny_skia::Pixmap> {
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    for step in &drawing.steps {
        let Some(path) = step.shape.path(dy) else { continue };
        let mut paint = tiny_skia::Paint {
            anti_alias: true,
            ..Default::default()
        };
        if step.add {
            paint.set_color_rgba8(255, 255, 255, 255);
        } else {
            paint.set_color_rgba8(0, 0, 0, 255);
            paint.blend_mode = tiny_skia::BlendMode::Clear;
        }
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );
    }
    Some(pixmap)
}

/// Paints the glyph onto `pixmap`: its soft shadow, black at the shadow's opacity once at each
/// offset, then the glyph in white.
pub fn paint(pixmap: &mut tiny_skia::Pixmap, drawing: &GlyphDrawing) {
    let (width, height) = (pixmap.width(), pixmap.height());
    let Some(full) = tiny_skia::Rect::from_xywh(0.0, 0.0, width as f32, height as f32) else {
        return;
    };
    let mut fill = |dy: f32, color: tiny_skia::Color| {
        let Some(layer) = layer(drawing, width, height, dy) else {
            return;
        };
        let mask = tiny_skia::Mask::from_pixmap(layer.as_ref(), tiny_skia::MaskType::Alpha);
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(color);
        pixmap.fill_rect(full, &paint, tiny_skia::Transform::identity(), Some(&mask));
    };
    let shadow = tiny_skia::Color::from_rgba(0.0, 0.0, 0.0, drawing.shadow_alpha.clamp(0.0, 1.0))
        .unwrap_or(tiny_skia::Color::BLACK);
    for offset in drawing.shadow_offsets.clone() {
        fill(offset, shadow);
    }
    fill(0.0, tiny_skia::Color::WHITE);
}

#[cfg(test)]
#[path = "cover_glyph_tests.rs"]
mod tests;
