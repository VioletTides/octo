//! A baseline JPEG with 4:2:0 chroma: the luma at full size and the two chroma planes at half
//! size each way, for every JPEG but the list covers (the placeholder, a badged cover, a cropped
//! or shrunk picture). The `image` crate's encoder only writes 4:4:4, which makes those JPEGs
//! half as large again (17.6 KB against 11.8 KB for the 600 px placeholder) for no difference
//! anyone can see. List covers stay 4:4:4.
//!
//! The tables are the standard ones (ITU T.81 Annex K), scaled for the quality as libjpeg and
//! the `image` crate scale them.

use image::RgbImage;

#[rustfmt::skip]
const LUMA_QUANT: [u8; 64] = [
    16, 11, 10, 16,  24,  40,  51,  61,
    12, 12, 14, 19,  26,  58,  60,  55,
    14, 13, 16, 24,  40,  57,  69,  56,
    14, 17, 22, 29,  51,  87,  80,  62,
    18, 22, 37, 56,  68, 109, 103,  77,
    24, 35, 55, 64,  81, 104, 113,  92,
    49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103,  99,
];

#[rustfmt::skip]
const CHROMA_QUANT: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99,
    18, 21, 26, 66, 99, 99, 99, 99,
    24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
];

/// The natural-order index of each zigzag position.
#[rustfmt::skip]
const ZIGZAG: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

const LUMA_DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const CHROMA_DC_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

const LUMA_AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
#[rustfmt::skip]
const LUMA_AC_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

const CHROMA_AC_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
#[rustfmt::skip]
const CHROMA_AC_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// A Huffman table as (code length, code) for each symbol.
struct HuffTable {
    codes: [(u8, u16); 256],
}

impl HuffTable {
    /// The canonical codes of a table given as its code-length counts and symbols (T.81 C.2).
    fn new(bits: &[u8; 16], values: &[u8]) -> HuffTable {
        let mut codes = [(0u8, 0u16); 256];
        let mut code: u16 = 0;
        let mut k = 0;
        for (index, &count) in bits.iter().enumerate() {
            let length = index as u8 + 1;
            for _ in 0..count {
                codes[values[k] as usize] = (length, code);
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        HuffTable { codes }
    }
}

/// The entropy-coded bits, with a 0 stuffed after every 0xFF byte.
struct Bits {
    out: Vec<u8>,
    acc: u32,
    count: u8,
}

impl Bits {
    fn put(&mut self, length: u8, value: u16) {
        for shift in (0..length).rev() {
            self.acc = (self.acc << 1) | u32::from((value >> shift) & 1);
            self.count += 1;
            if self.count == 8 {
                let byte = self.acc as u8;
                self.out.push(byte);
                if byte == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.count = 0;
            }
        }
    }

    /// Fills the last byte with 1 bits.
    fn flush(&mut self) {
        if self.count > 0 {
            let missing = 8 - self.count;
            self.put(missing, (1u16 << missing) - 1);
        }
    }
}

/// The number of bits a coefficient's magnitude needs, and its bits as JPEG writes them (a
/// negative value as its one's complement).
fn magnitude(value: i32) -> (u8, u16) {
    let size = (32 - value.unsigned_abs().leading_zeros()) as u8;
    let bits = if value < 0 { value - 1 } else { value };
    let mask = if size == 0 { 0 } else { (1u32 << size) - 1 };
    (size, (bits as u32 & mask) as u16)
}

/// The quantization table for `quality`, in natural order: libjpeg's scaling of the standard
/// table, each value between 1 and 255.
fn scaled(table: &[u8; 64], quality: u8) -> [u16; 64] {
    let quality = u32::from(quality.clamp(1, 100));
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - quality * 2
    };
    let mut out = [0u16; 64];
    for (o, &v) in out.iter_mut().zip(table) {
        *o = ((u32::from(v) * scale + 50) / 100).clamp(1, 255) as u16;
    }
    out
}

/// The forward DCT of an 8x8 block of level-shifted samples (T.81 A.3.3), separable.
fn fdct(block: &[f32; 64]) -> [f32; 64] {
    use std::f32::consts::PI;
    let mut cos = [[0f32; 8]; 8];
    for (u, row) in cos.iter_mut().enumerate() {
        let c = if u == 0 {
            std::f32::consts::FRAC_1_SQRT_2
        } else {
            1.0
        };
        for (x, value) in row.iter_mut().enumerate() {
            *value = c * ((2 * x + 1) as f32 * u as f32 * PI / 16.0).cos() / 2.0;
        }
    }
    let mut rows = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            rows[y * 8 + u] = (0..8).map(|x| block[y * 8 + x] * cos[u][x]).sum();
        }
    }
    let mut out = [0f32; 64];
    for v in 0..8 {
        for u in 0..8 {
            out[v * 8 + u] = (0..8).map(|y| rows[y * 8 + u] * cos[v][y]).sum();
        }
    }
    out
}

struct Encoder {
    bits: Bits,
    luma_q: [u16; 64],
    chroma_q: [u16; 64],
    luma_dc: HuffTable,
    luma_ac: HuffTable,
    chroma_dc: HuffTable,
    chroma_ac: HuffTable,
}

impl Encoder {
    /// Quantizes and writes one block; returns its DC value for the next block's difference.
    fn block(&mut self, samples: &[f32; 64], previous_dc: i32, luma: bool) -> i32 {
        let coefficients = fdct(samples);
        let (quant, dc_table, ac_table) = if luma {
            (&self.luma_q, &self.luma_dc, &self.luma_ac)
        } else {
            (&self.chroma_q, &self.chroma_dc, &self.chroma_ac)
        };
        let mut zigzag = [0i32; 64];
        for (k, &natural) in ZIGZAG.iter().enumerate() {
            zigzag[k] = (coefficients[natural] / f32::from(quant[natural])).round() as i32;
        }
        let dc_codes = dc_table.codes;
        let ac_codes = ac_table.codes;

        let (size, value) = magnitude(zigzag[0] - previous_dc);
        let (length, code) = dc_codes[size as usize];
        self.bits.put(length, code);
        self.bits.put(size, value);

        let mut zeros = 0u8;
        for &coefficient in &zigzag[1..] {
            if coefficient == 0 {
                zeros += 1;
                continue;
            }
            while zeros >= 16 {
                let (length, code) = ac_codes[0xF0];
                self.bits.put(length, code);
                zeros -= 16;
            }
            let (size, value) = magnitude(coefficient);
            let (length, code) = ac_codes[usize::from((zeros << 4) | size)];
            self.bits.put(length, code);
            self.bits.put(size, value);
            zeros = 0;
        }
        if zeros > 0 {
            let (length, code) = ac_codes[0x00];
            self.bits.put(length, code);
        }
        zigzag[0]
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
}

fn huffman_segment(out: &mut Vec<u8>, class_and_id: u8, bits: &[u8; 16], values: &[u8]) {
    let mut body = vec![class_and_id];
    body.extend_from_slice(bits);
    body.extend_from_slice(values);
    segment(out, 0xC4, &body);
}

/// `image` as a baseline JPEG at `quality` with 4:2:0 chroma. None for an empty picture or one
/// larger than JPEG can say (65,535 pixels a side).
pub(crate) fn encode(image: &RgbImage, quality: u8) -> Option<Vec<u8>> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || width > 65_535 || height > 65_535 {
        return None;
    }
    let mut out = Vec::with_capacity((width * height / 4) as usize);
    out.extend_from_slice(&[0xFF, 0xD8]);
    // JFIF 1.01, no density.
    segment(
        &mut out,
        0xE0,
        &[b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0],
    );

    let luma_q = scaled(&LUMA_QUANT, quality);
    let chroma_q = scaled(&CHROMA_QUANT, quality);
    for (id, table) in [(0u8, &luma_q), (1u8, &chroma_q)] {
        let mut body = vec![id];
        body.extend(ZIGZAG.iter().map(|&natural| table[natural] as u8));
        segment(&mut out, 0xDB, &body);
    }

    let [w1, w0] = (width as u16).to_be_bytes();
    let [h1, h0] = (height as u16).to_be_bytes();
    segment(
        &mut out,
        0xC0,
        &[8, h1, h0, w1, w0, 3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1],
    );
    huffman_segment(&mut out, 0x00, &LUMA_DC_BITS, &DC_VALUES);
    huffman_segment(&mut out, 0x10, &LUMA_AC_BITS, &LUMA_AC_VALUES);
    huffman_segment(&mut out, 0x01, &CHROMA_DC_BITS, &DC_VALUES);
    huffman_segment(&mut out, 0x11, &CHROMA_AC_BITS, &CHROMA_AC_VALUES);
    segment(&mut out, 0xDA, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);

    // The planes, level-shifted, edges repeated out to whole 16 px blocks.
    let padded_w = width.div_ceil(16) * 16;
    let padded_h = height.div_ceil(16) * 16;
    let at = |x: u32, y: u32| image.get_pixel(x.min(width - 1), y.min(height - 1)).0;
    let ycc = |p: [u8; 3]| {
        let (r, g, b) = (f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
        (
            0.299 * r + 0.587 * g + 0.114 * b,
            -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0,
            0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0,
        )
    };

    let mut encoder = Encoder {
        bits: Bits {
            out,
            acc: 0,
            count: 0,
        },
        luma_q,
        chroma_q,
        luma_dc: HuffTable::new(&LUMA_DC_BITS, &DC_VALUES),
        luma_ac: HuffTable::new(&LUMA_AC_BITS, &LUMA_AC_VALUES),
        chroma_dc: HuffTable::new(&CHROMA_DC_BITS, &DC_VALUES),
        chroma_ac: HuffTable::new(&CHROMA_AC_BITS, &CHROMA_AC_VALUES),
    };
    let (mut dc_y, mut dc_cb, mut dc_cr) = (0, 0, 0);
    let mut luma = [[0f32; 64]; 4];
    let mut cb = [0f32; 64];
    let mut cr = [0f32; 64];
    for my in (0..padded_h).step_by(16) {
        for mx in (0..padded_w).step_by(16) {
            for dy in 0..16 {
                for dx in 0..16 {
                    let (y, _, _) = ycc(at(mx + dx, my + dy));
                    let block = (dy as usize / 8) * 2 + dx as usize / 8;
                    luma[block][(dy as usize % 8) * 8 + dx as usize % 8] = y - 128.0;
                }
            }
            // Each chroma sample is the mean of the 2x2 pixels it stands for.
            for cy in 0..8 {
                for cx in 0..8 {
                    let (mut sum_cb, mut sum_cr) = (0.0, 0.0);
                    for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (_, b, r) = ycc(at(mx + cx * 2 + ox, my + cy * 2 + oy));
                        sum_cb += b;
                        sum_cr += r;
                    }
                    cb[(cy * 8 + cx) as usize] = sum_cb / 4.0 - 128.0;
                    cr[(cy * 8 + cx) as usize] = sum_cr / 4.0 - 128.0;
                }
            }
            for block in &luma {
                dc_y = encoder.block(block, dc_y, true);
            }
            dc_cb = encoder.block(&cb, dc_cb, false);
            dc_cr = encoder.block(&cr, dc_cr, false);
        }
    }
    encoder.bits.flush();
    let mut out = encoder.bits.out;
    out.extend_from_slice(&[0xFF, 0xD9]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageReader, Rgb};
    use std::io::Cursor;

    fn decode(bytes: &[u8]) -> RgbImage {
        ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .expect("a format")
            .decode()
            .expect("a JPEG the image crate reads")
            .to_rgb8()
    }

    /// The root mean square difference of two pictures, per channel value.
    fn rms(a: &RgbImage, b: &RgbImage) -> f64 {
        let sum: f64 = a
            .as_raw()
            .iter()
            .zip(b.as_raw())
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum();
        (sum / a.as_raw().len() as f64).sqrt()
    }

    /// A cover-like picture: soft color gradients with a white disc on them (a sharp edge in
    /// brightness, as a logo or lettering has).
    fn picture(width: u32, height: u32) -> RgbImage {
        let (cx, cy, r) = (
            width as f64 / 2.0,
            height as f64 / 2.0,
            width.min(height) as f64 / 4.0,
        );
        RgbImage::from_fn(width, height, |x, y| {
            let inside = (x as f64 - cx).hypot(y as f64 - cy) < r;
            if inside {
                return Rgb([245, 245, 245]);
            }
            Rgb([
                (40 + x * 160 / width.max(1)) as u8,
                (30 + y * 120 / height.max(1)) as u8,
                (120 + (x + y) * 60 / (width + height).max(1)) as u8,
            ])
        })
    }

    #[test]
    fn the_picture_comes_back_with_its_chroma_at_half_size() {
        for (width, height) in [(600, 600), (160, 160), (37, 21), (1, 1), (17, 33)] {
            let original = picture(width, height);
            let bytes = encode(&original, 85).expect("encodes");
            let decoded = decode(&bytes);
            assert_eq!(decoded.dimensions(), (width, height));
            assert!(
                rms(&original, &decoded) < 6.0,
                "{width}x{height}: {}",
                rms(&original, &decoded)
            );
            // The frame header says 2x2 luma and 1x1 chroma.
            let sof = bytes
                .windows(2)
                .position(|w| w == [0xFF, 0xC0])
                .expect("a frame header");
            assert_eq!(
                &bytes[sof + 9..sof + 19],
                &[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]
            );
        }
    }

    #[test]
    fn it_is_smaller_than_the_image_crate_s_4_4_4_for_the_same_quality() {
        let original = picture(600, 600);
        let ours = encode(&original, 85).expect("encodes");
        let mut theirs = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut theirs, 85)
            .encode_image(&original)
            .expect("encodes");
        assert!(
            ours.len() * 10 < theirs.len() * 8,
            "4:2:0 {} bytes, 4:4:4 {} bytes",
            ours.len(),
            theirs.len()
        );
    }

    #[test]
    fn flat_colors_survive_exactly_enough_to_read_as_themselves() {
        for color in [[0, 0, 0], [255, 255, 255], [200, 30, 60], [16, 140, 220]] {
            let original = RgbImage::from_pixel(48, 48, Rgb(color));
            let decoded = decode(&encode(&original, 90).expect("encodes"));
            for (a, b) in decoded.get_pixel(20, 20).0.iter().zip(color) {
                assert!(
                    (i32::from(*a) - i32::from(b)).abs() <= 2,
                    "{color:?} came back {:?}",
                    decoded.get_pixel(20, 20)
                );
            }
        }
    }

    #[test]
    fn magnitudes_are_written_as_jpeg_writes_them() {
        assert_eq!(magnitude(0), (0, 0));
        assert_eq!(magnitude(1), (1, 1));
        assert_eq!(magnitude(-1), (1, 0));
        assert_eq!(magnitude(5), (3, 5));
        assert_eq!(magnitude(-5), (3, 2));
        assert_eq!(magnitude(1023), (10, 1023));
        assert_eq!(magnitude(-1023), (10, 0));
    }
}
