//! `sequence_header()` through `picture_coding_extension()` (H.262 §6.2.2,
//! §6.2.3), parsed at decode granularity — every field this crate's
//! reconstruction needs, not just the identifying ones
//! `vaco-parse-mpegvideo` reads.
//!
//! Start-code scanning and access-unit splitting are **not** this module's
//! job; [`crate::decoder`] already has a complete access unit (one picture's
//! worth of bytes, headers through slice data) before anything here runs.

use vaco_bitstream::BitReader;

use crate::tables;

/// `sequence_header()` (§6.2.2.1) plus `sequence_extension()` (§6.2.2.3)
/// folded in, matching `sequence_extension()`'s presence being the only
/// on-wire signal that a stream is MPEG-2 rather than MPEG-1 — see
/// `vaco-parse-mpegvideo`'s module docs for the same observation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SequenceHeader {
    pub width: u32,
    pub height: u32,
    pub intra_matrix: [u8; 64],
    pub non_intra_matrix: [u8; 64],
}

/// `progressive_sequence` is parsed (correct bitstream framing requires
/// reading every field `sequence_extension()` declares) but not read back
/// by this crate: it has no effect on frame-picture decode, the only kind
/// this crate handles — interlaced *or* progressive content can equally be
/// carried as frame pictures. `chroma_format` **is** consumed: it drives
/// `crate::macroblock`'s block count/geometry (§6.3.17.4, Table 6-20),
/// `coded_block_pattern`'s extension bits (§6.2.5.3), chrominance motion
/// vector scaling (§7.6.3.7), and the output `PixFmt`
/// (`crate::decoder::begin_picture`). `crate::decoder::Sequence::ext` is
/// what a caller reads to tell MPEG-1 from MPEG-2 (`Option::is_some`) —
/// MPEG-1 has no `sequence_extension()` at all and is always 4:2:0.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SequenceExtension {
    #[allow(dead_code, reason = "parsed for correct framing; genuinely has no effect on frame-picture decode, see this struct's own doc comment")]
    pub progressive_sequence: bool,
    /// Raw `chroma_format` bits (Table 6-5): `1` = 4:2:0, `2` = 4:2:2, `3`
    /// = 4:4:4, `0` reserved/non-conforming. Callers should route through
    /// `crate::macroblock::ChromaFormat::from_raw`, which folds the
    /// reserved code to 4:2:0 defensively rather than propagating it.
    pub chroma_format: u8,
}

/// Parse `sequence_header()`'s body (`payload` starts right after the
/// `00 00 01 B3` start code) into width/height and the two weighting
/// matrices, which reset to their §6.2.3.2 defaults on every
/// `sequence_header()` regardless of whether a previous one already ran.
#[must_use]
pub(crate) fn sequence_header(payload: &[u8]) -> SequenceHeader {
    let mut r = BitReader::new(payload);
    let width = r.get(12);
    let height = r.get(12);
    let _aspect_ratio_information = r.get(4);
    let _frame_rate_code = r.get(4);
    let _bit_rate_value = r.get(18);
    let _marker_bit = r.get(1);
    let _vbv_buffer_size_value = r.get(10);
    let _constrained_parameters_flag = r.get(1);

    let mut intra_matrix = tables::DEFAULT_INTRA_MATRIX;
    if r.get(1) != 0 {
        read_quant_matrix(&mut r, &mut intra_matrix);
    }
    let mut non_intra_matrix = tables::DEFAULT_NON_INTRA_MATRIX;
    if r.get(1) != 0 {
        read_quant_matrix(&mut r, &mut non_intra_matrix);
    }

    SequenceHeader {
        width,
        height,
        intra_matrix,
        non_intra_matrix,
    }
}

/// Read a zigzag-ordered `matrix[64]` (§7.3.1: matrix download always uses
/// [`tables::ZIGZAG_SCAN`], never the alternate scan) into natural `[v][u]`
/// order.
fn read_quant_matrix(r: &mut BitReader<'_>, out: &mut [u8; 64]) {
    for n in 0..64usize {
        let value = r.get(8) as u8;
        if let Some(pos) = tables::ZIGZAG_SCAN.iter().position(|&s| usize::from(s) == n)
            && let Some(slot) = out.get_mut(pos)
        {
            *slot = value;
        }
    }
}

/// `sequence_extension()`'s body, from just after the 4-bit
/// `extension_start_code_identifier` (§6.2.2.3).
#[must_use]
pub(crate) fn sequence_extension(r: &mut BitReader<'_>) -> (SequenceExtension, u32, u32) {
    let _profile_and_level_indication = r.get(8);
    let progressive_sequence = r.get(1) != 0;
    let chroma_format = r.get(2) as u8;
    let horizontal_size_extension = r.get(2);
    let vertical_size_extension = r.get(2);
    let _bit_rate_extension = r.get(12);
    let _marker_bit = r.get(1);
    let _vbv_buffer_size_extension = r.get(8);
    let _low_delay = r.get(1);
    let _frame_rate_extension_n = r.get(2);
    let _frame_rate_extension_d = r.get(5);
    (
        SequenceExtension {
            progressive_sequence,
            chroma_format,
        },
        horizontal_size_extension,
        vertical_size_extension,
    )
}

/// `quant_matrix_extension()`'s body (§6.2.3.2, reached via
/// `picture_coding_extension()`'s trailing extension data), which may
/// overwrite either or both matrices — only the loaded ones change; the
/// others keep whatever `sequence_header()` last set.
pub(crate) fn quant_matrix_extension(
    r: &mut BitReader<'_>,
    intra: &mut [u8; 64],
    non_intra: &mut [u8; 64],
) {
    if r.get(1) != 0 {
        read_quant_matrix(r, intra);
    }
    if r.get(1) != 0 {
        read_quant_matrix(r, non_intra);
    }
    // 4:2:2/4:4:4 chroma matrices are a lower-priority extensions-pass
    // concern, not this crate's core decode path.
}

/// Picture coding type, ITU-T H.262 Table 6-12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PictureType {
    I,
    P,
    B,
    /// MPEG-1 only (`picture_coding_type == 4`), a DC-only picture no
    /// MPEG-2 encoder emits. Treated as a decode error if actually seen —
    /// see `decoder::Mpeg12Decoder`.
    D,
}

impl PictureType {
    fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::I),
            2 => Some(Self::P),
            3 => Some(Self::B),
            4 => Some(Self::D),
            _ => None,
        }
    }
}

/// `picture_header()` (§6.2.3). `temporal_reference` and the two
/// `full_pel_*_vector` flags are parsed for correct framing (the fields
/// after them are conditional on having read them) but not yet consumed:
/// `temporal_reference` is not needed since this crate uses each packet's
/// own PTS rather than reconstructing display order from it, and
/// `full_pel_*_vector` (motion vectors in whole-pixel units rather than
/// half-pixel) is an unimplemented rare MPEG-1 mode.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code, reason = "parsed for correct framing, not yet consumed — see this struct's own doc comment")]
pub(crate) struct PictureHeader {
    pub temporal_reference: u16,
    pub coding_type: PictureType,
    pub full_pel_forward_vector: bool,
    pub forward_f_code: u8,
    pub full_pel_backward_vector: bool,
    pub backward_f_code: u8,
}

/// Parse `picture_header()`'s body (`payload` starts right after the
/// `00 00 01 00` start code). Returns `None` for a reserved/forbidden
/// `picture_coding_type` — a non-conforming bitstream, not a gap in this
/// crate.
#[must_use]
pub(crate) fn picture_header(payload: &[u8]) -> Option<PictureHeader> {
    let mut r = BitReader::new(payload);
    let temporal_reference = r.get(10) as u16;
    let coding_type = PictureType::from_code(r.get(3))?;
    let _vbv_delay = r.get(16);
    let (mut full_pel_forward_vector, mut forward_f_code) = (false, 0u8);
    let (mut full_pel_backward_vector, mut backward_f_code) = (false, 0u8);
    if matches!(coding_type, PictureType::P | PictureType::B) {
        full_pel_forward_vector = r.get(1) != 0;
        forward_f_code = r.get(3) as u8;
    }
    if coding_type == PictureType::B {
        full_pel_backward_vector = r.get(1) != 0;
        backward_f_code = r.get(3) as u8;
    }
    Some(PictureHeader {
        temporal_reference,
        coding_type,
        full_pel_forward_vector,
        forward_f_code,
        full_pel_backward_vector,
        backward_f_code,
    })
}

/// `picture_coding_extension()`'s body, from just after the 4-bit
/// `extension_start_code_identifier` (§6.2.3.1). MPEG-1 streams have no
/// picture-coding extension at all; [`crate::decoder`] substitutes
/// [`PictureCodingExtension::mpeg1_default`] for them.
/// `top_field_first` and `progressive_frame` are parsed but not yet
/// consumed: both only matter for a decoder's own display-order or
/// deinterlacing choices, which are out of this crate's scope (a `Frame`'s
/// `FrameFlags::TOP_FIELD_FIRST` is not set here).
#[derive(Debug, Clone, Copy)]
#[allow(dead_code, reason = "parsed for correct framing, not yet consumed — see this struct's own doc comment")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "picture_coding_extension() is a fixed bitstream layout of independently-meaningful flags (H.262 §6.2.3.1); a state machine would not reduce the count, only hide it"
)]
pub(crate) struct PictureCodingExtension {
    /// `f_code[s][t]`, `[forward/backward][horizontal/vertical]`.
    pub f_code: [[u8; 2]; 2],
    pub intra_dc_precision: u8,
    /// `0b11` = frame picture, `0b01` = top field, `0b10` = bottom field
    /// (§6.3.10). Only frame pictures are decoded.
    pub picture_structure: u8,
    pub top_field_first: bool,
    pub frame_pred_frame_dct: bool,
    pub concealment_motion_vectors: bool,
    pub q_scale_type: bool,
    pub intra_vlc_format: bool,
    pub alternate_scan: bool,
    pub progressive_frame: bool,
}

impl PictureCodingExtension {
    /// What every MPEG-1 picture behaves as: frame pictures, frame
    /// prediction only (`frame_pred_frame_dct` — MPEG-1 has no field
    /// concept), the type-0 quantiser mapping, Table B.14, zigzag scan.
    /// `f_code` still comes from `picture_header()` in MPEG-1 (its
    /// `forward_f_code`/`backward_f_code` fields), not from this struct.
    #[must_use]
    pub(crate) const fn mpeg1_default(forward_f_code: u8, backward_f_code: u8) -> Self {
        Self {
            f_code: [[forward_f_code, forward_f_code], [
                backward_f_code,
                backward_f_code,
            ]],
            intra_dc_precision: 0,
            picture_structure: 0b11,
            top_field_first: false,
            frame_pred_frame_dct: true,
            concealment_motion_vectors: false,
            q_scale_type: false,
            intra_vlc_format: false,
            alternate_scan: false,
            progressive_frame: true,
        }
    }

    #[must_use]
    pub(crate) const fn is_frame_picture(&self) -> bool {
        self.picture_structure == 0b11
    }
}

/// Parse `picture_coding_extension()`'s body.
#[must_use]
pub(crate) fn picture_coding_extension(r: &mut BitReader<'_>) -> PictureCodingExtension {
    let f_code = [
        [r.get(4) as u8, r.get(4) as u8],
        [r.get(4) as u8, r.get(4) as u8],
    ];
    let intra_dc_precision = r.get(2) as u8;
    let picture_structure = r.get(2) as u8;
    let top_field_first = r.get(1) != 0;
    let frame_pred_frame_dct = r.get(1) != 0;
    let concealment_motion_vectors = r.get(1) != 0;
    let q_scale_type = r.get(1) != 0;
    let intra_vlc_format = r.get(1) != 0;
    let alternate_scan = r.get(1) != 0;
    let _repeat_first_field = r.get(1);
    let _chroma_420_type = r.get(1);
    let progressive_frame = r.get(1) != 0;
    // composite_display_flag and its payload, if present, are not read:
    // this crate's reconstruction has no use for them and stops before
    // `next_start_code()` regardless.
    PictureCodingExtension {
        f_code,
        intra_dc_precision,
        picture_structure,
        top_field_first,
        frame_pred_frame_dct,
        concealment_motion_vectors,
        q_scale_type,
        intra_vlc_format,
        alternate_scan,
        progressive_frame,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_header_reads_dimensions() {
        // width=176 (0x0B0), height=144 (0x090), rest zero.
        // 12+12 bits: 0000_1011_0000 0000_1001_0000 -> bytes:
        let bits = [0x0Bu8, 0x00, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00];
        let seq = sequence_header(&bits);
        assert_eq!(seq.width, 176);
        assert_eq!(seq.height, 144);
        assert_eq!(seq.intra_matrix, tables::DEFAULT_INTRA_MATRIX);
    }

    #[test]
    fn picture_header_rejects_reserved_coding_type() {
        // temporal_reference=0 (10 bits), coding_type=0 (3 bits, reserved).
        let bits = [0x00u8, 0x00, 0x00, 0x00];
        assert!(picture_header(&bits).is_none());
    }

    #[test]
    fn mpeg1_default_is_frame_picture_frame_prediction() {
        let pce = PictureCodingExtension::mpeg1_default(1, 1);
        assert!(pce.is_frame_picture());
        assert!(pce.frame_pred_frame_dct);
    }
}
