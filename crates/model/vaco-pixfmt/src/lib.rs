//! Pixel formats and their descriptor metadata.
//!
//! The format table is **generated** from a declarative source rather than
//! hand-written (plan 11): there are ~268 formats, each with plane count,
//! component offsets, shifts, depths, subsampling and endianness, and
//! hand-maintaining that invites silent metadata drift. `cargo xtask gen-pixfmt`
//! emits the table; CI re-runs it and fails if the committed output differs.

use vaco_core::Error;

/// A pixel format.
///
/// Non-exhaustive because the generated table grows; matching exhaustively on
/// ~268 variants is never the right thing for a caller to do anyway — query the
/// [`PixFmtDescriptor`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum PixFmt {
    Yuv420p,
    Yuv422p,
    Yuv444p,
    Nv12,
    Rgb24,
    Bgr24,
    Rgba,
    Gray8,
    // ... generated
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PixFmtFlags: u16 {
        const BIG_ENDIAN = 1 << 0;
        const PALETTE    = 1 << 1;
        const BITSTREAM  = 1 << 2;
        const HW_ACCEL   = 1 << 3;
        const PLANAR     = 1 << 4;
        const RGB        = 1 << 5;
        const ALPHA      = 1 << 6;
        const BAYER      = 1 << 7;
        const FLOAT      = 1 << 8;
        const XYZ        = 1 << 9;
    }
}

/// Where one component lives within a plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
    pub plane: u8,
    /// Distance in bytes between consecutive samples of this component.
    pub step: u8,
    /// Byte offset of the first sample within the plane.
    pub offset: u8,
    /// Bit shift within the containing word.
    pub shift: u8,
    pub depth: u8,
}

/// Everything a caller needs to interpret a frame's planes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixFmtDescriptor {
    pub name: &'static str,
    pub components: &'static [Component],
    pub planes: u8,
    /// log2 of horizontal chroma subsampling (1 = 4:2:0 / 4:2:2).
    pub log2_chroma_w: u8,
    pub log2_chroma_h: u8,
    pub flags: PixFmtFlags,
}

impl PixFmt {
    /// Constant-time descriptor lookup.
    ///
    /// `const fn` indexing into a static table, so a call with a known format
    /// folds to an immediate at the call site.
    #[must_use]
    pub const fn descriptor(self) -> &'static PixFmtDescriptor {
        // P0-03 freeze: index the generated table.
        todo!()
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        self.descriptor().name
    }

    /// Parse a CLI-facing format name such as `yuv420p`.
    ///
    /// # Errors
    /// Returns [`Error::Option`] when the name is not a known format.
    pub fn from_name(name: &str) -> Result<Self, Error> {
        let _ = name;
        todo!("P0-03 freeze: perfect-hash lookup over the generated table")
    }

    /// Bytes required for one plane at the given dimensions and stride.
    #[must_use]
    pub const fn plane_size(self, plane: u8, height: u32, stride: usize) -> usize {
        // P0-03 freeze: apply vertical subsampling for chroma planes.
        let _ = (plane, height, stride);
        todo!()
    }
}
