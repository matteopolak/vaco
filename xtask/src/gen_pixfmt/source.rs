//! The declarative pixel-format source.
//!
//! This is the file a human edits. It is a Rust `const` rather than TOML or RON
//! so that `rustc` type-checks every declaration before it can produce a bad
//! table, and so the generator needs no data-format parser.
//!
//! Read `docs/model/vaco-pixfmt.md` before changing anything here.
//!
//! ORDER IS LOAD-BEARING. Enum discriminants are assigned in declaration order,
//! and within a family in expansion order. Appending is free; inserting in the
//! middle renumbers everything after it. That is fine — discriminants are ours,
//! never serialised, and never a compatibility surface — but it does mean the
//! generated table's diff is only reviewable if you append.

use super::model::Flag;

/// Chroma subsampling, named the way the format names name it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sub {
    S444,
    S440,
    S422,
    S420,
    S411,
    S410,
}

impl Sub {
    pub const fn tag(self) -> &'static str {
        match self {
            Self::S444 => "444",
            Self::S440 => "440",
            Self::S422 => "422",
            Self::S420 => "420",
            Self::S411 => "411",
            Self::S410 => "410",
        }
    }

    /// `(log2 horizontal, log2 vertical)` chroma decimation.
    pub const fn log2(self) -> (u8, u8) {
        match self {
            Self::S444 => (0, 0),
            Self::S440 => (0, 1),
            Self::S422 => (1, 0),
            Self::S420 => (1, 1),
            Self::S411 => (2, 0),
            Self::S410 => (2, 2),
        }
    }
}

/// Whether a family emits a big/little-endian pair.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    /// One sample fits in a byte, so byte order cannot differ.
    Never,
    /// Emit `<name>le` and `<name>be`, identical but for the `BE` flag.
    Pair,
}

/// Which alpha variants a family emits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alpha {
    No,
    Yes,
    Both,
}

impl Alpha {
    pub const fn variants(self) -> &'static [bool] {
        match self {
            Self::No => &[false],
            Self::Yes => &[true],
            Self::Both => &[false, true],
        }
    }
}

/// How a sample sits inside its container.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Store {
    /// Right-aligned integer: the significant bits are the low bits.
    Int,
    /// Left-aligned ("MSB-packed") integer in a 16-bit container.
    Msb,
    /// IEEE-754 binary16.
    F16,
    /// IEEE-754 binary32.
    F32,
}

impl Store {
    /// `(container bytes, shift, significant bits)` for a nominal depth.
    pub const fn sample(self, depth: u8) -> (u8, u8, u8) {
        match self {
            Self::Int => (super::model::sample_bytes(depth), 0, depth),
            Self::Msb => (2, 16 - depth, depth),
            Self::F16 => (2, 0, 16),
            Self::F32 => (4, 0, 32),
        }
    }

    pub const fn is_float(self) -> bool {
        matches!(self, Self::F16 | Self::F32)
    }
}

/// A channel in a packed layout.
///
/// `R`/`G`/`B` and `Y`/`U`/`V` are the same three descriptor slots under the two
/// naming conventions; `X` is padding and is not a component.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chan {
    R,
    G,
    B,
    A,
    X,
    Y,
    U,
    V,
}

impl Chan {
    /// Logical component index, or `None` for padding.
    pub const fn slot(self) -> Option<usize> {
        match self {
            Self::R | Self::Y => Some(0),
            Self::G | Self::U => Some(1),
            Self::B | Self::V => Some(2),
            Self::A => Some(3),
            Self::X => None,
        }
    }
}

/// How the channels of a packed format are arranged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pack {
    /// Each channel gets its own container of `n` bytes, laid out in **memory
    /// order**: the first channel in `order` sits at offset 0.
    Bytes(u8),
    /// All channels are bitfields inside one container of `n` bytes, listed
    /// **most-significant first**.
    Field(u8),
    /// As `Field`, but the container is `n` bits and is not byte-aligned, so
    /// `step`/`offset` count bits and the format is flagged `BITSTREAM`.
    Bitstream(u8),
}

/// One packed format, before endianness expansion.
#[derive(Clone, Copy, Debug)]
pub struct PackedDef {
    pub name: &'static str,
    pub order: &'static [Chan],
    /// Bits per entry in `order`, positionally.
    pub bits: &'static [u8],
    pub pack: Pack,
    pub end: End,
    pub store: Store,
    /// Extra flags beyond the ones derived from the layout (`RGB`, `XYZ`, …).
    pub extra: &'static [Flag],
}

/// One biplanar (NV-style) format.
#[derive(Clone, Copy, Debug)]
pub struct BiplanarDef {
    pub name: &'static str,
    pub sub: Sub,
    pub depth: u8,
    pub store: Store,
    /// `true` when the interleaved plane holds Cr before Cb (`nv21`, `nv42`).
    pub swapped: bool,
    pub end: End,
}

/// A format whose layout no family expresses.
#[derive(Clone, Copy, Debug)]
pub struct ExplicitDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// `(plane, step, offset, shift, depth)` indexed by logical channel.
    pub comps: &'static [(u8, u8, u8, u8, u8)],
    pub planes: u8,
    pub log2_chroma: (u8, u8),
    pub flags: &'static [Flag],
    pub end: End,
    /// Bits per pixel, when the generic derivation does not apply.
    pub bpp: Option<u8>,
}

/// A family declaration.
#[derive(Clone, Copy, Debug)]
pub enum Family {
    /// `yuv420p`, `yuvj422p`, `yuva444p16be`, `yuv444p10msble`, …
    PlanarYuv {
        stem: &'static str,
        subs: &'static [Sub],
        depths: &'static [u8],
        alpha: Alpha,
        store: Store,
        end: End,
    },
    /// `gbrp`, `gbrap12le`, `gbrp10msbbe`, `gbrapf32be`, …
    PlanarGbr {
        depths: &'static [u8],
        alpha: Alpha,
        store: Store,
        end: End,
    },
    /// `nv12`, `nv21`, `p010le`, `p416be`, …
    Biplanar(&'static [BiplanarDef]),
    /// `gray`, `gray12be`, `grayf32le`, `ya8`, `yaf16be`, …
    Gray {
        depths: &'static [u8],
        alpha: Alpha,
        store: Store,
        end: End,
    },
    /// Every packed layout — RGB and 4:4:4 YUV alike.
    Packed(&'static [PackedDef]),
    /// `bayer_bggr8`, `bayer_rggb16le`, …
    Bayer {
        patterns: &'static [&'static str],
        depths: &'static [u8],
        end: End,
    },
    /// Opaque hardware surface handles: a name and nothing else.
    HwSurface(&'static [&'static str]),
    /// Layouts no family expresses.
    Explicit(&'static [ExplicitDef]),
}

use Chan::{A, B, G, R, U, V, X, Y};
use Pack::{Bitstream, Bytes, Field};
use Store::{F16, F32, Int, Msb};
use Sub::{S410, S411, S420, S422, S440, S444};

pub const FAMILIES: &[Family] = &[
    // ---------------------------------------------------------------- planar YUV
    //
    // Three planes (four with alpha), one channel each, no packing: the only
    // things that vary are decimation, depth and byte order.
    Family::PlanarYuv {
        stem: "yuv",
        subs: &[S410, S411, S420, S422, S440, S444],
        depths: &[8],
        alpha: Alpha::No,
        store: Int,
        end: End::Never,
    },
    // The `yuvj*` spellings are the deprecated full-range duplicates. Range is a
    // colour property, not a layout property, so their descriptors are identical
    // to the plain ones — but the names appear in real command lines and files,
    // so they must resolve.
    Family::PlanarYuv {
        stem: "yuvj",
        subs: &[S411, S420, S422, S440, S444],
        depths: &[8],
        alpha: Alpha::No,
        store: Int,
        end: End::Never,
    },
    Family::PlanarYuv {
        stem: "yuv",
        subs: &[S420, S422, S444],
        depths: &[8],
        alpha: Alpha::Yes,
        store: Int,
        end: End::Never,
    },
    Family::PlanarYuv {
        stem: "yuv",
        subs: &[S420, S422, S444],
        depths: &[9, 10, 12, 14, 16],
        alpha: Alpha::No,
        store: Int,
        end: End::Pair,
    },
    Family::PlanarYuv {
        stem: "yuv",
        subs: &[S440],
        depths: &[10, 12],
        alpha: Alpha::No,
        store: Int,
        end: End::Pair,
    },
    Family::PlanarYuv {
        stem: "yuv",
        subs: &[S444],
        depths: &[10, 12],
        alpha: Alpha::No,
        store: Msb,
        end: End::Pair,
    },
    Family::PlanarYuv {
        stem: "yuv",
        subs: &[S420],
        depths: &[9, 10, 16],
        alpha: Alpha::Yes,
        store: Int,
        end: End::Pair,
    },
    Family::PlanarYuv {
        stem: "yuv",
        subs: &[S422],
        depths: &[9, 10, 12, 16],
        alpha: Alpha::Yes,
        store: Int,
        end: End::Pair,
    },
    Family::PlanarYuv {
        stem: "yuv",
        subs: &[S444],
        depths: &[9, 10, 12, 16],
        alpha: Alpha::Yes,
        store: Int,
        end: End::Pair,
    },
    // ---------------------------------------------------------------- planar GBR
    //
    // RGB in three planes stored G, B, R — the name says so, and that ordering is
    // the whole reason the format exists (it lets an RGB codec reuse a YUV
    // planar pipeline). Component 0 is R and therefore lives in plane 2.
    Family::PlanarGbr {
        depths: &[8],
        alpha: Alpha::Both,
        store: Int,
        end: End::Never,
    },
    Family::PlanarGbr {
        depths: &[9, 10, 12, 14, 16],
        alpha: Alpha::No,
        store: Int,
        end: End::Pair,
    },
    Family::PlanarGbr {
        depths: &[10, 12],
        alpha: Alpha::No,
        store: Msb,
        end: End::Pair,
    },
    Family::PlanarGbr {
        depths: &[10, 12, 14, 16],
        alpha: Alpha::Yes,
        store: Int,
        end: End::Pair,
    },
    Family::PlanarGbr {
        depths: &[32],
        alpha: Alpha::Yes,
        store: Int,
        end: End::Pair,
    },
    Family::PlanarGbr {
        depths: &[16],
        alpha: Alpha::Both,
        store: F16,
        end: End::Pair,
    },
    Family::PlanarGbr {
        depths: &[32],
        alpha: Alpha::Both,
        store: F32,
        end: End::Pair,
    },
    // ------------------------------------------------------------- biplanar YUV
    //
    // Luma plane plus one plane of interleaved chroma. `nv21`/`nv42` are the same
    // layout with Cr and Cb exchanged; the `p<sub><depth>` family is the
    // high-bit-depth generalisation, left-aligned in 16-bit words.
    Family::Biplanar(&[
        BiplanarDef {
            name: "nv12",
            sub: S420,
            depth: 8,
            store: Int,
            swapped: false,
            end: End::Never,
        },
        BiplanarDef {
            name: "nv21",
            sub: S420,
            depth: 8,
            store: Int,
            swapped: true,
            end: End::Never,
        },
        BiplanarDef {
            name: "nv16",
            sub: S422,
            depth: 8,
            store: Int,
            swapped: false,
            end: End::Never,
        },
        BiplanarDef {
            name: "nv24",
            sub: S444,
            depth: 8,
            store: Int,
            swapped: false,
            end: End::Never,
        },
        BiplanarDef {
            name: "nv42",
            sub: S444,
            depth: 8,
            store: Int,
            swapped: true,
            end: End::Never,
        },
        BiplanarDef {
            name: "p010",
            sub: S420,
            depth: 10,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        BiplanarDef {
            name: "p012",
            sub: S420,
            depth: 12,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        BiplanarDef {
            name: "p016",
            sub: S420,
            depth: 16,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        BiplanarDef {
            name: "p210",
            sub: S422,
            depth: 10,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        BiplanarDef {
            name: "p212",
            sub: S422,
            depth: 12,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        BiplanarDef {
            name: "p216",
            sub: S422,
            depth: 16,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        BiplanarDef {
            name: "p410",
            sub: S444,
            depth: 10,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        BiplanarDef {
            name: "p412",
            sub: S444,
            depth: 12,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        BiplanarDef {
            name: "p416",
            sub: S444,
            depth: 16,
            store: Msb,
            swapped: false,
            end: End::Pair,
        },
        // UNVERIFIED, see docs/model/vaco-pixfmt.md §"formats to re-check".
        // `nv20` is a 10-bit 4:2:2 semi-planar format with no public
        // specification. It is modelled right-aligned, which is what makes it
        // distinct from `p210`; if that is backwards the differential harness
        // will say so.
        BiplanarDef {
            name: "nv20",
            sub: S422,
            depth: 10,
            store: Int,
            swapped: false,
            end: End::Pair,
        },
    ]),
    // --------------------------------------------------------------------- gray
    Family::Gray {
        depths: &[8],
        alpha: Alpha::Both,
        store: Int,
        end: End::Never,
    },
    Family::Gray {
        depths: &[9, 10, 12, 14, 16],
        alpha: Alpha::No,
        store: Int,
        end: End::Pair,
    },
    Family::Gray {
        depths: &[32],
        alpha: Alpha::No,
        store: Int,
        end: End::Pair,
    },
    Family::Gray {
        depths: &[16],
        alpha: Alpha::Yes,
        store: Int,
        end: End::Pair,
    },
    Family::Gray {
        depths: &[16],
        alpha: Alpha::Both,
        store: F16,
        end: End::Pair,
    },
    Family::Gray {
        depths: &[32],
        alpha: Alpha::Both,
        store: F32,
        end: End::Pair,
    },
    // ------------------------------------------------------------- packed RGB(A)
    //
    // `Bytes` layouts list channels in memory order. `Field` and `Bitstream`
    // layouts list them most-significant-bit first inside the container, which is
    // the order the format's own name states them in.
    Family::Packed(&[
        PackedDef {
            name: "rgb24",
            order: &[R, G, B],
            bits: &[8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr24",
            order: &[B, G, R],
            bits: &[8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "argb",
            order: &[A, R, G, B],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgba",
            order: &[R, G, B, A],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "abgr",
            order: &[A, B, G, R],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgra",
            order: &[B, G, R, A],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "0rgb",
            order: &[X, R, G, B],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgb0",
            order: &[R, G, B, X],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "0bgr",
            order: &[X, B, G, R],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr0",
            order: &[B, G, R, X],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        // Sub-byte packings. The name states the field widths and their order.
        PackedDef {
            name: "rgb8",
            order: &[B, G, R],
            bits: &[2, 3, 3],
            pack: Field(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr8",
            order: &[R, G, B],
            bits: &[2, 3, 3],
            pack: Field(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgb4_byte",
            order: &[X, B, G, R],
            bits: &[4, 1, 2, 1],
            pack: Field(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr4_byte",
            order: &[X, R, G, B],
            bits: &[4, 1, 2, 1],
            pack: Field(1),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgb4",
            order: &[B, G, R],
            bits: &[1, 2, 1],
            pack: Bitstream(4),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr4",
            order: &[R, G, B],
            bits: &[1, 2, 1],
            pack: Bitstream(4),
            end: End::Never,
            store: Int,
            extra: &[Flag::Rgb],
        },
        // 16-bit container packings.
        PackedDef {
            name: "rgb444",
            order: &[X, R, G, B],
            bits: &[4, 4, 4, 4],
            pack: Field(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgb555",
            order: &[X, R, G, B],
            bits: &[1, 5, 5, 5],
            pack: Field(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgb565",
            order: &[R, G, B],
            bits: &[5, 6, 5],
            pack: Field(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr444",
            order: &[X, B, G, R],
            bits: &[4, 4, 4, 4],
            pack: Field(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr555",
            order: &[X, B, G, R],
            bits: &[1, 5, 5, 5],
            pack: Field(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr565",
            order: &[B, G, R],
            bits: &[5, 6, 5],
            pack: Field(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        // 32-bit container, 2 bits of padding then 10 bits per channel.
        PackedDef {
            name: "x2rgb10",
            order: &[X, R, G, B],
            bits: &[2, 10, 10, 10],
            pack: Field(4),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "x2bgr10",
            order: &[X, B, G, R],
            bits: &[2, 10, 10, 10],
            pack: Field(4),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        // One container per channel.
        PackedDef {
            name: "rgb48",
            order: &[R, G, B],
            bits: &[16, 16, 16],
            pack: Bytes(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgr48",
            order: &[B, G, R],
            bits: &[16, 16, 16],
            pack: Bytes(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgba64",
            order: &[R, G, B, A],
            bits: &[16, 16, 16, 16],
            pack: Bytes(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "bgra64",
            order: &[B, G, R, A],
            bits: &[16, 16, 16, 16],
            pack: Bytes(2),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgb96",
            order: &[R, G, B],
            bits: &[32, 32, 32],
            pack: Bytes(4),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgba128",
            order: &[R, G, B, A],
            bits: &[32, 32, 32, 32],
            pack: Bytes(4),
            end: End::Pair,
            store: Int,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgbf16",
            order: &[R, G, B],
            bits: &[16, 16, 16],
            pack: Bytes(2),
            end: End::Pair,
            store: F16,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgbaf16",
            order: &[R, G, B, A],
            bits: &[16, 16, 16, 16],
            pack: Bytes(2),
            end: End::Pair,
            store: F16,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgbf32",
            order: &[R, G, B],
            bits: &[32, 32, 32],
            pack: Bytes(4),
            end: End::Pair,
            store: F32,
            extra: &[Flag::Rgb],
        },
        PackedDef {
            name: "rgbaf32",
            order: &[R, G, B, A],
            bits: &[32, 32, 32, 32],
            pack: Bytes(4),
            end: End::Pair,
            store: F32,
            extra: &[Flag::Rgb],
        },
        // CIE XYZ shares the packed-RGB machinery: three 16-bit containers with
        // 12 significant bits left-aligned in each.
        PackedDef {
            name: "xyz12",
            order: &[R, G, B],
            bits: &[12, 12, 12],
            pack: Bytes(2),
            end: End::Pair,
            store: Msb,
            extra: &[Flag::Rgb, Flag::Xyz],
        },
        // -------- packed 4:4:4 YUV. Same machinery, YUV channel names.
        PackedDef {
            name: "ayuv",
            order: &[A, Y, U, V],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[],
        },
        PackedDef {
            name: "uyva",
            order: &[U, Y, V, A],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[],
        },
        PackedDef {
            name: "vuya",
            order: &[V, U, Y, A],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[],
        },
        PackedDef {
            name: "vuyx",
            order: &[V, U, Y, X],
            bits: &[8, 8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[],
        },
        PackedDef {
            name: "vyu444",
            order: &[V, Y, U],
            bits: &[8, 8, 8],
            pack: Bytes(1),
            end: End::Never,
            store: Int,
            extra: &[],
        },
        PackedDef {
            name: "ayuv64",
            order: &[A, Y, U, V],
            bits: &[16, 16, 16, 16],
            pack: Bytes(2),
            end: End::Pair,
            store: Int,
            extra: &[],
        },
        // The XVYU group: X, V, Y, U most-significant first. `xv30` fits one
        // 32-bit word; `xv36` and `xv48` use four 16-bit words, so in memory the
        // least-significant channel (U) comes first.
        PackedDef {
            name: "xv30",
            order: &[X, V, Y, U],
            bits: &[2, 10, 10, 10],
            pack: Field(4),
            end: End::Pair,
            store: Int,
            extra: &[],
        },
        PackedDef {
            name: "xv36",
            order: &[U, Y, V, X],
            bits: &[12, 12, 12, 12],
            pack: Bytes(2),
            end: End::Pair,
            store: Msb,
            extra: &[],
        },
        PackedDef {
            name: "xv48",
            order: &[U, Y, V, X],
            bits: &[16, 16, 16, 16],
            pack: Bytes(2),
            end: End::Pair,
            store: Int,
            extra: &[],
        },
        // UNVERIFIED, see docs/model/vaco-pixfmt.md §"formats to re-check".
        // Modelled as `xv30` with the padding moved to the low bits, which is
        // what the name's channel order says and the only reading under which it
        // is not a duplicate of `xv30`.
        PackedDef {
            name: "v30x",
            order: &[V, Y, U, X],
            bits: &[10, 10, 10, 2],
            pack: Field(4),
            end: End::Pair,
            store: Int,
            extra: &[],
        },
    ]),
    // -------------------------------------------------------------------- bayer
    //
    // A colour-filter-array mosaic: one sample per pixel, and the four-letter
    // pattern in the name says which filter sits over which pixel of the 2x2
    // cell. Modelled as a single component at the sample depth, which is what is
    // physically there; demosaicing is a filter's job, not the descriptor's.
    Family::Bayer {
        patterns: &["bggr", "rggb", "gbrg", "grbg"],
        depths: &[8],
        end: End::Never,
    },
    Family::Bayer {
        patterns: &["bggr", "rggb", "gbrg", "grbg"],
        depths: &[16],
        end: End::Pair,
    },
    // ------------------------------------------------------------------ the rest
    Family::Explicit(&[
        // Packed 4:2:2, one chroma pair per two luma samples. The three 8-bit
        // spellings differ only in which byte holds what.
        ExplicitDef {
            name: "yuyv422",
            aliases: &[],
            comps: &[(0, 2, 0, 0, 8), (0, 4, 1, 0, 8), (0, 4, 3, 0, 8)],
            planes: 1,
            log2_chroma: (1, 0),
            flags: &[],
            end: End::Never,
            bpp: None,
        },
        ExplicitDef {
            name: "uyvy422",
            aliases: &[],
            comps: &[(0, 2, 1, 0, 8), (0, 4, 0, 0, 8), (0, 4, 2, 0, 8)],
            planes: 1,
            log2_chroma: (1, 0),
            flags: &[],
            end: End::Never,
            bpp: None,
        },
        ExplicitDef {
            name: "yvyu422",
            aliases: &[],
            comps: &[(0, 2, 0, 0, 8), (0, 4, 3, 0, 8), (0, 4, 1, 0, 8)],
            planes: 1,
            log2_chroma: (1, 0),
            flags: &[],
            end: End::Never,
            bpp: None,
        },
        // Packed 4:1:1 as `U Y Y V Y Y`: six bytes per four pixels. The luma
        // samples are not evenly spaced, so `step`/`offset` cannot describe them
        // exactly; luma is given the tightest uniform stride that reaches every
        // sample and consumers must respect the 4:1:1 grouping.
        ExplicitDef {
            name: "uyyvyy411",
            aliases: &[],
            comps: &[(0, 1, 1, 0, 8), (0, 6, 0, 0, 8), (0, 6, 3, 0, 8)],
            planes: 1,
            log2_chroma: (2, 0),
            flags: &[],
            end: End::Never,
            bpp: None,
        },
        // Packed 4:2:2 in 16-bit words, `Y U Y V`, left-aligned.
        ExplicitDef {
            name: "y210",
            aliases: &[],
            comps: &[(0, 4, 0, 6, 10), (0, 8, 2, 6, 10), (0, 8, 6, 6, 10)],
            planes: 1,
            log2_chroma: (1, 0),
            flags: &[],
            end: End::Pair,
            bpp: None,
        },
        ExplicitDef {
            name: "y212",
            aliases: &[],
            comps: &[(0, 4, 0, 4, 12), (0, 8, 2, 4, 12), (0, 8, 6, 4, 12)],
            planes: 1,
            log2_chroma: (1, 0),
            flags: &[],
            end: End::Pair,
            bpp: None,
        },
        ExplicitDef {
            name: "y216",
            aliases: &[],
            comps: &[(0, 4, 0, 0, 16), (0, 8, 2, 0, 16), (0, 8, 6, 0, 16)],
            planes: 1,
            log2_chroma: (1, 0),
            flags: &[],
            end: End::Pair,
            bpp: None,
        },
        // One bit per pixel, packed. `monow` has 0 as white.
        ExplicitDef {
            name: "monow",
            aliases: &["monowhite"],
            comps: &[(0, 1, 0, 0, 1)],
            planes: 1,
            log2_chroma: (0, 0),
            flags: &[Flag::Bitstream],
            end: End::Never,
            bpp: Some(1),
        },
        ExplicitDef {
            name: "monob",
            aliases: &["monoblack"],
            comps: &[(0, 1, 0, 0, 1)],
            planes: 1,
            log2_chroma: (0, 0),
            flags: &[Flag::Bitstream],
            end: End::Never,
            bpp: Some(1),
        },
        // An 8-bit index into a 256-entry RGB32 palette carried beside the frame.
        ExplicitDef {
            name: "pal8",
            aliases: &[],
            comps: &[(0, 1, 0, 0, 8)],
            planes: 1,
            log2_chroma: (0, 0),
            flags: &[Flag::Pal],
            end: End::Never,
            bpp: None,
        },
    ]),
    // ------------------------------------------------------- hardware surfaces
    //
    // Opaque handles to a backend-owned surface. There is no component metadata
    // to describe: the payload's real layout is the driver's business, and the
    // only correct thing to do with one is hand it back to that backend.
    Family::HwSurface(&[
        "vaapi",
        "dxva2_vld",
        "d3d11va_vld",
        "d3d11",
        "d3d12",
        "vdpau",
        "videotoolbox",
        "cuda",
        "qsv",
        "mmal",
        "mediacodec",
        "opencl",
        "drm_prime",
        "vulkan",
        "amf_surface",
        "ohcodec",
        "cuarray",
    ]),
];

/// Alternate spellings `from_name` must resolve, beyond the per-format aliases
/// declared above. `(alias, canonical name)`.
pub const ALIASES: &[(&str, &str)] = &[
    ("y400a", "ya8"),
    ("gray8a", "ya8"),
    ("gbr24p", "gbrp"),
    ("gray8", "gray"),
];

/// Formats whose derived enum variant would read badly. `(format name, variant)`.
///
/// The default derivation is: split on `_`, capitalise each segment, and prefix
/// an `X` if the result would start with a digit (`0rgb` -> `X0rgb`).
pub const VARIANT_OVERRIDES: &[(&str, &str)] = &[
    ("gray", "Gray8"),
    ("monow", "MonoWhite"),
    ("monob", "MonoBlack"),
];
