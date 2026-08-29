//! Frame, picture, and slice header parsing — RDD 36 SS5.1, SS5.2, SS6.1, SS6.2.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// `chroma_format` values (Table 1). Reserved values are rejected at parse
/// time — there is nothing a decoder could do with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChromaFormat {
    Yuv422,
    Yuv444,
}

impl ChromaFormat {
    fn from_code(code: u32) -> Result<Self> {
        match code {
            2 => Ok(Self::Yuv422),
            3 => Ok(Self::Yuv444),
            _ => Err(Error::Unsupported("prores: reserved chroma_format")),
        }
    }

    /// Number of Cb (or Cr) blocks per macroblock — `nC` in SS7.2.1.
    pub(crate) const fn chroma_blocks_per_mb(self) -> usize {
        match self {
            Self::Yuv422 => 2,
            Self::Yuv444 => 4,
        }
    }
}

/// `interlace_mode` values (Table 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterlaceMode {
    Progressive,
    /// First coded picture is the top field.
    TopFirst,
    /// Second coded picture is the top field.
    BottomFirst,
}

impl InterlaceMode {
    fn from_code(code: u32) -> Result<Self> {
        match code {
            0 => Ok(Self::Progressive),
            1 => Ok(Self::TopFirst),
            2 => Ok(Self::BottomFirst),
            _ => Err(Error::Unsupported("prores: reserved interlace_mode")),
        }
    }

    pub(crate) const fn is_interlaced(self) -> bool {
        !matches!(self, Self::Progressive)
    }
}

/// One 8x8 quantization weight matrix, indexed `[v][u]` (row-major, matching
/// the spec's own `for (v ...) for (u ...)` load order and every later use).
pub(crate) type QuantMatrix = [u8; 64];

/// The default matrix (SS7.3): every weight is 4.
pub(crate) const DEFAULT_QUANT_MATRIX: QuantMatrix = [4; 64];

#[derive(Debug, Clone)]
pub(crate) struct FrameHeader {
    #[allow(
        dead_code,
        reason = "kept on the parsed header for provenance/debugging even though this pass's decode path only checks it (0 or 1) at parse time"
    )]
    pub(crate) bitstream_version: u8,
    pub(crate) horizontal_size: u16,
    pub(crate) vertical_size: u16,
    pub(crate) chroma_format: ChromaFormat,
    pub(crate) interlace_mode: InterlaceMode,
    /// `alpha_channel_type` (Table 7): 0 = none, 1 = 8-bit, 2 = 16-bit.
    pub(crate) alpha_channel_type: u8,
    pub(crate) luma_quant: QuantMatrix,
    pub(crate) chroma_quant: QuantMatrix,
}

impl FrameHeader {
    /// Bit depth of color component samples. Not a bitstream syntax element
    /// (RDD 36 leaves it to the container) — see this crate's top-level doc
    /// for the `chroma_format`-derived rule this measured against real
    /// `ffmpeg -c:v prores_ks` output for every documented profile.
    pub(crate) const fn bit_depth(&self) -> u32 {
        match self.chroma_format {
            ChromaFormat::Yuv422 => 10,
            ChromaFormat::Yuv444 => 12,
        }
    }

    pub(crate) fn width_in_mb(&self) -> u32 {
        u32::from(self.horizontal_size).div_ceil(16)
    }
}

fn read_quant_matrix(r: &mut BitReader<'_>) -> Result<QuantMatrix> {
    let mut m = [0u8; 64];
    for v in 0..8usize {
        for u in 0..8usize {
            let idx = v * 8 + u;
            let val = r
                .try_get(8)
                .map_err(|_| Error::InvalidData("prores: quant matrix truncated"))?;
            if let Some(slot) = m.get_mut(idx) {
                *slot = val as u8;
            }
        }
    }
    Ok(m)
}

/// Parse `frame_header()`, SS5.1.1. `r` must be positioned at the start of
/// the header (immediately after `frame_identifier`).
pub(crate) fn parse_frame_header(r: &mut BitReader<'_>) -> Result<FrameHeader> {
    let frame_header_size = r
        .try_get(16)
        .map_err(|_| Error::InvalidData("prores: frame header truncated"))?;
    if frame_header_size < 20 {
        return Err(Error::InvalidData("prores: frame_header_size too small"));
    }
    let _reserved = r.get(8);
    let bitstream_version = r.get(8) as u8;
    if bitstream_version > 1 {
        return Err(Error::Unsupported("prores: unsupported bitstream_version"));
    }
    let _encoder_identifier = r.get(32);
    let horizontal_size = r.get(16) as u16;
    let vertical_size = r.get(16) as u16;
    if horizontal_size == 0 || vertical_size == 0 {
        return Err(Error::InvalidData("prores: zero frame dimension"));
    }
    let chroma_format_code = r.get(2);
    let _reserved = r.get(2);
    let interlace_code = r.get(2);
    let _reserved = r.get(2);
    let _aspect_ratio_information = r.get(4);
    let _frame_rate_code = r.get(4);
    let _color_primaries = r.get(8);
    let _transfer_characteristic = r.get(8);
    let _matrix_coefficients = r.get(8);
    let _reserved = r.get(4);
    let alpha_channel_type = r.get(4) as u8;
    if alpha_channel_type > 2 {
        return Err(Error::Unsupported("prores: reserved alpha_channel_type"));
    }
    let _reserved = r.get(14);
    let load_luma = r.get(1);
    let load_chroma = r.get(1);

    let chroma_format = ChromaFormat::from_code(chroma_format_code)?;
    let interlace_mode = InterlaceMode::from_code(interlace_code)?;
    if bitstream_version == 0 && (chroma_format != ChromaFormat::Yuv422 || alpha_channel_type != 0)
    {
        return Err(Error::InvalidData(
            "prores: bitstream_version 0 requires 4:2:2 and no alpha",
        ));
    }

    let luma_quant = if load_luma == 1 {
        let m = read_quant_matrix(r)?;
        if m.iter().any(|&w| !(2..=63).contains(&w)) {
            return Err(Error::InvalidData("prores: quant weight out of range"));
        }
        m
    } else {
        DEFAULT_QUANT_MATRIX
    };
    let chroma_quant = if load_chroma == 1 {
        let m = read_quant_matrix(r)?;
        if m.iter().any(|&w| !(2..=63).contains(&w)) {
            return Err(Error::InvalidData("prores: quant weight out of range"));
        }
        m
    } else {
        luma_quant
    };

    Ok(FrameHeader {
        bitstream_version,
        horizontal_size,
        vertical_size,
        chroma_format,
        interlace_mode,
        alpha_channel_type,
        luma_quant,
        chroma_quant,
    })
}

/// Height, in luma samples, of the picture this `frame_header` describes for
/// a given field (SS6.2 `picture_vertical_size`).
pub(crate) fn picture_vertical_size(fh: &FrameHeader, is_first: bool) -> u32 {
    let v = u32::from(fh.vertical_size);
    match fh.interlace_mode {
        InterlaceMode::Progressive => v,
        InterlaceMode::TopFirst => {
            if is_first {
                v.div_ceil(2)
            } else {
                v.saturating_sub(v.div_ceil(2))
            }
        }
        InterlaceMode::BottomFirst => {
            if is_first {
                v.saturating_sub(v.div_ceil(2))
            } else {
                v.div_ceil(2)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PictureHeader {
    pub(crate) picture_size: u32,
    /// 0..=3, meaning 1/2/4/8 macroblocks per slice (SS6.2.1).
    pub(crate) log2_desired_slice_size_in_mb: u8,
}

/// Parse `picture_header()`, SS5.2.1. Returns the header and the number of
/// bytes it occupies (`picture_header_size`), so the caller can seek past
/// any bitstream-version-variant tail this crate does not know about.
pub(crate) fn parse_picture_header(r: &mut BitReader<'_>) -> Result<(PictureHeader, u32)> {
    let picture_header_size = r
        .try_get(5)
        .map_err(|_| Error::InvalidData("prores: picture header truncated"))?;
    let _reserved = r.get(3);
    if picture_header_size < 8 {
        return Err(Error::InvalidData("prores: picture_header_size too small"));
    }
    let picture_size = r.get(32);
    let _deprecated_number_of_slices = r.get(16);
    let _reserved = r.get(2);
    let log2_desired_slice_size_in_mb = r.get(2) as u8;
    let _reserved = r.get(4);
    Ok((
        PictureHeader { picture_size, log2_desired_slice_size_in_mb },
        picture_header_size,
    ))
}

/// `slice_size_in_mb`/`number_of_slices_per_mb_row`, SS6.2's algorithm:
/// a `do`/`while` that halves the desired slice size each pass until the
/// macroblock row is exhausted. Transcribed as a bounded loop (`slice_size`
/// strictly halves down to 1 and then the row must be exhausted, since a
/// size-1 slice always fits) rather than the spec's literal `do`/`while`, to
/// keep every intermediate value read.
pub(crate) fn slice_sizes_in_mb(width_in_mb: u32, log2_desired: u8) -> Vec<u32> {
    let mut sizes = Vec::new();
    let mut slice_size = 1u32 << log2_desired.min(3);
    let mut remaining = width_in_mb;
    loop {
        while remaining >= slice_size && slice_size > 0 {
            sizes.push(slice_size);
            remaining -= slice_size;
        }
        if remaining == 0 || slice_size <= 1 {
            break;
        }
        slice_size /= 2;
    }
    sizes
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SliceHeader {
    pub(crate) quantization_index: u8,
    pub(crate) coded_size_of_y_data: u16,
    pub(crate) coded_size_of_cb_data: u16,
    /// Present only when the frame carries alpha (SS5.3.1).
    pub(crate) coded_size_of_cr_data: Option<u16>,
}

impl SliceHeader {
    /// The quantization scale factor `qScale`, Table 15.
    pub(crate) fn q_scale(self) -> u32 {
        let qi = u32::from(self.quantization_index);
        if qi <= 128 {
            qi
        } else {
            128 + 4 * (qi - 128)
        }
    }
}

/// Parse `slice_header()`, SS5.3.1.
pub(crate) fn parse_slice_header(r: &mut BitReader<'_>, has_alpha: bool) -> Result<SliceHeader> {
    let slice_header_size = r
        .try_get(5)
        .map_err(|_| Error::InvalidData("prores: slice header truncated"))?;
    let _reserved = r.get(3);
    let min_size = if has_alpha { 8 } else { 6 };
    if slice_header_size < min_size {
        return Err(Error::InvalidData("prores: slice_header_size too small"));
    }
    let quantization_index = r.get(8) as u8;
    if quantization_index == 0 || quantization_index > 224 {
        return Err(Error::InvalidData("prores: quantization_index out of range"));
    }
    let coded_size_of_y_data = r.get(16) as u16;
    let coded_size_of_cb_data = r.get(16) as u16;
    let coded_size_of_cr_data = if has_alpha { Some(r.get(16) as u16) } else { None };
    Ok(SliceHeader {
        quantization_index,
        coded_size_of_y_data,
        coded_size_of_cb_data,
        coded_size_of_cr_data,
    })
}

/// Read the `slice_table()` of `count` `u(16)` entries, bounded by `budget`.
pub(crate) fn parse_slice_table(
    r: &mut BitReader<'_>,
    count: usize,
    budget: &mut Budget,
) -> Result<Vec<u16>> {
    let mut sizes = budget.alloc::<u16>(count)?;
    for slot in &mut sizes {
        *slot = r
            .try_get(16)
            .map_err(|_| Error::InvalidData("prores: slice table truncated"))? as u16;
    }
    Ok(sizes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn slice_sizes_match_the_worked_example() {
        // RDD 36 SS4's own example: 45 macroblocks wide, desired size 8.
        let sizes = slice_sizes_in_mb(45, 3);
        assert_eq!(sizes, vec![8, 8, 8, 8, 8, 4, 1]);
    }

    #[test]
    fn slice_sizes_exact_multiple() {
        assert_eq!(slice_sizes_in_mb(16, 3), vec![8, 8]);
        assert_eq!(slice_sizes_in_mb(1, 0), vec![1]);
    }

    #[test]
    fn q_scale_matches_table_15() {
        assert_eq!(SliceHeader { quantization_index: 1, coded_size_of_y_data: 0, coded_size_of_cb_data: 0, coded_size_of_cr_data: None }.q_scale(), 1);
        assert_eq!(SliceHeader { quantization_index: 128, coded_size_of_y_data: 0, coded_size_of_cb_data: 0, coded_size_of_cr_data: None }.q_scale(), 128);
        assert_eq!(SliceHeader { quantization_index: 129, coded_size_of_y_data: 0, coded_size_of_cb_data: 0, coded_size_of_cr_data: None }.q_scale(), 132);
        assert_eq!(SliceHeader { quantization_index: 224, coded_size_of_y_data: 0, coded_size_of_cb_data: 0, coded_size_of_cr_data: None }.q_scale(), 512);
    }
}
