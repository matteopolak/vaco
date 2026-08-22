//! Turning a [`PixFmt`] descriptor into read/write instructions.
//!
//! This module is the whole of the crate's format knowledge. Everything above
//! it works on `i32` component planes and never learns whether the picture
//! arrived as `bgr24` or `nv21`. That is what collapses an *n*×*m* format matrix
//! into *n* + *m* pieces of code (plan 17 §A.1).
//!
//! # The geometry model
//!
//! `vaco-pixfmt` gives, per logical channel `c`, a `(plane, step, offset, shift,
//! depth)` tuple. This module adds the four facts a reader also needs:
//!
//! * **How wide the component is.** Channels 1 and 2 — and only those — follow
//!   the chroma decimation. `ceil` division, so an odd width still has a chroma
//!   sample for its last pixel. This mirrors `PixFmt::plane_width`'s rule, which
//!   keys on the *component* index rather than the plane, and it is why a packed
//!   `yuyv422` (all three channels in plane 0) still decimates correctly.
//! * **How big the container is.** `step` is the distance between samples, not
//!   the size of the load: `p010`'s chroma has `step = 4` but each sample is a
//!   16-bit word. The container is derived from `shift + depth`, rounded up to
//!   1, 2 or 4 bytes.
//! * **Which byte order to load it in.**
//! * **Where the sample sits inside the container**, which is `shift`.
//!
//! # What is rejected, and why
//!
//! Formats whose samples are not addressable by "read N bytes, shift, mask" are
//! refused at plan time with [`Error::Unsupported`] rather than mis-read:
//! `BITSTREAM` (sub-byte packing), `PALETTE` (needs side data), `BAYER` (needs
//! demosaicing), `FLOAT`, `HW_ACCEL`, and depths above 16.

use vaco_core::{Error, Result};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// The most component channels any format in the table carries.
pub const MAX_COMPS: usize = 4;

/// Byte order of a multi-byte container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    /// Least significant byte first.
    Little,
    /// Most significant byte first.
    Big,
}

/// How to find one logical channel's samples inside a frame.
///
/// Constructed by [`ComponentLayout::derive`]; never written by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentLayout {
    /// Plane this channel lives in.
    pub plane: u8,
    /// Byte offset of sample 0 within a row.
    pub offset: usize,
    /// Byte distance between consecutive samples.
    pub step: usize,
    /// Container width in bytes: 1, 2 or 4.
    pub container: u8,
    /// Byte order of the container. Irrelevant when `container == 1`.
    pub endian: Endian,
    /// Right shift applied after loading the container.
    pub shift: u8,
    /// Significant bits.
    pub depth: u8,
    /// Samples per row of this component.
    pub width: u32,
    /// Rows of this component.
    pub height: u32,
}

impl ComponentLayout {
    /// The largest code value this component can hold.
    #[must_use]
    pub const fn max_value(&self) -> u32 {
        // `depth` is validated to 1..=16 by `derive`, so the shift is in range.
        (1u32 << self.depth) - 1
    }

    /// Byte offset of sample `x` within its row.
    #[must_use]
    pub const fn byte_of(&self, x: usize) -> usize {
        self.offset + x * self.step
    }

    /// Bytes the last sample of a row reaches, i.e. the minimum row length.
    #[must_use]
    pub const fn row_span(&self) -> usize {
        if self.width == 0 {
            return 0;
        }
        self.offset + (self.width as usize - 1) * self.step + self.container as usize
    }
}

/// Every component of one image, plus the facts the planner asks about the
/// format as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatLayout {
    /// The format this was derived from.
    pub format: PixFmt,
    /// Frame width in luma samples.
    pub width: u32,
    /// Frame height in luma samples.
    pub height: u32,
    /// One entry per logical channel actually present.
    pub comps: [ComponentLayout; MAX_COMPS],
    /// How many entries of `comps` are meaningful.
    pub ncomp: usize,
    /// Number of planes.
    pub planes: usize,
    /// log2 horizontal chroma decimation.
    pub log2_w: u8,
    /// log2 vertical chroma decimation.
    pub log2_h: u8,
    /// Components are R/G/B rather than Y/Cb/Cr.
    pub rgb: bool,
    /// An alpha channel is present.
    pub alpha: bool,
}

/// A neutral layout used to fill unused `comps` slots.
const EMPTY_COMP: ComponentLayout = ComponentLayout {
    plane: 0,
    offset: 0,
    step: 1,
    container: 1,
    endian: Endian::Little,
    shift: 0,
    depth: 8,
    width: 0,
    height: 0,
};

impl ComponentLayout {
    /// Derive the layout of every channel of `format` at `width` × `height`.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a format this crate cannot address — see the
    /// module docs for the list — and [`Error::InvalidData`] for a zero
    /// dimension.
    pub fn derive(format: PixFmt, width: u32, height: u32) -> Result<FormatLayout> {
        let d = format.descriptor();
        reject_unsupported(format, d.flags)?;
        if width == 0 || height == 0 {
            return Err(Error::InvalidData("image dimension is zero"));
        }

        let mut comps = [EMPTY_COMP; MAX_COMPS];
        let ncomp = d.components.len().min(MAX_COMPS);
        if ncomp == 0 {
            return Err(Error::Unsupported("pixel format has no components"));
        }
        for (i, slot) in comps.iter_mut().enumerate().take(ncomp) {
            let Some(c) = d.components.get(i) else {
                return Err(Error::InvalidData("component table shorter than declared"));
            };
            if c.depth == 0 || c.depth > 16 {
                return Err(Error::Unsupported("component depth outside 1..=16"));
            }
            // The container is the *storage unit*, which is not the same as
            // this component's own width: `rgb565`'s blue field is five bits at
            // shift zero, but it lives in a 16-bit word, and reading one byte
            // of it picks the wrong half on a big-endian host. So the width is
            // the maximum over every component sharing the same slot.
            let bits = container_bits(d.components, c.plane, c.offset);
            let container = match bits.div_ceil(8) {
                0 | 1 => 1u8,
                2 => 2,
                3 | 4 => 4,
                _ => return Err(Error::Unsupported("component wider than 32 bits")),
            };
            if usize::from(c.step) < usize::from(container) {
                return Err(Error::Unsupported(
                    "component step narrower than its container",
                ));
            }
            let chroma = i == 1 || i == 2;
            let (sw, sh) = if chroma {
                (d.log2_chroma_w, d.log2_chroma_h)
            } else {
                (0, 0)
            };
            *slot = ComponentLayout {
                plane: c.plane,
                offset: usize::from(c.offset),
                step: usize::from(c.step),
                container,
                endian: if d.flags.contains(PixFmtFlags::BIG_ENDIAN) {
                    Endian::Big
                } else {
                    Endian::Little
                },
                shift: c.shift,
                depth: c.depth,
                width: ceil_shr(width, sw),
                height: ceil_shr(height, sh),
            };
        }

        check_no_overlap(&comps, ncomp)?;

        Ok(FormatLayout {
            format,
            width,
            height,
            comps,
            ncomp,
            planes: usize::from(d.planes),
            log2_w: d.log2_chroma_w,
            log2_h: d.log2_chroma_h,
            rgb: d.flags.contains(PixFmtFlags::RGB),
            alpha: d.flags.contains(PixFmtFlags::ALPHA),
        })
    }
}

impl FormatLayout {
    /// Layout of channel `c`, or `None` if the format does not carry it.
    #[must_use]
    pub fn comp(&self, c: usize) -> Option<&ComponentLayout> {
        if c >= self.ncomp {
            return None;
        }
        self.comps.get(c)
    }

    /// The deepest channel.
    #[must_use]
    pub fn max_depth(&self) -> u8 {
        self.comps
            .iter()
            .take(self.ncomp)
            .map(|c| c.depth)
            .max()
            .unwrap_or(8)
    }

    /// Whether every channel is byte-aligned, one channel per container, in
    /// host order — the shape a plane copy is allowed for.
    #[must_use]
    pub fn is_trivially_addressed(&self) -> bool {
        self.comps.iter().take(self.ncomp).all(|c| {
            c.shift == 0
                && u32::from(c.depth) == u32::from(c.container) * 8
                && (c.container == 1 || c.endian == host_endian())
        })
    }
}

/// The byte order this build runs in.
#[must_use]
pub const fn host_endian() -> Endian {
    if cfg!(target_endian = "big") {
        Endian::Big
    } else {
        Endian::Little
    }
}

/// `ceil(v / 2^s)`, saturating rather than panicking on a silly shift.
#[must_use]
pub const fn ceil_shr(v: u32, s: u8) -> u32 {
    if s == 0 {
        return v;
    }
    if s >= 32 {
        return if v != 0 { 1 } else { 0 };
    }
    // `v + 2^s - 1` can overflow for a huge `v`; do it in 64 bits.
    (((v as u64) + (1u64 << s) - 1) >> s) as u32
}

/// Reject a descriptor whose components would read each other's bytes.
///
/// Two components in one plane either share a storage unit — same offset and
/// step, disjoint bit fields, which is how `rgb565` works — or they must occupy
/// disjoint bytes. `uyyvyy411` satisfies neither: its luma is described as
/// `step = 1, offset = 1`, which walks straight through the chroma samples at
/// bytes 0, 3, 6. The layout is real, the linear description of it is not, and a
/// reader that trusted the description would silently return chroma as luma.
///
/// Detected rather than special-cased by name, so a future table entry with the
/// same problem is refused too.
fn check_no_overlap(comps: &[ComponentLayout; MAX_COMPS], ncomp: usize) -> Result<()> {
    for i in 0..ncomp {
        for j in (i + 1)..ncomp {
            let (Some(a), Some(b)) = (comps.get(i), comps.get(j)) else {
                continue;
            };
            if a.plane != b.plane {
                continue;
            }
            if a.offset == b.offset && a.step == b.step {
                // Shared storage unit: the bit fields must not overlap.
                let (a0, a1) = (a.shift, a.shift.saturating_add(a.depth));
                let (b0, b1) = (b.shift, b.shift.saturating_add(b.depth));
                if a0 < b1 && b0 < a1 {
                    return Err(Error::Unsupported(
                        "pixel format with overlapping bit fields",
                    ));
                }
                continue;
            }
            if bytes_collide(a, b) {
                return Err(Error::Unsupported(
                    "pixel format whose component description overlaps itself",
                ));
            }
        }
    }
    Ok(())
}

/// Whether `a` and `b` can ever address the same byte.
///
/// The addresses are `off + k * step`, so their differences are exactly the
/// residue class of `off_a - off_b` modulo `gcd(step_a, step_b)`. A collision is
/// then a member of that class inside `(-len_b, len_a)`.
#[allow(
    clippy::integer_division,
    reason = "a residue-class walk; the divisor is a gcd and is never zero"
)]
fn bytes_collide(first: &ComponentLayout, second: &ComponentLayout) -> bool {
    let step = i64::try_from(gcd(first.step.max(1), second.step.max(1))).unwrap_or(1);
    let (len_a, len_b) = (i64::from(first.container), i64::from(second.container));
    let diff = i64::try_from(first.offset).unwrap_or(0) - i64::try_from(second.offset).unwrap_or(0);
    let residue = diff.rem_euclid(step);
    // Candidates in `(-len_b, len_a)` congruent to `residue` modulo `step`.
    let mut k = (-len_b - residue) / step - 1;
    let stop = (len_a - residue) / step + 1;
    while k <= stop {
        let v = residue + k * step;
        if v > -len_b && v < len_a {
            return true;
        }
        k += 1;
    }
    false
}

const fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    if a == 0 { 1 } else { a }
}

/// Bits spanned by the storage unit at `(plane, offset)`.
fn container_bits(comps: &[vaco_pixfmt::Component], plane: u8, offset: u8) -> u32 {
    comps
        .iter()
        .filter(|c| c.plane == plane && c.offset == offset)
        .map(|c| u32::from(c.shift) + u32::from(c.depth))
        .max()
        .unwrap_or(8)
}

fn reject_unsupported(format: PixFmt, flags: PixFmtFlags) -> Result<()> {
    let refuse = |what: &'static str| Err(Error::Unsupported(what));
    if flags.contains(PixFmtFlags::HW_ACCEL) {
        return refuse("hardware pixel format in the scaler");
    }
    if flags.contains(PixFmtFlags::BITSTREAM) {
        return refuse("sub-byte packed pixel format");
    }
    if flags.contains(PixFmtFlags::PALETTE) {
        return refuse("palettised pixel format");
    }
    if flags.contains(PixFmtFlags::BAYER) {
        return refuse("Bayer mosaic pixel format");
    }
    if flags.contains(PixFmtFlags::FLOAT) {
        return refuse("floating-point pixel format");
    }
    if flags.contains(PixFmtFlags::XYZ) {
        return refuse("XYZ pixel format");
    }
    if format.plane_count() == 0 {
        return refuse("pixel format with no planes");
    }
    Ok(())
}

/// Whether this crate can read `fmt` as a source.
#[must_use]
pub fn supports_input(fmt: PixFmt) -> bool {
    ComponentLayout::derive(fmt, 16, 16).is_ok()
}

/// Whether this crate can write `fmt` as a destination.
#[must_use]
pub fn supports_output(fmt: PixFmt) -> bool {
    supports_input(fmt)
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

    #[test]
    fn yuv420p_geometry() {
        let l = ComponentLayout::derive(PixFmt::Yuv420p, 7, 5).expect("supported");
        assert_eq!(l.ncomp, 3);
        let y = l.comp(0).expect("luma");
        assert_eq!((y.width, y.height), (7, 5));
        let u = l.comp(1).expect("cb");
        // ceil: an odd dimension still gets a chroma sample for its last pixel.
        assert_eq!((u.width, u.height), (4, 3));
        assert_eq!(u.plane, 1);
        assert_eq!(u.step, 1);
    }

    #[test]
    fn packed_yuyv_decimates_by_component_not_by_plane() {
        let l = ComponentLayout::derive(PixFmt::Yuyv422, 5, 3).expect("supported");
        let y = l.comp(0).expect("luma");
        let u = l.comp(1).expect("cb");
        let v = l.comp(2).expect("cr");
        assert_eq!((y.width, y.step, y.offset), (5, 2, 0));
        assert_eq!((u.width, u.step, u.offset), (3, 4, 1));
        assert_eq!((v.width, v.step, v.offset), (3, 4, 3));
        // Every read stays inside the row the allocator sized.
        let row = PixFmt::Yuyv422.min_stride(5, 0);
        assert!(y.row_span() <= row && u.row_span() <= row && v.row_span() <= row);
    }

    #[test]
    fn p010_container_is_wider_than_a_byte_but_narrower_than_step() {
        let l = ComponentLayout::derive(PixFmt::P010le, 8, 8).expect("supported");
        let u = l.comp(1).expect("cb");
        assert_eq!((u.step, u.container, u.shift, u.depth), (4, 2, 6, 10));
        assert_eq!(u.offset, 0);
        let v = l.comp(2).expect("cr");
        assert_eq!(v.offset, 2);
    }

    #[test]
    fn rgb565_is_three_fields_of_one_container() {
        let l = ComponentLayout::derive(PixFmt::Rgb565le, 4, 4).expect("supported");
        for c in 0..3 {
            let c = l.comp(c).expect("component");
            assert_eq!(c.container, 2);
            assert_eq!(c.step, 2);
        }
        assert_eq!(l.comp(0).expect("r").depth, 5);
        assert_eq!(l.comp(1).expect("g").depth, 6);
    }

    #[test]
    fn unsupported_families_are_refused_not_misread() {
        for fmt in [
            PixFmt::Pal8,
            PixFmt::MonoWhite,
            PixFmt::BayerBggr8,
            PixFmt::Gbrpf32le,
            PixFmt::Vaapi,
        ] {
            assert!(
                ComponentLayout::derive(fmt, 16, 16).is_err(),
                "{} should be refused",
                fmt.name()
            );
        }
    }

    #[test]
    fn every_supported_format_addresses_inside_its_own_rows() {
        // The geometry contract: no read this module describes may reach past
        // the row length `vaco-pixfmt` tells the allocator to reserve.
        for &fmt in PixFmt::all() {
            for (w, h) in [(1, 1), (2, 3), (7, 5), (16, 16), (63, 31)] {
                let Ok(layout) = ComponentLayout::derive(fmt, w, h) else {
                    continue;
                };
                for i in 0..layout.ncomp {
                    let c = layout.comps.get(i).expect("in range");
                    let stride = fmt.min_stride(w, c.plane);
                    assert!(
                        c.row_span() <= stride,
                        "{} comp {i} spans {} > stride {stride} at {w}x{h}",
                        fmt.name(),
                        c.row_span()
                    );
                    let rows = fmt.plane_height(h, c.plane);
                    assert!(
                        c.height <= rows,
                        "{} comp {i} has {} rows > plane {}",
                        fmt.name(),
                        c.height,
                        rows
                    );
                }
            }
        }
    }
}
