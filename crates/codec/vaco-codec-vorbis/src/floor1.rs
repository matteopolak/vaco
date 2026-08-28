//! Floor type 1: piecewise-linear spectral envelope (spec section 7). This is
//! the floor type every native `ffmpeg` Vorbis encoding this crate has
//! differential-tested against actually uses.
//!
//! `Vaco-Spec-Ref: vorbis-i sections 7.2, 7.2.3, 7.2.4 and 9.2.4-9.2.7`

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::bitreader::{BitReaderLsb, ilog};
use crate::codebook::Codebook;
use crate::floor1_table::FLOOR1_INVERSE_DB_TABLE;

const MAX_X_LIST: usize = 65;

#[derive(Debug, Clone)]
pub(crate) struct Floor1Config {
    partition_class_list: Vec<u8>,
    class_dimensions: Vec<u8>,
    class_subclasses: Vec<u8>,
    class_masterbooks: Vec<u8>,
    subclass_books: Vec<Vec<i16>>,
    multiplier: u8,
    x_list: Vec<u32>,
}

impl Floor1Config {
    #[allow(
        clippy::cast_possible_wrap,
        clippy::cast_lossless,
        clippy::cast_possible_truncation,
        reason = "every field here is a spec-bounded small bit-width read (4/8 bits), so the narrowing casts cannot lose information"
    )]
    pub(crate) fn parse_header(
        r: &mut BitReaderLsb<'_>,
        budget: &mut Budget,
        max_codebook: u32,
    ) -> Result<Self> {
        let partitions = r.get(5);
        let mut partition_class_list: Vec<u8> = budget.alloc(partitions as usize)?;
        let mut maximum_class: i32 = -1;
        for slot in &mut partition_class_list {
            let c = r.get(4);
            *slot = c as u8;
            maximum_class = maximum_class.max(c as i32);
        }
        let class_count = usize::try_from(maximum_class.saturating_add(1)).unwrap_or(0);
        budget.consume_fuel(class_count as u64 * 8)?;
        let mut class_dimensions: Vec<u8> = budget.alloc(class_count)?;
        let mut class_subclasses: Vec<u8> = budget.alloc(class_count)?;
        let mut class_masterbooks: Vec<u8> = budget.alloc(class_count)?;
        // `Vec<Vec<i16>>` cannot go through `Budget::alloc` (it needs
        // `T: Copy`); `class_count` is bounded to at most 16 by the 4-bit
        // partition-class field, so an ordinary `push` is not an
        // attacker-controlled allocation the way the inner rows are.
        let mut subclass_books: Vec<Vec<i16>> = Vec::new();
        for i in 0..class_count {
            let dim = r.get(3).saturating_add(1);
            let subclasses = r.get(2);
            if let Some(s) = class_dimensions.get_mut(i) {
                *s = dim as u8;
            }
            if let Some(s) = class_subclasses.get_mut(i) {
                *s = subclasses as u8;
            }
            let masterbook = if subclasses != 0 {
                let b = r.get(8);
                if b > max_codebook {
                    return Err(Error::InvalidData("vorbis: floor1 masterbook out of range"));
                }
                b as u8
            } else {
                0
            };
            if let Some(s) = class_masterbooks.get_mut(i) {
                *s = masterbook;
            }
            let count = 1usize << subclasses;
            let mut books: Vec<i16> = budget.alloc(count)?;
            for b in &mut books {
                let raw = r.get(8) as i32 - 1;
                if raw >= 0 && raw as u32 > max_codebook {
                    return Err(Error::InvalidData(
                        "vorbis: floor1 subclass book out of range",
                    ));
                }
                *b = raw as i16;
            }
            subclass_books.push(books);
        }
        let multiplier = r.get(2).saturating_add(1);
        let range_bits = r.get(4);
        if r.overran() {
            return Err(Error::InvalidData("vorbis: eop decoding floor1 header"));
        }

        let mut x_list: Vec<u32> = vec![0, 1u32.checked_shl(range_bits).unwrap_or(u32::MAX)];
        for &class in &partition_class_list {
            let dim = class_dimensions.get(class as usize).copied().unwrap_or(1) as u32;
            for _ in 0..dim {
                if x_list.len() >= MAX_X_LIST {
                    return Err(Error::InvalidData("vorbis: floor1 x_list too long"));
                }
                x_list.push(r.get(range_bits));
            }
        }
        if r.overran() {
            return Err(Error::InvalidData("vorbis: eop decoding floor1 x_list"));
        }
        let mut sorted = x_list.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w.first() == w.get(1)) {
            return Err(Error::InvalidData(
                "vorbis: floor1 x_list has duplicate values",
            ));
        }

        Ok(Self {
            partition_class_list,
            class_dimensions,
            class_subclasses,
            class_masterbooks,
            subclass_books,
            multiplier: multiplier as u8,
            x_list,
        })
    }
}

pub(crate) enum Floor1Decoded {
    Unused,
    Used { y: Vec<u32> },
}

/// Packet decode (spec section 7.2.3).
pub(crate) fn decode_packet(
    cfg: &Floor1Config,
    r: &mut BitReaderLsb<'_>,
    codebooks: &[Codebook],
) -> Floor1Decoded {
    if !r.get_bool() {
        return Floor1Decoded::Unused;
    }
    let range: u32 = match cfg.multiplier {
        1 => 256,
        2 => 128,
        3 => 86,
        _ => 64,
    };
    let ilog_range = ilog(i64::from(range.saturating_sub(1)));
    let mut y: Vec<u32> = vec![0; cfg.x_list.len()];
    if let Some(v) = y.get_mut(0) {
        *v = r.get(ilog_range);
    }
    if let Some(v) = y.get_mut(1) {
        *v = r.get(ilog_range);
    }
    let mut offset = 2usize;
    for &class in &cfg.partition_class_list {
        let cdim = cfg
            .class_dimensions
            .get(class as usize)
            .copied()
            .unwrap_or(1) as usize;
        let cbits = u32::from(
            cfg.class_subclasses
                .get(class as usize)
                .copied()
                .unwrap_or(0),
        );
        let csub = (1u32 << cbits).saturating_sub(1);
        let mut cval: u32 = 0;
        if cbits > 0 {
            let masterbook = cfg
                .class_masterbooks
                .get(class as usize)
                .copied()
                .unwrap_or(0);
            let Some(book) = codebooks.get(masterbook as usize) else {
                return Floor1Decoded::Unused;
            };
            let Some(v) = book.decode_scalar(r) else {
                return Floor1Decoded::Unused;
            };
            cval = v;
        }
        for j in 0..cdim {
            let book_idx = cfg
                .subclass_books
                .get(class as usize)
                .and_then(|row| row.get((cval & csub) as usize))
                .copied()
                .unwrap_or(-1);
            cval >>= cbits.min(31);
            let value = if book_idx >= 0 {
                let Some(book) = codebooks.get(book_idx as usize) else {
                    return Floor1Decoded::Unused;
                };
                match book.decode_scalar(r) {
                    Some(v) => v,
                    None => return Floor1Decoded::Unused,
                }
            } else {
                0
            };
            if let Some(slot) = y.get_mut(offset + j) {
                *slot = value;
            }
        }
        offset += cdim;
        if r.overran() {
            return Floor1Decoded::Unused;
        }
    }
    Floor1Decoded::Used { y }
}

fn low_neighbor(v: &[u32], x: usize) -> usize {
    let target = v.get(x).copied().unwrap_or(0);
    let mut best: Option<(usize, u32)> = None;
    for (n, &val) in v.iter().enumerate().take(x) {
        if val < target && best.is_none_or(|(_, bv)| val > bv) {
            best = Some((n, val));
        }
    }
    best.map_or(0, |(n, _)| n)
}

fn high_neighbor(v: &[u32], x: usize) -> usize {
    let target = v.get(x).copied().unwrap_or(0);
    let mut best: Option<(usize, u32)> = None;
    for (n, &val) in v.iter().enumerate().take(x) {
        if val > target && best.is_none_or(|(_, bv)| val < bv) {
            best = Some((n, val));
        }
    }
    best.map_or(0, |(n, _)| n)
}

/// `render_point` (spec section 9.2.6).
#[allow(
    clippy::integer_division,
    reason = "spec 9.2.6's own definition of off = err / adx"
)]
fn render_point(x0: i64, y0: i64, x1: i64, y1: i64, x: i64) -> i64 {
    let dy = y1 - y0;
    let adx = (x1 - x0).max(1);
    let ady = dy.abs();
    let err = ady.saturating_mul(x - x0);
    let off = err / adx;
    if dy < 0 { y0 - off } else { y0 + off }
}

/// `render_line` (spec section 9.2.7): fill `v[x0..=x1]` with the integer
/// line from `(x0,y0)` to `(x1,y1)`.
#[allow(
    clippy::integer_division,
    reason = "spec 9.2.7's own definition of base = dy / adx, truncating toward zero"
)]
fn render_line(x0: i64, y0: i64, x1: i64, y1: i64, v: &mut [f32], lookup: impl Fn(i64) -> f32) {
    let dy = y1 - y0;
    let adx = (x1 - x0).max(1);
    let mut ady = dy.abs();
    let base = dy / adx;
    let mut x = x0;
    let mut y = y0;
    let mut err = 0i64;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    ady -= base.abs() * adx;
    if let Some(slot) = usize::try_from(x).ok().and_then(|i| v.get_mut(i)) {
        *slot = lookup(y);
    }
    x += 1;
    while x < x1 {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        if let Some(slot) = usize::try_from(x).ok().and_then(|i| v.get_mut(i)) {
            *slot = lookup(y);
        }
        x += 1;
    }
}

/// Curve computation (spec section 7.2.4), producing an `n`-element linear
/// spectral envelope.
#[allow(
    clippy::integer_division,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "spec 7.2.4's own predicted +/- val/2 halving, and n is capped at 8192 so the i64 round-trip is exact"
)]
pub(crate) fn compute_curve(cfg: &Floor1Config, y: &[u32], n: usize) -> Vec<f32> {
    let range: i64 = match cfg.multiplier {
        1 => 256,
        2 => 128,
        3 => 86,
        _ => 64,
    };
    let values = cfg.x_list.len();
    let mut step2_flag = vec![false; values];
    let mut final_y = vec![0i64; values];
    if let (Some(f), Some(y0)) = (final_y.get_mut(0), y.first()) {
        *f = i64::from(*y0);
    }
    if let (Some(f), Some(y1)) = (final_y.get_mut(1), y.get(1)) {
        *f = i64::from(*y1);
    }
    if let Some(f) = step2_flag.get_mut(0) {
        *f = true;
    }
    if let Some(f) = step2_flag.get_mut(1) {
        *f = true;
    }

    for i in 2..values {
        let lo = low_neighbor(&cfg.x_list, i);
        let hi = high_neighbor(&cfg.x_list, i);
        let (lo_x, lo_y) = (
            i64::from(cfg.x_list.get(lo).copied().unwrap_or(0)),
            final_y.get(lo).copied().unwrap_or(0),
        );
        let (hi_x, hi_y) = (
            i64::from(cfg.x_list.get(hi).copied().unwrap_or(0)),
            final_y.get(hi).copied().unwrap_or(0),
        );
        let predicted = render_point(
            lo_x,
            lo_y,
            hi_x,
            hi_y,
            i64::from(cfg.x_list.get(i).copied().unwrap_or(0)),
        );
        let val = i64::from(y.get(i).copied().unwrap_or(0));
        let highroom = range - predicted;
        let lowroom = predicted;
        let room = if highroom < lowroom {
            highroom * 2
        } else {
            lowroom * 2
        };

        let new_y = if val != 0 {
            if let Some(f) = step2_flag.get_mut(lo) {
                *f = true;
            }
            if let Some(f) = step2_flag.get_mut(hi) {
                *f = true;
            }
            if let Some(f) = step2_flag.get_mut(i) {
                *f = true;
            }
            if val >= room {
                if highroom > lowroom {
                    val - lowroom + predicted
                } else {
                    predicted - val + highroom - 1
                }
            } else if val % 2 == 1 {
                predicted - (val + 1) / 2
            } else {
                predicted + val / 2
            }
        } else {
            predicted
        };
        if let Some(f) = final_y.get_mut(i) {
            *f = new_y.clamp(0, range.saturating_sub(1));
        }
    }

    // Sort (x_list, final_y, step2_flag) together by ascending x, per spec.
    let mut order: Vec<usize> = (0..values).collect();
    order.sort_by_key(|&i| cfg.x_list.get(i).copied().unwrap_or(0));

    let mut floor = vec![0f32; n];
    let lookup = |v: i64| -> f32 {
        FLOOR1_INVERSE_DB_TABLE
            .get(v.clamp(0, 255) as usize)
            .copied()
            .unwrap_or(0.0)
    };

    let Some(&first) = order.first() else {
        return floor;
    };
    let mut lx = 0i64;
    let mut ly = final_y.get(first).copied().unwrap_or(0) * i64::from(cfg.multiplier);
    let mut hx = 0i64;
    let mut hy = ly;
    for &idx in order.iter().skip(1) {
        if step2_flag.get(idx).copied().unwrap_or(false) {
            hy = final_y.get(idx).copied().unwrap_or(0) * i64::from(cfg.multiplier);
            hx = i64::from(cfg.x_list.get(idx).copied().unwrap_or(0));
            render_line(lx, ly, hx, hy, &mut floor, lookup);
            lx = hx;
            ly = hy;
        }
    }
    if hx < n as i64 {
        render_line(hx, hy, n as i64, hy, &mut floor, lookup);
    }
    floor
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn render_line_produces_a_monotone_ramp() {
        let mut v = vec![0f32; 20];
        render_line(0, 0, 16, 160, &mut v, |y| y as f32);
        // Spec 9.2.7 writes the half-open range [x0, x1); index 16 (== x1) is
        // deliberately left to whichever call renders the next segment.
        for w in v[..16].windows(2) {
            assert!(w[1] >= w[0]);
        }
    }

    #[test]
    fn low_high_neighbor_matches_spec_example() {
        // spec 7.2.1 example: X values 0,128,64,32,96,16,48,80,112 in list order
        let xs = vec![0u32, 128, 64, 32, 96, 16, 48, 80, 112];
        // low/high neighbor of position 2 (x=64) among positions 0,1 (x=0,128):
        // low: greatest value < 64 among {0,128} -> 0 at index 0
        // high: smallest value > 64 among {0,128} -> 128 at index 1
        assert_eq!(low_neighbor(&xs, 2), 0);
        assert_eq!(high_neighbor(&xs, 2), 1);
    }

    #[test]
    fn compute_curve_is_finite_and_nonnegative() {
        let cfg = Floor1Config {
            partition_class_list: vec![0],
            class_dimensions: vec![1],
            class_subclasses: vec![0],
            class_masterbooks: vec![0],
            subclass_books: vec![vec![-1]],
            multiplier: 1,
            x_list: vec![0, 64, 32],
        };
        let y = vec![100u32, 50, 75];
        let curve = compute_curve(&cfg, &y, 64);
        assert_eq!(curve.len(), 64);
        for &v in &curve {
            assert!(v.is_finite() && v >= 0.0);
        }
    }
}
