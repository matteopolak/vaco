#![forbid(unsafe_code)]
//! Pixel formats and their descriptor metadata.
//!
//! Every scaler, filter, decoder and encoder asks this crate the same questions:
//! how many planes, which plane holds which channel, how far apart are samples,
//! how deep are they, how is chroma decimated. The answers must be correct,
//! complete, self-consistent, and free at the point of use.
//!
//! # The table is generated
//!
//! There are ~270 formats, each with up to four components of five numbers, plus
//! subsampling and a flag set. Hand-maintaining that guarantees silent drift, and
//! drift here corrupts every frame that touches the format. So
//! [`table`](self) is produced by `cargo xtask gen-pixfmt` from a declarative
//! family description in `xtask/src/gen_pixfmt/source.rs`, and
//! `cargo xtask gen-pixfmt --check` fails CI if the committed table has drifted.
//! Editing `src/table.rs` by hand is therefore impossible to land.
//!
//! See `docs/model/vaco-pixfmt.md`, in particular "how to add a format".
//!
//! # Why the queries are free
//!
//! [`PixFmt::descriptor`] is one array index into a static, in a `const fn`. When
//! the format is a compile-time constant — which it is inside a monomorphised
//! conversion kernel — `fmt.descriptor().components[1].offset` folds to an
//! immediate. When it is dynamic, it is one load from a table of a few kilobytes
//! that stays resident. There is no map, no allocation, and no `Option` in the
//! descriptor.
//!
//! # What this crate deliberately does not contain
//!
//! No conversion code, no format-compatibility scoring, no "best format for"
//! logic. Those need colour knowledge and belong in `vaco-scale`. This crate is
//! pure metadata, which is what keeps it trivially testable.

mod table;

pub use table::PixFmt;

use vaco_core::Error;

bitflags::bitflags! {
    /// Properties of a format that callers branch on.
    ///
    /// These are descriptive, not a bitfield anyone serialises: they exist so a
    /// scaler can ask "does this need a byte swap" without matching on 270
    /// variants.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PixFmtFlags: u16 {
        /// Multi-byte samples are stored most-significant byte first.
        const BIG_ENDIAN = 1 << 0;
        /// Samples are indices into a palette carried beside the frame.
        const PALETTE    = 1 << 1;
        /// Samples are not byte-aligned; `step` and `offset` count bits.
        const BITSTREAM  = 1 << 2;
        /// An opaque handle to a surface owned by a hardware backend.
        const HW_ACCEL   = 1 << 3;
        /// Components live in more than one plane.
        const PLANAR     = 1 << 4;
        /// Components are R/G/B (or a colorimetric space stored like them),
        /// not Y/Cb/Cr.
        const RGB        = 1 << 5;
        /// An alpha component is present.
        const ALPHA      = 1 << 6;
        /// A colour-filter-array mosaic; one sample per pixel.
        const BAYER      = 1 << 7;
        /// Samples are IEEE-754 floats of the stated width.
        const FLOAT      = 1 << 8;
        /// Components are CIE XYZ rather than R/G/B.
        const XYZ        = 1 << 9;
    }
}

/// Where one component lives within a plane.
///
/// `step` and `offset` are byte counts for every format except a
/// [`PixFmtFlags::BITSTREAM`] one, where they count bits — a sub-byte packing
/// has no byte-aligned unit to measure in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
    /// Index of the plane this component lives in.
    pub plane: u8,
    /// Distance between consecutive samples of this component.
    pub step: u8,
    /// Offset of the first sample within the plane row.
    pub offset: u8,
    /// Bits to shift right after loading, for packings that share a container.
    pub shift: u8,
    /// Significant bits.
    pub depth: u8,
}

/// Everything a caller needs to interpret a frame's planes.
///
/// Components are indexed by logical channel: 0 is Y or R, 1 is U or G, 2 is V
/// or B, 3 is alpha. Padding channels are not components — `0rgb` has three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixFmtDescriptor {
    /// The CLI-facing name, as it appears in `-pix_fmts` output.
    pub name: &'static str,
    /// Indexed by logical channel; length is the component count.
    pub components: &'static [Component],
    /// Number of planes. Zero for a hardware surface.
    pub planes: u8,
    /// log2 of horizontal chroma decimation (1 = 4:2:0 / 4:2:2).
    pub log2_chroma_w: u8,
    /// log2 of vertical chroma decimation (1 = 4:2:0).
    pub log2_chroma_h: u8,
    /// Average bits per pixel, padding excluded.
    ///
    /// Truncated: 4:2:0 at 9 bits genuinely averages 13.5 and this reports 13.
    pub bits_per_pixel: u8,
    /// Properties callers branch on.
    pub flags: PixFmtFlags,
}

impl PixFmtDescriptor {
    /// Whether `plane` holds only chroma, and therefore follows the chroma
    /// decimation rather than the frame dimensions.
    ///
    /// Derived from the component table rather than stored: a plane is chroma
    /// exactly when the first component in it is logical channel 1 or 2. Alpha
    /// and luma planes are always full resolution, and an RGB format has zero
    /// decimation so the answer never matters.
    #[expect(
        clippy::indexing_slicing,
        reason = "the loop bound is the slice length; `.get` is not usable here \
                  because `<[T]>::get` is not callable in a const fn"
    )]
    const fn chroma_plane(&self, plane: u8) -> bool {
        let mut i = 0;
        while i < self.components.len() {
            if self.components[i].plane == plane {
                return i == 1 || i == 2;
            }
            i += 1;
        }
        false
    }
}

impl PixFmt {
    /// Every format, in discriminant order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &table::ALL
    }

    /// Constant-time descriptor lookup.
    ///
    /// One array index into a static, in a `const fn`, so a call with a known
    /// format folds to an immediate at the call site.
    #[inline]
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        reason = "discriminants are dense 0..DESCRIPTORS.len(); the generator \
                  emits them and `generated_invariants` asserts it"
    )]
    pub const fn descriptor(self) -> &'static PixFmtDescriptor {
        &table::DESCRIPTORS[self as usize]
    }

    /// The CLI-facing name, as it appears in `-pix_fmts` output.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.descriptor().name
    }

    /// Parse a CLI-facing format name such as `yuv420p`.
    ///
    /// Accepts the canonical name, every declared alias, and — matching the
    /// reference tool — a name without an endianness suffix, which resolves to
    /// the host-endian member of the pair (`gray16` is `gray16le` on x86).
    ///
    /// # Errors
    /// Returns [`Error::Option`] when the name is not a known format.
    pub fn from_name(name: &str) -> Result<Self, Error> {
        if let Some(fmt) = Self::lookup(name) {
            return Ok(fmt);
        }
        let native = if cfg!(target_endian = "big") {
            "be"
        } else {
            "le"
        };
        let widened = format!("{name}{native}");
        if let Some(fmt) = Self::lookup(&widened) {
            return Ok(fmt);
        }
        Err(Error::Option {
            name: "pix_fmt".to_owned(),
            detail: format!("unknown pixel format `{name}`"),
        })
    }

    fn lookup(name: &str) -> Option<Self> {
        let i = table::NAMES_SORTED
            .binary_search_by_key(&name, |entry| entry.0)
            .ok()?;
        table::NAMES_SORTED.get(i).map(|entry| entry.1)
    }

    /// Number of planes. Zero for a hardware surface.
    #[inline]
    #[must_use]
    pub const fn plane_count(self) -> usize {
        self.descriptor().planes as usize
    }

    /// Number of components. Padding channels do not count.
    #[inline]
    #[must_use]
    pub const fn component_count(self) -> usize {
        self.descriptor().components.len()
    }

    /// `(log2 horizontal, log2 vertical)` chroma decimation.
    #[inline]
    #[must_use]
    pub const fn log2_chroma(self) -> (u8, u8) {
        let d = self.descriptor();
        (d.log2_chroma_w, d.log2_chroma_h)
    }

    /// Average bits per pixel, padding excluded.
    #[inline]
    #[must_use]
    pub const fn bits_per_pixel(self) -> u8 {
        self.descriptor().bits_per_pixel
    }

    /// The deepest component. The most-queried derived property, precomputed
    /// nowhere because the loop folds away for a known format.
    #[inline]
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        reason = "the loop bound is the slice length; `.get` is not usable here \
                  because `<[T]>::get` is not callable in a const fn"
    )]
    pub const fn max_depth(self) -> u8 {
        let comps = self.descriptor().components;
        let mut best = 0;
        let mut i = 0;
        while i < comps.len() {
            if comps[i].depth > best {
                best = comps[i].depth;
            }
            i += 1;
        }
        best
    }

    /// Whether `flags` contains all of `f`.
    #[inline]
    #[must_use]
    pub const fn has(self, f: PixFmtFlags) -> bool {
        self.descriptor().flags.contains(f)
    }

    /// Components live in more than one plane.
    #[inline]
    #[must_use]
    pub const fn is_planar(self) -> bool {
        self.has(PixFmtFlags::PLANAR)
    }

    /// Components are R/G/B rather than Y/Cb/Cr.
    #[inline]
    #[must_use]
    pub const fn is_rgb(self) -> bool {
        self.has(PixFmtFlags::RGB)
    }

    /// An alpha component is present.
    #[inline]
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        self.has(PixFmtFlags::ALPHA)
    }

    /// An opaque handle to a hardware-owned surface.
    #[inline]
    #[must_use]
    pub const fn is_hw(self) -> bool {
        self.has(PixFmtFlags::HW_ACCEL)
    }

    /// What converting `self` into `dst` costs, lowest first.
    ///
    /// The four axes are ordered by how visible the damage is rather than by
    /// how many bits it costs: dropping colour outranks subsampling it, which
    /// outranks losing depth, which outranks losing alpha. Comparing the
    /// tuples lexicographically is what makes that ordering the decision.
    #[must_use]
    pub const fn conversion_loss(self, dst: Self) -> (u8, u8, u8, u8) {
        let colour = (self.component_count() >= 3) & (dst.component_count() < 3);
        let (sx, sy) = self.log2_chroma();
        let (dx, dy) = dst.log2_chroma();
        let chroma = dx.saturating_sub(sx) + dy.saturating_sub(sy);
        let depth = self.max_depth().saturating_sub(dst.max_depth());
        let alpha = self.has_alpha() & !dst.has_alpha();
        (colour as u8, chroma, depth, alpha as u8)
    }

    /// The candidate in `candidates` that `self` converts into most cheaply.
    ///
    /// `self` itself wins outright when it appears, so a pipeline that already
    /// agrees with an encoder never pays for a conversion. Otherwise the least
    /// lossy candidate wins and ties go to the earliest, which keeps an
    /// encoder's own preference order meaningful among equals.
    ///
    /// Taking `candidates.first()` instead is what this replaces, and it is
    /// wrong in a way that does not announce itself: an encoder listing
    /// `monoblack` before `rgb24` would silently reduce a colour frame to one
    /// bit.
    #[must_use]
    pub fn best_of(self, candidates: &[Self]) -> Option<Self> {
        if candidates.contains(&self) {
            return Some(self);
        }
        candidates
            .iter()
            .copied()
            .min_by_key(|&c| self.conversion_loss(c))
    }

    /// Multi-byte samples are stored most-significant byte first.
    #[inline]
    #[must_use]
    pub const fn is_big_endian(self) -> bool {
        self.has(PixFmtFlags::BIG_ENDIAN)
    }

    /// The opposite-endianness sibling, if the format has one.
    ///
    /// A table lookup, not a search: the scaler's byte-swap step asks this per
    /// conversion setup.
    #[inline]
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        reason = "ENDIAN_SWAP is index-aligned with the discriminants"
    )]
    pub const fn swap_endianness(self) -> Option<Self> {
        table::ENDIAN_SWAP[self as usize]
    }

    /// Width in samples of one row of `plane`, at a frame width of `width`.
    ///
    /// Chroma planes round up, so an odd width still has a chroma sample for the
    /// last pixel.
    #[must_use]
    pub const fn plane_width(self, width: u32, plane: u8) -> u32 {
        let d = self.descriptor();
        if d.chroma_plane(plane) {
            let s = d.log2_chroma_w;
            (width + (1 << s) - 1) >> s
        } else {
            width
        }
    }

    /// Number of rows in `plane`, at a frame height of `height`.
    #[must_use]
    pub const fn plane_height(self, height: u32, plane: u8) -> u32 {
        let d = self.descriptor();
        if d.chroma_plane(plane) {
            let s = d.log2_chroma_h;
            (height + (1 << s) - 1) >> s
        } else {
            height
        }
    }

    /// Smallest row stride, in bytes, that holds one row of `plane`.
    ///
    /// Derived as the largest `step x samples-in-this-plane` over the components
    /// that live there, which is exactly the span the last sample of the row
    /// reaches. For a [`PixFmtFlags::BITSTREAM`] format the same product is in
    /// bits and is rounded up to a byte.
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        reason = "the loop bound is the slice length; `.get` is not usable here \
                  because `<[T]>::get` is not callable in a const fn"
    )]
    pub const fn min_stride(self, width: u32, plane: u8) -> usize {
        let d = self.descriptor();
        let comps = d.components;
        let mut span: u64 = 0;
        let mut i = 0;
        while i < comps.len() {
            if comps[i].plane == plane {
                let samples = if i == 1 || i == 2 {
                    let s = d.log2_chroma_w;
                    ((width as u64) + (1 << s) - 1) >> s
                } else {
                    width as u64
                };
                let this = samples * comps[i].step as u64;
                if this > span {
                    span = this;
                }
            }
            i += 1;
        }
        if d.flags.contains(PixFmtFlags::BITSTREAM) {
            span = span.div_ceil(8);
        }
        span as usize
    }

    /// Bytes required for one plane at the given height and stride.
    ///
    /// Applies vertical chroma decimation to chroma planes. Returns 0 for a
    /// plane index the format does not have, including every plane of a hardware
    /// surface.
    #[inline]
    #[must_use]
    pub const fn plane_size(self, plane: u8, height: u32, stride: usize) -> usize {
        if plane >= self.descriptor().planes {
            return 0;
        }
        (self.plane_height(height, plane) as usize).saturating_mul(stride)
    }

    /// Strides and sizes for a whole frame, with each plane's stride rounded up
    /// to `align`.
    ///
    /// # Errors
    /// [`Error::Option`] if `align` is not a power of two, and
    /// [`Error::LimitExceeded`] if the frame does not fit in a `usize`.
    pub fn plane_layout(self, width: u32, height: u32, align: usize) -> Result<PlaneLayout, Error> {
        if align == 0 || !align.is_power_of_two() {
            return Err(Error::Option {
                name: "align".to_owned(),
                detail: format!("must be a power of two, got {align}"),
            });
        }
        let overflow = || Error::LimitExceeded {
            limit: "image buffer",
            requested: u64::MAX,
            cap: usize::MAX as u64,
        };

        let mut out = PlaneLayout {
            strides: [0; 4],
            sizes: [0; 4],
            planes: self.plane_count(),
            total: 0,
        };
        for plane in 0..out.planes {
            let p = plane as u8;
            let min = self.min_stride(width, p);
            let stride = min.checked_next_multiple_of(align).ok_or_else(overflow)?;
            let rows = self.plane_height(height, p) as usize;
            let size = stride.checked_mul(rows).ok_or_else(overflow)?;
            let slot = out
                .strides
                .get_mut(plane)
                .ok_or(Error::Unsupported("more than four planes"))?;
            *slot = stride;
            let slot = out
                .sizes
                .get_mut(plane)
                .ok_or(Error::Unsupported("more than four planes"))?;
            *slot = size;
            out.total = out.total.checked_add(size).ok_or_else(overflow)?;
        }
        Ok(out)
    }
}

/// Where every plane of a frame sits, per [`PixFmt::plane_layout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneLayout {
    /// Row stride in bytes, per plane. Unused entries are 0.
    pub strides: [usize; 4],
    /// Byte size, per plane. Unused entries are 0.
    pub sizes: [usize; 4],
    /// How many entries of `strides`/`sizes` are meaningful.
    pub planes: usize,
    /// Sum of `sizes`.
    pub total: usize,
}

#[cfg(test)]
mod tests;
