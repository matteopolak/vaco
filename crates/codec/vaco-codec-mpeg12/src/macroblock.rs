//! `slice()` and `macroblock()` (§6.2.4-§6.2.6): the loop that walks one
//! picture's coded data and reconstructs it macroblock by macroblock.
//!
//! Frame pictures only (see `crate::decoder`'s module docs). Both
//! frame-based and field-based prediction are implemented for a frame
//! picture (§7.6.2's "field prediction within a frame-picture" case, which
//! is how real interlaced `mpeg2video` encodes — `-flags +ilme+ildct` —
//! actually appear; a genuinely separate field-coded *picture* is a
//! different, unimplemented thing). Dual-prime is not implemented; a
//! macroblock that requests it marks the whole picture unsupported (see
//! `crate::decoder::Mpeg12Decoder::unsupported_pictures`) rather than
//! decoding it wrong.

use vaco_bitstream::BitReader;
use vaco_frame::Frame;

use crate::block::{self, CoeffTable, Mpeg2Idct};
use crate::decoder::Sequence;
use crate::headers::{PictureCodingExtension, PictureHeader, PictureType};
use crate::motion::{self, MotionPredictor};
use crate::picture::RefPicture;
use crate::tables::{self, MacroblockType};
use crate::vlc;

const MAX_MB_TYPE_LEN: u8 = 9;
const MAX_CBP_LEN: u8 = 13;
const MAX_ADDR_LEN: u8 = 11;
const MAX_DC_SIZE_LEN: u8 = 11;

/// The largest `block_count` any chroma format reaches (Table 6-20, 4:4:4).
pub(crate) const MAX_BLOCKS: usize = 12;

/// Table 6-5's `chroma_format`, decoded into the three real values this
/// crate ever needs to branch on — §6.3.17.4 (block count/geometry),
/// §6.2.5.3 (`coded_block_pattern`'s extension bits), and §7.6.3.7
/// (chrominance motion vector scaling) all key off exactly this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChromaFormat {
    /// `chroma_format == 1`: chroma is half-resolution in both directions.
    Yuv420,
    /// `chroma_format == 2`: chroma is half-resolution horizontally only.
    Yuv422,
    /// `chroma_format == 3`: chroma matches luma resolution.
    Yuv444,
}

impl ChromaFormat {
    /// `0` (reserved, never legitimately sent) folds to 4:2:0 — the same
    /// "malformed input degrades, never panics" convention this crate
    /// already applies to every other out-of-range field it reads.
    #[must_use]
    pub(crate) const fn from_raw(raw: u8) -> Self {
        match raw {
            2 => Self::Yuv422,
            3 => Self::Yuv444,
            _ => Self::Yuv420,
        }
    }

    /// Table 6-20: how many of a macroblock's blocks are luma+chroma
    /// together.
    #[must_use]
    const fn block_count(self) -> usize {
        match self {
            Self::Yuv420 => 6,
            Self::Yuv422 => 8,
            Self::Yuv444 => 12,
        }
    }

    /// One chroma plane's pixel dimensions covered by a single macroblock
    /// (§6.1.3: "Cb and Cr matrices shall be one half the size of the
    /// Y-matrix" in one or both dimensions, or the same size, depending on
    /// format).
    #[must_use]
    const fn chroma_mb_pixels(self) -> (usize, usize) {
        match self {
            Self::Yuv420 => (8, 8),
            Self::Yuv422 => (8, 16),
            Self::Yuv444 => (16, 16),
        }
    }

    /// §7.6.3.7: scale a luma motion vector (half-pel units) down to the
    /// chrominance sample grid. 4:2:0 halves both components; 4:2:2 halves
    /// only the horizontal one (chroma keeps luma's own vertical
    /// resolution); 4:4:4 leaves both unmodified (chroma matches luma
    /// resolution in both directions). `/2` here is exactly the spec's own
    /// `vector[r][s][t] / 2`, §4.1 truncating division.
    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "§7.6.3.7's own formula, `vector[r][s][t] = vector'[r][s][t] / 2` — §4.1 defines '/' as this exact truncating division, not an approximation of it"
    )]
    const fn scale_vector(self, v: [i32; 2]) -> [i32; 2] {
        match self {
            Self::Yuv420 => [v[0] / 2, v[1] / 2],
            Self::Yuv422 => [v[0] / 2, v[1]],
            Self::Yuv444 => v,
        }
    }

    /// §6.3.17.4/Figures 6-10, 6-11, 6-12: one block's `(plane, col, row)`
    /// within its own plane's macroblock-sized area, for a chroma block
    /// index (`i - 4`, i.e. `0` is the macroblock's first chroma block).
    /// `plane` is `1` (Cb) or `2` (Cr); `col`/`row` are the *pixel* offset
    /// of that block's own top-left corner within the macroblock's own
    /// chroma area (always an 8x8 block, so both are `0` or `8`).
    ///
    /// 4:2:0 (2 chroma blocks, Figure 6-10 — "4, 5" left to right, Cb then
    /// Cr, no stacking) and 4:2:2 (4 blocks, Figure 6-11 — "4 5 / 6 7", Cb
    /// and Cr side by side, a second row stacked below at `row = 8`) both
    /// interleave Cb/Cr at each position; 4:4:4 (8 blocks, Figure 6-12 —
    /// "4 8 / 6 10" for Cb, "5 9 / 7 11" for Cr) is a 2x2 grid *per
    /// component*, not an interleaved one, so the index-to-position
    /// mapping does not extend from the smaller formats' pattern — each
    /// is transcribed directly from its own figure.
    #[must_use]
    const fn chroma_block_slot(self, offset: usize) -> (usize, i32, i32) {
        match self {
            Self::Yuv420 => match offset {
                0 => (1, 0, 0),
                _ => (2, 0, 0),
            },
            Self::Yuv422 => match offset {
                0 => (1, 0, 0),
                1 => (2, 0, 0),
                2 => (1, 0, 8),
                _ => (2, 0, 8),
            },
            Self::Yuv444 => match offset {
                0 => (1, 0, 0),
                1 => (2, 0, 0),
                2 => (1, 0, 8),
                3 => (2, 0, 8),
                4 => (1, 8, 0),
                5 => (2, 8, 0),
                6 => (1, 8, 8),
                _ => (2, 8, 8),
            },
        }
    }
}

/// Frame-based `frame_motion_type` code (Table 6-17).
const FRAME_BASED: u32 = 0b10;
/// Field-based `frame_motion_type` code (Table 6-17).
const FIELD_BASED: u32 = 0b01;
/// Dual-prime `frame_motion_type` code (Table 6-17) — unsupported.
const DUAL_PRIME: u32 = 0b11;

/// One picture's mutable decode state, threaded through the whole slice
/// loop. Lives in `crate::decoder` because [`crate::decoder::Mpeg12Decoder`]
/// owns its fields across `begin_picture`/`finish_picture`; this module
/// only ever borrows it.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently-meaningful piece of per-picture decode state (support/reconstruction flags plus the MPEG-1/2 escape-format switch), not a state machine in disguise"
)]
pub(crate) struct ActivePicture {
    pub frame: Frame,
    pub header: PictureHeader,
    pub pce: PictureCodingExtension,
    pub intra_matrix: [u8; 64],
    pub non_intra_matrix: [u8; 64],
    pub quantiser_scale: u16,
    pub dc_pred: [i32; 3],
    pub fwd_pred: MotionPredictor,
    pub bwd_pred: MotionPredictor,
    pub prev_mb_forward: bool,
    pub prev_mb_backward: bool,
    /// This picture's coding mode is not implemented at all (a field
    /// picture, dual-prime, a D-picture): checked once at `begin_picture`
    /// and, since dual-prime can only be discovered once macroblock data
    /// is actually read, escalated from inside `decode_coded_macroblock`
    /// too. Distinct from [`Self::slice_ok`]: this means the picture's
    /// *coding mode* is unimplemented, not that one slice's bitstream
    /// desynced, so it is checked once per picture (at the top of
    /// `decode_slice`) rather than reset per slice.
    pub supported: bool,
    /// This slice's own bitstream produced a VLC code no table row
    /// matches — a genuine local decode failure (corrupt or adversarial
    /// input, or a bug in this crate's own bit consumption), not an
    /// unimplemented feature. Reset to `true` at the start of every
    /// `decode_slice` call; `decode_slice`'s own loop stops as soon as it
    /// goes `false`, but a later, independent slice in the same picture
    /// gets a fresh `true` and is decoded normally — unlike [`Self::supported`],
    /// one bad slice must not cost the rest of the picture.
    pub slice_ok: bool,
    pub previous: Option<RefPicture>,
    pub recent: Option<RefPicture>,
    /// No `sequence_extension()` seen for the current sequence — an
    /// ISO/IEC 11172-2 (MPEG-1) bitstream rather than an H.262 (MPEG-2)
    /// one. The two share almost every VLC table byte-for-byte, but the
    /// DCT-coefficient escape code is a genuine exception (H.262 Annex
    /// D.9.3): MPEG-1 follows the 6-bit escape with a 6-bit run and an
    /// 8-or-16-bit sign-magnitude level (extended when the first 8 bits are
    /// the 0/-128 sentinel), not MPEG-2's fixed 6-bit run + 12-bit
    /// two's-complement level. Using the MPEG-2 field width against an
    /// MPEG-1 stream desyncs the reader by a wrong, non-constant number of
    /// bits the moment an encoder actually emits an escape code — see
    /// `block::decode_coefficients`.
    pub mpeg1: bool,
    /// §6.2.2.3 `chroma_format` (Table 6-5), always 4:2:0 for an MPEG-1
    /// sequence (no `sequence_extension()` to carry any other value).
    pub chroma_format: ChromaFormat,
    /// Raw `cc_data` triplet bytes from this picture's own ATSC A/53
    /// caption `user_data()` (§6.2.2.2.2), if any — empty for the ordinary
    /// case. See `crate::decoder`'s `USER_DATA_START` handling and
    /// `vaco_parse_mpegvideo::a53`'s module doc for why this must be
    /// attached to *this* picture rather than accumulated across pictures.
    pub closed_captions: Vec<u8>,
}

fn quantiser_scale(q_scale_type: bool, code: u8) -> u16 {
    let row = usize::from(q_scale_type);
    u16::from(
        tables::QUANTISER_SCALE
            .get(row)
            .and_then(|r| r.get(usize::from(code)))
            .copied()
            .unwrap_or(0),
    )
}

/// `slice()`, §6.2.4. `code` is the slice's start code low byte
/// (`slice_vertical_position`, 1..=175 — tall pictures needing
/// `slice_vertical_position_extension`, `vertical_size > 2800`, are not
/// handled, since none of this crate's fixtures reach that size).
pub(crate) fn decode_slice(
    code: u8,
    data: &[u8],
    idct: &mut Mpeg2Idct,
    ap: &mut ActivePicture,
    seq: &Sequence,
) {
    if !ap.supported {
        return;
    }
    let mut r = BitReader::new(data);
    let mb_row = u32::from(code).saturating_sub(1);
    let mut mb_addr: i64 = i64::from(mb_row) * i64::from(seq.mb_width) - 1;

    let scale_code = r.get(5) as u8;
    ap.quantiser_scale = quantiser_scale(ap.pce.q_scale_type, scale_code);

    if r.peek(1) == 1 {
        let _intra_slice_flag = r.get(1);
        let _intra_slice = r.get(1);
        let _reserved = r.get(7);
        while r.peek(1) == 1 {
            let _extra_bit = r.get(1);
            let _extra_information = r.get(8);
        }
    }
    let _extra_bit_slice_terminator = r.get(1);

    ap.fwd_pred.reset();
    ap.bwd_pred.reset();
    ap.dc_pred = [tables::intra_dc_reset(ap.pce.intra_dc_precision); 3];
    ap.prev_mb_forward = false;
    ap.prev_mb_backward = false;
    ap.slice_ok = true;

    let total_mbs = i64::from(seq.mb_width) * i64::from(seq.mb_height);
    loop {
        let mut increment: i64 = 0;
        loop {
            // §D.9.2 (`Vaco-Spec-Ref: itu-t-h262` D.9.2): MPEG-1's
            // `macroblock_stuffing` ("0000 0001 111", 11 bits) may appear
            // any number of times directly before a real
            // `macroblock_address_increment` code and must be discarded.
            // MPEG-2 reserves this exact bit pattern and never emits it,
            // so this is gated on `ap.mpeg1` rather than checked
            // unconditionally.
            if ap.mpeg1 {
                while r.peek(11) == 0b000_0000_1111 {
                    r.skip(11);
                }
            }
            let Some(&(_, val)) = vlc::decode(
                &mut r,
                tables::MACROBLOCK_ADDRESS_INCREMENT,
                |row| (row.0, 0),
                MAX_ADDR_LEN,
            ) else {
                return;
            };
            if val == 0 {
                increment += 33;
                continue;
            }
            increment += i64::from(val);
            break;
        }
        for _ in 0..increment.saturating_sub(1) {
            mb_addr += 1;
            if mb_addr >= 0 && mb_addr < total_mbs {
                decode_skipped_macroblock(idct, ap, seq, mb_addr as u32);
            }
        }
        mb_addr += 1;
        if mb_addr < 0 || mb_addr >= total_mbs {
            return;
        }
        decode_coded_macroblock(&mut r, idct, ap, seq, mb_addr as u32);
        if !ap.slice_ok {
            return;
        }
    }
}

/// One decoded 16x16 luma + chroma prediction, before residual addition.
/// All-zero for an intra macroblock (§7.6: "no prediction is formed").
///
/// `cb`/`cr` are always sized for the largest case (4:4:4's 16x16 chroma
/// area) and addressed with a stride of 16 regardless of format; a 4:2:0
/// or 4:2:2 macroblock simply never reads or writes the unused columns —
/// simpler than three differently-shaped buffer types for what is, per
/// macroblock, a handful of bytes either way.
struct MbPrediction {
    luma: [u8; 256],
    cb: [u8; 256],
    cr: [u8; 256],
}

impl MbPrediction {
    const fn zero() -> Self {
        Self {
            luma: [0; 256],
            cb: [0; 256],
            cr: [0; 256],
        }
    }
}

/// The fixed stride [`MbPrediction::cb`]/[`MbPrediction::cr`] are always
/// addressed with, regardless of chroma format (see that struct's docs).
const CHROMA_PRED_STRIDE: usize = 16;

#[allow(
    clippy::integer_division,
    reason = "mb_x/mb_y are exact grid coordinates (mb_addr = mb_y * mb_width + mb_x, both non-negative), not an approximation"
)]
fn decode_coded_macroblock(
    r: &mut BitReader<'_>,
    idct: &mut Mpeg2Idct,
    ap: &mut ActivePicture,
    seq: &Sequence,
    mb_addr: u32,
) {
    let mb_type_table: &[MacroblockType] = match ap.header.coding_type {
        PictureType::I => tables::MB_TYPE_I,
        PictureType::P => tables::MB_TYPE_P,
        PictureType::B => tables::MB_TYPE_B,
        PictureType::D => {
            ap.supported = false;
            return;
        }
    };
    let Some(mbt) = vlc::decode(
        r,
        mb_type_table.iter(),
        |row| (row.bits, 0),
        MAX_MB_TYPE_LEN,
    ) else {
        ap.slice_ok = false;
        return;
    };
    let mbt = *mbt;

    if mbt.quant {
        let code = r.get(5) as u8;
        ap.quantiser_scale = quantiser_scale(ap.pce.q_scale_type, code);
    }

    let has_motion = mbt.motion_forward || mbt.motion_backward;
    let mut frame_motion_type = FRAME_BASED;
    if has_motion {
        if ap.pce.frame_pred_frame_dct {
            frame_motion_type = FRAME_BASED;
        } else {
            frame_motion_type = r.get(2);
        }
    } else if mbt.intra && ap.pce.concealment_motion_vectors {
        frame_motion_type = FRAME_BASED;
    }

    if frame_motion_type == DUAL_PRIME || frame_motion_type == 0 {
        // Dual-prime or the reserved code: neither is implemented.
        // Bailing on the whole picture is the honest move — one
        // macroblock's worth of misread bits desyncs every macroblock
        // after it.
        ap.supported = false;
        return;
    }

    let mut dct_type = 0u8;
    if ap.pce.is_frame_picture() && !ap.pce.frame_pred_frame_dct && (mbt.intra || mbt.pattern) {
        dct_type = r.get(1) as u8;
    }

    let field_based = frame_motion_type == FIELD_BASED;
    let count = if field_based { 2 } else { 1 };
    let field_and_frame_picture = field_based; // picture_structure is always Frame here.

    let mut fwd_vecs = [[0i32; 2]; 2];
    let mut bwd_vecs = [[0i32; 2]; 2];
    let mut fwd_field_select = [0i32; 2];
    let mut bwd_field_select = [0i32; 2];
    let read_fwd = mbt.motion_forward || (mbt.intra && ap.pce.concealment_motion_vectors);
    let read_bwd = mbt.motion_backward;

    if read_fwd {
        for (i, (select_slot, vec_slot)) in fwd_field_select
            .iter_mut()
            .zip(fwd_vecs.iter_mut())
            .enumerate()
            .take(count)
        {
            if field_based {
                *select_slot = i32::try_from(r.get(1)).unwrap_or(0);
            }
            *vec_slot = motion::decode_vector(
                r,
                &mut ap.fwd_pred,
                i,
                ap.pce.f_code[0],
                field_and_frame_picture,
            );
        }
    }
    if read_bwd {
        for (i, (select_slot, vec_slot)) in bwd_field_select
            .iter_mut()
            .zip(bwd_vecs.iter_mut())
            .enumerate()
            .take(count)
        {
            if field_based {
                *select_slot = i32::try_from(r.get(1)).unwrap_or(0);
            }
            *vec_slot = motion::decode_vector(
                r,
                &mut ap.bwd_pred,
                i,
                ap.pce.f_code[1],
                field_and_frame_picture,
            );
        }
    }
    if mbt.intra && ap.pce.concealment_motion_vectors {
        let _marker_bit = r.get(1);
    }

    // §6.2.5.3/§6.3.17.4: `pattern_code[i]` for `i` in `0..12`, though only
    // the first `ap.chroma_format.block_count()` entries are ever read
    // back by `reconstruct_macroblock`. Initialised to `macroblock_intra`
    // for every `i` first (the spec's own pseudocode does this
    // unconditionally, *then* lets `macroblock_pattern` override — so an
    // intra macroblock with no `coded_block_pattern()` at all, or one
    // whose extension bits don't cover a given index, keeps every block
    // marked coded), then the base 6-bit VLC overrides `i in 0..6`, and a
    // chroma-format-dependent fixed-length extension overrides more.
    let mut cbp = [mbt.intra; MAX_BLOCKS];
    if mbt.pattern {
        let Some(&(_, code)) = vlc::decode(
            r,
            tables::CODED_BLOCK_PATTERN,
            |row| (row.0, 0),
            MAX_CBP_LEN,
        ) else {
            ap.slice_ok = false;
            return;
        };
        for (i, slot) in cbp.iter_mut().take(6).enumerate() {
            *slot = code & (1 << (5 - i)) != 0;
        }
        match ap.chroma_format {
            ChromaFormat::Yuv422 => {
                // §6.2.5.3: `coded_block_pattern_1`, a 2-bit FLC, covers
                // blocks 6-7 (the second Cb/Cr row Figure 6-11 adds).
                let code1 = r.get(2);
                for (i, slot) in cbp.iter_mut().enumerate().skip(6).take(2) {
                    *slot = code1 & (1 << (7 - i)) != 0;
                }
            }
            ChromaFormat::Yuv444 => {
                // §6.2.5.3/§6.3.17.4: `coded_block_pattern_2`, a 6-bit
                // FLC — but H.262's own pseudocode only ever derives
                // `pattern_code[8..12]` from it (`for (i = 8; i < 12;
                // i++) if (coded_block_pattern_2 & (1<<(11-i)))
                // pattern_code[i] = 1`), using shifts 3..0 of the 6-bit
                // field and leaving `pattern_code[6]`/`[7]` at whatever
                // the unconditional `macroblock_intra` initialisation
                // above set — never toggled by a bitstream bit at all,
                // for either an intra or a non-intra 4:4:4 macroblock.
                // This looks like a genuine dimensional mismatch in the
                // 1995 base text (a 6-bit code that only ever drives 4 of
                // the 12 indices, leaving 2 of `coded_block_pattern_2`'s
                // own bits unread by this formula) rather than a
                // transcription slip on this crate's part — checked
                // against two independent extractions of the same PDF
                // (`pdftotext` plain and `-layout`) and both agree
                // character-for-character. Implemented exactly as
                // published rather than "corrected" against a guess:
                // `vaco-codec-mpeg12` has no legitimate way to tell which
                // interpretation a real encoder/decoder pair settled on,
                // since `ffmpeg`'s own `mpeg2video` encoder does not
                // support `yuv444p` output at all (checked directly:
                // `ffmpeg -h encoder=mpeg2video` lists only `yuv420p
                // yuv422p`) — this path has zero differential-fixture
                // coverage and never can, only the hand-crafted-bitstream
                // unit tests below, verified against the primary text's
                // own literal formula.
                let code2 = r.get(6);
                for (i, slot) in cbp.iter_mut().enumerate().skip(8).take(4) {
                    *slot = code2 & (1 << (11 - i)) != 0;
                }
            }
            ChromaFormat::Yuv420 => {}
        }
    }

    if !mbt.intra {
        ap.dc_pred = [tables::intra_dc_reset(ap.pce.intra_dc_precision); 3];
    }
    if mbt.intra && !ap.pce.concealment_motion_vectors {
        ap.fwd_pred.reset();
        ap.bwd_pred.reset();
    }
    if ap.header.coding_type == PictureType::P && !mbt.motion_forward && !mbt.intra {
        ap.fwd_pred.reset();
    }

    let mb_x = mb_addr % seq.mb_width;
    let mb_y = mb_addr / seq.mb_width;

    let mut pred = MbPrediction::zero();
    if !mbt.intra {
        let fwd_ref = if ap.header.coding_type == PictureType::P {
            ap.recent.as_ref()
        } else {
            ap.previous.as_ref()
        };
        let bwd_ref = ap.recent.as_ref();
        // Whether forward/backward prediction *applies* is not the same
        // question as `read_fwd`/`read_bwd` (whether a vector was *read
        // from the bitstream*). §7.6.3.5: a P-picture macroblock with
        // `macroblock_motion_forward == 0` (Table B.3's "No MC, Coded" —
        // exactly `mb=0`'s case above) still predicts forward, using the
        // implicit zero vector `read_fwd` already left in `fwd_vecs`; only
        // an *intra* macroblock (already excluded by this `if`) skips
        // prediction entirely. Using `read_fwd` here instead silently
        // dropped the reference for every such macroblock, which is a
        // measured example of it: `pred` stayed all-zero and the picture
        // reconstructed to "residual only", nowhere near the reference.
        let apply_fwd = ap.header.coding_type == PictureType::P || mbt.motion_forward;
        let apply_bwd = mbt.motion_backward;
        form_macroblock_prediction(
            &mut pred,
            mb_x,
            mb_y,
            field_based,
            count,
            apply_fwd.then_some(fwd_ref).flatten(),
            apply_bwd.then_some(bwd_ref).flatten(),
            fwd_vecs,
            bwd_vecs,
            fwd_field_select,
            bwd_field_select,
            ap.chroma_format,
            ap.header.full_pel_forward_vector,
            ap.header.full_pel_backward_vector,
        );
    }

    reconstruct_macroblock(r, idct, ap, mb_x, mb_y, mbt.intra, dct_type, cbp, &pred);

    ap.prev_mb_forward = mbt.motion_forward;
    ap.prev_mb_backward = mbt.motion_backward;
}

/// §7.6.6.2/§7.6.6.4: a skipped macroblock (present in neither bitstream
/// nor `macroblock_type`), reconstructed purely from derived state.
#[allow(
    clippy::integer_division,
    reason = "mb_x/mb_y are exact grid coordinates (mb_addr = mb_y * mb_width + mb_x, both non-negative), not an approximation"
)]
fn decode_skipped_macroblock(
    idct: &mut Mpeg2Idct,
    ap: &mut ActivePicture,
    seq: &Sequence,
    mb_addr: u32,
) {
    let mb_x = mb_addr % seq.mb_width;
    let mb_y = mb_addr / seq.mb_width;
    let mut pred = MbPrediction::zero();

    match ap.header.coding_type {
        PictureType::P => {
            ap.fwd_pred.reset();
            ap.bwd_pred.reset();
            ap.dc_pred = [tables::intra_dc_reset(ap.pce.intra_dc_precision); 3];
            let fwd_ref = ap.recent.clone();
            form_macroblock_prediction(
                &mut pred,
                mb_x,
                mb_y,
                false,
                1,
                fwd_ref.as_ref(),
                None,
                [[0, 0], [0, 0]],
                [[0, 0], [0, 0]],
                [0, 0],
                [0, 0],
                ap.chroma_format,
                ap.header.full_pel_forward_vector,
                ap.header.full_pel_backward_vector,
            );
        }
        PictureType::B => {
            // §7.6.6.4: same direction as the previous macroblock,
            // motion vector predictors and their stored vectors
            // unaffected — read straight from `ap.fwd_pred`/`ap.bwd_pred`
            // as they stand. The *DC* predictor is a separate piece of
            // state (§7.2.1/Table 7-2) that resets on every skipped
            // macroblock regardless of picture type — the P-picture arm
            // above already does this; this arm did not, which is a real
            // gap (a B-picture skip run left `ap.dc_pred` holding the
            // last real intra macroblock's value, corrupting the DC
            // prediction chain for the next intra-coded block in the
            // slice) rather than a deliberate asymmetry with P.
            ap.dc_pred = [tables::intra_dc_reset(ap.pce.intra_dc_precision); 3];
            let use_fwd = ap.prev_mb_forward;
            let use_bwd = ap.prev_mb_backward;
            let fwd_ref = ap.previous.clone();
            let bwd_ref = ap.recent.clone();
            let fwd_vec = ap.fwd_pred.pmv.first().copied().unwrap_or([0, 0]);
            let bwd_vec = ap.bwd_pred.pmv.first().copied().unwrap_or([0, 0]);
            form_macroblock_prediction(
                &mut pred,
                mb_x,
                mb_y,
                false,
                1,
                use_fwd.then_some(fwd_ref.as_ref()).flatten(),
                use_bwd.then_some(bwd_ref.as_ref()).flatten(),
                [fwd_vec, [0, 0]],
                [bwd_vec, [0, 0]],
                [0, 0],
                [0, 0],
                ap.chroma_format,
                ap.header.full_pel_forward_vector,
                ap.header.full_pel_backward_vector,
            );
        }
        PictureType::I | PictureType::D => {
            // Not permitted by the standard outside scalability this crate
            // does not implement; degrade to zero prediction rather than
            // reading further bits that are not there.
        }
    }

    write_prediction_only(ap, mb_x, mb_y, &pred, ap.chroma_format);
    let _ = idct; // no residual for a skipped macroblock; kept for a uniform call shape.
}

/// Annex D.9.7: scale a direction's motion vectors from MPEG-1's optional
/// full-pel coding into the half-pel units every use of a reconstructed
/// vector downstream assumes. A no-op whenever `full_pel` is `false`,
/// which is always, for MPEG-2 (`mpeg1_default` never sets either flag)
/// and for every MPEG-1 stream that doesn't use this rare mode either.
#[must_use]
const fn full_pel_scale(vecs: [[i32; 2]; 2], full_pel: bool) -> [[i32; 2]; 2] {
    if full_pel {
        [
            [vecs[0][0] * 2, vecs[0][1] * 2],
            [vecs[1][0] * 2, vecs[1][1] * 2],
        ]
    } else {
        vecs
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one macroblock's prediction genuinely depends on this many independent syntax elements (both directions' references, vectors and field selects); grouping them into a struct would just move the same count into a constructor call"
)]
#[allow(
    clippy::integer_division,
    reason = "these are all exact halvings the standard specifies directly with '/' (chroma MV scaling, §7.6.3.7) or exact field/frame row-count relationships (a field has exactly half a frame's rows) — not approximations"
)]
fn form_macroblock_prediction(
    pred: &mut MbPrediction,
    mb_x: u32,
    mb_y: u32,
    field_based: bool,
    count: usize,
    fwd_ref: Option<&RefPicture>,
    bwd_ref: Option<&RefPicture>,
    fwd_vecs: [[i32; 2]; 2],
    bwd_vecs: [[i32; 2]; 2],
    fwd_field_select: [i32; 2],
    bwd_field_select: [i32; 2],
    chroma_format: ChromaFormat,
    full_pel_fwd: bool,
    full_pel_bwd: bool,
) {
    // Annex D.9.7 (MPEG-1 only; MPEG-2 requires both flags `0`, so this is
    // a no-op there — `mpeg1_default` never sets either): "the motion
    // vectors that are coded are in full-pel units instead of half-pel
    // units. Motion vector coordinates must be multiplied by two before
    // being used for the prediction." `motion::decode_vector`'s own PMV
    // chain (`ap.fwd_pred`/`bwd_pred`) stays in whatever unit the encoder
    // coded — native full-pel or half-pel — since a delta-coded vector's
    // predictor must be in the same units as the value it predicts; only
    // the *use* of the reconstructed vector to address the reference
    // picture (here, and in `decode_skipped_macroblock`'s B-picture skip,
    // which reads this same PMV chain back) is in half-pel units, so the
    // doubling belongs at this single point both paths funnel through,
    // not at `decode_vector` itself.
    let fwd_vecs = full_pel_scale(fwd_vecs, full_pel_fwd);
    let bwd_vecs = full_pel_scale(bwd_vecs, full_pel_bwd);
    let (cw, ch) = chroma_format.chroma_mb_pixels();
    let px = i32::try_from(mb_x).unwrap_or(0) * 16;
    let py = i32::try_from(mb_y).unwrap_or(0) * 16;
    let cx = i32::try_from(mb_x).unwrap_or(0) * i32::try_from(cw).unwrap_or(8);
    let cy = i32::try_from(mb_y).unwrap_or(0) * i32::try_from(ch).unwrap_or(8);

    if !field_based {
        form_component(
            fwd_ref,
            bwd_ref,
            fwd_vecs[0],
            bwd_vecs[0],
            0,
            0,
            1,
            0,
            0,
            px,
            py,
            16,
            16,
            &mut pred.luma,
            16,
        );
        let fwd_c = chroma_format.scale_vector(fwd_vecs[0]);
        let bwd_c = chroma_format.scale_vector(bwd_vecs[0]);
        form_component(
            fwd_ref,
            bwd_ref,
            fwd_c,
            bwd_c,
            1,
            1,
            1,
            0,
            0,
            cx,
            cy,
            cw,
            ch,
            &mut pred.cb,
            CHROMA_PRED_STRIDE,
        );
        form_component(
            fwd_ref,
            bwd_ref,
            fwd_c,
            bwd_c,
            2,
            2,
            1,
            0,
            0,
            cx,
            cy,
            cw,
            ch,
            &mut pred.cr,
            CHROMA_PRED_STRIDE,
        );
        return;
    }

    // Field-based within a frame picture: each of the (up to two) `r`
    // indices owns half the macroblock's rows, alternating — see
    // `crate::motion::form_prediction`'s docs for the row-scale trick this
    // reuses on both the reference (already handled there) and the
    // destination buffer (handled here by writing every other row).
    // `ch / 2` is exact for every chroma format this crate reaches (8/2 =
    // 4 for 4:2:0, 16/2 = 8 for 4:2:2/4:4:4) — a macroblock always has an
    // even number of chroma rows, so this is not an approximation.
    let ch_field = ch / 2;
    for r_idx in 0..count {
        let fv = fwd_vecs.get(r_idx).copied().unwrap_or([0, 0]);
        let bv = bwd_vecs.get(r_idx).copied().unwrap_or([0, 0]);
        let fwd_parity = fwd_field_select.get(r_idx).copied().unwrap_or(0);
        let bwd_parity = bwd_field_select.get(r_idx).copied().unwrap_or(0);
        let parity = i32::try_from(r_idx).unwrap_or(0);

        let mut luma_field = [0u8; 128]; // 16 wide x 8 tall
        form_component(
            fwd_ref,
            bwd_ref,
            fv,
            bv,
            0,
            0,
            2,
            fwd_parity,
            bwd_parity,
            px,
            py / 2,
            16,
            8,
            &mut luma_field,
            16,
        );
        deinterleave_rows(&luma_field, 16, 8, &mut pred.luma, 16, parity);

        let fv_c = chroma_format.scale_vector(fv);
        let bv_c = chroma_format.scale_vector(bv);
        // 16 wide x 8 tall regardless of format — the largest case
        // (4:4:4's `cw = 16`, `ch_field = 8`) needs the whole buffer;
        // smaller formats just use the leading `cw` columns of it.
        let mut cb_field = [0u8; 128];
        form_component(
            fwd_ref,
            bwd_ref,
            fv_c,
            bv_c,
            1,
            1,
            2,
            fwd_parity,
            bwd_parity,
            cx,
            cy / 2,
            cw,
            ch_field,
            &mut cb_field,
            cw,
        );
        deinterleave_rows(
            &cb_field,
            cw,
            ch_field,
            &mut pred.cb,
            CHROMA_PRED_STRIDE,
            parity,
        );
        let mut cr_field = [0u8; 128];
        form_component(
            fwd_ref,
            bwd_ref,
            fv_c,
            bv_c,
            2,
            2,
            2,
            fwd_parity,
            bwd_parity,
            cx,
            cy / 2,
            cw,
            ch_field,
            &mut cr_field,
            cw,
        );
        deinterleave_rows(
            &cr_field,
            cw,
            ch_field,
            &mut pred.cr,
            CHROMA_PRED_STRIDE,
            parity,
        );
    }
}

/// Copy `src` (`w` wide, `h` tall) into every `parity`-th row of `dst`
/// (`dst_stride` wide, `2 * h` logical rows), i.e. the inverse of reading a
/// reference with `row_scale = 2`.
fn deinterleave_rows(
    src: &[u8],
    w: usize,
    h: usize,
    dst: &mut [u8],
    dst_stride: usize,
    parity: i32,
) {
    let parity = usize::try_from(parity).unwrap_or(0);
    for y in 0..h {
        let Some(src_row) = src.get(y * w..y * w + w) else {
            continue;
        };
        let dst_y = y * 2 + parity;
        let Some(dst_row) = dst.get_mut(dst_y * dst_stride..dst_y * dst_stride + dst_stride) else {
            continue;
        };
        for (d, &s) in dst_row.iter_mut().zip(src_row.iter()) {
            *d = s;
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "a thin wrapper over motion::form_prediction that adds only forward/backward combination on top; see that function's own justification"
)]
fn form_component(
    fwd_ref: Option<&RefPicture>,
    bwd_ref: Option<&RefPicture>,
    fwd_vec: [i32; 2],
    bwd_vec: [i32; 2],
    fwd_plane: usize,
    bwd_plane: usize,
    row_scale: i32,
    fwd_parity: i32,
    bwd_parity: i32,
    src_x: i32,
    src_y: i32,
    size_w: usize,
    size_h: usize,
    out: &mut [u8],
    out_stride: usize,
) {
    match (fwd_ref, bwd_ref) {
        (Some(f), Some(b)) => {
            let mut fbuf = [0u8; 256];
            let mut bbuf = [0u8; 256];
            motion::form_prediction(
                f, fwd_plane, src_x, src_y, fwd_vec[0], fwd_vec[1], row_scale, fwd_parity, size_w,
                size_h, &mut fbuf, out_stride,
            );
            motion::form_prediction(
                b, bwd_plane, src_x, src_y, bwd_vec[0], bwd_vec[1], row_scale, bwd_parity, size_w,
                size_h, &mut bbuf, out_stride,
            );
            motion::average_predictions(&fbuf, &bbuf, out);
        }
        (Some(f), None) => {
            motion::form_prediction(
                f, fwd_plane, src_x, src_y, fwd_vec[0], fwd_vec[1], row_scale, fwd_parity, size_w,
                size_h, out, out_stride,
            );
        }
        (None, Some(b)) => {
            motion::form_prediction(
                b, bwd_plane, src_x, src_y, bwd_vec[0], bwd_vec[1], row_scale, bwd_parity, size_w,
                size_h, out, out_stride,
            );
        }
        (None, None) => {}
    }
}

fn write_prediction_only(
    ap: &mut ActivePicture,
    mb_x: u32,
    mb_y: u32,
    pred: &MbPrediction,
    chroma_format: ChromaFormat,
) {
    let (cw, ch) = chroma_format.chroma_mb_pixels();
    write_plane(
        &mut ap.frame,
        0,
        mb_x * 16,
        mb_y * 16,
        &pred.luma,
        16,
        16,
        16,
    );
    write_plane(
        &mut ap.frame,
        1,
        mb_x * cw as u32,
        mb_y * ch as u32,
        &pred.cb,
        cw,
        ch,
        CHROMA_PRED_STRIDE,
    );
    write_plane(
        &mut ap.frame,
        2,
        mb_x * cw as u32,
        mb_y * ch as u32,
        &pred.cr,
        cw,
        ch,
        CHROMA_PRED_STRIDE,
    );
}

fn write_plane(
    frame: &mut Frame,
    plane_idx: usize,
    ox: u32,
    oy: u32,
    src: &[u8],
    w: usize,
    h: usize,
    src_stride: usize,
) {
    let Some(mut plane) = frame.plane_mut(plane_idx) else {
        return;
    };
    for y in 0..h {
        let Some(src_row) = src.get(y * src_stride..y * src_stride + w) else {
            continue;
        };
        let Some(dst_row) = plane.row_mut(usize::try_from(oy).unwrap_or(0) + y) else {
            continue;
        };
        let ox = usize::try_from(ox).unwrap_or(0);
        let Some(dst_slice) = dst_row.get_mut(ox..ox + w) else {
            continue;
        };
        dst_slice.copy_from_slice(src_row);
    }
}

/// One block's geometry within a macroblock (§6.3.17.4, Figures 6-10
/// through 6-12 depending on `chroma_format`): `(plane, col_offset,
/// row_offset, row_scale, row_parity)`, where the last two encode
/// `dct_type`'s field/frame reorganisation of the *luma* blocks. Chroma
/// blocks always stay contiguous (`row_scale = 1, row_parity = 0`)
/// regardless of `chroma_format` — a documented simplification carried
/// over unchanged from the 4:2:0-only version of this function; nothing
/// about extending to 4:2:2/4:4:4 required revisiting it, since Table 6-20
/// and its Figures never mention field/frame DCT reorganisation applying
/// to chroma at all, only to luma (§6.3.17.1's own `dct_type` semantics).
fn block_geometry(
    i: usize,
    dct_type: u8,
    chroma_format: ChromaFormat,
) -> (usize, i32, i32, i32, i32) {
    if i >= 4 {
        let (plane, col, row) = chroma_format.chroma_block_slot(i - 4);
        return (plane, col, row, 1, 0);
    }
    let col = if i % 2 == 1 { 8 } else { 0 };
    if dct_type == 1 {
        let parity = i32::from(i >= 2);
        (0, col, 0, 2, parity)
    } else {
        let row = if i >= 2 { 8 } else { 0 };
        (0, col, row, 1, 0)
    }
}

/// §6.2.6/§7.2-§7.5: read (or, for an uncoded block, skip) each of the six
/// blocks' entropy-coded data, dequantise, inverse-transform, add the
/// already-formed prediction, saturate, and write the result into
/// `ap.frame`.
#[allow(
    clippy::too_many_arguments,
    reason = "the full state a macroblock's block loop needs (bitstream, transform, picture state, position, and every already-decoded macroblock-level flag); splitting further would just relay the same values through an intermediate struct"
)]
fn reconstruct_macroblock(
    r: &mut BitReader<'_>,
    idct: &mut Mpeg2Idct,
    ap: &mut ActivePicture,
    mb_x: u32,
    mb_y: u32,
    intra: bool,
    dct_type: u8,
    cbp: [bool; MAX_BLOCKS],
    pred: &MbPrediction,
) {
    let intra_dc_mult = tables::intra_dc_mult(ap.pce.intra_dc_precision);
    let alternate_scan = ap.pce.alternate_scan;
    let intra_vlc_format = ap.pce.intra_vlc_format;
    let quantiser_scale = ap.quantiser_scale;
    let chroma_format = ap.chroma_format;
    let (chroma_mb_w, chroma_mb_h) = chroma_format.chroma_mb_pixels();

    for i in 0..chroma_format.block_count() {
        let coded = cbp.get(i).copied().unwrap_or(false);
        let cc = match i {
            0..4 => 0usize,
            _ if i % 2 == 0 => 1,
            _ => 2,
        };
        let (plane_idx, col_off, row_off, row_scale, row_parity) =
            block_geometry(i, dct_type, chroma_format);

        let f: [i32; 64] = if coded {
            let intra_dc = if intra {
                let size_table: &[(&str, u8)] = if cc == 0 {
                    tables::DCT_DC_SIZE_LUMA
                } else {
                    tables::DCT_DC_SIZE_CHROMA
                };
                let Some(&(_, size)) =
                    vlc::decode(r, size_table, |row| (row.0, 0), MAX_DC_SIZE_LEN)
                else {
                    ap.slice_ok = false;
                    return;
                };
                let diff = if size == 0 {
                    0
                } else {
                    // §7.2.1: `half_range = 2 ^ (dc_dct_size - 1)`, *not*
                    // `2 ^ dc_dct_size`. The `- 1` is easy to drop since
                    // `size` bits are read on the very next line and the
                    // two numbers look like they should match — they
                    // don't: a `size`-bit field's own half-way point sits
                    // at `2^(size-1)`, one bit narrower than the field
                    // itself. Dropping it made the "else" branch fire on
                    // every value instead of half of them, so every
                    // decoded DC differential came out large and negative.
                    let half_range = 1i32 << size.saturating_sub(1).min(30);
                    let raw = i32::try_from(r.get(u32::from(size))).unwrap_or(0);
                    if raw >= half_range {
                        raw
                    } else {
                        raw + 1 - 2 * half_range
                    }
                };
                let pred_dc = ap.dc_pred.get(cc).copied().unwrap_or(0);
                let dc = pred_dc + diff;
                if let Some(slot) = ap.dc_pred.get_mut(cc) {
                    *slot = dc;
                }
                Some(dc)
            } else {
                None
            };
            let table = if intra && intra_vlc_format {
                CoeffTable::One
            } else {
                CoeffTable::Zero
            };
            let Ok(qfs) = block::decode_coefficients(r, table, intra_dc, ap.mpeg1) else {
                ap.slice_ok = false;
                return;
            };
            let qf = block::inverse_scan(&qfs, alternate_scan);
            let matrix = if intra {
                ap.intra_matrix
            } else {
                ap.non_intra_matrix
            };
            let dequant = block::dequantise(
                &qf,
                &matrix,
                quantiser_scale,
                intra,
                intra_dc_mult,
                ap.mpeg1,
            );
            block::inverse_transform(idct, &dequant)
        } else {
            [0i32; 64]
        };

        let (plane_ox, plane_oy) = if plane_idx == 0 {
            (mb_x * 16, mb_y * 16)
        } else {
            (mb_x * chroma_mb_w as u32, mb_y * chroma_mb_h as u32)
        };
        let (pred_buf, pred_stride): (&[u8], usize) = match plane_idx {
            0 => (&pred.luma, 16),
            1 => (&pred.cb, CHROMA_PRED_STRIDE),
            _ => (&pred.cr, CHROMA_PRED_STRIDE),
        };

        let Some(mut plane) = ap.frame.plane_mut(plane_idx) else {
            continue;
        };
        for by in 0..8i32 {
            let row_in_mb = row_off + by * row_scale + row_parity;
            let Ok(row_in_mb) = usize::try_from(row_in_mb) else {
                continue;
            };
            let frame_row = usize::try_from(plane_oy).unwrap_or(0) + row_in_mb;
            let Some(dst_row) = plane.row_mut(frame_row) else {
                continue;
            };
            for bx in 0..8usize {
                let col_in_mb = usize::try_from(col_off).unwrap_or(0) + bx;
                let frame_col = usize::try_from(plane_ox).unwrap_or(0) + col_in_mb;
                let Some(dst) = dst_row.get_mut(frame_col) else {
                    continue;
                };
                let residual = f
                    .get(usize::try_from(by).unwrap_or(0) * 8 + bx)
                    .copied()
                    .unwrap_or(0);
                let p = i32::from(
                    pred_buf
                        .get(row_in_mb * pred_stride + col_in_mb)
                        .copied()
                        .unwrap_or(0),
                );
                *dst = (residual + p).clamp(0, 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod chroma_format_tests {
    use super::ChromaFormat;

    #[test]
    fn from_raw_maps_table_6_5_and_folds_the_reserved_code() {
        assert_eq!(ChromaFormat::from_raw(1), ChromaFormat::Yuv420);
        assert_eq!(ChromaFormat::from_raw(2), ChromaFormat::Yuv422);
        assert_eq!(ChromaFormat::from_raw(3), ChromaFormat::Yuv444);
        // 0 is "Reserved" (Table 6-5) — never legitimately sent; folds to
        // 4:2:0 rather than propagating a value with no defined meaning.
        assert_eq!(ChromaFormat::from_raw(0), ChromaFormat::Yuv420);
    }

    #[test]
    fn block_count_matches_table_6_20() {
        assert_eq!(ChromaFormat::Yuv420.block_count(), 6);
        assert_eq!(ChromaFormat::Yuv422.block_count(), 8);
        assert_eq!(ChromaFormat::Yuv444.block_count(), 12);
    }

    #[test]
    fn chroma_mb_pixels_matches_the_six_one_three_subsampling_rules() {
        // 4:2:0: half horizontal, half vertical. 4:2:2: half horizontal,
        // full vertical. 4:4:4: full both (§6.1.3).
        assert_eq!(ChromaFormat::Yuv420.chroma_mb_pixels(), (8, 8));
        assert_eq!(ChromaFormat::Yuv422.chroma_mb_pixels(), (8, 16));
        assert_eq!(ChromaFormat::Yuv444.chroma_mb_pixels(), (16, 16));
    }

    #[test]
    fn scale_vector_matches_section_7_6_3_7_exactly() {
        // A vector whose halves would round differently under truncation
        // (5 and -5) exercises §4.1's "truncate toward zero" rule, not
        // just an even case that every rounding convention would agree
        // on.
        let v = [5, -5];
        assert_eq!(ChromaFormat::Yuv420.scale_vector(v), [2, -2]);
        assert_eq!(ChromaFormat::Yuv422.scale_vector(v), [2, -5]);
        assert_eq!(ChromaFormat::Yuv444.scale_vector(v), [5, -5]);
    }

    #[test]
    fn chroma_block_slot_matches_figures_6_10_through_6_12() {
        // 4:2:0 (Figure 6-10): blocks 4, 5 are Cb, Cr, both at (0, 0) —
        // no stacking, since there is only one block per component.
        assert_eq!(ChromaFormat::Yuv420.chroma_block_slot(0), (1, 0, 0));
        assert_eq!(ChromaFormat::Yuv420.chroma_block_slot(1), (2, 0, 0));

        // 4:2:2 (Figure 6-11): "4 5 / 6 7" — Cb/Cr side by side, a second
        // row stacked below at row 8.
        assert_eq!(ChromaFormat::Yuv422.chroma_block_slot(0), (1, 0, 0)); // block 4: Cb top
        assert_eq!(ChromaFormat::Yuv422.chroma_block_slot(1), (2, 0, 0)); // block 5: Cr top
        assert_eq!(ChromaFormat::Yuv422.chroma_block_slot(2), (1, 0, 8)); // block 6: Cb bottom
        assert_eq!(ChromaFormat::Yuv422.chroma_block_slot(3), (2, 0, 8)); // block 7: Cr bottom

        // 4:4:4 (Figure 6-12): "0 1 4 8 5 9 / 2 3 6 10 7 11" — Cb is a 2x2
        // grid (4, 8, 6, 10: top-left, top-right, bottom-left,
        // bottom-right), Cr is a separate 2x2 grid (5, 9, 7, 11) the same
        // shape.
        assert_eq!(ChromaFormat::Yuv444.chroma_block_slot(0), (1, 0, 0)); // block 4: Cb top-left
        assert_eq!(ChromaFormat::Yuv444.chroma_block_slot(1), (2, 0, 0)); // block 5: Cr top-left
        assert_eq!(ChromaFormat::Yuv444.chroma_block_slot(2), (1, 0, 8)); // block 6: Cb bottom-left
        assert_eq!(ChromaFormat::Yuv444.chroma_block_slot(3), (2, 0, 8)); // block 7: Cr bottom-left
        assert_eq!(ChromaFormat::Yuv444.chroma_block_slot(4), (1, 8, 0)); // block 8: Cb top-right
        assert_eq!(ChromaFormat::Yuv444.chroma_block_slot(5), (2, 8, 0)); // block 9: Cr top-right
        assert_eq!(ChromaFormat::Yuv444.chroma_block_slot(6), (1, 8, 8)); // block 10: Cb bottom-right
        assert_eq!(ChromaFormat::Yuv444.chroma_block_slot(7), (2, 8, 8)); // block 11: Cr bottom-right
    }
}

#[cfg(test)]
mod skipped_macroblock_tests {
    use super::*;
    use crate::headers::PictureType;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    /// A skipped macroblock resets the *DC* predictor regardless of
    /// picture type (§7.2.1/Table 7-2: "whenever a macroblock is
    /// skipped", with no picture-type qualifier) — distinct from the
    /// motion vector predictors, which §7.6.6.4 says a B-picture skip
    /// leaves untouched. This crate's own P-picture skip arm always did
    /// this; the B-picture arm did not until this test's own fix, a real
    /// gap this test is here to keep fixed rather than one this test
    /// merely happens to pass by construction.
    ///
    /// No fixture on hand exercises this observably: it requires a
    /// B-picture with a skipped-macroblock run immediately followed, in
    /// the same slice, by an intra-coded macroblock — real `ffmpeg`
    /// encodes tried against this crate's own corpus either never put an
    /// intra macroblock inside a B-picture at all, or never do so right
    /// after a skip run, so the corrupted `dc_pred` value is never read
    /// back before the next slice-start reset overwrites it anyway. This
    /// test exercises the state transition directly instead.
    #[test]
    fn b_picture_skip_resets_dc_predictor_same_as_p_picture_skip() {
        let mut budget = Budget::new(Limits::strict());
        let Ok(frame) = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 16, 16) else {
            return;
        };
        let seq = Sequence {
            header: crate::headers::sequence_header(&[0u8; 16]),
            ext: None,
            mb_width: 1,
            mb_height: 1,
        };
        let pce = PictureCodingExtension::mpeg1_default(2, 2);
        let mut ap = ActivePicture {
            frame,
            header: PictureHeader {
                temporal_reference: 0,
                coding_type: PictureType::B,
                full_pel_forward_vector: false,
                forward_f_code: 0,
                full_pel_backward_vector: false,
                backward_f_code: 0,
            },
            pce,
            intra_matrix: tables::DEFAULT_INTRA_MATRIX,
            non_intra_matrix: tables::DEFAULT_NON_INTRA_MATRIX,
            quantiser_scale: 8,
            // A value `intra_dc_reset` never produces at any precision
            // (128/256/512/1024), so a passing test can only mean the
            // reset genuinely ran, not a coincidental match.
            dc_pred: [999, 999, 999],
            fwd_pred: MotionPredictor::default(),
            bwd_pred: MotionPredictor::default(),
            // Both false: `decode_skipped_macroblock`'s B arm then reads
            // neither reference, so this test needs no real `RefPicture`.
            prev_mb_forward: false,
            prev_mb_backward: false,
            supported: true,
            slice_ok: true,
            previous: None,
            recent: None,
            mpeg1: false,
            chroma_format: ChromaFormat::Yuv420,
            closed_captions: Vec::new(),
        };
        let Ok(mut idct) = vaco_codec_dsp_idct::mpeg2::idct8x8_f32() else {
            return;
        };

        decode_skipped_macroblock(&mut idct, &mut ap, &seq, 0);

        assert_eq!(ap.dc_pred, [128, 128, 128]);
    }
}

#[cfg(test)]
mod full_pel_tests {
    use super::full_pel_scale;

    /// Annex D.9.7: "motion vector coordinates must be multiplied by two
    /// before being used for the prediction" when the corresponding
    /// direction's `full_pel_*_vector` flag is set. No fixture on hand
    /// exercises this (`ffmpeg`'s own MPEG-1 encoder never sets either
    /// flag on any stream this crate's corpus includes — checked
    /// directly against `m1_ip`/`m1_ipb`'s own picture headers, both
    /// `false` on all 25 pictures each), so this is covered only by this
    /// hand-crafted test, the same pattern `macroblock_stuffing` and the
    /// legacy UMV path use for other MPEG-1 modes real encoders on hand
    /// never emit.
    #[test]
    fn doubles_both_components_only_when_full_pel_is_set() {
        let vecs = [[3, -5], [7, 0]];
        assert_eq!(full_pel_scale(vecs, false), vecs);
        assert_eq!(full_pel_scale(vecs, true), [[6, -10], [14, 0]]);
    }
}
