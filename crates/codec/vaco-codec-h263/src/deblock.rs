//! Annex J — Deblocking Filter mode: a post-reconstruction edge filter
//! run once per picture, after every macroblock has already been
//! reconstructed and clipped to `0..=255` (§6.3.2), on the picture that
//! is about to become the reference for the next one.
//!
//! `Vaco-Spec-Ref: itu-t-h263` Annex J (01/2005 edition).
//!
//! # Scope
//!
//! Only the plain case this crate's own `PLUSPTYPE` parser lets through:
//! Independent Segment Decoding (Annex R) and Reduced-Resolution Update
//! (Annex Q) are both still rejected as `unsupported` before a picture
//! reaches here, which is what makes the two extra exclusion rules in
//! §J.3 — "no filtering across slice edges... or across the top boundary
//! of GOBs having GOB headers present" and RRU's `STRENGTH = infinity`
//! override — inapplicable rather than merely unhandled: neither
//! condition can be true of any picture this function ever sees. The one
//! exclusion that *does* still apply, and is implemented, is the
//! ordinary picture-edge one ("no filtering is performed across a
//! picture edge").

#![allow(
    clippy::integer_division,
    reason = "every division in this module is one of §J.3's own formulas (`d = (A-4B+4C-D)/8`, `(A-D)/4`, `d1/2`) or a block/macroblock-index divisor (`/8`, `/16`, `/blocks_per_mb`) — §4.1 defines '/' as truncation toward zero, which is exactly what integer division on these already-non-negative or already-signed values does; there is no float alternative that would not have to immediately truncate right back"
)]
#![allow(
    clippy::many_single_char_names,
    reason = "A, B, C, D (and their filtered replacements A1/B1/C1/D1, here a/b/c/d and a1/b1/c1/d1) are §J.3's own names for the four samples a filter application reads and writes; renaming them would make every formula harder to check against the spec text, not easier"
)]

use vaco_frame::Frame;

/// Table J.2: `QUANT` (1..=31) to `STRENGTH`, indexed `[quant - 1]`.
const STRENGTH_TABLE: [i32; 31] = [
    1, 1, 2, 2, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9, 9, 10, 10, 10, 11, 11, 11, 12,
    12, 12,
];

#[must_use]
fn strength_for(quant: u8) -> i32 {
    let idx = usize::from(quant.clamp(1, 31)) - 1;
    STRENGTH_TABLE.get(idx).copied().unwrap_or(12)
}

/// §J.3's `UpDownRamp`: zero outside `±2*STRENGTH`, ramping linearly to
/// zero as `|x|` approaches `2*STRENGTH`, exactly `x` for `|x| <=
/// STRENGTH`.
#[must_use]
fn up_down_ramp(x: i32, strength: i32) -> i32 {
    let ax = x.abs();
    let reduced = 0.max(ax - 0.max(2 * (ax - strength)));
    x.signum() * reduced
}

/// §J.3's `clipd1`: clip `x` to `±|lim|`.
#[must_use]
fn clipd1(x: i32, lim: i32) -> i32 {
    let bound = lim.abs();
    x.clamp(-bound, bound)
}

/// One 4-pixel filter application (§J.3), returning the four replacement
/// values in `(A1, B1, C1, D1)` order. `a`/`b`/`c`/`d` are the *current*
/// sample values (already reflecting any earlier pass this picture's own
/// filtering order requires — see the module docs on the two-pass
/// horizontal-then-vertical structure).
#[must_use]
fn filter4(a: i32, b: i32, c: i32, d: i32, strength: i32) -> (u8, u8, u8, u8) {
    // §4.1: "/" truncates toward zero — Rust's integer division on `i32`
    // already does exactly that, so no extra rounding step is needed
    // here or below.
    let d_val = (a - 4 * b + 4 * c - d) / 8;
    let d1 = up_down_ramp(d_val, strength);
    let d2 = clipd1((a - d) / 4, d1 / 2);
    let clip = |v: i32| v.clamp(0, 255) as u8;
    (clip(a - d2), clip(b + d1), clip(c - d1), clip(d + d2))
}

/// Per-picture geometry and per-macroblock state the filter needs, kept
/// generic over which plane it is filtering (luma or chroma) via
/// `blocks_per_mb`: `2` for luma (each macroblock is a 2x2 grid of 8x8
/// blocks), `1` for chroma (one 8x8 block per macroblock per plane).
struct PlaneGeometry {
    /// Blocks per macroblock along one axis (both axes are square: `2`
    /// or `1`).
    blocks_per_mb: u32,
    mb_width: u32,
}

impl PlaneGeometry {
    /// The macroblock a given 8-pixel block index (`block_pos / 8`)
    /// belongs to, along one axis.
    #[must_use]
    const fn mb_of_block(&self, block_index: u32) -> u32 {
        block_index / self.blocks_per_mb
    }
}

/// Whether macroblock `(mb_x, mb_y)` counts as "coded" for §J.3's edge
/// condition (`COD == 0 || MB-type == INTRA`) — out-of-range coordinates
/// (never reached by a conforming picture, but not `unwrap`-worthy
/// either) count as not coded, so a filter never applies across a
/// boundary this decoder cannot actually attribute to a real macroblock.
#[must_use]
fn mb_coded(mb_coded: &[bool], mb_width: u32, mb_x: u32, mb_y: u32) -> bool {
    let idx = (mb_y * mb_width + mb_x) as usize;
    mb_coded.get(idx).copied().unwrap_or(false)
}

#[must_use]
fn mb_quant(mb_quant: &[u8], mb_width: u32, mb_x: u32, mb_y: u32) -> u8 {
    let idx = (mb_y * mb_width + mb_x) as usize;
    mb_quant.get(idx).copied().unwrap_or(1)
}

/// One plane's worth of both filtering passes (§J.3's mandated order:
/// every horizontal edge first, using only pre-pass sample values; then
/// every vertical edge, using the horizontal pass's results). Within one
/// pass, adjacent edges are always at least 8 samples apart (the
/// smallest block dimension), so no edge's own 4-sample window is ever
/// touched by another edge's write in the *same* pass — each edge can be
/// read and written in place without a second buffer.
#[allow(
    clippy::too_many_arguments,
    reason = "one plane's full filter state: geometry (width/height/mb grid), the two per-macroblock decode-state grids the edge condition and STRENGTH both read, and the row-accessor closures over the frame's own mutable plane"
)]
fn filter_plane(
    frame: &mut Frame,
    plane_idx: usize,
    width: u32,
    height: u32,
    geom: &PlaneGeometry,
    mb_coded_grid: &[bool],
    mb_quant_grid: &[u8],
) {
    if width < 8 || height < 8 {
        return;
    }
    // Horizontal edges: boundary rows `y = 8, 16, ..., height-8`, each
    // separating "block1" (above) from "block2" (below).
    let mut y = 8u32;
    while y < height {
        for x in 0..width {
            let block_x = x / 8;
            let mb_x = geom.mb_of_block(block_x);
            let mb_y_top = geom.mb_of_block(y / 8 - 1);
            let mb_y_bot = geom.mb_of_block(y / 8);
            let top_coded = mb_coded(mb_coded_grid, geom.mb_width, mb_x, mb_y_top);
            let bot_coded = mb_coded(mb_coded_grid, geom.mb_width, mb_x, mb_y_bot);
            if !top_coded && !bot_coded {
                continue;
            }
            let quant = if bot_coded {
                mb_quant(mb_quant_grid, geom.mb_width, mb_x, mb_y_bot)
            } else {
                mb_quant(mb_quant_grid, geom.mb_width, mb_x, mb_y_top)
            };
            let strength = strength_for(quant);
            let Some(mut plane) = frame.plane_mut(plane_idx) else {
                return;
            };
            let (ya, yb, yc, yd) = (y - 2, y - 1, y, y + 1);
            let a = sample(&mut plane, x, ya);
            let b = sample(&mut plane, x, yb);
            let c = sample(&mut plane, x, yc);
            let d = sample(&mut plane, x, yd);
            let (a1, b1, c1, d1) = filter4(a, b, c, d, strength);
            write_sample(&mut plane, x, ya, a1);
            write_sample(&mut plane, x, yb, b1);
            write_sample(&mut plane, x, yc, c1);
            write_sample(&mut plane, x, yd, d1);
        }
        y += 8;
    }

    // Vertical edges: boundary columns `x = 8, 16, ..., width-8`, each
    // separating "block1" (left) from "block2" (right). Must run after
    // every horizontal edge above (§J.3), which the sequencing here
    // already guarantees.
    let mut x = 8u32;
    while x < width {
        for y in 0..height {
            let block_y = y / 8;
            let mb_y = geom.mb_of_block(block_y);
            let mb_x_left = geom.mb_of_block(x / 8 - 1);
            let mb_x_right = geom.mb_of_block(x / 8);
            let left_coded = mb_coded(mb_coded_grid, geom.mb_width, mb_x_left, mb_y);
            let right_coded = mb_coded(mb_coded_grid, geom.mb_width, mb_x_right, mb_y);
            if !left_coded && !right_coded {
                continue;
            }
            let quant = if right_coded {
                mb_quant(mb_quant_grid, geom.mb_width, mb_x_right, mb_y)
            } else {
                mb_quant(mb_quant_grid, geom.mb_width, mb_x_left, mb_y)
            };
            let strength = strength_for(quant);
            let Some(mut plane) = frame.plane_mut(plane_idx) else {
                return;
            };
            let (xa, xb, xc, xd) = (x - 2, x - 1, x, x + 1);
            let a = sample(&mut plane, xa, y);
            let b = sample(&mut plane, xb, y);
            let c = sample(&mut plane, xc, y);
            let d = sample(&mut plane, xd, y);
            let (a1, b1, c1, d1) = filter4(a, b, c, d, strength);
            write_sample(&mut plane, xa, y, a1);
            write_sample(&mut plane, xb, y, b1);
            write_sample(&mut plane, xc, y, c1);
            write_sample(&mut plane, xd, y, d1);
        }
        x += 8;
    }
}

#[must_use]
fn sample(plane: &mut vaco_frame::PlaneMut<'_>, x: u32, y: u32) -> i32 {
    plane
        .row(y as usize)
        .and_then(|row| row.get(x as usize))
        .copied()
        .map_or(0, i32::from)
}

fn write_sample(plane: &mut vaco_frame::PlaneMut<'_>, x: u32, y: u32, value: u8) {
    if let Some(row) = plane.row_mut(y as usize)
        && let Some(slot) = row.get_mut(x as usize)
    {
        *slot = value;
    }
}

/// Run Annex J's deblocking filter over every plane of `frame` in place.
/// `mb_width`/`mb_height` are the picture's macroblock grid dimensions;
/// `mb_coded`/`mb_quant` are row-major, one entry per macroblock (see
/// `h263::ActivePicture`'s own fields of these names for exactly what
/// they hold).
pub(crate) fn filter_picture(
    frame: &mut Frame,
    mb_width: u32,
    mb_height: u32,
    mb_coded: &[bool],
    mb_quant: &[u8],
) {
    let luma_geom = PlaneGeometry {
        blocks_per_mb: 2,
        mb_width,
    };
    let chroma_geom = PlaneGeometry {
        blocks_per_mb: 1,
        mb_width,
    };
    filter_plane(
        frame,
        0,
        mb_width * 16,
        mb_height * 16,
        &luma_geom,
        mb_coded,
        mb_quant,
    );
    filter_plane(
        frame,
        1,
        mb_width * 8,
        mb_height * 8,
        &chroma_geom,
        mb_coded,
        mb_quant,
    );
    filter_plane(
        frame,
        2,
        mb_width * 8,
        mb_height * 8,
        &chroma_geom,
        mb_coded,
        mb_quant,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_table_matches_table_j_2_endpoints() {
        assert_eq!(strength_for(1), 1);
        assert_eq!(strength_for(16), 7);
        assert_eq!(strength_for(17), 8);
        assert_eq!(strength_for(31), 12);
    }

    #[test]
    fn up_down_ramp_is_identity_below_strength_and_zero_at_twice_strength() {
        assert_eq!(up_down_ramp(3, 8), 3);
        assert_eq!(up_down_ramp(-3, 8), -3);
        assert_eq!(up_down_ramp(16, 8), 0);
        assert_eq!(up_down_ramp(0, 8), 0);
        // Strictly between STRENGTH and 2*STRENGTH: ramps down, not a
        // step function — 12 is 4 past strength=8, twice that (8) is
        // subtracted from the raw 12, leaving 4.
        assert_eq!(up_down_ramp(12, 8), 4);
    }

    #[test]
    fn filter4_is_a_no_op_on_a_flat_region() {
        let (a1, b1, c1, d1) = filter4(100, 100, 100, 100, 8);
        assert_eq!((a1, b1, c1, d1), (100, 100, 100, 100));
    }

    #[test]
    fn filter4_smooths_a_small_step_and_leaves_a_large_one_alone() {
        // A small step (d = (0-4*0+4*10-10)/8 = 30/8 = 3, inside
        // strength) gets smoothed.
        let (_, b1, c1, _) = filter4(0, 0, 10, 10, 8);
        assert!(
            b1 > 0 && c1 < 10,
            "small step should be smoothed: b1={b1} c1={c1}"
        );
        // A large step (d = (0-4*0+4*100-100)/8 = 300/8 = 37, past
        // 2*strength=16) is left alone — this is a real edge, not
        // blocking noise.
        let (a1, b1, c1, d1) = filter4(0, 0, 100, 100, 8);
        assert_eq!((a1, b1, c1, d1), (0, 0, 100, 100));
    }
}
