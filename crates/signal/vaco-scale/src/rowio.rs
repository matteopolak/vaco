//! Reading and writing one component row.
//!
//! Every load is `read container -> shift -> mask`, and every store is the
//! inverse. Writing the general form once and specialising the three shapes that
//! cover the common formats (planar 8-bit, strided 8-bit, planar 16-bit) is
//! deliberate: the general form is the definition, the specialisations are only
//! there to give LLVM a fixed stride to vectorise, and they are checked against
//! the general form by a property test.
//!
//! Nothing here can panic or read out of bounds. A row shorter than the
//! component claims produces fewer samples, not an error — the caller has
//! already validated the geometry, and a defensive `get` is cheaper than being
//! wrong.

use crate::geometry::{ComponentLayout, Endian};

/// Read up to `out.len()` samples of one component from `row`.
///
/// Returns the number of samples actually read; a short row yields a short
/// result rather than a panic.
pub fn read_row(row: &[u8], comp: &ComponentLayout, out: &mut [i32]) -> usize {
    let n = out.len().min(comp.width as usize);
    let host = comp.container == 1 || comp.endian == crate::geometry::host_endian();
    match (comp.container, comp.shift, comp.depth, host) {
        // Planar 8-bit: the single most common shape in the whole table.
        (1, 0, 8, _) if comp.step == 1 => {
            let Some(src) = row.get(comp.offset..) else {
                return 0;
            };
            let n = n.min(src.len());
            for (o, &s) in out.iter_mut().zip(src.iter()).take(n) {
                *o = i32::from(s);
            }
            n
        }
        // Packed 8-bit: rgb24, rgba, nv12 chroma, yuyv422.
        (1, 0, 8, _) => {
            let mut count = 0;
            for (i, o) in out.iter_mut().enumerate().take(n) {
                let Some(&s) = row.get(comp.byte_of(i)) else {
                    break;
                };
                *o = i32::from(s);
                count = i + 1;
            }
            count
        }
        // Planar 16-bit in host order: yuv420p10le and friends on a little-
        // endian host.
        (2, shift, depth, true) => {
            let mask = mask_of(depth);
            let mut count = 0;
            for (i, o) in out.iter_mut().enumerate().take(n) {
                let at = comp.byte_of(i);
                let Some(bytes) = at.checked_add(2).and_then(|e| row.get(at..e)) else {
                    break;
                };
                let (Some(&b0), Some(&b1)) = (bytes.first(), bytes.get(1)) else {
                    break;
                };
                let raw = if comp.endian == Endian::Little {
                    u32::from(b0) | (u32::from(b1) << 8)
                } else {
                    u32::from(b1) | (u32::from(b0) << 8)
                };
                *o = i32::try_from((raw >> shift) & mask).unwrap_or(i32::MAX);
                count = i + 1;
            }
            count
        }
        _ => read_row_general(row, comp, out),
    }
}

/// The definition every specialisation above is checked against.
pub fn read_row_general(row: &[u8], comp: &ComponentLayout, out: &mut [i32]) -> usize {
    let n = out.len().min(comp.width as usize);
    let mask = mask_of(comp.depth);
    let mut count = 0;
    for (i, o) in out.iter_mut().enumerate().take(n) {
        let Some(raw) = load(row, comp, i) else {
            break;
        };
        *o = i32::try_from((raw >> comp.shift) & mask).unwrap_or(i32::MAX);
        count = i + 1;
    }
    count
}

/// Write up to `src.len()` samples of one component into `row`.
///
/// Components that share a container (`rgb565`) are merged with a read-modify-
/// write, so the order components are written in does not matter.
pub fn write_row(row: &mut [u8], comp: &ComponentLayout, src: &[i32]) {
    let n = src.len().min(comp.width as usize);
    let max = i32::try_from(comp.max_value()).unwrap_or(i32::MAX);
    let host = comp.container == 1 || comp.endian == crate::geometry::host_endian();
    if comp.container == 1 && comp.shift == 0 && comp.depth == 8 {
        for (i, &v) in src.iter().enumerate().take(n) {
            let at = comp.byte_of(i);
            let Some(slot) = row.get_mut(at) else { break };
            *slot = v.clamp(0, max) as u8;
        }
        return;
    }
    if comp.container == 2 && comp.shift == 0 && comp.depth == 16 && host {
        for (i, &v) in src.iter().enumerate().take(n) {
            let at = comp.byte_of(i);
            let Some(slot) = at.checked_add(2).and_then(|e| row.get_mut(at..e)) else {
                break;
            };
            let bytes = match comp.endian {
                Endian::Little => (v.clamp(0, max) as u16).to_le_bytes(),
                Endian::Big => (v.clamp(0, max) as u16).to_be_bytes(),
            };
            slot.copy_from_slice(&bytes);
        }
        return;
    }
    write_row_general(row, comp, src);
}

/// The definition every specialisation above is checked against.
pub fn write_row_general(row: &mut [u8], comp: &ComponentLayout, src: &[i32]) {
    let n = src.len().min(comp.width as usize);
    let mask = mask_of(comp.depth);
    let max = i32::try_from(comp.max_value()).unwrap_or(i32::MAX);
    for (i, &v) in src.iter().enumerate().take(n) {
        let field = ((v.clamp(0, max) as u32) & mask) << comp.shift;
        let keep = !(mask << comp.shift);
        let Some(old) = load(row, comp, i) else { break };
        store(row, comp, i, (old & keep) | field);
    }
}

/// `(1 << depth) - 1`, without shifting by 32.
#[must_use]
pub const fn mask_of(depth: u8) -> u32 {
    if depth >= 32 {
        u32::MAX
    } else {
        (1u32 << depth) - 1
    }
}

fn load(row: &[u8], comp: &ComponentLayout, i: usize) -> Option<u32> {
    let at = comp.byte_of(i);
    let bytes = row.get(at..at.checked_add(usize::from(comp.container))?)?;
    let mut v = 0u32;
    match comp.endian {
        Endian::Little => {
            for (k, &b) in bytes.iter().enumerate() {
                v |= u32::from(b) << (8 * k);
            }
        }
        Endian::Big => {
            for &b in bytes {
                v = (v << 8) | u32::from(b);
            }
        }
    }
    Some(v)
}

fn store(row: &mut [u8], comp: &ComponentLayout, i: usize, v: u32) {
    let at = comp.byte_of(i);
    let Some(end) = at.checked_add(usize::from(comp.container)) else {
        return;
    };
    let Some(bytes) = row.get_mut(at..end) else {
        return;
    };
    let len = bytes.len();
    match comp.endian {
        Endian::Little => {
            for (k, b) in bytes.iter_mut().enumerate() {
                *b = (v >> (8 * k)) as u8;
            }
        }
        Endian::Big => {
            for (k, b) in bytes.iter_mut().enumerate() {
                *b = (v >> (8 * (len - 1 - k))) as u8;
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;
    use crate::geometry::ComponentLayout;
    use vaco_pixfmt::PixFmt;

    #[test]
    fn specialised_and_general_readers_agree_on_every_format() {
        let mut row = [0u8; 512];
        for (i, b) in row.iter_mut().enumerate() {
            *b = (i.wrapping_mul(37) ^ 0x5a) as u8;
        }
        for &fmt in PixFmt::all() {
            let Ok(layout) = ComponentLayout::derive(fmt, 16, 16) else {
                continue;
            };
            for c in 0..layout.ncomp {
                let comp = layout.comps.get(c).expect("in range");
                let mut a = [0i32; 16];
                let mut b = [0i32; 16];
                let na = read_row(&row, comp, &mut a);
                let nb = read_row_general(&row, comp, &mut b);
                assert_eq!(na, nb, "{} comp {c}", fmt.name());
                assert_eq!(a, b, "{} comp {c}", fmt.name());
            }
        }
    }

    #[test]
    fn write_then_read_round_trips_every_format() {
        for &fmt in PixFmt::all() {
            let Ok(layout) = ComponentLayout::derive(fmt, 16, 16) else {
                continue;
            };
            let mut planes = [[0u8; 512]; 4];
            let mut expect = [[0i32; 16]; 4];
            let mut widths = [0usize; 4];
            for c in 0..layout.ncomp {
                let comp = layout.comps.get(c).expect("in range");
                let max = comp.max_value() as i32;
                let n = (comp.width as usize).min(16);
                let vals: Vec<i32> = (0..n as i32)
                    .map(|i| (i * 7 + c as i32 * 3) % (max + 1))
                    .collect();
                let Some(dst) = planes.get_mut(usize::from(comp.plane)) else {
                    continue;
                };
                write_row(dst, comp, &vals);
                if let Some(slot) = expect.get_mut(c) {
                    for (o, v) in slot.iter_mut().zip(&vals) {
                        *o = *v;
                    }
                }
                widths[c] = n;
            }
            for c in 0..layout.ncomp {
                let comp = layout.comps.get(c).expect("in range");
                let Some(src) = planes.get(usize::from(comp.plane)) else {
                    continue;
                };
                let n = widths[c];
                let mut got = [0i32; 16];
                read_row(src, comp, &mut got);
                assert_eq!(
                    &got[..n],
                    &expect.get(c).expect("in range")[..n],
                    "{} comp {c} did not round trip",
                    fmt.name()
                );
            }
        }
    }

    #[test]
    fn a_short_row_truncates_rather_than_panicking() {
        let layout = ComponentLayout::derive(PixFmt::Rgb24, 8, 1).expect("supported");
        let comp = layout.comps.first().expect("r");
        let row = [1u8, 2, 3, 4];
        let mut out = [0i32; 8];
        assert_eq!(read_row(&row, comp, &mut out), 2);
    }
}
