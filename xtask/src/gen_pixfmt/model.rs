//! The intermediate form a family expands into, and the flag vocabulary.
//!
//! Everything here describes *what a pixel format physically is*: which plane a
//! channel lives in, how far apart consecutive samples sit, where inside a
//! container word the significant bits are. None of it is a stylistic choice —
//! it is dictated by the format itself — which is what makes it derivable from
//! the format definition rather than copied from anyone's table (D7/D15).

/// A descriptor flag, mirroring the CLI-visible flag vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Flag {
    Be,
    Pal,
    Bitstream,
    HwAccel,
    Planar,
    Rgb,
    Alpha,
    Bayer,
    Float,
    Xyz,
}

impl Flag {
    /// The associated constant on `PixFmtFlags` this maps to.
    pub const fn ident(self) -> &'static str {
        match self {
            Self::Be => "BIG_ENDIAN",
            Self::Pal => "PALETTE",
            Self::Bitstream => "BITSTREAM",
            Self::HwAccel => "HW_ACCEL",
            Self::Planar => "PLANAR",
            Self::Rgb => "RGB",
            Self::Alpha => "ALPHA",
            Self::Bayer => "BAYER",
            Self::Float => "FLOAT",
            Self::Xyz => "XYZ",
        }
    }
}

/// One component's placement.
///
/// `step` and `offset` are byte counts for every format except a `BITSTREAM`
/// one, where they count bits — there is no byte-aligned unit to measure in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Comp {
    pub plane: u8,
    pub step: u8,
    pub offset: u8,
    pub shift: u8,
    pub depth: u8,
}

impl Comp {
    pub const fn new(plane: u8, step: u8, offset: u8, shift: u8, depth: u8) -> Self {
        Self {
            plane,
            step,
            offset,
            shift,
            depth,
        }
    }
}

/// A fully expanded pixel format, ready to be emitted.
#[derive(Clone, Debug)]
pub struct Format {
    /// CLI-facing name. This is an interface fact and must match exactly (D15).
    pub name: String,
    /// Rust enum variant identifier.
    pub variant: String,
    /// Additional spellings `from_name` must accept.
    pub aliases: Vec<String>,
    /// Indexed by logical channel: 0 = Y or R, 1 = U or G, 2 = V or B, 3 = A.
    /// Padding channels are not components and do not appear.
    pub comps: Vec<Comp>,
    pub planes: u8,
    pub log2_chroma_w: u8,
    pub log2_chroma_h: u8,
    pub bits_per_pixel: u8,
    pub flags: Vec<Flag>,
    /// Name of the opposite-endianness sibling, if the format has one.
    pub endian_sibling: Option<String>,
}

impl Format {
    pub fn has(&self, f: Flag) -> bool {
        self.flags.contains(&f)
    }

    /// Average bits per pixel, padding excluded.
    ///
    /// Derived, not tabulated: count the bits one macro-pixel block actually
    /// stores and divide by the pixels in it. Chroma channels (logical index 1
    /// and 2) contribute one sample per block; luma, RGB and alpha contribute
    /// one per pixel. Truncating division is deliberate — 4:2:0 at 9 bits
    /// genuinely averages 13.5 bits and the reported figure is the floor.
    pub fn derive_bpp(comps: &[Comp], log2_w: u8, log2_h: u8) -> u8 {
        let block = 1u32 << (log2_w + log2_h);
        let mut bits = 0u32;
        for (i, c) in comps.iter().enumerate() {
            bits += if i == 1 || i == 2 {
                u32::from(c.depth)
            } else {
                u32::from(c.depth) * block
            };
        }
        (bits / block) as u8
    }
}

/// Bytes a sample of `depth` significant bits is stored in.
pub const fn sample_bytes(depth: u8) -> u8 {
    if depth <= 8 {
        1
    } else if depth <= 16 {
        2
    } else {
        4
    }
}
