//! ITU-T H.263 (baseline) `Decoder` implementation: picture/GOB/macroblock/
//! block layers, and the byte-aligned start-code scanner H.263's own
//! `PSTUF`/`GSTUF` stuffing rules make possible — contrast [`crate::h261`],
//! whose bitstream carries no such guarantee.
//!
//! `Vaco-Spec-Ref: itu-t-h263` (03/96).
//!
//! # Scope
//!
//! Baseline I/P coding only — see this crate's own top-level docs for the
//! excluded annexes. Two further, disclosed simplifications inside that
//! scope:
//!
//! - **One macroblock row per GOB.** §5.2 permits a GOB to span "one or
//!   more rows"; every conformance stream this crate has been checked
//!   against uses exactly one, which is what [`decode_access_unit`] hard-
//!   codes (`gn` is read directly as the picture's macroblock row index).
//!   A stream that legitimately uses more rows per GOB is not rejected,
//!   but is decoded on the wrong row boundaries.
//! - **GOB-header emptiness is not tracked for motion vector prediction.**
//!   §6.1.1 rule 3's "outside the GOB (at top) if the GOB header of the
//!   current GOB is non-empty" clause is applied as if every GOB header
//!   were non-empty (true of essentially every real encoder's output),
//!   rather than tracking whether each GOB actually transmitted one.
//!
//! `mb_type` 2 (`INTER4V`, four vectors per macroblock — only meaningful
//! under Annex F's Advanced Prediction mode, itself out of scope) stops
//! decoding the rest of the picture rather than guessing at syntax this
//! crate was never told how to read; whatever macroblocks decoded before
//! it are kept.

use vaco_bitstream::BitReader;
use vaco_codec_core::{Caps, Decoder};
use vaco_core::Result;
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use crate::block::{self, H26xIdct};
use crate::motion;
use crate::picture::RefPicture;
use crate::tables;
use crate::vlc;

/// A fixed, non-zero transform length (N=8); shared with [`crate::h261`],
/// which has no `Idct8x8` constructor of its own.
pub(crate) fn new_idct() -> H26xIdct {
    match vaco_codec_dsp_idct::mpeg2::idct8x8_f32() {
        Ok(idct) => idct,
        Err(_) => {
            #[allow(
                clippy::expect_used,
                reason = "genuinely unreachable: a length-8 DCT-III plan cannot fail to build"
            )]
            vaco_codec_dsp_idct::mpeg2::idct8x8_f32().expect("length-8 IDCT construction cannot fail")
        }
    }
}

/// `(width, height)` for PTYPE's 3-bit source-format code (Table 2/H.263).
/// `None` for the forbidden code (`000`) and the two reserved ones
/// (`110`, `111`, which in later annexes introduce `PLUSPTYPE` — out of
/// this crate's baseline scope).
const fn source_format_dims(code: u32) -> Option<(u32, u32)> {
    match code {
        1 => Some((128, 96)),   // sub-QCIF
        2 => Some((176, 144)),  // QCIF
        3 => Some((352, 288)),  // CIF
        4 => Some((704, 576)),  // 4CIF
        5 => Some((1408, 1152)), // 16CIF
        _ => None,
    }
}

/// One in-progress picture.
#[derive(Debug)]
struct ActivePicture {
    frame: Frame,
    mb_width: u32,
    mb_height: u32,
    intra: bool,
    quant: u8,
    cpm: bool,
    /// Whether this picture uses a mode outside this crate's scope
    /// (`UMV`/`SAC`/`AP`/`PB-frames`, or an unrecognised source format).
    /// Set, the picture is filled neutral and no GOB is decoded for it.
    unsupported: bool,
    /// One motion vector per macroblock, row-major, used only for
    /// [`motion::median3`]'s neighbour lookups (§6.1.1) — an intra or
    /// not-coded macroblock's slot is left `[0, 0]`, which is exactly
    /// what that clause's rule 1 requires a neighbour referencing it to
    /// see.
    mv_grid: Vec<[i32; 2]>,
}

/// ITU-T H.263 (baseline) video decoder. See the module docs.
#[derive(Debug)]
pub struct H263Decoder {
    machine: vaco_codec_core::Machine<Frame>,
    budget: Budget,
    idct: H26xIdct,
    reference: Option<RefPicture>,
    current: Option<ActivePicture>,
}

impl H263Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: vaco_codec_core::Machine::new(Caps::DELAY.union(Caps::SUBFRAMES)),
            budget: Budget::new(limits),
            idct: new_idct(),
            reference: None,
            current: None,
        }
    }

    fn finish_picture(&mut self) {
        let Some(ap) = self.current.take() else {
            return;
        };
        self.reference = Some(RefPicture::new(ap.frame.clone()));
        self.machine.emit(ap.frame);
    }

    fn decode_access_unit(&mut self, data: &[u8], pts: vaco_core::Timestamp, duration: vaco_core::Duration) -> Result<()> {
        let mut pos = 0usize;
        while let Some(sc) = find_prefix(data, pos) {
            let mut r = BitReader::new(data);
            r.skip_bytes(sc);
            r.skip(17); // the 17-bit prefix shared by PSC and GBSC.
            let gn = r.get(5);
            if gn == 0 {
                self.finish_picture();
                let _tr = r.get(8);
                let ptype = r.get(13);
                let pquant = r.get(5) as u8;
                let cpm = r.get_bit() == 1;
                if cpm {
                    r.skip(2); // PSBI, not used by this crate.
                }
                let is_pb = ptype & 1 == 1;
                if is_pb {
                    r.skip(5); // TRB(3) + DBQUANT(2), PB-frames out of scope.
                }
                skip_pei_chain(&mut r);

                let source_format = (ptype >> 5) & 0b111;
                let intra = (ptype >> 4) & 1 == 0;
                let umv = (ptype >> 3) & 1 == 1;
                let sac = (ptype >> 2) & 1 == 1;
                let advanced_pred = (ptype >> 1) & 1 == 1;
                let unsupported = umv || sac || advanced_pred || is_pb || source_format_dims(source_format).is_none();
                let (w, h) = source_format_dims(source_format).unwrap_or((176, 144));

                let mut frame = Frame::alloc_video(&mut self.budget, PixFmt::Yuv420p, w, h)?;
                frame.pts = pts;
                frame.duration = duration;
                if intra {
                    frame.flags |= FrameFlags::KEY;
                }
                let mb_width = w.div_ceil(16).max(1);
                let mb_height = h.div_ceil(16).max(1);
                if unsupported {
                    fill_neutral(&mut frame);
                    frame.flags |= FrameFlags::CORRUPT;
                }
                self.current = Some(ActivePicture {
                    frame,
                    mb_width,
                    mb_height,
                    intra,
                    quant: pquant.clamp(1, 31),
                    cpm,
                    unsupported,
                    mv_grid: vec![[0, 0]; (mb_width * mb_height) as usize],
                });
                // §5.2: "For the first GOB in each picture (with number
                // 0), no GOB header shall be transmitted" — its
                // macroblock row (row 0) follows immediately, bit-precise
                // (not byte-realigned: only *start codes* carry that
                // guarantee), so this continues on the very same `r`
                // rather than handing off to the outer byte-level scan.
                if let Some(ap) = self.current.as_mut()
                    && !ap.unsupported
                {
                    decode_gob(&mut r, ap, &mut self.idct, self.reference.as_ref(), 0);
                }
                pos = usize::try_from(r.bit_pos().div_ceil(8)).unwrap_or(data.len()).max(sc + 3);
                continue;
            }

            if (1..=17).contains(&gn) {
                if ap_cpm(self.current.as_ref()) {
                    r.skip(2); // GSBI, not used by this crate.
                }
                r.skip(2); // GFID, not used by this crate.
                let gquant = r.get(5) as u8;
                if let Some(ap) = self.current.as_mut()
                    && !ap.unsupported
                {
                    ap.quant = gquant.clamp(1, 31);
                    decode_gob(&mut r, ap, &mut self.idct, self.reference.as_ref(), gn);
                }
                pos = usize::try_from(r.bit_pos().div_ceil(8)).unwrap_or(data.len()).max(sc + 3);
                continue;
            }

            // gn 18-30 reserved, 31 = EOS: nothing this decoder acts on.
            pos = sc + 3;
        }
        self.finish_picture();
        Ok(())
    }
}

fn ap_cpm(ap: Option<&ActivePicture>) -> bool {
    ap.is_some_and(|ap| ap.cpm)
}

/// §5.1.9/.10 and §5.2.1's `PEI`/`PSPARE`: the identical recursive shape
/// H.261's own `PEI`/`PSPARE` uses (see `h261::skip_pei_chain`'s docs).
fn skip_pei_chain(r: &mut BitReader<'_>) {
    while r.get_bit() == 1 {
        r.skip(8);
        if r.check().is_err() {
            break;
        }
    }
}

/// Fill every plane mid-grey: the placeholder for a picture using a mode
/// this crate does not implement (see the module docs' "Scope" section),
/// matching `vaco-codec-mpeg12`'s identical convention for its own
/// unsupported pictures.
fn fill_neutral(frame: &mut Frame) {
    for plane_idx in 0..3 {
        if let Some(mut plane) = frame.plane_mut(plane_idx) {
            for row in plane.rows_mut() {
                row.fill(128);
            }
        }
    }
}

/// Byte-level scan for `00 00 1xxxxxxx`: `PSC`/`GBSC`'s shared 17-bit
/// pattern (`0000 0000 0000 0000 1`), guaranteed byte-aligned by
/// `PSTUF`/`GSTUF` (§5.1.13/§5.2.1) — unlike H.261, so a byte-oriented
/// scan is correct here (contrast `h261::find_prefix`'s bit-level one).
fn find_prefix(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while let Some(&[a, b, c]) = data.get(i..).and_then(|s| s.first_chunk::<3>()) {
        if a == 0 && b == 0 && c & 0x80 != 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The six blocks' geometry within a macroblock (§5.4/Figure 5, 4:2:0
/// only): `(plane, col_offset, row_offset)`.
const fn block_geometry(i: usize) -> (usize, u32, u32) {
    match i {
        0 => (0, 0, 0),
        1 => (0, 8, 0),
        2 => (0, 0, 8),
        3 => (0, 8, 8),
        4 => (1, 0, 0),
        _ => (2, 0, 0),
    }
}

/// §6.1.1: the three candidate predictor macroblocks' own stored vectors
/// for `(mb_x, mb_y)`, with the border substitution rules already applied
/// (see the module docs for the one simplification: "outside the GOB" is
/// never distinguished from "inside the GOB, just a different row").
fn predictors(mv_grid: &[[i32; 2]], mb_width: u32, mb_height: u32, mb_x: u32, mb_y: u32) -> ([i32; 2], [i32; 2], [i32; 2]) {
    let get = |x: i32, y: i32| -> Option<[i32; 2]> {
        if x < 0 || y < 0 || x >= i32::try_from(mb_width).unwrap_or(0) || y >= i32::try_from(mb_height).unwrap_or(0) {
            return None;
        }
        let idx = usize::try_from(y).unwrap_or(0) * mb_width as usize + usize::try_from(x).unwrap_or(0);
        mv_grid.get(idx).copied()
    };
    let x = i32::try_from(mb_x).unwrap_or(0);
    let y = i32::try_from(mb_y).unwrap_or(0);

    // Rule 2: MV1 (left) is zero if that macroblock is outside the picture.
    let mv1 = get(x - 1, y).unwrap_or([0, 0]);
    // Rule 3: MV2 (above) and MV3 (above-right) fall back to MV1 if their
    // macroblock is outside the picture (top row).
    let mv2 = get(x, y - 1).unwrap_or(mv1);
    let mut mv3 = get(x + 1, y - 1).unwrap_or(mv1);
    // Rule 4 (applied last, so it overrides rule 3's fallback too): MV3 is
    // zero if that macroblock is outside the picture on the right.
    if x + 1 >= i32::try_from(mb_width).unwrap_or(0) {
        mv3 = [0, 0];
    }
    (mv1, mv2, mv3)
}

/// Whether `r` sits exactly on a byte-aligned `00 00 1xxxxxxx` — the
/// `PSC`/`GBSC` prefix both start codes share (see [`find_prefix`]) — so
/// the macroblock loop below can recognise one without needing the raw
/// byte slice itself.
fn at_start_code(r: &BitReader<'_>) -> bool {
    if !r.bit_pos().is_multiple_of(8) {
        return false;
    }
    matches!(r.remaining_bytes().first_chunk::<3>(), Some(&[0, 0, c]) if c & 0x80 != 0)
}

/// A linear macroblock index's `(row, col)` in an `mb_width`-wide raster —
/// exact integer grid coordinates from a linear index, not an
/// approximated average (mirrors `h261::addr_to_col_row`'s own rationale).
#[allow(
    clippy::integer_division,
    reason = "recovering (row, col) raster coordinates from a linear macroblock index is exact integer arithmetic, not a lossy average"
)]
const fn mb_row_col(mb_index: u32, mb_width: u32) -> (u32, u32) {
    (mb_index / mb_width, mb_index % mb_width)
}

/// Decode macroblocks in raster order starting at `(start_row, 0)`,
/// continuing across row boundaries until either a genuine start code is
/// found or the picture's last macroblock is reached.
///
/// This crosses rows within one call because §5.2 allows a GOB header to
/// be entirely absent ("the GOB header may be empty, depending on the
/// encoder strategy") for every row but the first: ffmpeg's own default
/// `h263` encoder was observed doing exactly that for a whole QCIF
/// picture (every row after 0 has no header at all), so a design that
/// decoded one row and then went back to the outer byte-level scan for
/// the next GOB header found nothing there and silently stopped after
/// row 0, leaving every row below it undecoded. Checking [`at_start_code`]
/// at the top of every macroblock, rather than only between calls, is
/// what lets this same function also stop *early* and hand back to the
/// outer scan when a GOB header genuinely is present partway through.
///
/// Returns `false` on a coefficient-decode failure or unsupported
/// `mb_type`, at which point the caller stops the whole picture rather
/// than continuing on an untrustworthy bit position.
fn decode_gob(r: &mut BitReader<'_>, ap: &mut ActivePicture, idct: &mut H26xIdct, reference: Option<&RefPicture>, start_row: u32) -> bool {
    let total_mbs = ap.mb_width * ap.mb_height;
    let mut mb_index = start_row * ap.mb_width;
    while mb_index < total_mbs {
        if r.check().is_err() {
            return false;
        }
        if at_start_code(r) {
            return true;
        }
        let (row, col) = mb_row_col(mb_index, ap.mb_width);
        let cod = if ap.intra {
            false
        } else {
            r.get_bit() == 1
        };
        if cod {
            // §5.3.1: fully skipped — INTER, zero vector, no coefficients.
            let idx = mb_index as usize;
            if let Some(slot) = ap.mv_grid.get_mut(idx) {
                *slot = [0, 0];
            }
            reconstruct_macroblock(r, idct, ap, reference, col, row, false, [0, 0], 0, false);
            mb_index += 1;
            continue;
        }

        let mcbpc_table: &[(&str, u8, u8)] = if ap.intra {
            tables::H263_MCBPC_INTRA
        } else {
            tables::H263_MCBPC_INTER
        };
        let Some(&(_, mb_type, cbpc)) = vlc::decode(r, mcbpc_table, |c| c.0, 9) else {
            return false;
        };
        let is_stuffing = if ap.intra { mb_type == 8 } else { mb_type == 20 };
        if is_stuffing {
            if r.check().is_err() {
                return false;
            }
            continue; // §5.3.2: not a real macroblock; retry this column.
        }
        if mb_type == 2 {
            return false; // INTER4V: out of scope (see module docs).
        }

        let intra_mb = matches!(mb_type, 3 | 4);
        let has_dquant = matches!(mb_type, 1 | 4);
        let has_mv = matches!(mb_type, 0 | 1);

        if has_dquant {
            let d = r.get(2) as usize;
            let delta = i32::from(tables::H263_DQUANT.get(d).copied().unwrap_or(0));
            ap.quant = i32::from(ap.quant).saturating_add(delta).clamp(1, 31) as u8;
        }

        let cbpy_table: &[(&str, u8)] = if intra_mb {
            tables::H263_CBPY_INTRA
        } else {
            tables::H263_CBPY_INTER
        };
        let Some(&(_, cbpy)) = vlc::decode(r, cbpy_table, |c| c.0, 6) else {
            return false;
        };
        let cbp = (cbpy << 2) | cbpc;

        let mv = if has_mv {
            let (mv1, mv2, mv3) = predictors(&ap.mv_grid, ap.mb_width, ap.mb_height, col, row);
            let pred_x = motion::median3(mv1[0], mv2[0], mv3[0]);
            let pred_y = motion::median3(mv1[1], mv2[1], mv3[1]);
            let Some(&(_, dh)) = vlc::decode(r, tables::H263_MVD, |c| c.0, 13) else {
                return false;
            };
            let Some(&(_, dv)) = vlc::decode(r, tables::H263_MVD, |c| c.0, 13) else {
                return false;
            };
            [
                motion::h263_vector(pred_x, i32::from(dh)),
                motion::h263_vector(pred_y, i32::from(dv)),
            ]
        } else {
            [0, 0]
        };

        let idx = mb_index as usize;
        if let Some(slot) = ap.mv_grid.get_mut(idx) {
            *slot = if intra_mb { [0, 0] } else { mv };
        }

        if !reconstruct_macroblock(r, idct, ap, reference, col, row, intra_mb, mv, cbp, true) {
            return false;
        }
        mb_index += 1;
    }
    true
}

/// Decode (or, for an uncoded block, skip) each of the six blocks, form
/// each one's own motion-compensated prediction, add the decoded
/// residual, saturate, and write the result. `coded_mb` is `false` only
/// for a `COD=1` fully-skipped macroblock, in which case no block carries
/// any coefficient data regardless of `cbp`.
#[allow(
    clippy::too_many_arguments,
    reason = "the full state a macroblock's block loop needs (bitstream, transform, picture state, reference, position, and every already-decoded macroblock-level flag); splitting further would just relay the same values through an intermediate struct"
)]
fn reconstruct_macroblock(
    r: &mut BitReader<'_>,
    idct: &mut H26xIdct,
    ap: &mut ActivePicture,
    reference: Option<&RefPicture>,
    mb_x: u32,
    mb_y: u32,
    intra: bool,
    mv: [i32; 2],
    cbp: u8,
    coded_mb: bool,
) -> bool {
    for i in 0..6usize {
        let (plane, col_off, row_off) = block_geometry(i);
        let (bw, bh): (u32, u32) = (8, 8);
        let (mv_x, mv_y) = if plane == 0 {
            (mv[0], mv[1])
        } else {
            (motion::h263_chroma_mv(mv[0]), motion::h263_chroma_mv(mv[1]))
        };
        let (px_ox, px_oy) = if plane == 0 {
            (mb_x * 16 + col_off, mb_y * 16 + row_off)
        } else {
            (mb_x * 8 + col_off, mb_y * 8 + row_off)
        };

        let mut pred = [0u8; 64];
        if !intra {
            match reference {
                Some(refp) => {
                    for y in 0..bh {
                        for x in 0..bw {
                            let sx = i32::try_from(px_ox + x).unwrap_or(0);
                            let sy = i32::try_from(px_oy + y).unwrap_or(0);
                            if let Some(slot) = pred.get_mut((y * bw + x) as usize) {
                                *slot = motion::sample_half_pel(refp, plane, sx, sy, mv_x, mv_y);
                            }
                        }
                    }
                }
                // Never valid in a conforming stream (a P-type macroblock
                // before any picture has been decoded), but bounded fuzz
                // input can still reach here — a flat mid-grey prediction
                // keeps this branch as harmless as every other no-
                // reference fallback in this crate.
                None => pred = [128u8; 64],
            }
        }

        // §5.4: `INTRADC` is unconditional for an intra block, but `TCOEF`
        // (the AC part) is gated by the *same* `CBP` bit an inter block's
        // whole residual is gated by — "TCOEF is present if indicated by
        // MCBPC or CBPY". An intra block with its `CBPY`/`CBPC` bit clear
        // still reads its 8-bit `INTRADC` and nothing else.
        let cbp_bit = (cbp >> (5 - i)) & 1 == 1;
        let residual: [i32; 64] = if intra {
            let Ok(qfs) = block::decode_h263_coefficients(r, true, cbp_bit) else {
                return false;
            };
            let qf = block::inverse_scan(&qfs);
            let dequant = block::dequantise(&qf, ap.quant, true);
            block::inverse_transform(idct, &dequant)
        } else if coded_mb && cbp_bit {
            let Ok(qfs) = block::decode_h263_coefficients(r, false, true) else {
                return false;
            };
            let qf = block::inverse_scan(&qfs);
            let dequant = block::dequantise(&qf, ap.quant, false);
            block::inverse_transform(idct, &dequant)
        } else {
            [0i32; 64]
        };

        let Some(mut plane_buf) = ap.frame.plane_mut(plane) else {
            continue;
        };
        for y in 0..bh {
            let Some(dst_row) = plane_buf.row_mut(usize::try_from(px_oy + y).unwrap_or(0)) else {
                continue;
            };
            for x in 0..bw {
                let Some(dst) = dst_row.get_mut(usize::try_from(px_ox + x).unwrap_or(0)) else {
                    continue;
                };
                let r_val = residual.get((y * bw + x) as usize).copied().unwrap_or(0);
                let p_val = i32::from(pred.get((y * bw + x) as usize).copied().unwrap_or(0));
                *dst = (r_val + p_val).clamp(0, 255) as u8;
            }
        }
    }
    true
}

impl Decoder for H263Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let accept = self.machine.accept(packet.is_none())?;
        if matches!(accept, vaco_codec_core::Accept::Drain) {
            self.machine.finish();
            return Ok(());
        }
        let Some(packet) = packet else {
            return Ok(());
        };
        self.decode_access_unit(packet.payload(), packet.pts, packet.duration)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.reference = None;
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vaco_core::Error;

    #[test]
    fn decoder_reports_need_more_input_before_any_packet() {
        let mut dec = H263Decoder::new(Limits::strict());
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn source_format_dims_covers_the_five_standard_formats() {
        assert_eq!(source_format_dims(1), Some((128, 96)));
        assert_eq!(source_format_dims(2), Some((176, 144)));
        assert_eq!(source_format_dims(3), Some((352, 288)));
        assert_eq!(source_format_dims(0), None);
        assert_eq!(source_format_dims(6), None);
    }

    #[test]
    fn find_prefix_locates_a_byte_aligned_start_code() {
        let bytes: [u8; 4] = [0x00, 0x00, 0x80, 0x00];
        assert_eq!(find_prefix(&bytes, 0), Some(0));
    }

    #[test]
    fn predictors_use_zero_for_a_top_left_macroblock() {
        let grid = vec![[0, 0]; 9];
        let (mv1, mv2, mv3) = predictors(&grid, 3, 3, 0, 0);
        assert_eq!(mv1, [0, 0]);
        assert_eq!(mv2, [0, 0]);
        assert_eq!(mv3, [0, 0]);
    }

    proptest::proptest! {
        #[test]
        fn decode_never_panics_on_arbitrary_bytes(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
            let mut budget = Budget::new(Limits::strict());
            let Ok(packet) = vaco_packet::Packet::from_slice(&mut budget, &data) else {
                return Ok(());
            };
            let mut dec = H263Decoder::new(Limits::strict());
            if dec.send_packet(Some(&packet)).is_ok() {
                while let Ok(frame) = dec.receive_frame() {
                    for idx in 0..3 {
                        if let Some(plane) = frame.plane(idx) {
                            let _ = plane.row(0);
                        }
                    }
                }
            }
        }
    }
}
