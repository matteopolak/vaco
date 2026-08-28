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

/// One decoded 16x16 luma + 8x8 Cb + 8x8 Cr prediction, before residual
/// addition. All-zero for an intra macroblock (§7.6: "no prediction is
/// formed").
struct MbPrediction {
    luma: [u8; 256],
    cb: [u8; 64],
    cr: [u8; 64],
}

impl MbPrediction {
    const fn zero() -> Self {
        Self {
            luma: [0; 256],
            cb: [0; 64],
            cr: [0; 64],
        }
    }
}

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
    let Some(mbt) = vlc::decode(r, mb_type_table.iter(), |row| (row.bits, 0), MAX_MB_TYPE_LEN)
    else {
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
    if ap.pce.is_frame_picture()
        && !ap.pce.frame_pred_frame_dct
        && (mbt.intra || mbt.pattern)
    {
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
            *vec_slot = motion::decode_vector(r, &mut ap.fwd_pred, i, ap.pce.f_code[0], field_and_frame_picture);
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
            *vec_slot = motion::decode_vector(r, &mut ap.bwd_pred, i, ap.pce.f_code[1], field_and_frame_picture);
        }
    }
    if mbt.intra && ap.pce.concealment_motion_vectors {
        let _marker_bit = r.get(1);
    }

    let mut cbp = [false; 6];
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
        for (i, slot) in cbp.iter_mut().enumerate() {
            *slot = code & (1 << (5 - i)) != 0;
        }
    } else if mbt.intra {
        cbp = [true; 6];
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
fn decode_skipped_macroblock(idct: &mut Mpeg2Idct, ap: &mut ActivePicture, seq: &Sequence, mb_addr: u32) {
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
            );
        }
        PictureType::B => {
            // §7.6.6.4: same direction as the previous macroblock,
            // predictors and their stored vectors unaffected — read
            // straight from `ap.fwd_pred`/`ap.bwd_pred` as they stand.
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
            );
        }
        PictureType::I | PictureType::D => {
            // Not permitted by the standard outside scalability this crate
            // does not implement; degrade to zero prediction rather than
            // reading further bits that are not there.
        }
    }

    write_prediction_only(ap, mb_x, mb_y, &pred);
    let _ = idct; // no residual for a skipped macroblock; kept for a uniform call shape.
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
) {
    let px = i32::try_from(mb_x).unwrap_or(0) * 16;
    let py = i32::try_from(mb_y).unwrap_or(0) * 16;
    let cx = i32::try_from(mb_x).unwrap_or(0) * 8;
    let cy = i32::try_from(mb_y).unwrap_or(0) * 8;

    if !field_based {
        form_component(
            fwd_ref, bwd_ref, fwd_vecs[0], bwd_vecs[0], 0, 0, 1, 0, 0, px, py, 16, 16,
            &mut pred.luma, 16,
        );
        let fwd_c = [fwd_vecs[0][0] / 2, fwd_vecs[0][1] / 2];
        let bwd_c = [bwd_vecs[0][0] / 2, bwd_vecs[0][1] / 2];
        form_component(fwd_ref, bwd_ref, fwd_c, bwd_c, 1, 1, 1, 0, 0, cx, cy, 8, 8, &mut pred.cb, 8);
        form_component(
            fwd_ref, bwd_ref, fwd_c, bwd_c, 2, 2, 1, 0, 0, cx, cy, 8, 8, &mut pred.cr, 8,
        );
        return;
    }

    // Field-based within a frame picture: each of the (up to two) `r`
    // indices owns half the macroblock's rows, alternating — see
    // `crate::motion::form_prediction`'s docs for the row-scale trick this
    // reuses on both the reference (already handled there) and the
    // destination buffer (handled here by writing every other row).
    for r_idx in 0..count {
        let fv = fwd_vecs.get(r_idx).copied().unwrap_or([0, 0]);
        let bv = bwd_vecs.get(r_idx).copied().unwrap_or([0, 0]);
        let fwd_parity = fwd_field_select.get(r_idx).copied().unwrap_or(0);
        let bwd_parity = bwd_field_select.get(r_idx).copied().unwrap_or(0);
        let parity = i32::try_from(r_idx).unwrap_or(0);

        let mut luma_field = [0u8; 128]; // 16 wide x 8 tall
        form_component(
            fwd_ref, bwd_ref, fv, bv, 0, 0, 2, fwd_parity, bwd_parity, px, py / 2, 16, 8,
            &mut luma_field, 16,
        );
        deinterleave_rows(&luma_field, 16, 8, &mut pred.luma, 16, parity);

        let fv_c = [fv[0] / 2, fv[1] / 2];
        let bv_c = [bv[0] / 2, bv[1] / 2];
        let mut cb_field = [0u8; 32]; // 8 wide x 4 tall
        form_component(
            fwd_ref, bwd_ref, fv_c, bv_c, 1, 1, 2, fwd_parity, bwd_parity, cx, cy / 2, 8, 4,
            &mut cb_field, 8,
        );
        deinterleave_rows(&cb_field, 8, 4, &mut pred.cb, 8, parity);
        let mut cr_field = [0u8; 32];
        form_component(
            fwd_ref, bwd_ref, fv_c, bv_c, 2, 2, 2, fwd_parity, bwd_parity, cx, cy / 2, 8, 4,
            &mut cr_field, 8,
        );
        deinterleave_rows(&cr_field, 8, 4, &mut pred.cr, 8, parity);
    }
}

/// Copy `src` (`w` wide, `h` tall) into every `parity`-th row of `dst`
/// (`dst_stride` wide, `2 * h` logical rows), i.e. the inverse of reading a
/// reference with `row_scale = 2`.
fn deinterleave_rows(src: &[u8], w: usize, h: usize, dst: &mut [u8], dst_stride: usize, parity: i32) {
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

fn write_prediction_only(ap: &mut ActivePicture, mb_x: u32, mb_y: u32, pred: &MbPrediction) {
    write_plane(&mut ap.frame, 0, mb_x * 16, mb_y * 16, &pred.luma, 16, 16, 16);
    write_plane(&mut ap.frame, 1, mb_x * 8, mb_y * 8, &pred.cb, 8, 8, 8);
    write_plane(&mut ap.frame, 2, mb_x * 8, mb_y * 8, &pred.cr, 8, 8, 8);
}

fn write_plane(frame: &mut Frame, plane_idx: usize, ox: u32, oy: u32, src: &[u8], w: usize, h: usize, src_stride: usize) {
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

/// The six blocks' geometry within a macroblock (§6.3.1 Figure 6-8, 4:2:0
/// only — this crate's whole scope): `(plane, col_offset, row_offset,
/// row_scale, row_parity)`, where the last two encode `dct_type`'s
/// field/frame reorganisation of the *luma* blocks (chroma always stays
/// contiguous, a documented simplification for the 4:2:0-only scope this
/// crate targets).
fn block_geometry(i: usize, dct_type: u8) -> (usize, i32, i32, i32, i32) {
    if i >= 4 {
        let plane = if i == 4 { 1 } else { 2 };
        return (plane, 0, 0, 1, 0);
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
    cbp: [bool; 6],
    pred: &MbPrediction,
) {
    let intra_dc_mult = tables::intra_dc_mult(ap.pce.intra_dc_precision);
    let alternate_scan = ap.pce.alternate_scan;
    let intra_vlc_format = ap.pce.intra_vlc_format;
    let quantiser_scale = ap.quantiser_scale;

    for i in 0..6usize {
        let coded = cbp.get(i).copied().unwrap_or(false);
        let cc = match i {
            0..4 => 0usize,
            4 => 1,
            _ => 2,
        };
        let (plane_idx, col_off, row_off, row_scale, row_parity) = block_geometry(i, dct_type);

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
            let dequant = block::dequantise(&qf, &matrix, quantiser_scale, intra, intra_dc_mult);
            block::inverse_transform(idct, &dequant)
        } else {
            [0i32; 64]
        };

        let (plane_ox, plane_oy) = if plane_idx == 0 {
            (mb_x * 16, mb_y * 16)
        } else {
            (mb_x * 8, mb_y * 8)
        };
        let (pred_buf, pred_stride): (&[u8], usize) = match plane_idx {
            0 => (&pred.luma, 16),
            1 => (&pred.cb, 8),
            _ => (&pred.cr, 8),
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
                let residual = f.get(usize::try_from(by).unwrap_or(0) * 8 + bx).copied().unwrap_or(0);
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
