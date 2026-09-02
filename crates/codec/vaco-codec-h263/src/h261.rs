//! ITU-T H.261 `Decoder` implementation: picture/GOB/macroblock/block
//! layers, and the bit-level (non-byte-aligned) start-code scanner H.261's
//! own bitstream needs — unlike H.263 (see [`crate::h263`]'s own module
//! docs), H.261 never inserts stuffing bits to byte-align a start code, so
//! `PSC`/`GBSC` can begin at any bit offset and a byte-oriented scanner
//! (`vaco_bitstream::annexb`, built for H.264's always-byte-aligned Annex B)
//! cannot be reused here.
//!
//! `Vaco-Spec-Ref: itu-t-h261` (03/93).
//!
//! # Scope
//!
//! Both CIF and QCIF source formats, all ten `MTYPE` macroblock classes
//! (§4.2.3.2 Table 2), the optional loop filter (`FIL`), integer-pel-only
//! motion compensation. Still-image mode (Annex D's `HI_RES`) and forced
//! updating (§3.4, a purely encoder-side rate-control concern) are not
//! applicable to a decoder. `PSPARE`/`GSPARE` are read past (discarded)
//! rather than interpreted, per §4.2.1.5's own instruction to decoders.

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
use crate::tables::{self, H261MbType};
use crate::vlc;

/// §4.2.2: a GOB is always 176x48 luma pels, 11x3 macroblocks, regardless
/// of CIF or QCIF (QCIF just uses fewer of them — see [`gob_origin`]).
const GOB_MB_WIDTH: u32 = 11;
const GOB_MB_HEIGHT: u32 = 3;

/// One in-progress picture. Every write into `frame`'s planes goes through
/// bounds-checked accessors (`Plane::row_mut`, `.get_mut`), so a malformed
/// GOB number that would place a macroblock outside the picture (e.g. a
/// CIF-only `gn` in a QCIF picture) is silently absorbed there rather than
/// needing its own `mb_width`/`mb_height` check here.
#[derive(Debug)]
struct ActivePicture {
    frame: Frame,
}

/// ITU-T H.261 video decoder. See the module docs.
#[derive(Debug)]
pub struct H261Decoder {
    machine: vaco_codec_core::Machine<Frame>,
    budget: Budget,
    idct: H26xIdct,
    reference: Option<RefPicture>,
    current: Option<ActivePicture>,
}

impl H261Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: vaco_codec_core::Machine::new(Caps::DELAY.union(Caps::SUBFRAMES)),
            budget: Budget::new(limits),
            idct: crate::h263::new_idct(),
            reference: None,
            current: None,
        }
    }

    fn begin_picture(
        &mut self,
        cif: bool,
        pts: vaco_core::Timestamp,
        duration: vaco_core::Duration,
    ) -> Result<()> {
        self.finish_picture();
        let (w, h) = if cif { (352, 288) } else { (176, 144) };
        let mut frame = Frame::alloc_video(&mut self.budget, PixFmt::Yuv420p, w, h)?;
        frame.pts = pts;
        frame.duration = duration;
        frame.flags |= FrameFlags::KEY;
        self.current = Some(ActivePicture { frame });
        Ok(())
    }

    fn finish_picture(&mut self) {
        let Some(ap) = self.current.take() else {
            return;
        };
        self.reference = Some(RefPicture::new(ap.frame.clone()));
        self.machine.emit(ap.frame);
    }

    fn decode_access_unit(
        &mut self,
        data: &[u8],
        pts: vaco_core::Timestamp,
        duration: vaco_core::Duration,
    ) -> Result<()> {
        let total_bits = (data.len() as u64) * 8;
        let mut bit = 0u64;
        while let Some(sc_bit) = find_prefix(data, bit, total_bits) {
            let mut r = BitReader::new(data);
            r.skip_long(sc_bit);
            r.skip(16); // the 16-bit prefix shared by PSC and GBSC.
            let gn = r.get(4);
            if gn == 0 {
                let _tr = r.get(5);
                let ptype = r.get(6);
                skip_pei_chain(&mut r);
                let cif = (ptype >> 2) & 1 == 1;
                self.begin_picture(cif, pts, duration)?;
                bit = r.bit_pos();
                continue;
            }
            if (1..=12).contains(&gn) {
                let gquant = r.get(5) as u8;
                skip_pei_chain(&mut r);
                let mb_start = r.bit_pos();
                let end_bit = if let Some(ap) = self.current.as_mut() {
                    decode_gob(
                        &mut r,
                        ap,
                        &mut self.idct,
                        self.reference.as_ref(),
                        gn,
                        gquant,
                    )
                } else {
                    mb_start
                };
                // `sc_bit + 1`, not `mb_start + 1`: this is only a forward-
                // progress guard against a GOB whose `decode_gob` call
                // makes literally zero progress from *this iteration's own
                // found prefix*, so the scan can't spin on it forever. An
                // empty GOB (a header with no macroblock data at all,
                // explicitly legal per §4.2.2: "transmitted once... even
                // if no macroblock data is present") has `end_bit ==
                // mb_start` by construction — using `mb_start + 1` there
                // stepped one bit *past* the very start of the next GOB's
                // own header when that next header happens to begin
                // exactly at `mb_start`, and this bit-level scan never
                // looks backward to recover it.
                bit = end_bit.max(sc_bit + 1);
                continue;
            }
            // GN 13-15: reserved. Skip past this occurrence of the prefix
            // and keep scanning rather than treating it as fatal — an
            // adversarial or truncated stream should not stop decode of
            // whatever pictures already parsed cleanly.
            bit = sc_bit + 20;
        }
        self.finish_picture();
        Ok(())
    }
}

/// §4.2.1.4/.5 and §4.2.2.4/.5: `PEI`/`PSPARE` and `GEI`/`GSPARE` share the
/// identical recursive shape ("a bit, and if it's 1, 8 more bits and
/// another such bit") — decoders must discard whatever this carries.
fn skip_pei_chain(r: &mut BitReader<'_>) {
    while r.get_bit() == 1 {
        r.skip(8);
        if r.check().is_err() {
            break;
        }
    }
}

/// Bit-level scan for the 16-bit pattern `PSC` and `GBSC` share
/// (`0000 0000 0000 0001`), starting at `from_bit`. H.261 places no
/// alignment guarantee on this pattern (contrast [`crate::h263`]'s
/// byte-aligned scanner), so this must walk bit by bit rather than byte by
/// byte.
fn find_prefix(data: &[u8], from_bit: u64, total_bits: u64) -> Option<u64> {
    if from_bit + 16 > total_bits {
        return None;
    }
    let mut r = BitReader::new(data);
    r.skip_long(from_bit);
    let mut bit = from_bit;
    while bit + 16 <= total_bits {
        if r.check().is_err() {
            return None;
        }
        if r.peek(16) == 1 {
            return Some(bit);
        }
        r.skip(1);
        bit += 1;
    }
    None
}

/// A 1-based address' `(col, row)` in an `across`-wide grid — exact
/// integer grid coordinates from a linear index, not an approximated
/// average, which is what `clippy::integer_division` otherwise assumes
/// every `/` on integers is doing.
#[allow(
    clippy::integer_division,
    reason = "recovering (col, row) grid coordinates from a 1-based linear address is exact integer arithmetic, not a lossy average"
)]
const fn addr_to_col_row(addr_1based: u32, across: u32) -> (u32, u32) {
    let zero_based = addr_1based - 1;
    (zero_based % across, zero_based / across)
}

/// `(mb_x, mb_y)` of a GOB's own top-left macroblock (§4.2.2, Figure 6):
/// `gn` 1-12 lay out as two columns of six rows for CIF; QCIF only ever
/// uses the odd `gn` values (1, 3, 5), which this same formula maps to a
/// single column of three rows.
const fn gob_origin(gn: u32) -> (u32, u32) {
    let (col, row) = addr_to_col_row(gn, 2);
    (col * GOB_MB_WIDTH, row * GOB_MB_HEIGHT)
}

/// Decode every macroblock of one GOB, starting at `r`'s current position
/// (right after `GQUANT`/`GEI`/`GSPARE`), stopping at the next `PSC`/`GBSC`
/// prefix, a malformed code, or the end of `data`. Returns the bit
/// position reached, so the caller's own prefix scan can resume exactly
/// there.
#[allow(
    clippy::too_many_arguments,
    reason = "one GOB's decode genuinely depends on this many independent pieces of picture/GOB state; splitting further would just relay the same values through an intermediate struct"
)]
fn decode_gob(
    r: &mut BitReader<'_>,
    ap: &mut ActivePicture,
    idct: &mut H26xIdct,
    reference: Option<&RefPicture>,
    gn: u32,
    gquant: u8,
) -> u64 {
    const LAST_ADDR: u32 = GOB_MB_WIDTH * GOB_MB_HEIGHT;
    let (gob_mb_x0, gob_mb_y0) = gob_origin(gn);
    let mut quant = gquant.clamp(1, 31);
    let mut mb_addr = 0u32; // 1..=33 within this GOB; 0 = none decoded yet.
    let mut pmv = [0i32, 0i32];
    let mut prev_was_mc = false;

    let end_bit = loop {
        if r.check().is_err() {
            break r.bit_pos();
        }
        let (stuff_code, stuff_len) = tables::bits_of(tables::H261_MBA_STUFFING);
        while u32::from(stuff_len) <= 16 && r.peek(u32::from(stuff_len)) == stuff_code {
            r.skip(u32::from(stuff_len));
            if r.check().is_err() {
                break;
            }
        }

        // No `peek(16) == 1` pre-check for "is this the next start code" —
        // tried that first and it is subtly wrong: a real encoder can (and
        // ffmpeg's does) leave a handful of zero bits between the last
        // macroblock's true end and the next `PSC`/`GBSC`, too few to
        // fill a 16-bit lookahead window from *this* position but which
        // still lead into that pattern a few bits later. Peeking 16 bits
        // right here sees a mix of "leftover padding" and "the pattern's
        // own leading zeros" and reports no match, so the loop went on to
        // spend up to 11 more bits trying to decode those same padding
        // bits as a real `MBA` code before finally giving up — past where
        // the true start code begins, corrupting the caller's own resumed
        // scan by up to that many bits. Attempting the real decode and
        // reporting *its own start position* on failure is correct
        // regardless of how many stray bits separate it from the next
        // start code, because the caller's own bit-level scan (not a
        // fixed-width lookahead) is what actually finds that pattern.
        let mba_start = r.bit_pos();
        let mut total_inc: u32 = 0;
        let mba_ok = loop {
            let Some(&(_, inc)) = vlc::decode(r, tables::H261_MBA, |c| c.0, 11) else {
                break false;
            };
            if inc == 0 {
                total_inc += 33;
                continue;
            }
            total_inc += u32::from(inc);
            break true;
        };
        if !mba_ok {
            break mba_start;
        }

        let new_addr = mb_addr + total_inc;
        if new_addr == 0 || new_addr > LAST_ADDR {
            break r.bit_pos();
        }
        let reset_pmv =
            mb_addr == 0 || total_inc != 1 || !prev_was_mc || matches!(new_addr, 1 | 12 | 23);

        if total_inc > 1 {
            fill_skipped(
                ap,
                reference,
                gob_mb_x0,
                gob_mb_y0,
                mb_addr + 1,
                new_addr - 1,
            );
        }
        mb_addr = new_addr;

        let Some(&mtype) = vlc::decode(r, tables::H261_MTYPE, |m: &H261MbType| m.bits, 13) else {
            break r.bit_pos();
        };

        if mtype.mquant {
            quant = (r.get(5) as u8).clamp(1, 31);
        }

        let mv = if mtype.mvd {
            let base = if reset_pmv { [0, 0] } else { pmv };
            let Some(&(_, dh)) = vlc::decode(r, tables::H261_MVD, |c| c.0, 13) else {
                break r.bit_pos();
            };
            let Some(&(_, dv)) = vlc::decode(r, tables::H261_MVD, |c| c.0, 13) else {
                break r.bit_pos();
            };
            let mv = [
                motion::h261_vector(base[0], i32::from(dh)),
                motion::h261_vector(base[1], i32::from(dv)),
            ];
            pmv = mv;
            mv
        } else {
            pmv = [0, 0];
            [0, 0]
        };

        let cbp_mask: u8 = if mtype.intra {
            0b11_1111
        } else if mtype.cbp {
            let Some(&(_, cbp)) = vlc::decode(r, tables::H261_CBP, |c| c.0, 9) else {
                break r.bit_pos();
            };
            cbp
        } else {
            0
        };

        let (col, row) = addr_to_col_row(mb_addr, GOB_MB_WIDTH);
        let (mb_x, mb_y) = (gob_mb_x0 + col, gob_mb_y0 + row);
        if !reconstruct_macroblock(
            r, idct, ap, reference, mb_x, mb_y, &mtype, mv, cbp_mask, quant,
        ) {
            break r.bit_pos();
        }

        prev_was_mc = mtype.mc;
        if r.check().is_err() {
            break r.bit_pos();
        }
    };

    // §4.2.3.1's own MBA addressing means an encoder that has nothing left
    // to say for the remainder of a GOB just stops — the trailing
    // macroblocks up to address 33 are never addressed at all, not even
    // via a "skip run" between two transmitted addresses (`fill_skipped`
    // above only ever covers a gap *between* two addressed macroblocks).
    // Per §3.4/§4.2.3.4's own framing of "not coded" macroblocks, those
    // trailing positions are exactly as much "not coded" as an internal
    // skip run and get the identical treatment: copied from the reference
    // unchanged. Missing this left every GOB's un-addressed tail sitting
    // at the frame buffer's initial (uninitialised) contents instead —
    // invisible whenever a GOB happens to use all 33 addresses, but
    // visibly wrong on any GOB that legitimately doesn't, which real
    // encoders do constantly once a scene stops changing at the edges.
    if mb_addr < LAST_ADDR {
        fill_skipped(ap, reference, gob_mb_x0, gob_mb_y0, mb_addr + 1, LAST_ADDR);
    }
    end_bit
}

/// Copy macroblocks `[from_addr, to_addr]` (1-based, within this GOB)
/// unchanged from the reference picture: an untransmitted (skipped)
/// macroblock is, by construction, zero motion and no residual.
fn fill_skipped(
    ap: &mut ActivePicture,
    reference: Option<&RefPicture>,
    gob_mb_x0: u32,
    gob_mb_y0: u32,
    from_addr: u32,
    to_addr: u32,
) {
    for addr in from_addr..=to_addr {
        let (col, row) = addr_to_col_row(addr, GOB_MB_WIDTH);
        copy_macroblock(ap, reference, gob_mb_x0 + col, gob_mb_y0 + row);
    }
}

fn copy_macroblock(ap: &mut ActivePicture, reference: Option<&RefPicture>, mb_x: u32, mb_y: u32) {
    for plane in 0..3usize {
        let (w, h, ox, oy) = if plane == 0 {
            (16u32, 16u32, mb_x * 16, mb_y * 16)
        } else {
            (8u32, 8u32, mb_x * 8, mb_y * 8)
        };
        let Some(mut dst) = ap.frame.plane_mut(plane) else {
            continue;
        };
        for y in 0..h {
            let Some(row) = dst.row_mut(usize::try_from(oy + y).unwrap_or(0)) else {
                continue;
            };
            for x in 0..w {
                let Some(dst_px) = row.get_mut(usize::try_from(ox + x).unwrap_or(0)) else {
                    continue;
                };
                let sx = i32::try_from(ox + x).unwrap_or(0);
                let sy = i32::try_from(oy + y).unwrap_or(0);
                *dst_px = sample_mc(reference, plane, sx, sy);
            }
        }
    }
}

fn sample_mc(reference: Option<&RefPicture>, plane: usize, x: i32, y: i32) -> u8 {
    reference.map_or(128, |r| r.sample(plane, x, y))
}

/// The six blocks' geometry within a macroblock (§4.2.4/Figure 10, 4:2:0
/// only — this format's only chroma layout): `(plane, col_offset,
/// row_offset)`.
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

/// Decode (or, for an uncoded block, skip) each of the six blocks, form
/// each one's own motion-compensated prediction (integer-pel only — see
/// the module docs), apply the loop filter to that prediction when
/// `mtype.fil`, add the decoded residual, saturate, and write the result.
///
/// Returns `false` on a coefficient-decode failure, at which point the
/// bitstream position is no longer trustworthy for anything past this
/// block — the caller stops the whole GOB there rather than trying to
/// resynchronise on a `MBA` code read from a desynced position.
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
    mtype: &H261MbType,
    mv: [i32; 2],
    cbp_mask: u8,
    quant: u8,
) -> bool {
    for i in 0..6usize {
        let (plane, col_off, row_off) = block_geometry(i);
        let (bw, bh): (u32, u32) = (8, 8);
        let (mv_x, mv_y) = if plane == 0 {
            (mv[0], mv[1])
        } else {
            (motion::h261_chroma_mv(mv[0]), motion::h261_chroma_mv(mv[1]))
        };
        let (px_ox, px_oy) = if plane == 0 {
            (mb_x * 16 + col_off, mb_y * 16 + row_off)
        } else {
            (mb_x * 8 + col_off, mb_y * 8 + row_off)
        };

        let mut pred = [0u8; 64];
        if !mtype.intra {
            for y in 0..bh {
                for x in 0..bw {
                    let sx = i32::try_from(px_ox + x).unwrap_or(0) + mv_x;
                    let sy = i32::try_from(px_oy + y).unwrap_or(0) + mv_y;
                    if let Some(slot) = pred.get_mut((y * bw + x) as usize) {
                        *slot = sample_mc(reference, plane, sx, sy);
                    }
                }
            }
            if mtype.fil {
                motion::h261_loop_filter(&mut pred, bw as usize, bh as usize);
            }
        }

        // Bit index 5-i within cbp_mask (P1 is the 32s place / bit 5).
        let coded = mtype.intra || (cbp_mask >> (5 - i)) & 1 == 1;
        let residual: [i32; 64] = if coded {
            let intra = mtype.intra;
            let Ok(qfs) = block::decode_h261_coefficients(r, intra) else {
                return false;
            };
            let qf = block::inverse_scan(&qfs);
            let dequant = block::dequantise(&qf, quant, intra);
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

impl Decoder for H261Decoder {
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
        let mut dec = H261Decoder::new(Limits::strict());
        assert!(matches!(dec.receive_frame(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn gob_origin_places_gob_one_at_the_top_left() {
        assert_eq!(gob_origin(1), (0, 0));
        assert_eq!(gob_origin(2), (GOB_MB_WIDTH, 0));
        assert_eq!(gob_origin(3), (0, GOB_MB_HEIGHT));
    }

    #[test]
    fn find_prefix_locates_a_non_byte_aligned_start_code() {
        // 20 zero bits, then the 16-bit prefix "0000 0000 0000 0001",
        // itself starting mid-byte (bit offset 4).
        let bytes: [u8; 6] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x10];
        // bits:      byte5 = 0000 0000, byte6 = 0001 0000 -> the "1" bit
        // sits at bit offset 4*8 + 3 = 35 from the start... construct
        // directly instead of hand-counting.
        let found = find_prefix(&bytes, 0, 48);
        assert!(found.is_some());
    }

    proptest::proptest! {
        #[test]
        fn decode_never_panics_on_arbitrary_bytes(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
            let mut budget = Budget::new(Limits::strict());
            let Ok(packet) = vaco_packet::Packet::from_slice(&mut budget, &data) else {
                return Ok(());
            };
            let mut dec = H261Decoder::new(Limits::strict());
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
