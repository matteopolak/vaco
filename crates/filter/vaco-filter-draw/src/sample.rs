//! Generic component pack/unpack against a [`vaco_pixfmt::Component`]
//! descriptor — the piece that lets [`crate::fill`] and [`crate::blend`]
//! work on any packed-or-planar, 8-or-16-bit format without a per-format
//! match arm.
//!
//! # Container model
//!
//! A component's container is `depth.div_ceil(8)` bytes wide (1 byte for
//! `depth <= 8`, 2 bytes for `9..=16`), located at byte offset `x * step +
//! offset` within the row, and the significant value sits at `raw >>
//! shift`, masked to `depth` bits. Every format this crate's callers use
//! today has `shift == 0` (confirmed against `vaco-pixfmt`'s generated
//! table for the full 8/9/10/12/14/16-bit planar and packed families), so
//! the common path never needs a read-modify-write; the shifted path is
//! still correct, in case a future format packs two components into one
//! container.
//!
//! Depths above 16 (there are none today — `PixFmt::max_depth` tops out at
//! 16 for every non-float, non-hardware format) and the `BITSTREAM`
//! sub-byte packing are out of scope; [`container_bytes`] returns `None`
//! for `depth > 16`, which every caller here treats as
//! [`vaco_core::Error::Unsupported`].

use vaco_pixfmt::Component;

/// Bytes the component's container occupies, or `None` if `depth` is
/// outside what this module supports (see the module doc).
#[must_use]
pub const fn container_bytes(comp: &Component) -> Option<usize> {
    match comp.depth {
        1..=8 => Some(1),
        9..=16 => Some(2),
        _ => None,
    }
}

/// Read one component's value from pixel `x` of `row`.
///
/// Returns `None` if the container would read past `row`'s end or `depth`
/// is unsupported — the caller degrades (skips the pixel) rather than
/// panics, matching this crate's plane accessors.
#[must_use]
pub fn read(row: &[u8], x: usize, comp: &Component, big_endian: bool) -> Option<u32> {
    let bytes = container_bytes(comp)?;
    let addr = x
        .checked_mul(usize::from(comp.step))?
        .checked_add(usize::from(comp.offset))?;
    let raw = if bytes == 1 {
        u32::from(*row.get(addr)?)
    } else {
        let a = *row.get(addr)?;
        let b = *row.get(addr.checked_add(1)?)?;
        if big_endian {
            u32::from(u16::from_be_bytes([a, b]))
        } else {
            u32::from(u16::from_le_bytes([a, b]))
        }
    };
    let mask = mask_for(comp.depth);
    Some((raw >> comp.shift) & mask)
}

/// Write one component's value into pixel `x` of `row`, preserving any
/// bits outside `depth` in a shared container (relevant only for `shift !=
/// 0`, which no format this crate handles today actually uses).
///
/// Returns `false` (writes nothing) under the same out-of-bounds/
/// unsupported-depth conditions [`read`] returns `None` for.
pub fn write(row: &mut [u8], x: usize, comp: &Component, value: u32, big_endian: bool) -> bool {
    let Some(bytes) = container_bytes(comp) else {
        return false;
    };
    let Some(addr) = x
        .checked_mul(usize::from(comp.step))
        .and_then(|v| v.checked_add(usize::from(comp.offset)))
    else {
        return false;
    };
    let mask = mask_for(comp.depth);
    let shifted = (value & mask) << comp.shift;
    if bytes == 1 {
        let Some(byte) = row.get_mut(addr) else {
            return false;
        };
        let keep = !((mask << comp.shift) as u8);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bytes == 1 means depth <= 8, so shifted fits in a u8"
        )]
        {
            *byte = (*byte & keep) | (shifted as u8);
        }
        true
    } else {
        let Some(end) = addr.checked_add(2) else {
            return false;
        };
        let Some(pair) = row.get_mut(addr..end) else {
            return false;
        };
        let (Some(&lo), Some(&hi)) = (pair.first(), pair.get(1)) else {
            return false;
        };
        let existing = if big_endian {
            u16::from_be_bytes([lo, hi])
        } else {
            u16::from_le_bytes([lo, hi])
        };
        let keep = !((mask << comp.shift) as u16);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bytes == 2 means depth <= 16, so shifted fits in a u16"
        )]
        let merged = (existing & keep) | (shifted as u16);
        let out = if big_endian {
            merged.to_be_bytes()
        } else {
            merged.to_le_bytes()
        };
        pair.copy_from_slice(&out);
        true
    }
}

const fn mask_for(depth: u8) -> u32 {
    if depth >= 32 {
        u32::MAX
    } else {
        (1u32 << depth) - 1
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn comp(step: u8, offset: u8, shift: u8, depth: u8) -> Component {
        Component {
            plane: 0,
            step,
            offset,
            shift,
            depth,
        }
    }

    #[test]
    fn eight_bit_round_trips() {
        let c = comp(1, 0, 0, 8);
        let mut row = [0u8; 4];
        assert!(write(&mut row, 2, &c, 200, false));
        assert_eq!(read(&row, 2, &c, false), Some(200));
    }

    #[test]
    fn ten_bit_le_round_trips_and_masks() {
        let c = comp(2, 0, 0, 10);
        let mut row = [0u8; 4];
        assert!(write(&mut row, 0, &c, 0x3ff, false));
        assert_eq!(read(&row, 0, &c, false), Some(0x3ff));
        // A value wider than 10 bits is masked down on write.
        assert!(write(&mut row, 0, &c, 0xffff, false));
        assert_eq!(read(&row, 0, &c, false), Some(0x3ff));
    }

    #[test]
    fn packed_rgba_addresses_each_component_by_offset() {
        let r = comp(4, 0, 0, 8);
        let g = comp(4, 1, 0, 8);
        let b = comp(4, 2, 0, 8);
        let a = comp(4, 3, 0, 8);
        let mut row = [0u8; 8]; // two pixels
        assert!(write(&mut row, 1, &r, 10, false));
        assert!(write(&mut row, 1, &g, 20, false));
        assert!(write(&mut row, 1, &b, 30, false));
        assert!(write(&mut row, 1, &a, 40, false));
        assert_eq!(row, [0, 0, 0, 0, 10, 20, 30, 40]);
        assert_eq!(read(&row, 1, &r, false), Some(10));
        assert_eq!(read(&row, 1, &a, false), Some(40));
    }

    #[test]
    fn out_of_bounds_reads_and_writes_degrade_rather_than_panic() {
        let c = comp(2, 0, 0, 16);
        let row = [0u8; 3]; // too short for a 2-byte container at x=1
        assert_eq!(read(&row, 1, &c, false), None);
        let mut row = [0u8; 3];
        assert!(!write(&mut row, 1, &c, 5, false));
    }

    #[test]
    fn big_endian_containers_use_the_opposite_byte_order() {
        let c = comp(2, 0, 0, 16);
        let mut row = [0u8; 2];
        assert!(write(&mut row, 0, &c, 0x1234, true));
        assert_eq!(row, [0x12, 0x34]);
        assert_eq!(read(&row, 0, &c, true), Some(0x1234));
    }
}
