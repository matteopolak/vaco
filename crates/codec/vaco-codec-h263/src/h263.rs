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
use crate::deblock;
use crate::motion;
use crate::picture::RefPicture;
use crate::plus;
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
pub(crate) const fn source_format_dims(code: u32) -> Option<(u32, u32)> {
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "each of these mirrors one independent mode bit this crate reads from PTYPE/PLUSPTYPE (UMV in two distinct encodings, MQ, Annex K and its own RS submode) plus the pre-existing baseline `unsupported`/`intra`/`cpm` flags; H.263+'s own header genuinely has this many independent yes/no switches, and a state machine would not make any of them less independent"
)]
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
    /// §6.1.2's `RCONTROL` (the Rounding Type, `RTYPE`, bit in `MPPTYPE`):
    /// which of the two half-pel interpolation rounding conventions this
    /// picture's own motion compensation uses. Always `false` outside
    /// `PLUSPTYPE` — see `plus::PlusHeader::rtype`'s own docs.
    rcontrol: bool,
    /// Annex K §K.1 rule 1: which slice each macroblock was decoded in,
    /// parallel to `mv_grid` — consulted by [`predictors`] only when
    /// `slice_structured`, so it stays all-zero (and unused) otherwise.
    mb_slice_id: Vec<u32>,
    /// Annex D §D.2: the original H.263 version 1 `UMV` bit (`PTYPE` bit
    /// 10, no `PLUSPTYPE`) is active — see
    /// [`motion::h263_umv_vector_legacy`].
    umv_legacy: bool,
    /// Annex D §D.2: `PLUSPTYPE`'s own UMV mode (`UUI == "1"`) is active
    /// — see [`motion::h263_umv_vector_plus`].
    umv_plus: bool,
    /// Annex T: the Modified Quantization mode is active for this
    /// picture — see [`block::decode_mq_dquant`]/[`block::quant_c`].
    mq: bool,
    /// Annex K: this picture uses the slice layer (§K.2) in place of the
    /// GOB layer.
    slice_structured: bool,
    /// Annex K §K.1: the Rectangular Slice submode is active (only
    /// meaningful together with `slice_structured`).
    rectangular_slices: bool,
    /// Annex J: the Deblocking Filter mode is active for this picture —
    /// see [`crate::deblock`]. Always `false` outside `PLUSPTYPE`, same
    /// as every other Annex-J-through-T mode bit.
    deblocking_filter: bool,
    /// One entry per macroblock, row-major: `true` unless that
    /// macroblock was a `COD == 1` fully-skipped one (§5.3.1). An intra
    /// macroblock is never skipped, so this is exactly Annex J §J.3's
    /// "`COD==0 || MB-type == INTRA`" condition for one side of a block
    /// edge — consulted only when `deblocking_filter` is set.
    mb_coded: Vec<bool>,
    /// One entry per macroblock, row-major: the `QUANT` value actually
    /// used to decode that macroblock's own coefficients (after any
    /// `DQUANT`). Meaningless for a skipped (`mb_coded == false`) slot,
    /// since Annex J §J.3's `STRENGTH` lookup only ever reads the quant
    /// of a *coded* block on either side of an edge.
    mb_quant: Vec<u8>,
    /// Annex F (`Vaco-Spec-Ref: itu-t-h263` F.2): the Advanced
    /// Prediction mode is active for this picture. Always `false`
    /// outside `PLUSPTYPE`. Gates every field below, and the one-
    /// macroblock reconstruction lookahead `try_decode_one_mb` uses
    /// instead of reconstructing each macroblock's pixels immediately —
    /// see `pending`'s own docs for why.
    advanced_prediction: bool,
    /// One entry per macroblock, row-major: `true` for an INTRA-coded
    /// macroblock in an otherwise INTER picture. Distinct from
    /// `mv_grid`'s own "store zero for INTRA" convention — Annex F's
    /// OBMC remote-vector rule (`§F.3`) treats an INTRA neighbour
    /// differently from a not-coded one (replaced by the *current*
    /// block's own vector, not zero), so the zero-baked-in trick
    /// `mv_grid`/[`predictors`] rely on is not enough here; only read
    /// when `advanced_prediction` is set.
    mb_intra: Vec<bool>,
    /// One motion vector per 8x8 luma block, row-major over a grid twice
    /// `mb_width` by twice `mb_height` (Figure 5's four-blocks-per-
    /// macroblock numbering: block 0 top-left, 1 top-right, 2
    /// bottom-left, 3 bottom-right, at fine-grid position `(2*mb_x +
    /// col, 2*mb_y + row)`). Only allocated and read when
    /// `advanced_prediction` is set. A macroblock using one vector for
    /// all four blocks (§F.2: "this is defined as four vectors with the
    /// same value") replicates it into all four of its own slots here.
    block_mv: Vec<[i32; 2]>,
    /// Annex F's one-macroblock reconstruction lookahead. `None`
    /// outside Advanced Prediction mode. Within it: a macroblock's own
    /// syntax and residual are fully decoded (in bitstream order, as
    /// every other mode already does) and held here — its *pixels* are
    /// not written until the *next* macroblock's own vector is known,
    /// because OBMC's rightward remote vector (`§F.3`) can need a
    /// macroblock this crate's raster-order decode has not reached yet.
    /// Above/left/below never have this problem (see
    /// `docs/codec/vaco-codec-h263.md`'s own account of why only one
    /// macroblock of lookahead, confined to this one loop, is enough).
    /// Flushed whenever a new macroblock's own vector becomes known
    /// (`try_decode_one_mb`) and once more, unconditionally, at the end
    /// of the picture (`H263Decoder::finish_picture`) for whichever
    /// macroblock decoded last.
    pending: Option<PendingMb>,
}

/// One already-parsed macroblock's syntax and residual, held by
/// `ActivePicture::pending` until its neighbour to the right (if any) is
/// known. `four_vector` selects which chrominance rule
/// `apply_reconstruction` uses (`motion::annex_f_chroma_mv`'s
/// four-vector combination vs. the default `motion::h263_chroma_mv`,
/// applied to `mv`); `mv` is the representative vector either way (the
/// single decoded/replicated one, or block 0's own — F.2's own
/// equivalence).
#[derive(Debug)]
struct PendingMb {
    mb_x: u32,
    mb_y: u32,
    intra: bool,
    mv: [i32; 2],
    four_vector: bool,
    /// One decoded (dequantised, inverse-transformed) residual per
    /// block, in `block_geometry`'s own six-block order — the part of
    /// reconstruction that must happen immediately, in bitstream order;
    /// only the prediction + write is deferred.
    residuals: [[i32; 64]; 6],
}

/// ITU-T H.263 (baseline) video decoder. See the module docs.
#[derive(Debug)]
pub struct H263Decoder {
    machine: vaco_codec_core::Machine<Frame>,
    budget: Budget,
    idct: H26xIdct,
    reference: Option<RefPicture>,
    current: Option<ActivePicture>,
    /// §5.1.4.5's mode-persistence state, carried across pictures — see
    /// [`plus::PlusModes`]'s own docs for the inference rules.
    plus_modes: plus::PlusModes,
    /// The last `PLUSPTYPE` picture's width/height, used as the fallback
    /// source format for a later `UFEP == 0` picture (§5.1.4.3: such a
    /// picture does not resend its own dimensions).
    last_plus_dims: Option<(u32, u32)>,
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
            plus_modes: plus::PlusModes::default(),
            last_plus_dims: None,
        }
    }

    fn finish_picture(&mut self) {
        let Some(mut ap) = self.current.take() else {
            return;
        };
        // Annex F: whatever macroblock decoded last has no "next"
        // macroblock to learn its own rightward OBMC neighbour from —
        // exactly the picture-border case `annex_f_remote` already
        // handles, so finalizing it with no more information than that
        // is correct, not a compromise.
        if let Some(prev) = ap.pending.take() {
            apply_reconstruction(&mut ap, self.reference.as_ref(), &prev);
        }
        // Annex J: run once, after every macroblock in the picture has
        // already been reconstructed and clipped, and before the frame
        // becomes either the next picture's reference or this one's own
        // output — both readers must see the filtered result (§J.3: "the
        // filtering... alters the picture that is to be stored in the
        // picture store for future prediction", and the filtered picture
        // is what a real encoder/decoder pair both display and predict
        // from). Skipped for an `unsupported` (mid-grey placeholder)
        // picture: `deblocking_filter` reflects §5.1.4.5's persisted
        // mode state, which survives a later picture that itself failed
        // to parse for an unrelated reason, and filtering a flat
        // placeholder would be pure waste even though it is a no-op.
        if ap.deblocking_filter && !ap.unsupported {
            deblock::filter_picture(&mut ap.frame, ap.mb_width, ap.mb_height, &ap.mb_coded, &ap.mb_quant);
        }
        self.reference = Some(RefPicture::new(ap.frame.clone()));
        self.machine.emit(ap.frame);
    }

    fn decode_access_unit(&mut self, data: &[u8], pts: vaco_core::Timestamp, duration: vaco_core::Duration) -> Result<()> {
        let mut pos = 0usize;
        while let Some(sc) = find_prefix(data, pos) {
            let mut r = BitReader::new(data);
            r.skip_bytes(sc);
            r.skip(17); // the 17-bit prefix shared by PSC, GBSC and Annex K's SSC.

            // Annex K §K.2.2/§K.2.3: a slice header (whether the
            // abbreviated first-slice form or a full `SSC`-prefixed one)
            // always has `SEPB1 == "1"` as its very next bit, where a
            // genuine `PSC`'s corresponding bits are the fixed `"00000"`
            // that begins its own trailing byte — the same one-bit
            // distinction the `gn == 0` check below makes for `PSC` vs
            // `GBSC`, just checked one bit earlier so a slice header is
            // never misread as a 5-bit `GN`.
            if self.current.as_ref().is_some_and(|ap| ap.slice_structured && !ap.unsupported) && r.peek(1) == 1 {
                if let Some(ap) = self.current.as_mut() {
                    decode_slice(&mut r, ap, &mut self.idct, self.reference.as_ref());
                }
                pos = usize::try_from(r.bit_pos().div_ceil(8)).unwrap_or(data.len()).max(sc + 3);
                continue;
            }

            let gn = r.get(5);
            if gn == 0 {
                self.finish_picture();
                let _tr = r.get(8);
                let ptype8 = r.get(8);
                if ptype8 & 0b111 == 0b111 {
                    self.decode_plus_picture(&mut r, ptype8, pts, duration)?;
                } else {
                    // A picture with no `PLUSPTYPE` at all resets every
                    // persisted mode to "off" (§5.1.4.5).
                    self.plus_modes = plus::PlusModes::default();
                    let ptype_low5 = r.get(5);
                    let ptype = (ptype8 << 5) | ptype_low5;
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
                    let unsupported = sac || advanced_pred || is_pb || source_format_dims(source_format).is_none();
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
                        mb_slice_id: vec![0; (mb_width * mb_height) as usize],
                        umv_legacy: umv,
                        umv_plus: false,
                        mq: false,
                        slice_structured: false,
                        rectangular_slices: false,
                        rcontrol: false,
                        deblocking_filter: false,
                        mb_coded: vec![true; (mb_width * mb_height) as usize],
                        mb_quant: vec![pquant.clamp(1, 31); (mb_width * mb_height) as usize],
                        advanced_prediction: false,
                        mb_intra: Vec::new(),
                        block_mv: Vec::new(),
                        pending: None,
                    });
                    // §5.2: "For the first GOB in each picture (with
                    // number 0), no GOB header shall be transmitted" —
                    // its macroblock row (row 0) follows immediately,
                    // bit-precise (not byte-realigned: only *start
                    // codes* carry that guarantee), so this continues on
                    // the very same `r` rather than handing off to the
                    // outer byte-level scan.
                    if let Some(ap) = self.current.as_mut()
                        && !ap.unsupported
                    {
                        decode_gob(&mut r, ap, &mut self.idct, self.reference.as_ref(), 0, 0);
                    }
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
                    decode_gob(&mut r, ap, &mut self.idct, self.reference.as_ref(), gn * ap.mb_width, 0);
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

    /// The `PLUSPTYPE` picture-header path (`ptype8` bits 6-8 `== "111"`,
    /// already consumed by the caller): parse the extended header via
    /// [`plus::parse`], allocate the picture, and — unlike the baseline
    /// path — decode the abbreviated first slice inline only when Annex K
    /// is in use; a `PLUSPTYPE` picture with no Slice Structured mode
    /// still uses the ordinary GOB layer (§K.1: the slice layer "is used
    /// in place of the GOB layer" only in that one optional mode).
    fn decode_plus_picture(&mut self, r: &mut BitReader<'_>, _ptype8: u32, pts: vaco_core::Timestamp, duration: vaco_core::Duration) -> Result<()> {
        let header = plus::parse(r, &mut self.plus_modes, self.last_plus_dims);
        let (w, h, intra, cpm, unsupported) = if let Some(hdr) = &header {
            self.last_plus_dims = Some((hdr.width, hdr.height));
            (hdr.width, hdr.height, hdr.intra, hdr.cpm, false)
        } else {
            let (w, h) = self.last_plus_dims.unwrap_or((176, 144));
            (w, h, true, false, true)
        };

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
        let modes = self.plus_modes;
        self.current = Some(ActivePicture {
            frame,
            mb_width,
            mb_height,
            intra,
            quant: 1,
            cpm,
            unsupported,
            mv_grid: vec![[0, 0]; (mb_width * mb_height) as usize],
            mb_slice_id: vec![0; (mb_width * mb_height) as usize],
            umv_legacy: false,
            umv_plus: modes.umv,
            mq: modes.modified_quantization,
            slice_structured: modes.slice_structured,
            rectangular_slices: modes.rectangular_slices,
            rcontrol: header.as_ref().is_some_and(|hdr| hdr.rtype),
            deblocking_filter: modes.deblocking_filter,
            mb_coded: vec![true; (mb_width * mb_height) as usize],
            mb_quant: vec![1; (mb_width * mb_height) as usize],
            advanced_prediction: modes.advanced_prediction,
            mb_intra: vec![false; (mb_width * mb_height) as usize],
            block_mv: vec![[0, 0]; (4 * mb_width * mb_height) as usize],
            pending: None,
        });

        if unsupported {
            return Ok(());
        }
        // §5.1.11: PQUANT follows the whole PLUSPTYPE cascade (UFEP/
        // OPPTYPE/MPPTYPE, then CPM/PSBI, then any of CPFMT/EPAR/CPCFC/
        // ETR/UUI/SSS that applied) regardless of whether Annex K is in
        // use — the first slice's own abbreviated header (§K.2) carries
        // no `SQUANT` of its own, so this is where its starting `QUANT`
        // comes from.
        let pquant = r.get(5) as u8;
        // Figure 7's `PEI`/`PSUPP`/`PEI` chain is not one of Figure 8's
        // `PLUSPTYPE`-only fields — it keeps its ordinary position right
        // before the picture data regardless of whether `PLUSPTYPE` was
        // used, same as the non-extended header (see
        // [`skip_pei_chain`]'s own call in the sibling branch above). At
        // minimum this is one `PEI = "0"` bit even when no supplemental
        // data is sent at all; omitting it here left every `PLUSPTYPE`
        // picture's first slice/GOB one bit short of alignment.
        skip_pei_chain(r);
        if let Some(ap) = self.current.as_mut() {
            ap.quant = pquant.clamp(1, 31);
            if ap.slice_structured {
                decode_first_slice(r, ap, &mut self.idct, self.reference.as_ref());
            } else {
                decode_gob(r, ap, &mut self.idct, self.reference.as_ref(), 0, 0);
            }
        }
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
#[allow(
    clippy::too_many_arguments,
    reason = "the slice-boundary check adds exactly two related parameters (the id grid and the current macroblock's own id) to what was already every input this function's neighbour lookup needs"
)]
fn predictors(
    mv_grid: &[[i32; 2]],
    mb_width: u32,
    mb_height: u32,
    mb_x: u32,
    mb_y: u32,
    slice_ids: Option<(&[u32], u32)>,
) -> ([i32; 2], [i32; 2], [i32; 2]) {
    let get = |x: i32, y: i32| -> Option<[i32; 2]> {
        if x < 0 || y < 0 || x >= i32::try_from(mb_width).unwrap_or(0) || y >= i32::try_from(mb_height).unwrap_or(0) {
            return None;
        }
        let idx = usize::try_from(y).unwrap_or(0) * mb_width as usize + usize::try_from(x).unwrap_or(0);
        // Annex K §K.1 rule 1 (`Vaco-Spec-Ref: itu-t-h263` K.1): "the
        // prediction of motion vector values are the same as if a GOB
        // header were present" — a neighbour in a different slice is
        // treated exactly like one outside the picture, never like one
        // in the same GOB/slice just a row up. `slice_ids` is `None` for
        // every non-slice-structured picture, where this check never
        // applies (see the module docs' own baseline "GOB-header
        // emptiness is not tracked" simplification — a genuine choice
        // for the common case where GOB headers are mostly absent, but
        // Annex K's slices always have a real, non-empty header).
        if let Some((ids, current)) = slice_ids
            && ids.get(idx).copied().unwrap_or(current) != current
        {
            return None;
        }
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

/// Figure 5's block-within-macroblock column (`block % 2`: 0 for blocks
/// 0/2, 1 for blocks 1/3) and row (`block / 2`: 0 for blocks 0/1, 1 for
/// blocks 2/3) — Annex F's four-vector numbering, matching
/// [`block_geometry`]'s own `col_off`/`row_off` pairing for `i in 0..4`.
#[allow(
    clippy::integer_division,
    reason = "`block` is always 0..4 by construction; `block / 2` recovers its row within the macroblock exactly (0 or 1), not a lossy average"
)]
fn block_row_col(block: u8) -> (u32, u32) {
    (u32::from(block % 2), u32::from(block / 2))
}

/// A fine-grid (8x8-block) coordinate's owning macroblock index along
/// the same axis — two fine cells per macroblock.
#[allow(
    clippy::integer_division,
    reason = "the fine grid is exactly twice the macroblock grid in each dimension by construction (`set_block_mv`'s own `grid_w = mb_width * 2`); `/ 2` recovers the owning macroblock index exactly, not a lossy average"
)]
fn fine_to_mb(coord: u32) -> u32 {
    coord / 2
}

/// Write all four of one macroblock's 8x8 luma blocks' vectors into
/// `ap.block_mv`'s fine grid at once — Figure 5's numbering (0 top-left,
/// 1 top-right, 2 bottom-left, 3 bottom-right).
fn set_block_mv(ap: &mut ActivePicture, mb_x: u32, mb_y: u32, mvs: [[i32; 2]; 4]) {
    let grid_w = ap.mb_width * 2;
    for (b, mv) in mvs.into_iter().enumerate() {
        let b = u8::try_from(b).unwrap_or(0);
        let (bx, by) = block_row_col(b);
        let gx = mb_x * 2 + bx;
        let gy = mb_y * 2 + by;
        let idx = (gy * grid_w + gx) as usize;
        if let Some(slot) = ap.block_mv.get_mut(idx) {
            *slot = mv;
        }
    }
}

/// Write exactly one of a macroblock's four 8x8 luma blocks' vectors —
/// the `INTER4V` counterpart to [`set_block_mv`]'s "same value in all
/// four" case, since each of the four blocks is decoded (and its own
/// vector determined) one at a time.
fn set_one_block_mv(ap: &mut ActivePicture, mb_x: u32, mb_y: u32, block: u8, mv: [i32; 2]) {
    let grid_w = ap.mb_width * 2;
    let (bx, by) = block_row_col(block);
    let gx = mb_x * 2 + bx;
    let gy = mb_y * 2 + by;
    let idx = (gy * grid_w + gx) as usize;
    if let Some(slot) = ap.block_mv.get_mut(idx) {
        *slot = mv;
    }
}

/// Annex F §F.2 (`Vaco-Spec-Ref: itu-t-h263` Figure F.1): the three
/// candidate predictors for one 8x8 luma block of macroblock `(mb_x,
/// mb_y)`, `block` in `0..4` (Figure 5's numbering). F.2's own text says
/// to reuse "the decision rules given in 6.1.1" for these redefined
/// candidates — this is [`predictors`]'s exact same rule 2/3/4 shape
/// (MV1 zero if off the left edge, MV2/MV3 fall back to MV1 if off the
/// top, MV3 forced to zero if off the right), just resolved against
/// `ap.block_mv`'s finer, twice-as-wide/tall grid via
/// [`motion::annex_f_predictor_sources`]'s offsets instead of
/// [`predictors`]'s fixed `(-1, 0)`/`(0, -1)`/`(1, -1)` macroblock ones.
///
/// Used unconditionally under Advanced Prediction mode, for a
/// one-vector macroblock too (block 0's own rule) — not only a
/// four-vector one's own blocks — because a *neighbouring* macroblock
/// may itself be four-vector, and then "the corresponding macroblock's
/// vector" [`predictors`] would read from `ap.mv_grid` is no longer
/// well-defined; only this fine-grid version can name the one specific
/// 8x8 block Figure F.1 actually wants.
///
/// Slice Structured mode combined with Advanced Prediction is rejected
/// at the picture header (`plus::parse`) before this is ever called, so
/// unlike [`predictors`] this has no slice-boundary case to check.
fn annex_f_predictors(ap: &ActivePicture, mb_x: u32, mb_y: u32, block: u8) -> ([i32; 2], [i32; 2], [i32; 2]) {
    let grid_w = ap.mb_width * 2;
    let grid_h = ap.mb_height * 2;
    let (bx, by) = block_row_col(block);
    let own_gx = i32::try_from(mb_x * 2 + bx).unwrap_or(0);
    let own_gy = i32::try_from(mb_y * 2 + by).unwrap_or(0);
    let offsets = motion::annex_f_predictor_sources(block);
    let get = |dx: i32, dy: i32| -> Option<[i32; 2]> {
        let gx = own_gx + dx;
        let gy = own_gy + dy;
        if gx < 0 || gy < 0 || gx >= i32::try_from(grid_w).unwrap_or(0) || gy >= i32::try_from(grid_h).unwrap_or(0) {
            return None;
        }
        let idx = usize::try_from(gy).unwrap_or(0) * grid_w as usize + usize::try_from(gx).unwrap_or(0);
        ap.block_mv.get(idx).copied()
    };
    let mv1 = get(offsets[0].0, offsets[0].1).unwrap_or([0, 0]);
    let mv2 = get(offsets[1].0, offsets[1].1).unwrap_or(mv1);
    let mut mv3 = get(offsets[2].0, offsets[2].1).unwrap_or(mv1);
    if own_gx + offsets[2].0 >= i32::try_from(grid_w).unwrap_or(0) {
        mv3 = [0, 0];
    }
    (mv1, mv2, mv3)
}

/// Annex F §F.3 (`Vaco-Spec-Ref: itu-t-h263` F.3): one "remote" motion
/// vector for OBMC, resolved through every substitution rule the
/// primary text states, in the order it states them:
///
/// 1. (checked first, "in all cases") if this is the vertically-below
///    direction and `block` is in the bottom row of its macroblock
///    (Figure 5's block 2 or 3), the macroblock below has not been
///    decoded yet regardless of picture geometry — replaced by the
///    current block's own vector.
/// 2. If the neighbour position is outside the picture (border) —
///    replaced by the current block's own vector (no neighbour
///    present). This is also what makes the "right" direction safe to
///    look up one macroblock ahead of reconstruction (see
///    `ActivePicture::pending`): a macroblock at the last column's
///    right lookup lands here, regardless of whether a macroblock in
///    the next row has been decoded yet.
/// 3. If the neighbouring macroblock was not coded (`COD == 1`,
///    skipped) — zero.
/// 4. If the neighbouring macroblock was INTRA — the current block's
///    own vector (except PB-frames mode, out of this crate's scope).
/// 5. Otherwise, the neighbour's own real vector from `ap.block_mv`.
#[allow(
    clippy::too_many_arguments,
    reason = "one remote lookup genuinely needs the macroblock position, which block within it, its own already-decoded vector, the direction offset, and whether that direction is the special-cased 'below' one — collapsing these into a struct would not make any call site clearer, there being exactly four call sites, one per direction"
)]
fn annex_f_remote(ap: &ActivePicture, mb_x: u32, mb_y: u32, block: u8, own_mv: [i32; 2], dgx: i32, dgy: i32, is_below: bool) -> [i32; 2] {
    if is_below && matches!(block, 2 | 3) {
        return own_mv;
    }
    let grid_w = ap.mb_width * 2;
    let grid_h = ap.mb_height * 2;
    let (bx, by) = block_row_col(block);
    let own_gx = i32::try_from(mb_x * 2 + bx).unwrap_or(0);
    let own_gy = i32::try_from(mb_y * 2 + by).unwrap_or(0);
    let gx = own_gx + dgx;
    let gy = own_gy + dgy;
    if gx < 0 || gy < 0 || gx >= i32::try_from(grid_w).unwrap_or(0) || gy >= i32::try_from(grid_h).unwrap_or(0) {
        return own_mv;
    }
    let nb_mb_x = fine_to_mb(u32::try_from(gx).unwrap_or(0));
    let nb_mb_y = fine_to_mb(u32::try_from(gy).unwrap_or(0));
    let nb_idx = (nb_mb_y * ap.mb_width + nb_mb_x) as usize;
    if !ap.mb_coded.get(nb_idx).copied().unwrap_or(true) {
        return [0, 0];
    }
    if ap.mb_intra.get(nb_idx).copied().unwrap_or(false) {
        return own_mv;
    }
    let idx = usize::try_from(gy).unwrap_or(0) * grid_w as usize + usize::try_from(gx).unwrap_or(0);
    ap.block_mv.get(idx).copied().unwrap_or(own_mv)
}

/// Whether `r` sits on a `00 00 1xxxxxxx` — the `PSC`/`GBSC`/Annex K
/// `SSC` prefix every start code shares (see [`find_prefix`]) — either
/// exactly byte-aligned already, or reachable by skipping nothing but
/// zero *stuffing* bits (`PSTUF`/`GSTUF`/`SSTUF`, always fewer than 8 of
/// them by construction) up to the next byte boundary.
///
/// The stuffing-aware branch matters whenever real data ends without
/// landing on a byte boundary by chance — the common case, since
/// macroblock coefficient data is variable-length — and a genuine start
/// code (rather than an absent header) follows it: checking only the
/// already-aligned case would misread those stuffing bits as one more
/// macroblock's own `COD`/`MCBPC` bits. Annex K's slice layer hits this
/// on essentially every row (every slice ends in `SSTUF` before the next
/// slice's `SSC`), where the baseline GOB layer mostly doesn't (ffmpeg's
/// own baseline encoder was observed leaving most GOB headers out
/// entirely, so [`decode_gob`] usually never needs to notice a boundary
/// mid-picture at all) — but the same misreading risk exists there too
/// whenever a GOB header genuinely is present and happens to follow
/// non-byte-aligned data.
fn at_start_code(r: &mut BitReader<'_>) -> bool {
    let pos = r.bit_pos();
    if pos.is_multiple_of(8) {
        return matches!(r.remaining_bytes().first_chunk::<3>(), Some(&[0, 0, c]) if c & 0x80 != 0);
    }
    let pad = 8 - u32::try_from(pos % 8).unwrap_or(0);
    let ahead = r.peek(pad + 24);
    let stuffing = ahead >> 24;
    if stuffing != 0 {
        return false; // real data, not pure zero stuffing.
    }
    let a = (ahead >> 16) & 0xFF;
    let b = (ahead >> 8) & 0xFF;
    let c = ahead & 0xFF;
    a == 0 && b == 0 && c & 0x80 != 0
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

/// One [`try_decode_one_mb`] outcome: whether it decoded a macroblock,
/// found the stuffing pseudo-macroblock (retry the same index), hit a
/// genuine start code (stop, successfully), or failed (stop, the rest of
/// the bit position is untrustworthy).
enum MbOutcome {
    Decoded,
    Retry,
    StartCode,
    Fail,
}

/// Decode exactly one macroblock (or recognise the stuffing pseudo-
/// macroblock, or a start code, without consuming either as a real one)
/// at absolute index `mb_index`. Factored out of the raster-order GOB
/// loop below so Annex K's rectangular-slice scan (which does not visit
/// `mb_index` in a simple `+1` sequence — see [`decode_slice_rect`]) can
/// share it exactly rather than re-deriving the macroblock layer.
fn try_decode_one_mb(r: &mut BitReader<'_>, ap: &mut ActivePicture, idct: &mut H26xIdct, reference: Option<&RefPicture>, mb_index: u32, slice_id: u32) -> MbOutcome {
    if r.check().is_err() {
        return MbOutcome::Fail;
    }
    if at_start_code(r) {
        return MbOutcome::StartCode;
    }
    let (row, col) = mb_row_col(mb_index, ap.mb_width);
    if let Some(slot) = ap.mb_slice_id.get_mut(mb_index as usize) {
        *slot = slice_id;
    }
    let cod = if ap.intra { false } else { r.get_bit() == 1 };
    if cod {
        // §5.3.1: fully skipped — INTER, zero vector, no coefficients.
        let idx = mb_index as usize;
        if let Some(slot) = ap.mv_grid.get_mut(idx) {
            *slot = [0, 0];
        }
        if let Some(slot) = ap.mb_coded.get_mut(idx) {
            *slot = false;
        }
        if ap.advanced_prediction {
            if let Some(slot) = ap.mb_intra.get_mut(idx) {
                *slot = false;
            }
            set_block_mv(ap, col, row, [[0, 0]; 4]);
        }
        return if reconstruct_or_defer(r, idct, ap, reference, col, row, false, [0, 0], 0, false, false) {
            MbOutcome::Decoded
        } else {
            MbOutcome::Fail
        };
    }

    let mcbpc_table: &[(&str, u8, u8)] = if ap.intra {
        tables::H263_MCBPC_INTRA
    } else {
        tables::H263_MCBPC_INTER
    };
    let Some(&(_, mb_type, cbpc)) = vlc::decode(r, mcbpc_table, |c| c.0, 9) else {
        return MbOutcome::Fail;
    };
    let is_stuffing = if ap.intra { mb_type == 8 } else { mb_type == 20 };
    if is_stuffing {
        if r.check().is_err() {
            return MbOutcome::Fail;
        }
        return MbOutcome::Retry; // §5.3.2: not a real macroblock; retry this index.
    }

    let intra_mb = matches!(mb_type, 3 | 4);
    let has_dquant = matches!(mb_type, 1 | 4);
    let has_mv = matches!(mb_type, 0 | 1);

    if has_dquant {
        if ap.mq {
            ap.quant = block::decode_mq_dquant(r, ap.quant);
        } else {
            let d = r.get(2) as usize;
            let delta = i32::from(tables::H263_DQUANT.get(d).copied().unwrap_or(0));
            ap.quant = i32::from(ap.quant).saturating_add(delta).clamp(1, 31) as u8;
        }
    }

    let cbpy_table: &[(&str, u8)] = if intra_mb {
        tables::H263_CBPY_INTRA
    } else {
        tables::H263_CBPY_INTER
    };
    let Some(&(_, cbpy)) = vlc::decode(r, cbpy_table, |c| c.0, 6) else {
        return MbOutcome::Fail;
    };
    let cbp = (cbpy << 2) | cbpc;

    if mb_type == 2 {
        // Annex F §F.2: `INTER4V` is only meaningful under Advanced
        // Prediction mode. A conforming encoder never sends it
        // otherwise; bounded fuzz input still can, so this stays the
        // same defensive bail every other out-of-scope mode gets.
        if !ap.advanced_prediction {
            return MbOutcome::Fail;
        }
        let mut block_final = [[0i32; 2]; 4];
        for (b, slot) in block_final.iter_mut().enumerate() {
            let block = u8::try_from(b).unwrap_or(0);
            let (mv1, mv2, mv3) = annex_f_predictors(ap, col, row, block);
            let pred_x = motion::median3(mv1[0], mv2[0], mv3[0]);
            let pred_y = motion::median3(mv1[1], mv2[1], mv3[1]);
            let Some(mv) = decode_one_mv(r, ap, pred_x, pred_y) else {
                return MbOutcome::Fail;
            };
            *slot = mv;
            // Written immediately (not after the loop): blocks 1, 2 and
            // 3's own predictors can reach an earlier block of this same
            // macroblock (`annex_f_predictor_sources`'s internal cases),
            // which must already be in `ap.block_mv` by the time it is
            // looked up, exactly as real decode order guarantees.
            set_one_block_mv(ap, col, row, block, mv);
        }
        let idx = mb_index as usize;
        if let Some(slot) = ap.mv_grid.get_mut(idx) {
            // F.2's own equivalence: block 0 is defined exactly as the
            // base single-vector rule, so it is the right representative
            // for any old single-vector-style neighbour lookup.
            *slot = block_final[0];
        }
        if let Some(slot) = ap.mb_intra.get_mut(idx) {
            *slot = false;
        }
        if let Some(slot) = ap.mb_coded.get_mut(idx) {
            *slot = true;
        }
        if let Some(slot) = ap.mb_quant.get_mut(idx) {
            *slot = ap.quant;
        }
        return if reconstruct_or_defer(r, idct, ap, reference, col, row, false, block_final[0], cbp, true, true) {
            MbOutcome::Decoded
        } else {
            MbOutcome::Fail
        };
    }

    let mv = if has_mv {
        // Annex F §F.2: "If only one vector per macroblock is present,
        // MV1, MV2 and MV3 are defined as for the 8*8 block numbered 1"
        // — unconditionally under Advanced Prediction mode, not only
        // when the *current* macroblock happens to be four-vector. This
        // matters whenever a *neighbour* is four-vector: the old
        // macroblock-granularity `predictors()` has no way to name one
        // specific 8x8 block of a four-vector neighbour (it only ever
        // reads that neighbour's single `mv_grid` "representative"
        // slot), where block 0's own Figure F.1 definition needs the
        // *left* neighbour's block 1, the *above* neighbour's block 2,
        // and the *above-right* neighbour's block 2 — three different,
        // specific blocks, not "whichever representative value
        // `mv_grid` happens to hold".
        let (mv1, mv2, mv3) = if ap.advanced_prediction {
            annex_f_predictors(ap, col, row, 0)
        } else {
            let slice_ctx = ap.slice_structured.then_some((ap.mb_slice_id.as_slice(), slice_id));
            predictors(&ap.mv_grid, ap.mb_width, ap.mb_height, col, row, slice_ctx)
        };
        let pred_x = motion::median3(mv1[0], mv2[0], mv3[0]);
        let pred_y = motion::median3(mv1[1], mv2[1], mv3[1]);
        let Some(mv) = decode_one_mv(r, ap, pred_x, pred_y) else {
            return MbOutcome::Fail;
        };
        mv
    } else {
        [0, 0]
    };

    let idx = mb_index as usize;
    if let Some(slot) = ap.mv_grid.get_mut(idx) {
        *slot = if intra_mb { [0, 0] } else { mv };
    }
    if let Some(slot) = ap.mb_coded.get_mut(idx) {
        *slot = true;
    }
    if let Some(slot) = ap.mb_quant.get_mut(idx) {
        *slot = ap.quant;
    }
    if ap.advanced_prediction {
        // §F.2: "If only one vector per macroblock is present, this is
        // defined as four vectors with the same value" — an INTRA
        // macroblock has no motion vector at all, so its four slots stay
        // the zeroed `mv_grid` convention already used above.
        if let Some(slot) = ap.mb_intra.get_mut(idx) {
            *slot = intra_mb;
        }
        set_block_mv(ap, col, row, [if intra_mb { [0, 0] } else { mv }; 4]);
    }

    if reconstruct_or_defer(r, idct, ap, reference, col, row, intra_mb, mv, cbp, true, false) {
        MbOutcome::Decoded
    } else {
        MbOutcome::Fail
    }
}

/// One `MVD`/`MVD2-4` motion vector component pair, decoded against
/// predictor `(pred_x, pred_y)` — factored out of the single-vector path
/// above so Annex F's four-vectors-per-macroblock case (`mb_type == 2`)
/// can call it four times without re-deriving the `UMV`
/// legacy/`PLUSPTYPE` branching.
fn decode_one_mv(r: &mut BitReader<'_>, ap: &ActivePicture, pred_x: i32, pred_y: i32) -> Option<[i32; 2]> {
    if ap.umv_plus {
        // Annex D §D.2, `PLUSPTYPE` present: Table D.3 (see
        // `motion::h263_umv_vector_plus`'s own docs for why no range
        // correction is needed here).
        let dh = block::decode_table_d3(r);
        let dv = block::decode_table_d3(r);
        let mv = [motion::h263_umv_vector_plus(pred_x, dh), motion::h263_umv_vector_plus(pred_y, dv)];
        // §D.2: a (+0.5, +0.5) difference pair needs one stuffing bit
        // consumed afterward, to prevent start-code emulation.
        if dh == 1 && dv == 1 {
            r.skip(1);
        }
        Some(mv)
    } else {
        let &(_, dh) = vlc::decode(r, tables::H263_MVD, |c| c.0, 13)?;
        let &(_, dv) = vlc::decode(r, tables::H263_MVD, |c| c.0, 13)?;
        Some(if ap.umv_legacy {
            [
                motion::h263_umv_vector_legacy(pred_x, i32::from(dh)),
                motion::h263_umv_vector_legacy(pred_y, i32::from(dv)),
            ]
        } else {
            [
                motion::h263_vector(pred_x, i32::from(dh)),
                motion::h263_vector(pred_y, i32::from(dv)),
            ]
        })
    }
}

/// Outside Advanced Prediction mode: reconstruct immediately, exactly as
/// every mode already did before this annex existed (a direct call to
/// [`reconstruct_macroblock`], byte-for-byte unchanged). Under it:
/// decode this macroblock's own residual now, in bitstream order (the
/// part that cannot be deferred), then finalize whichever macroblock was
/// previously pending — its own right-hand OBMC neighbour, this one, is
/// now known — before this one takes its place as the new pending item.
/// See `ActivePicture::pending`'s own docs for why one macroblock of
/// lookahead is enough.
#[allow(
    clippy::too_many_arguments,
    reason = "relays reconstruct_macroblock's own argument list (bitstream, transform, picture state, reference, position, mode flags, motion vector, CBP) plus four_vector; splitting further would just thread the same values through an intermediate struct"
)]
fn reconstruct_or_defer(
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
    four_vector: bool,
) -> bool {
    if !ap.advanced_prediction {
        return reconstruct_macroblock(r, idct, ap, reference, mb_x, mb_y, intra, mv, cbp, coded_mb);
    }
    let Some(residuals) = decode_block_residuals(r, idct, ap, intra, cbp, coded_mb) else {
        return false;
    };
    if let Some(prev) = ap.pending.take() {
        apply_reconstruction(ap, reference, &prev);
    }
    ap.pending = Some(PendingMb { mb_x, mb_y, intra, mv, four_vector, residuals });
    true
}

/// Phase 1 of Advanced Prediction reconstruction: decode (or, for an
/// uncoded block, skip) each of the six blocks' residual coefficients —
/// exactly [`reconstruct_macroblock`]'s own per-block coefficient decode,
/// factored out so it can run immediately (bitstream order) while the
/// prediction it will be added to waits for [`apply_reconstruction`].
fn decode_block_residuals(r: &mut BitReader<'_>, idct: &mut H26xIdct, ap: &ActivePicture, intra: bool, cbp: u8, coded_mb: bool) -> Option<[[i32; 64]; 6]> {
    let mut out = [[0i32; 64]; 6];
    for (i, slot) in out.iter_mut().enumerate() {
        let (plane, _, _) = block_geometry(i);
        let block_quant = if plane == 0 || !ap.mq { ap.quant } else { block::quant_c(ap.quant) };
        let cbp_bit = (cbp >> (5 - i)) & 1 == 1;
        let residual: [i32; 64] = if intra {
            let qfs = block::decode_h263_coefficients_mq(r, true, cbp_bit, ap.mq).ok()?;
            let qf = block::inverse_scan(&qfs);
            let dequant = block::dequantise_ranged(&qf, block_quant, true, ap.mq);
            block::inverse_transform(idct, &dequant)
        } else if coded_mb && cbp_bit {
            let qfs = block::decode_h263_coefficients_mq(r, false, true, ap.mq).ok()?;
            let qf = block::inverse_scan(&qfs);
            let dequant = block::dequantise_ranged(&qf, block_quant, false, ap.mq);
            block::inverse_transform(idct, &dequant)
        } else {
            [0i32; 64]
        };
        *slot = residual;
    }
    Some(out)
}

/// Phase 2 of Advanced Prediction reconstruction: form each of a pending
/// macroblock's six blocks' predictions — OBMC (`motion::annex_f_obmc_luma_block`)
/// for luma, using `ap.block_mv`/`ap.mb_coded`/`ap.mb_intra` state that is
/// now more complete than it was when this macroblock was first parsed
/// (specifically: its own right-hand neighbour's vector, if any, is now
/// known); plain single-vector motion compensation for chroma, using
/// either the four-vector `motion::annex_f_chroma_mv` combination or the
/// default `motion::h263_chroma_mv` rule, per `pending.four_vector` —
/// add the already-decoded residual, clip, and write.
fn apply_reconstruction(ap: &mut ActivePicture, reference: Option<&RefPicture>, pending: &PendingMb) {
    let mb_x = pending.mb_x;
    let mb_y = pending.mb_y;
    let intra = pending.intra;
    let mv = pending.mv;

    let chroma_mv = if pending.four_vector {
        let grid_w = (ap.mb_width * 2) as usize;
        let at = |b: u8| -> [i32; 2] {
            let (bx, by) = block_row_col(b);
            let gx = (mb_x * 2 + bx) as usize;
            let gy = (mb_y * 2 + by) as usize;
            ap.block_mv.get(gy * grid_w + gx).copied().unwrap_or([0, 0])
        };
        let (b0, b1, b2, b3) = (at(0), at(1), at(2), at(3));
        [
            motion::annex_f_chroma_mv([b0[0], b1[0], b2[0], b3[0]]),
            motion::annex_f_chroma_mv([b0[1], b1[1], b2[1], b3[1]]),
        ]
    } else {
        [motion::h263_chroma_mv(mv[0]), motion::h263_chroma_mv(mv[1])]
    };

    for i in 0..6usize {
        let (plane, col_off, row_off) = block_geometry(i);
        let (bw, bh): (u32, u32) = (8, 8);
        let (px_ox, px_oy) = if plane == 0 {
            (mb_x * 16 + col_off, mb_y * 16 + row_off)
        } else {
            (mb_x * 8 + col_off, mb_y * 8 + row_off)
        };

        let mut pred = [0u8; 64];
        if !intra {
            match reference {
                Some(refp) if plane == 0 => {
                    let block = u8::try_from(i).unwrap_or(0);
                    let grid_w = (ap.mb_width * 2) as usize;
                    let (bx, by) = block_row_col(block);
                    let gx = (mb_x * 2 + bx) as usize;
                    let gy = (mb_y * 2 + by) as usize;
                    let own = ap.block_mv.get(gy * grid_w + gx).copied().unwrap_or(mv);
                    let above = annex_f_remote(ap, mb_x, mb_y, block, own, 0, -1, false);
                    let below = annex_f_remote(ap, mb_x, mb_y, block, own, 0, 1, true);
                    let left = annex_f_remote(ap, mb_x, mb_y, block, own, -1, 0, false);
                    let right = annex_f_remote(ap, mb_x, mb_y, block, own, 1, 0, false);
                    pred = motion::annex_f_obmc_luma_block(
                        refp,
                        i32::try_from(px_ox).unwrap_or(0),
                        i32::try_from(px_oy).unwrap_or(0),
                        own,
                        above,
                        below,
                        left,
                        right,
                        ap.rcontrol,
                    );
                }
                Some(refp) => {
                    let (mv_x, mv_y) = (chroma_mv[0], chroma_mv[1]);
                    for y in 0..bh {
                        for x in 0..bw {
                            let sx = i32::try_from(px_ox + x).unwrap_or(0);
                            let sy = i32::try_from(px_oy + y).unwrap_or(0);
                            if let Some(slot) = pred.get_mut((y * bw + x) as usize) {
                                *slot = motion::sample_half_pel(refp, plane, sx, sy, mv_x, mv_y, ap.rcontrol);
                            }
                        }
                    }
                }
                None => pred = [128u8; 64],
            }
        }

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
                let r_val = pending.residuals.get(i).and_then(|res| res.get((y * bw + x) as usize)).copied().unwrap_or(0);
                let p_val = i32::from(pred.get((y * bw + x) as usize).copied().unwrap_or(0));
                *dst = (r_val + p_val).clamp(0, 255) as u8;
            }
        }
    }
}

/// Decode macroblocks in raster order starting at absolute index
/// `start_mb`, continuing across row boundaries until either a genuine
/// start code is found or the picture's last macroblock is reached.
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
/// Also used, unmodified, for a non-rectangular Annex K slice: both cases
/// are "decode consecutive macroblocks in plain raster order starting
/// somewhere other than 0", the only difference being *why* the starting
/// index isn't 0.
///
/// Returns `false` on a coefficient-decode failure or unsupported
/// `mb_type`, at which point the caller stops the whole picture rather
/// than continuing on an untrustworthy bit position.
fn decode_gob(r: &mut BitReader<'_>, ap: &mut ActivePicture, idct: &mut H26xIdct, reference: Option<&RefPicture>, start_mb: u32, slice_id: u32) -> bool {
    let total_mbs = ap.mb_width * ap.mb_height;
    let mut mb_index = start_mb;
    while mb_index < total_mbs {
        match try_decode_one_mb(r, ap, idct, reference, mb_index, slice_id) {
            MbOutcome::Decoded => mb_index += 1,
            MbOutcome::Retry => {}
            MbOutcome::StartCode => return true,
            MbOutcome::Fail => return false,
        }
    }
    true
}

/// Annex K §K.1's Rectangular Slice submode: as [`decode_gob`], but
/// macroblocks are visited in raster order *within a `rect_width`-wide
/// sub-rectangle* starting at `start_mb` — after `rect_width` consecutive
/// macroblocks, the index wraps to the same horizontal offset one
/// picture row down, rather than continuing into the next slice's own
/// macroblocks. A `rect_width` equal to the picture's own `mb_width`
/// degenerates to plain raster order, so this also covers a
/// rectangular-mode slice that happens to span the picture's full width.
fn decode_slice_rect(r: &mut BitReader<'_>, ap: &mut ActivePicture, idct: &mut H26xIdct, reference: Option<&RefPicture>, start_mb: u32, rect_width: u32, slice_id: u32) -> bool {
    let total_mbs = ap.mb_width * ap.mb_height;
    let rect_width = rect_width.max(1).min(ap.mb_width);
    let mut mb_index = start_mb;
    let mut col_in_rect: u32 = 0;
    loop {
        if mb_index >= total_mbs {
            return true;
        }
        match try_decode_one_mb(r, ap, idct, reference, mb_index, slice_id) {
            MbOutcome::Decoded => {
                col_in_rect += 1;
                if col_in_rect >= rect_width {
                    col_in_rect = 0;
                    mb_index += ap.mb_width - rect_width + 1;
                } else {
                    mb_index += 1;
                }
            }
            MbOutcome::Retry => {}
            MbOutcome::StartCode => return true,
            MbOutcome::Fail => return false,
        }
    }
}

/// Annex K, Table K.2 (`Vaco-Spec-Ref: itu-t-h263` K.2.5), "Default"
/// column only — this crate's own Reduced-Resolution Update bail (see
/// [`plus::parse`]) means the `RRU mode` column is never reached: `MBA`'s
/// bit width, given the picture's total macroblock count. Custom sizes
/// use "the first entry... that has an equal or larger number of
/// macroblocks", which the fallthrough band naturally provides.
const fn mba_field_width(mb_count: u32) -> u32 {
    if mb_count <= 48 {
        6
    } else if mb_count <= 99 {
        7
    } else if mb_count <= 396 {
        9
    } else if mb_count <= 1584 {
        11
    } else if mb_count <= 6336 {
        13
    } else {
        14
    }
}

/// Annex K, Table K.3 (`Vaco-Spec-Ref: itu-t-h263` K.2.8), "Default"
/// column: `SWI`'s bit width, given the picture's macroblock-column
/// count. "The next standard format size which is equal or larger in
/// width" collapses sub-QCIF and QCIF to the same 4-bit width here, since
/// both are `<= 11` macroblock columns.
const fn swi_field_width(mb_width: u32) -> u32 {
    if mb_width <= 11 {
        4
    } else if mb_width <= 22 {
        5
    } else if mb_width <= 44 {
        6
    } else {
        7
    }
}

/// Annex K §K.2 (`Vaco-Spec-Ref: itu-t-h263` K.2): the abbreviated slice
/// header for the one slice that immediately follows the picture start
/// code — unlike every later slice, it has no `SSTUF`/`SSC` of its own
/// (the picture header's own byte alignment already covers it) and no
/// `SSBI`/`SQUANT` (the picture layer's `PQUANT`, already applied to
/// `ap.quant` by the caller, is this slice's starting `QUANT`).
fn decode_first_slice(r: &mut BitReader<'_>, ap: &mut ActivePicture, idct: &mut H26xIdct, reference: Option<&RefPicture>) {
    r.skip(1); // SEPB1.
    let mba = r.get(mba_field_width(ap.mb_width * ap.mb_height));
    if ap.rectangular_slices {
        r.skip(1); // SEPB2: always present here when RS is active (§K.2.6).
    }
    let rect_width = ap.rectangular_slices.then(|| r.get(swi_field_width(ap.mb_width)) + 1);
    r.skip(1); // SEPB3.
    match rect_width {
        Some(w) => {
            decode_slice_rect(r, ap, idct, reference, mba, w, mba);
        }
        None => {
            decode_gob(r, ap, idct, reference, mba, mba);
        }
    }
}

/// Annex K §K.2 (`Vaco-Spec-Ref: itu-t-h263` K.2): a slice header for any
/// slice other than the one immediately following the picture start code
/// — reached from the main scan loop right after its `SSC` (the same
/// 17-bit prefix `PSC`/`GBSC` share) and one already-peeked `SEPB1` bit.
fn decode_slice(r: &mut BitReader<'_>, ap: &mut ActivePicture, idct: &mut H26xIdct, reference: Option<&RefPicture>) {
    r.skip(1); // SEPB1 (already peeked by the caller to reach here).
    if ap.cpm {
        r.skip(4); // SSBI.
    }
    let mba_width = mba_field_width(ap.mb_width * ap.mb_height);
    let mba = r.get(mba_width);
    // §K.2.6's own condition, verbatim.
    let needs_sepb2 = (mba_width > 11 && !ap.cpm) || (mba_width > 9 && ap.cpm);
    if needs_sepb2 {
        r.skip(1);
    }
    ap.quant = (r.get(5) as u8).clamp(1, 31); // SQUANT.
    let rect_width = ap.rectangular_slices.then(|| r.get(swi_field_width(ap.mb_width)) + 1);
    r.skip(1); // SEPB3.
    r.skip(2); // GFID.
    match rect_width {
        Some(w) => {
            decode_slice_rect(r, ap, idct, reference, mba, w, mba);
        }
        None => {
            decode_gob(r, ap, idct, reference, mba, mba);
        }
    }
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
                                *slot = motion::sample_half_pel(refp, plane, sx, sy, mv_x, mv_y, ap.rcontrol);
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

        // Annex T §T.3: chrominance blocks dequantise against `QUANT_C`,
        // not `QUANT`, whenever Modified Quantization is active.
        let block_quant = if plane == 0 || !ap.mq { ap.quant } else { block::quant_c(ap.quant) };

        // §5.4: `INTRADC` is unconditional for an intra block, but `TCOEF`
        // (the AC part) is gated by the *same* `CBP` bit an inter block's
        // whole residual is gated by — "TCOEF is present if indicated by
        // MCBPC or CBPY". An intra block with its `CBPY`/`CBPC` bit clear
        // still reads its 8-bit `INTRADC` and nothing else.
        let cbp_bit = (cbp >> (5 - i)) & 1 == 1;
        let residual: [i32; 64] = if intra {
            let Ok(qfs) = block::decode_h263_coefficients_mq(r, true, cbp_bit, ap.mq) else {
                return false;
            };
            let qf = block::inverse_scan(&qfs);
            let dequant = block::dequantise_ranged(&qf, block_quant, true, ap.mq);
            block::inverse_transform(idct, &dequant)
        } else if coded_mb && cbp_bit {
            let Ok(qfs) = block::decode_h263_coefficients_mq(r, false, true, ap.mq) else {
                return false;
            };
            let qf = block::inverse_scan(&qfs);
            let dequant = block::dequantise_ranged(&qf, block_quant, false, ap.mq);
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
        self.plus_modes = plus::PlusModes::default();
        self.last_plus_dims = None;
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
        let (mv1, mv2, mv3) = predictors(&grid, 3, 3, 0, 0, None);
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
