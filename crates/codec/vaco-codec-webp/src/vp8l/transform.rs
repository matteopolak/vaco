//! The four VP8L transforms (spec §4): reversible per-pixel manipulations
//! applied to the ARGB buffer before/after entropy coding.
//!
//! Every function here works on a flat, row-major `Vec<u32>` of packed
//! `0xAARRGGBB` pixels — the representation [`super`] decodes the raw image
//! data into before any inverse transform runs, and the one this crate's own
//! encoder applies its (much smaller) forward set to before entropy coding.
//!
//! This crate's own encoder only ever emits [`SUBTRACT_GREEN`]; the other
//! three are decode-only here (needed to read real `cwebp` output, which
//! uses all four freely) — see the module doc in `vp8l/mod.rs` for why that
//! split is fine: every transform is independently optional per spec, so an
//! encoder that skips three of them still produces fully valid, standard
//! bitstreams, just less densely packed ones.

pub(crate) const PREDICTOR: u8 = 0;
pub(crate) const COLOR: u8 = 1;
pub(crate) const SUBTRACT_GREEN: u8 = 2;
pub(crate) const COLOR_INDEXING: u8 = 3;

const fn alpha(p: u32) -> i32 {
    ((p >> 24) & 0xff).cast_signed()
}
const fn red(p: u32) -> i32 {
    ((p >> 16) & 0xff).cast_signed()
}
const fn green(p: u32) -> i32 {
    ((p >> 8) & 0xff).cast_signed()
}
const fn blue(p: u32) -> i32 {
    (p & 0xff).cast_signed()
}
const fn pack(a: i32, r: i32, g: i32, b: i32) -> u32 {
    (((a as u32) & 0xff) << 24) | (((r as u32) & 0xff) << 16) | (((g as u32) & 0xff) << 8) | ((b as u32) & 0xff)
}

fn at(buf: &[u32], w: usize, x: usize, y: usize) -> u32 {
    buf.get(y.saturating_mul(w).saturating_add(x)).copied().unwrap_or(0)
}

fn average2(a: i32, b: i32) -> i32 {
    // `a` and `b` are always 0..=255 channel values here, so the sum is
    // never negative and `>>1` matches the spec's unsigned `/2` exactly.
    (a + b) >> 1
}

/// C's `/2` truncates toward zero; Rust's `>>1` floors. They agree for
/// non-negative `x` but disagree by one for a negative odd `x`, which the
/// spec's `ClampAddSubtractHalf` can hit (its `(a - b)` is a genuine signed
/// difference). Implemented without `/` since the workspace denies
/// `clippy::integer_division`.
fn trunc_div2(x: i32) -> i32 {
    if x >= 0 { x >> 1 } else { -((-x) >> 1) }
}

fn clamp(a: i32) -> i32 {
    a.clamp(0, 255)
}

fn clamp_add_subtract_full(a: i32, b: i32, c: i32) -> i32 {
    clamp(a + b - c)
}

fn clamp_add_subtract_half(a: i32, b: i32) -> i32 {
    clamp(a + trunc_div2(a - b))
}

fn select(l: u32, t: u32, tl: u32) -> u32 {
    let p_a = alpha(l) + alpha(t) - alpha(tl);
    let p_r = red(l) + red(t) - red(tl);
    let p_g = green(l) + green(t) - green(tl);
    let p_b = blue(l) + blue(t) - blue(tl);
    let p_l = (p_a - alpha(l)).abs() + (p_r - red(l)).abs() + (p_g - green(l)).abs() + (p_b - blue(l)).abs();
    let p_t = (p_a - alpha(t)).abs() + (p_r - red(t)).abs() + (p_g - green(t)).abs() + (p_b - blue(t)).abs();
    if p_l < p_t { l } else { t }
}

/// One channel of a predictor's output, applied identically to A/R/G/B.
fn predict_channel(mode: u8, l: i32, t: i32, tl: i32, tr: i32) -> i32 {
    match mode {
        1 => l,
        2 => t,
        3 => tr,
        4 => tl,
        5 => average2(average2(l, tr), t),
        6 => average2(l, tl),
        7 => average2(l, t),
        8 => average2(tl, t),
        9 => average2(t, tr),
        10 => average2(average2(l, tl), average2(t, tr)),
        13 => clamp_add_subtract_half(average2(l, t), tl),
        _ => 0,
    }
}

fn predict_pixel(mode: u8, l: u32, t: u32, tl: u32, tr: u32) -> u32 {
    match mode {
        0 => 0xff00_0000,
        1 => l,
        2 => t,
        3 => tr,
        4 => tl,
        11 => select(l, t, tl),
        12 => pack(
            clamp_add_subtract_full(alpha(l), alpha(t), alpha(tl)),
            clamp_add_subtract_full(red(l), red(t), red(tl)),
            clamp_add_subtract_full(green(l), green(t), green(tl)),
            clamp_add_subtract_full(blue(l), blue(t), blue(tl)),
        ),
        5 | 6 | 7 | 8 | 9 | 10 | 13 => pack(
            predict_channel(mode, alpha(l), alpha(t), alpha(tl), alpha(tr)),
            predict_channel(mode, red(l), red(t), red(tl), red(tr)),
            predict_channel(mode, green(l), green(t), green(tl), green(tr)),
            predict_channel(mode, blue(l), blue(t), blue(tl), blue(tr)),
        ),
        _ => 0,
    }
}

/// Inverse predictor transform: `buf` holds residuals in, reconstructed
/// pixels out. `modes` is the (green channel of the) predictor sub-image,
/// one entry per `(1 << size_bits)`-square block, `mode_width` wide.
pub(crate) fn inverse_predictor(buf: &mut [u32], width: usize, height: usize, modes: &[u32], size_bits: u32, mode_width: usize) {
    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let Some(&residual) = buf.get(idx) else { continue };
            let pred = if x == 0 && y == 0 {
                0xff00_0000
            } else if y == 0 {
                at(buf, width, x - 1, y)
            } else if x == 0 {
                at(buf, width, x, y - 1)
            } else {
                let l = at(buf, width, x - 1, y);
                let t = at(buf, width, x, y - 1);
                let tl = at(buf, width, x - 1, y - 1);
                // spec §4.1: on the rightmost column, TR is *not* the pixel
                // above-and-right (there is none) but the leftmost pixel of
                // the current row — already decoded, since we scan left to
                // right.
                let tr = if x + 1 == width { at(buf, width, 0, y) } else { at(buf, width, x + 1, y - 1) };
                let block_idx = (y >> size_bits) * mode_width + (x >> size_bits);
                let mode_pixel = modes.get(block_idx).copied().unwrap_or(0);
                let mode = (green(mode_pixel) & 0x0f) as u8;
                predict_pixel(mode, l, t, tl, tr)
            };
            let out = pack(
                alpha(residual) + alpha(pred),
                red(residual) + red(pred),
                green(residual) + green(pred),
                blue(residual) + blue(pred),
            );
            if let Some(slot) = buf.get_mut(idx) {
                *slot = out;
            }
        }
    }
}

fn color_transform_delta(t: i8, c: i8) -> i32 {
    (i32::from(t) * i32::from(c)) >> 5
}

/// Inverse color transform: adds back the green-derived deltas to red/blue.
/// `cte` is the color-transform-element sub-image, one entry per
/// `(1 << size_bits)`-square block, `cte_width` wide.
pub(crate) fn inverse_color(buf: &mut [u32], width: usize, height: usize, cte: &[u32], size_bits: u32, cte_width: usize) {
    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let Some(&pixel) = buf.get(idx) else { continue };
            let block_idx = (y >> size_bits) * cte_width + (x >> size_bits);
            let cte_px = cte.get(block_idx).copied().unwrap_or(0);
            // spec §4.2: a ColorTransformElement is packed as a pixel with
            // alpha=255, red=red_to_blue, green=green_to_blue, blue=green_to_red.
            let green_to_red = blue(cte_px) as i8;
            let green_to_blue = green(cte_px) as i8;
            let red_to_blue = red(cte_px) as i8;
            let g = green(pixel) as i8;
            let mut new_red = red(pixel) + color_transform_delta(green_to_red, g);
            new_red &= 0xff;
            let mut new_blue = blue(pixel) + color_transform_delta(green_to_blue, g);
            new_blue += color_transform_delta(red_to_blue, new_red as i8);
            new_blue &= 0xff;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = pack(alpha(pixel), new_red, green(pixel), new_blue);
            }
        }
    }
}

/// Inverse subtract-green transform: adds green back into red and blue.
pub(crate) fn inverse_subtract_green(buf: &mut [u32]) {
    for slot in buf.iter_mut() {
        let p = *slot;
        let g = green(p);
        let r = (red(p) + g) & 0xff;
        let b = (blue(p) + g) & 0xff;
        *slot = pack(alpha(p), r, g, b);
    }
}

/// Forward subtract-green transform (this crate's encoder only).
pub(crate) fn forward_subtract_green(buf: &mut [u32]) {
    for slot in buf.iter_mut() {
        let p = *slot;
        let g = green(p);
        let r = (red(p) - g) & 0xff;
        let b = (blue(p) - g) & 0xff;
        *slot = pack(alpha(p), r, g, b);
    }
}

/// Inverse color-indexing transform: replace each pixel (an index packed
/// into the green channel, possibly several per pixel when the table is
/// small) with the palette entry it names. `indices` is `packed_width` wide;
/// the result is exactly `real_width * height` (spec's rounding-up to a
/// whole byte can overshoot the true width by a few columns, trimmed here).
pub(crate) fn inverse_color_indexing(
    indices: &[u32],
    packed_width: usize,
    real_width: usize,
    height: usize,
    table: &[u32],
    width_bits: u32,
) -> Vec<u32> {
    let per_pixel_bits: u32 = match width_bits {
        1 => 4,
        2 => 2,
        3 => 1,
        _ => 8,
    };
    let per_byte: usize = 1usize << width_bits;
    let mask: u32 = (1u32 << per_pixel_bits) - 1;
    let mut out = vec![0u32; real_width.saturating_mul(height)];
    for y in 0..height {
        for x in 0..packed_width {
            let packed = green(at(indices, packed_width, x, y)) as u32;
            for sub in 0..per_byte {
                let out_x = x * per_byte + sub;
                if out_x >= real_width {
                    break;
                }
                let idx = if width_bits == 0 { packed } else { (packed >> (u32::try_from(sub).unwrap_or(0) * per_pixel_bits)) & mask };
                let color = table.get(idx as usize).copied().unwrap_or(0);
                if let Some(slot) = out.get_mut(y * real_width + out_x) {
                    *slot = color;
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn subtract_green_round_trips() {
        let mut buf = vec![pack(255, 10, 200, 5), pack(0, 0, 0, 0), pack(128, 255, 1, 254)];
        let original = buf.clone();
        forward_subtract_green(&mut buf);
        inverse_subtract_green(&mut buf);
        assert_eq!(buf, original);
    }

    #[test]
    fn predictor_mode1_reproduces_left_pixel_with_zero_residual() {
        let width = 2;
        let height = 1;
        // buf[0] is the corner (always predicted 0xff000000); buf[1]'s
        // residual is zero, so under mode 1 (predict = left) it must come
        // out identical to the reconstructed buf[0].
        let mut buf = vec![0xff00_0000u32, 0];
        let modes = vec![pack(255, 0, 1, 0)]; // green channel = mode 1
        inverse_predictor(&mut buf, width, height, &modes, 8, 1);
        assert_eq!(buf[1], buf[0]);
    }

    #[test]
    fn color_indexing_expands_bundled_pixels_and_trims_padding() {
        // width_bits=3: 8 one-bit indices packed per byte, real width 5.
        let table = vec![pack(255, 0, 0, 0), pack(255, 255, 255, 255)];
        // Green byte 0b0000_0101 (LSB first per pixel: bit0=1,bit1=0,bit2=1,...)
        let indices = vec![pack(255, 0, 0b0000_0101, 0)];
        let out = inverse_color_indexing(&indices, 1, 5, 1, &table, 3);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0], table[1]);
        assert_eq!(out[1], table[0]);
        assert_eq!(out[2], table[1]);
        assert_eq!(out[3], table[0]);
        assert_eq!(out[4], table[0]);
    }
}
