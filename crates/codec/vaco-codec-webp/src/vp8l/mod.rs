//! Native VP8L (WebP lossless) codec: full decode, and a real (if
//! deliberately simple) encoder.
//!
//! See the sibling modules for the pieces: [`bitio`] (LSB-first bit
//! packing), [`huffman`]/[`prefix`] (canonical Huffman, including the
//! length-transmission RLE), [`transform`] (the four reversible pixel
//! transforms), [`codes`]/[`distance_map`] (the LZ77 length/distance prefix
//! arithmetic), [`lz`] (this crate's own match finder).
//!
//! # What this crate's encoder emits, and why that is still a fully valid file
//!
//! Every one of VP8L's transforms, the color cache, and the multi-group
//! meta-prefix mechanism are independently optional (spec §4, §5.2.3,
//! §6.2.2) — a compliant decoder must handle a file using none of them,
//! since the spec defines that case directly rather than as a corner no real
//! encoder hits. This crate's own encoder writes exactly that: one
//! subtract-green transform (free — no side data, and it never hurts), a
//! single prefix-code group, literal-only Huffman coding for anything
//! [`lz::Matcher`] does not turn into a backward reference, and no color
//! cache. What it does not do — predictor/color/palette transforms, the
//! color cache, multiple meta-prefix groups, an optimal (rather than
//! greedy, single-candidate) LZ77 parse — only affects file size, never
//! correctness: verified by decoding this crate's own output with
//! `dwebp`/`ffmpeg -c:v libwebp` (D6), and by decoding real
//! `cwebp -lossless` output (which uses every one of those freely) with
//! this crate's own [`decode`].
//!
//! # Pixel representation
//!
//! Internally, a decoded/pre-encode image is a row-major `Vec<u32>` of
//! packed `0xAARRGGBB` pixels, matching spec §1's own bit layout exactly so
//! every transform formula in [`transform`] is a direct transcription.

mod bitio;
mod codes;
mod distance_map;
mod huffman;
mod lz;
mod prefix;
mod transform;

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use bitio::{BitReaderLsb, BitWriterLsb};
use huffman::HuffmanTable;
use prefix::{read_prefix_code, write_prefix_code};

const GREEN_LITERALS: usize = 256;
const LENGTH_CODES: usize = 24;
const DISTANCE_CODES: usize = 40;

/// A decoded VP8L image.
#[derive(Debug)]
pub(crate) struct DecodedImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) alpha_is_used: bool,
    pub(crate) pixels: Vec<u32>,
}

#[allow(
    clippy::integer_division,
    reason = "row/column from a linear pixel position; the truncation is the point"
)]
fn row_col(pos: usize, width: usize) -> (usize, usize) {
    if width == 0 {
        return (0, 0);
    }
    (pos % width, pos / width)
}

fn div_round_up_shift(n: u32, shift: u32) -> u32 {
    let unit = 1u32 << shift;
    n.saturating_add(unit - 1) >> shift
}

fn insert_cache(cache: &mut [u32], bits: u32, color: u32) {
    if cache.is_empty() {
        return;
    }
    let idx = color.wrapping_mul(0x1e35_a7bd) >> (32 - bits);
    if let Some(slot) = cache.get_mut(idx as usize) {
        *slot = color;
    }
}

/// Decode one VP8L image stream (spec §7.3): `spatially-coded-image` at the
/// top level, `entropy-coded-image` for every sub-image role (predictor,
/// color, color-indexing table, and the entropy image itself). The only
/// difference is whether the meta-prefix mechanism is even consulted.
fn decode_image_stream(
    r: &mut BitReaderLsb<'_>,
    budget: &mut Budget,
    width: u32,
    height: u32,
    is_top_level: bool,
) -> Result<Vec<u32>> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("vp8l: zero-sized image stream"));
    }
    budget.check_frame(width, height, 4)?;

    let has_cache = r.read_bit() == 1;
    let cache_bits = if has_cache {
        let b = r.read_bits(4);
        if !(1..=11).contains(&b) {
            return Err(Error::InvalidData("vp8l: bad color cache size"));
        }
        Some(b)
    } else {
        None
    };
    let cache_size = cache_bits.map_or(0u32, |b| 1u32 << b);

    let (num_groups, entropy_image, prefix_bits, prefix_image_width) =
        if is_top_level && r.read_bit() == 1 {
            let prefix_bits = r.read_bits(3) + 2;
            let prefix_image_width = div_round_up_shift(width, prefix_bits);
            let prefix_image_height = div_round_up_shift(height, prefix_bits);
            let entropy =
                decode_image_stream(r, budget, prefix_image_width, prefix_image_height, false)?;
            let max_code = entropy
                .iter()
                .map(|&p| (p >> 8) & 0xffff)
                .max()
                .unwrap_or(0);
            (
                max_code.saturating_add(1),
                Some(entropy),
                prefix_bits,
                prefix_image_width,
            )
        } else {
            (1u32, None, 0u32, 0u32)
        };
    if num_groups == 0 || num_groups > (1 << 16) {
        return Err(Error::InvalidData("vp8l: too many prefix code groups"));
    }

    let mut groups: Vec<[HuffmanTable; 5]> = Vec::new();
    for _ in 0..num_groups {
        let green_alphabet = GREEN_LITERALS + LENGTH_CODES + cache_size as usize;
        let g = read_prefix_code(r, green_alphabet, budget)?;
        let red = read_prefix_code(r, 256, budget)?;
        let blue = read_prefix_code(r, 256, budget)?;
        let alpha = read_prefix_code(r, 256, budget)?;
        let dist = read_prefix_code(r, DISTANCE_CODES, budget)?;
        groups.push([g, red, blue, alpha, dist]);
    }

    let width_usize = width as usize;
    let total = width_usize.saturating_mul(height as usize);
    let mut out: Vec<u32> = budget.alloc(total)?;
    let mut cache: Vec<u32> = if let Some(b) = cache_bits {
        budget.alloc(1usize << b)?
    } else {
        Vec::new()
    };

    let mut pos: usize = 0;
    while pos < total {
        if r.overran() {
            return Err(Error::UnexpectedEof);
        }
        let group_idx = match &entropy_image {
            Some(entropy) => {
                let (x, y) = row_col(pos, width_usize);
                let bx = x >> prefix_bits;
                let by = y >> prefix_bits;
                let eidx = by
                    .saturating_mul(prefix_image_width as usize)
                    .saturating_add(bx);
                let code = entropy.get(eidx).copied().unwrap_or(0);
                ((code >> 8) & 0xffff) as usize
            }
            None => 0,
        };
        let Some(five) = groups.get(group_idx) else {
            return Err(Error::InvalidData("vp8l: meta prefix code out of range"));
        };
        let s = five[0].decode(r);
        if (s as usize) < GREEN_LITERALS {
            let red_v = five[1].decode(r);
            let blue_v = five[2].decode(r);
            let alpha_v = five[3].decode(r);
            let pixel = (alpha_v << 24) | (red_v << 16) | (s << 8) | blue_v;
            if let Some(slot) = out.get_mut(pos) {
                *slot = pixel;
            }
            if let Some(bits) = cache_bits {
                insert_cache(&mut cache, bits, pixel);
            }
            pos += 1;
        } else if (s as usize) < GREEN_LITERALS + LENGTH_CODES {
            let length_code = s - GREEN_LITERALS as u32;
            let length = codes::prefix_to_value(length_code, r) as usize;
            let dist_sym = five[4].decode(r);
            let dist_code_value = codes::prefix_to_value(dist_sym, r);
            let dist = codes::distance_code_to_dist(dist_code_value, width);
            if dist < 1 {
                return Err(Error::InvalidData("vp8l: non-positive backward distance"));
            }
            let dist = dist as usize;
            if dist > pos {
                return Err(Error::InvalidData(
                    "vp8l: backward reference before start of image",
                ));
            }
            let src_start = pos - dist;
            for i in 0..length {
                if pos + i >= total {
                    break;
                }
                let val = out.get(src_start + i).copied().unwrap_or(0);
                if let Some(slot) = out.get_mut(pos + i) {
                    *slot = val;
                }
                if let Some(bits) = cache_bits {
                    insert_cache(&mut cache, bits, val);
                }
            }
            pos += length;
        } else {
            let idx = (s as usize).saturating_sub(GREEN_LITERALS + LENGTH_CODES);
            let val = cache.get(idx).copied().unwrap_or(0);
            if let Some(slot) = out.get_mut(pos) {
                *slot = val;
            }
            if let Some(bits) = cache_bits {
                insert_cache(&mut cache, bits, val);
            }
            pos += 1;
        }
    }
    Ok(out)
}

struct TransformInfo {
    ty: u8,
    size_bits: u32,
    sub: Vec<u32>,
    sub_width: usize,
    width_bits: u32,
}

/// Decode the transform list (spec §4), applying each inverse against
/// `pixels` in place once every transform's own side data has been read.
/// Returns the working width the ARGB image was actually decoded at (a
/// color-indexing transform can shrink it).
fn read_transforms(
    r: &mut BitReaderLsb<'_>,
    budget: &mut Budget,
    width: u32,
    height: u32,
) -> Result<(Vec<TransformInfo>, u32)> {
    let mut transforms = Vec::new();
    let mut cur_width = width;
    let mut seen = [false; 4];
    while r.read_bit() == 1 {
        let ty = r.read_bits(2) as u8;
        let Some(slot) = seen.get_mut(ty as usize) else {
            return Err(Error::InvalidData("vp8l: bad transform type"));
        };
        if *slot {
            return Err(Error::InvalidData("vp8l: transform used more than once"));
        }
        *slot = true;
        match ty {
            transform::PREDICTOR | transform::COLOR => {
                let size_bits = r.read_bits(3) + 2;
                let tw = div_round_up_shift(cur_width, size_bits);
                let th = div_round_up_shift(height, size_bits);
                let sub = decode_image_stream(r, budget, tw, th, false)?;
                transforms.push(TransformInfo {
                    ty,
                    size_bits,
                    sub,
                    sub_width: tw as usize,
                    width_bits: 0,
                });
            }
            transform::SUBTRACT_GREEN => {
                transforms.push(TransformInfo {
                    ty,
                    size_bits: 0,
                    sub: Vec::new(),
                    sub_width: 0,
                    width_bits: 0,
                });
            }
            transform::COLOR_INDEXING => {
                let color_table_size = r.read_bits(8) + 1;
                let raw = decode_image_stream(r, budget, color_table_size, 1, false)?;
                let mut table: Vec<u32> = budget.alloc(raw.len())?;
                let mut acc = [0i32; 4];
                for (slot, &px) in table.iter_mut().zip(raw.iter()) {
                    acc[0] = (acc[0] + i32::from((px >> 24) as u8)) & 0xff;
                    acc[1] = (acc[1] + i32::from((px >> 16) as u8)) & 0xff;
                    acc[2] = (acc[2] + i32::from((px >> 8) as u8)) & 0xff;
                    acc[3] = (acc[3] + i32::from(px as u8)) & 0xff;
                    *slot = ((acc[0] as u32) << 24)
                        | ((acc[1] as u32) << 16)
                        | ((acc[2] as u32) << 8)
                        | (acc[3] as u32);
                }
                let width_bits = match color_table_size {
                    0..=2 => 3,
                    3..=4 => 2,
                    5..=16 => 1,
                    _ => 0,
                };
                cur_width = div_round_up_shift(cur_width, width_bits);
                transforms.push(TransformInfo {
                    ty,
                    size_bits: 0,
                    sub: table,
                    sub_width: color_table_size as usize,
                    width_bits,
                });
            }
            _ => return Err(Error::InvalidData("vp8l: bad transform type")),
        }
    }
    Ok((transforms, cur_width))
}

fn apply_inverse_transforms(
    pixels: &mut Vec<u32>,
    transforms: &[TransformInfo],
    decode_width: u32,
    real_width: u32,
    height: u32,
) {
    let mut working_width = decode_width as usize;
    for t in transforms.iter().rev() {
        match t.ty {
            transform::SUBTRACT_GREEN => transform::inverse_subtract_green(pixels),
            transform::PREDICTOR => {
                transform::inverse_predictor(
                    pixels,
                    working_width,
                    height as usize,
                    &t.sub,
                    t.size_bits,
                    t.sub_width,
                );
            }
            transform::COLOR => {
                transform::inverse_color(
                    pixels,
                    working_width,
                    height as usize,
                    &t.sub,
                    t.size_bits,
                    t.sub_width,
                );
            }
            transform::COLOR_INDEXING => {
                *pixels = transform::inverse_color_indexing(
                    pixels,
                    working_width,
                    real_width as usize,
                    height as usize,
                    &t.sub,
                    t.width_bits,
                );
                working_width = real_width as usize;
            }
            _ => {}
        }
    }
}

/// Decode a full VP8L stream, `payload` being the `VP8L` RIFF chunk's data
/// (leading `0x2f` signature byte included).
///
/// # Errors
///
/// [`Error::InvalidData`] for a bad signature, version, or any structural
/// violation (bad color cache size, transform used twice, backward
/// reference before the start of the image, ...). [`Error::UnexpectedEof`]
/// for a truncated stream. [`Error::LimitExceeded`] when the image or any
/// intermediate buffer would exceed `budget`.
pub(crate) fn decode(payload: &[u8], budget: &mut Budget) -> Result<DecodedImage> {
    let Some(&sig) = payload.first() else {
        return Err(Error::UnexpectedEof);
    };
    if sig != 0x2f {
        return Err(Error::InvalidData("vp8l: bad signature byte"));
    }
    let mut r = BitReaderLsb::new(payload.get(1..).unwrap_or(&[]));
    let width = r.read_bits(14) + 1;
    let height = r.read_bits(14) + 1;
    let alpha_is_used = r.read_bits(1) == 1;
    let version = r.read_bits(3);
    if version != 0 {
        return Err(Error::InvalidData("vp8l: unsupported version"));
    }

    let (transforms, decode_width) = read_transforms(&mut r, budget, width, height)?;
    let mut pixels = decode_image_stream(&mut r, budget, decode_width, height, true)?;
    if r.overran() {
        return Err(Error::UnexpectedEof);
    }
    apply_inverse_transforms(&mut pixels, &transforms, decode_width, width, height);

    Ok(DecodedImage {
        width,
        height,
        alpha_is_used,
        pixels,
    })
}

/// Encode `pixels` (`width * height` packed `0xAARRGGBB` values, row-major)
/// as a full VP8L stream (leading `0x2f` signature byte through the last
/// image-data bit, zero-padded to a byte).
///
/// # Errors
///
/// [`Error::InvalidData`] if `width`/`height` do not fit VP8L's 14-bit
/// dimensions, or `pixels.len() != width * height`.
pub(crate) fn encode(
    pixels: &[u32],
    width: u32,
    height: u32,
    alpha_is_used: bool,
    budget: &mut Budget,
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > (1 << 14) || height > (1 << 14) {
        return Err(Error::InvalidData("vp8l: image dimensions out of range"));
    }
    if pixels.len() != (width as usize).saturating_mul(height as usize) {
        return Err(Error::InvalidData("vp8l: pixel buffer size mismatch"));
    }

    let mut w = BitWriterLsb::new();
    w.write_bits(width - 1, 14);
    w.write_bits(height - 1, 14);
    w.write_bits(u32::from(alpha_is_used), 1);
    w.write_bits(0, 3); // version

    // One transform: subtract-green. Free (no side data) and it never hurts.
    w.write_bit(1);
    w.write_bits(u32::from(transform::SUBTRACT_GREEN), 2);
    w.write_bit(0); // no further transforms

    let mut working: Vec<u32> = budget.alloc(pixels.len())?;
    working.copy_from_slice(pixels);
    transform::forward_subtract_green(&mut working);

    encode_image_stream(&mut w, &working, budget)?;

    let mut bytes = w.finish();
    bytes.insert(0, 0x2f);
    Ok(bytes)
}

enum Token {
    Literal(u32),
    Match { length: u32, distance: u32 },
}

/// This crate's own encode path for `spatially-coded-image` (spec §7.3):
/// no color cache, a single prefix-code group, and whatever
/// [`lz::Matcher`] finds turned into backward references — everything else
/// literal. See the module doc for why that is still fully valid.
///
/// Takes no width/height: every distance this crate's own encoder ever
/// writes uses the "literal scan-order offset" code (`distance + 120`,
/// decoded via `distance_code - 120`), which — unlike the 2D neighbourhood
/// codes `1..=120` — does not need the image width to interpret.
fn encode_image_stream(w: &mut BitWriterLsb, pixels: &[u32], budget: &mut Budget) -> Result<()> {
    w.write_bit(0); // no color cache
    w.write_bit(0); // single meta-prefix group

    let mut tokens: Vec<Token> = Vec::new();
    let mut matcher = lz::Matcher::new(budget)?;
    let mut pos = 0usize;
    while pos < pixels.len() {
        if let Some(m) = matcher.find_and_insert(pixels, pos) {
            tokens.push(Token::Match {
                length: m.length as u32,
                distance: m.distance as u32,
            });
            pos += m.length;
        } else {
            let p = pixels.get(pos).copied().unwrap_or(0);
            tokens.push(Token::Literal(p));
            pos += 1;
        }
    }

    let green_alphabet = GREEN_LITERALS + LENGTH_CODES;
    let mut freq_green = vec![0u64; green_alphabet];
    let mut freq_red = vec![0u64; 256];
    let mut freq_blue = vec![0u64; 256];
    let mut freq_alpha = vec![0u64; 256];
    let mut freq_dist = vec![0u64; DISTANCE_CODES];
    for t in &tokens {
        match t {
            Token::Literal(p) => {
                if let Some(slot) = freq_green.get_mut(((p >> 8) & 0xff) as usize) {
                    *slot += 1;
                }
                if let Some(slot) = freq_red.get_mut(((p >> 16) & 0xff) as usize) {
                    *slot += 1;
                }
                if let Some(slot) = freq_blue.get_mut((p & 0xff) as usize) {
                    *slot += 1;
                }
                if let Some(slot) = freq_alpha.get_mut(((p >> 24) & 0xff) as usize) {
                    *slot += 1;
                }
            }
            Token::Match { length, distance } => {
                let (lcode, _, _) = codes::value_to_prefix(*length);
                if let Some(slot) = freq_green.get_mut(GREEN_LITERALS + lcode as usize) {
                    *slot += 1;
                }
                let (dcode, _, _) = codes::value_to_prefix(distance + 120);
                if let Some(slot) = freq_dist.get_mut(dcode as usize) {
                    *slot += 1;
                }
            }
        }
    }

    let green_table = write_prefix_code(w, &freq_green, green_alphabet)?;
    let red_table = write_prefix_code(w, &freq_red, 256)?;
    let blue_table = write_prefix_code(w, &freq_blue, 256)?;
    let alpha_table = write_prefix_code(w, &freq_alpha, 256)?;
    let dist_table = write_prefix_code(w, &freq_dist, DISTANCE_CODES)?;

    for t in &tokens {
        match t {
            Token::Literal(p) => {
                green_table.write(w, ((p >> 8) & 0xff) as usize);
                red_table.write(w, ((p >> 16) & 0xff) as usize);
                blue_table.write(w, (p & 0xff) as usize);
                alpha_table.write(w, ((p >> 24) & 0xff) as usize);
            }
            Token::Match { length, distance } => {
                let (lcode, lextra, lbits) = codes::value_to_prefix(*length);
                green_table.write(w, GREEN_LITERALS + lcode as usize);
                w.write_bits(lextra, lbits);
                let (dcode, dextra, dbits) = codes::value_to_prefix(distance + 120);
                dist_table.write(w, dcode as usize);
                w.write_bits(dextra, dbits);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn checker(width: u32, height: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let g = (x * 7 + y * 13) % 256;
                let r = (x * 3) % 256;
                let b = (y * 5) % 256;
                let a = 255u32;
                out.push((a << 24) | (r << 16) | (g << 8) | b);
            }
        }
        out
    }

    #[test]
    fn round_trips_a_synthetic_image() {
        let (w, h) = (37, 23);
        let pixels = checker(w, h);
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&pixels, w, h, true, &mut budget).unwrap();
        let decoded = decode(&bytes, &mut budget).unwrap();
        assert_eq!(decoded.width, w);
        assert_eq!(decoded.height, h);
        assert_eq!(decoded.pixels, pixels);
    }

    #[test]
    fn round_trips_flat_and_repeating_content_through_lz77() {
        let (w, h) = (16, 16);
        let mut pixels = vec![0xff10_2030u32; (w * h) as usize];
        // A few distinct pixels so the green/red/blue/alpha alphabets are
        // not all degenerate single-symbol tables.
        pixels[5] = 0xffaa_bbcc;
        pixels[200] = 0x8000_0000;
        let mut budget = Budget::new(Limits::permissive());
        let bytes = encode(&pixels, w, h, true, &mut budget).unwrap();
        let decoded = decode(&bytes, &mut budget).unwrap();
        assert_eq!(decoded.pixels, pixels);
    }

    #[test]
    fn rejects_bad_signature() {
        let mut budget = Budget::new(Limits::permissive());
        let err = decode(&[0x00, 0x00, 0x00, 0x00], &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn rejects_truncated_stream_without_panicking() {
        let mut budget = Budget::new(Limits::permissive());
        let pixels = checker(9, 9);
        let bytes = encode(&pixels, 9, 9, true, &mut budget).unwrap();
        for cut in [1usize, 2, 3, 5, bytes.len() / 2] {
            let _ = decode(&bytes[..cut.min(bytes.len())], &mut budget);
        }
    }
}
