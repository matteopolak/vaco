//! Generic, bit-depth-independent pixel component access.
//!
//! Every filter in this crate reads or writes individual *logical
//! components* (Y/U/V/R/G/B/A) rather than whole planes, and has to do it for
//! 8-bit and sub-16-bit-in-a-16-bit-container formats alike (`yuv420p` next
//! to `yuv420p10le`). Rather than special-case each depth per filter, this
//! module reads `PixFmt::descriptor()`'s [`Component`] table — plane index,
//! byte step, byte offset, post-load shift, significant bit depth — and
//! turns it into `u16`-in, `u16`-out accessors that work for any addressable
//! format.
//!
//! # What "addressable" excludes
//!
//! [`is_addressable`] rejects [`PixFmtFlags::BITSTREAM`] (sub-byte packing,
//! e.g. `bgr4`), [`PixFmtFlags::PALETTE`] (samples are palette indices, not
//! component values), [`PixFmtFlags::HW_ACCEL`] (no host-addressable planes
//! at all), [`PixFmtFlags::FLOAT`] (components are IEEE-754, not integers —
//! `grayf32` and friends) and any component deeper than 16 bits. Every
//! filter in this crate checks it at `configure`/`create` time and returns a
//! clean [`vaco_core::Error::Unsupported`] rather than misreading bytes.
//!
//! **`step` is a pixel stride, not a container width.** `Component::step`
//! is "distance between consecutive samples of this component" — for a
//! packed format that is the whole pixel's byte width (`rgb24`'s step is
//! 3, `rgba`'s is 4), not how many bytes *this one sample* occupies. The
//! byte width of one sample is `depth <= 8 ? 1 : 2`, independent of `step`;
//! [`read`]/[`write`] use `step` only to locate the component's byte
//! offset within the row and `depth` to decide how many bytes to read
//! there. Conflating the two was a real bug caught by this crate's own
//! tests: an early version rejected `rgb24`/`rgba` as "not addressable"
//! because it checked `step` where it should have checked `depth`.
//!
//! # Channel roles are positional, not named
//!
//! [`PixFmtDescriptor::components`] is indexed by logical channel: 0 is Y or
//! R, 1 is U or G, 2 is V or B, 3 is alpha (`vaco-pixfmt`'s own doc comment).
//! [`PixFmt::is_rgb`] tells a filter which naming applies to a given format.

use vaco_pixfmt::{Component, PixFmt, PixFmtFlags};

/// Whether this crate's generic per-sample accessors can address `fmt`.
#[must_use]
pub(crate) fn is_addressable(fmt: PixFmt) -> bool {
    let d = fmt.descriptor();
    if d.planes == 0 {
        return false;
    }
    if d.flags.intersects(
        PixFmtFlags::BITSTREAM
            | PixFmtFlags::PALETTE
            | PixFmtFlags::HW_ACCEL
            | PixFmtFlags::FLOAT
            | PixFmtFlags::BAYER,
    ) {
        return false;
    }
    d.components.iter().all(|c| c.depth <= 16)
}

/// The component descriptor for logical channel `ch`, if the format has one.
#[must_use]
pub(crate) fn component(fmt: PixFmt, ch: usize) -> Option<Component> {
    fmt.descriptor().components.get(ch).copied()
}

/// `(1 << depth) - 1`, the maximum representable sample value for `comp`.
#[must_use]
pub(crate) const fn max_value(comp: Component) -> u16 {
    max_for_depth(comp.depth)
}

/// `(1 << depth) - 1`, saturating at `u16::MAX` for a 16-bit container.
#[must_use]
pub(crate) const fn max_for_depth(depth: u8) -> u16 {
    if depth >= 16 {
        u16::MAX
    } else {
        (1u16 << depth) - 1
    }
}

/// Whether this crate's float accessors ([`read_float`]/[`write_float`]) can
/// address `fmt` (interface gap 15, `planning/INTERFACE-GAPS.md`).
///
/// The complement of [`is_addressable`], not a superset of it: a format
/// passes here only if every component is a 32-bit IEEE-754 float
/// (`gbrpf32le` and its siblings), never both this and [`is_addressable`] at
/// once. 16-bit float (`grayf16le` and friends, `depth == 16` with
/// [`PixFmtFlags::FLOAT`] set) is deliberately excluded: reinterpreting two
/// raw bytes as an `f32` needs four, so a half-precision component is a
/// different bit layout entirely, not a narrower case of this function — and
/// no filter in this crate reads one yet, so there is nothing to measure a
/// conversion against.
#[must_use]
pub(crate) fn is_float_addressable(fmt: PixFmt) -> bool {
    let d = fmt.descriptor();
    if d.planes == 0 {
        return false;
    }
    if !d.flags.contains(PixFmtFlags::FLOAT) {
        return false;
    }
    if d.flags.intersects(
        PixFmtFlags::BITSTREAM | PixFmtFlags::PALETTE | PixFmtFlags::HW_ACCEL | PixFmtFlags::BAYER,
    ) {
        return false;
    }
    d.components.iter().all(|c| c.depth == 32)
}

/// Read one 32-bit float component sample at plane-local column `x`.
///
/// Reinterprets the four raw bytes at the component's position as an
/// IEEE-754 `f32`, respecting `big_endian` the same way [`read`] does for an
/// integer component. Returns `0.0` past the end of the row, matching
/// [`read`]'s out-of-bounds contract — the caller's loop bound is the
/// plane's own geometry, so this only triggers on a genuinely malformed
/// frame.
///
/// Only meaningful when [`is_float_addressable`] holds for the frame's
/// format; `comp.depth` is not checked here, since every caller has already
/// gated on that.
#[must_use]
pub(crate) fn read_float(row: &[u8], x: usize, comp: Component, big_endian: bool) -> f32 {
    let off = x.saturating_mul(comp.step as usize);
    let off = off.saturating_add(comp.offset as usize);
    let bytes = [
        row.get(off).copied().unwrap_or(0),
        row.get(off.saturating_add(1)).copied().unwrap_or(0),
        row.get(off.saturating_add(2)).copied().unwrap_or(0),
        row.get(off.saturating_add(3)).copied().unwrap_or(0),
    ];
    if big_endian {
        f32::from_be_bytes(bytes)
    } else {
        f32::from_le_bytes(bytes)
    }
}

/// Write one 32-bit float component sample at plane-local column `x`.
///
/// Unlike [`write`], `value` is never masked — an IEEE-754 bit pattern has no
/// "significant depth" to clamp to, the way an integer component's does.
pub(crate) fn write_float(row: &mut [u8], x: usize, comp: Component, big_endian: bool, value: f32) {
    let off = x.saturating_mul(comp.step as usize);
    let off = off.saturating_add(comp.offset as usize);
    let bytes = if big_endian { value.to_be_bytes() } else { value.to_le_bytes() };
    for (i, b) in bytes.into_iter().enumerate() {
        if let Some(slot) = row.get_mut(off.saturating_add(i)) {
            *slot = b;
        }
    }
}

/// Read one component sample at plane-local column `x`, row `y`.
///
/// Returns 0 past the end of the row rather than panicking — the caller's
/// loop bound is the plane's own geometry, so this only triggers on a
/// genuinely malformed frame.
#[must_use]
pub(crate) fn read(row: &[u8], x: usize, comp: Component, big_endian: bool) -> u16 {
    let off = x.saturating_mul(comp.step as usize);
    let off = off.saturating_add(comp.offset as usize);
    if comp.depth <= 8 {
        row.get(off).copied().map_or(0, u16::from)
    } else {
        let b0 = row.get(off).copied().unwrap_or(0);
        let b1 = row.get(off.saturating_add(1)).copied().unwrap_or(0);
        let raw = if big_endian {
            u16::from_be_bytes([b0, b1])
        } else {
            u16::from_le_bytes([b0, b1])
        };
        raw >> comp.shift
    }
}

/// Write one component sample at plane-local column `x`, row `y`.
///
/// `value` is masked to `comp.depth` bits before it is shifted back into
/// place, so a caller that computed a value outside the component's range
/// cannot corrupt neighbouring bits sharing the same container.
pub(crate) fn write(row: &mut [u8], x: usize, comp: Component, big_endian: bool, value: u16) {
    let off = x.saturating_mul(comp.step as usize);
    let off = off.saturating_add(comp.offset as usize);
    let masked = value & max_value(comp);
    if comp.depth <= 8 {
        if let Some(b) = row.get_mut(off) {
            *b = masked as u8;
        }
    } else {
        let shifted = masked << comp.shift;
        let bytes = if big_endian {
            shifted.to_be_bytes()
        } else {
            shifted.to_le_bytes()
        };
        if let Some(b0) = row.get_mut(off) {
            *b0 = bytes[0];
        }
        if let Some(b1) = row.get_mut(off.saturating_add(1)) {
            *b1 = bytes[1];
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
#[allow(
    clippy::float_cmp,
    reason = "round-trips and reference measurements are asserted bit-exact, not approximate"
)]
mod tests {
    use super::*;

    #[test]
    fn eight_bit_round_trips() {
        let comp = Component { plane: 0, step: 1, offset: 0, shift: 0, depth: 8 };
        let mut row = [0u8; 4];
        write(&mut row, 2, comp, false, 0xab);
        assert_eq!(read(&row, 2, comp, false), 0xab);
    }

    #[test]
    fn ten_bit_le_masks_to_depth() {
        let comp = Component { plane: 0, step: 2, offset: 0, shift: 0, depth: 10 };
        let mut row = [0u8; 4];
        write(&mut row, 0, comp, false, 0x3ff);
        assert_eq!(read(&row, 0, comp, false), 0x3ff);
        write(&mut row, 0, comp, false, 0xffff);
        assert_eq!(read(&row, 0, comp, false), 0x3ff);
    }

    #[test]
    fn big_endian_round_trips() {
        let comp = Component { plane: 0, step: 2, offset: 0, shift: 0, depth: 16 };
        let mut row = [0u8; 4];
        write(&mut row, 1, comp, true, 0x1234);
        assert_eq!(read(&row, 1, comp, true), 0x1234);
        assert_eq!(&row[2..4], [0x12, 0x34]);
    }

    #[test]
    fn max_value_saturates_at_sixteen_bits() {
        let comp16 = Component { plane: 0, step: 2, offset: 0, shift: 0, depth: 16 };
        assert_eq!(max_value(comp16), u16::MAX);
        let comp9 = Component { plane: 0, step: 2, offset: 0, shift: 0, depth: 9 };
        assert_eq!(max_value(comp9), 511);
    }

    #[test]
    fn addressable_rejects_bitstream_and_palette() {
        assert!(is_addressable(PixFmt::Yuv420p));
        assert!(!is_addressable(PixFmt::Pal8));
    }

    #[test]
    fn float_addressable_is_the_complement_of_addressable() {
        assert!(is_float_addressable(PixFmt::Gbrpf32le));
        assert!(!is_addressable(PixFmt::Gbrpf32le));
        assert!(is_addressable(PixFmt::Yuv420p));
        assert!(!is_float_addressable(PixFmt::Yuv420p));
    }

    #[test]
    fn float_addressable_rejects_half_precision() {
        // grayf16le is FLOAT-flagged too, but depth 16 — a different bit
        // layout, not a narrower case of the 32-bit accessors.
        assert!(!is_float_addressable(PixFmt::Grayf16le));
    }

    #[test]
    fn float_round_trips_le_and_be() {
        let comp = Component { plane: 0, step: 4, offset: 0, shift: 0, depth: 32 };
        let mut row = [0u8; 8];
        write_float(&mut row, 1, comp, false, -1.5);
        assert_eq!(read_float(&row, 1, comp, false), -1.5);
        write_float(&mut row, 0, comp, true, 3.25);
        assert_eq!(read_float(&row, 0, comp, true), 3.25);
    }

    #[test]
    fn float_read_past_the_row_is_zero_not_a_panic() {
        let comp = Component { plane: 0, step: 4, offset: 0, shift: 0, depth: 32 };
        let row = [0u8; 2];
        assert_eq!(read_float(&row, 0, comp, false), 0.0);
    }
}
