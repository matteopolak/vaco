//! #420's own seam: composing [`crate::intra`]'s prediction,
//! [`crate::dequant`]'s scaling, [`crate::scan`]'s inverse zig-zag, and
//! [`vaco_codec_dsp_idct::h264`]'s transforms into actual reconstructed
//! luma samples, macroblock by macroblock, across a whole picture --
//! clause 8.5's own ordered steps for `Intra_16x16` (8.5.2) and
//! `Intra_4x4` (clause 8.3.1's own per-block interleaved
//! predict-then-reconstruct order, via [`PictureBuffer`]'s real,
//! multi-macroblock neighbour propagation).
//!
//! # What this module does
//!
//! [`reconstruct_picture_luma`] walks a decoded
//! [`crate::mb::SliceStats::macroblocks`] list in raster (decode) order,
//! reconstructing each `Intra_16x16` or `Intra_4x4` macroblock's luma
//! plane into a shared [`PictureBuffer`], so every macroblock after the
//! first can draw real, already-reconstructed neighbour samples from
//! whichever macroblock is actually adjacent to it -- not the
//! always-unavailable case [`reconstruct_intra16x16_luma`] alone (still
//! used internally, per macroblock) is limited to on its own.
//!
//! Confirmed byte-exact against real `ffmpeg` on every corpus tried,
//! including mixed `Intra_16x16`/`Intra_4x4` content with real
//! cross-macroblock neighbour propagation between two `Intra_16x16`
//! macroblocks (`cabac_intra_oracle_noise.264`, `_testsrc.264`,
//! `_multi.264`), and, as of this round, `cabac_i_only.264` (#418's own
//! corpus) too -- against a fair reference for it: this crate implements
//! no deblocking filter, so it is compared against `ffmpeg -skip_loop_filter
//! all` rather than `ffmpeg`'s default (deblocked) decode. Against
//! `ffmpeg`'s real, deblocked output, `cabac_i_only.264` still shows a
//! large, quantified mismatch -- but that mismatch is now settled as
//! entirely the missing loop filter, not a decode defect (see
//! `cabac_i_only_matches_ffmpeg_with_deblocking_skipped` and
//! `cabac_i_only_reconstructs_without_error_and_mostly_matches_ffmpeg`'s
//! own doc comments for the full account).
//!
//! # What this module does not implement
//!
//! - **Chroma reconstruction.** Every fixture reconstructed so far has
//!   `CodedBlockPatternChroma == 0` (zero chroma residual), so chroma
//!   reconstruction is exactly [`crate::intra`]'s own already-verified
//!   prediction output with nothing added -- clause 8.5.3's chroma
//!   residual path ([`crate::scan::inverse_scan_chroma_dc`]'s
//!   already-tested raster-not-zigzag scan) is written but not yet
//!   composed into a `predC + r` sum here, since nothing on hand
//!   exercises it, and [`reconstruct_picture_luma`] returns luma only.
//! - **`I_PCM`.** Refused with an error rather than attempted -- not
//!   exercised by any fixture on hand.
//! - **Anything beyond one slice == one whole picture.** Every fixture
//!   this module has been run against has exactly this shape (confirmed
//!   structurally, `first_mb_in_slice == 0` on every slice); real
//!   multi-slice-per-picture neighbour-availability handling (clause
//!   6.4.8's "different slice" rule) is not implemented.

#![allow(
    dead_code,
    reason = "exercised by this module's own tests; not yet wired into vaco-codec-h264's own public decode/receive_frame surface"
)]

use vaco_codec_dsp_idct::h264::idct4x4;

use crate::dequant::{dequant_4x4, dequant_luma_dc_4x4};
use crate::intra::{Neighbours4, Neighbours16, predict_intra4x4, predict_intra16x16};
use crate::mb::{MbResidual, MbSummary, blk_xy};
use crate::scan::{build_luma_ac_block, inverse_scan_luma_dc};

/// Clause 8.5.1/8.5.2, `Intra_16x16` luma only: predict, then add clause
/// 8.5.2's own per-4x4-block dequantised-and-transformed residual, then
/// `Clip1` (eq. (8-245); this crate's 8-bit-only scope makes `Clip1`
/// exactly `clamp(0, 255)`).
///
/// `mode` is `Intra16x16PredMode` (Table 8-3), `neighbours` the
/// already-resolved luma neighbour state [`crate::intra`] takes, `qpy`
/// this macroblock's own luma QP (clause 7.4.5, eq. (7-23) -- e.g.
/// [`crate::mb::SliceStats::first_slice_mb_qpy`] for the first macroblock
/// of a real decode), and `residual` this macroblock's own decoded
/// coefficients, still in scan order (e.g.
/// [`crate::mb::SliceStats::first_slice_mb_residual`]).
#[must_use]
#[allow(
    clippy::indexing_slicing,
    reason = "xO/yO are 4*blk_xy(0..16) in 0..12, i/j in 0..4, so every index into the fixed 16x16/16-element arrays below is provably in range -- not bitstream-derived"
)]
pub(crate) fn reconstruct_intra16x16_luma(
    mode: u8,
    neighbours: Neighbours16,
    qpy: i32,
    residual: &MbResidual,
) -> [[u8; 16]; 16] {
    let pred = predict_intra16x16(mode, neighbours);

    // Clause 8.5.2 step 1: the macroblock-wide luma DC transform, shared
    // by all 16 AC blocks below.
    let dc_raw = inverse_scan_luma_dc(residual.luma_dc.as_ref());
    let dc_y = dequant_luma_dc_4x4(&dc_raw, qpy);

    let mut out = pred;
    for blk in 0..16u32 {
        // Clause 8.5.2 step 2a, Figure 8-6: dcY's own (i, j) for this
        // luma4x4BlkIdx is addressed by the same z-order `blk_xy` this
        // crate's neighbour/context derivations already use for the
        // block's own spatial position -- dcY[i][j] at row i = the
        // block's y-in-blocks, column j = the block's x-in-blocks.
        let (bx, by) = blk_xy(blk);
        let dc_val = dc_y[(by * 4 + bx) as usize];

        let ac = residual.luma_ac.get(blk as usize).and_then(Option::as_ref);
        let c = build_luma_ac_block(dc_val, ac);
        // dc_already_scaled = true: position (0, 0) already went through
        // dequant_luma_dc_4x4's own scaling above (clause 8.5.6, not
        // 8.5.8) and must pass through this step untouched.
        let d = dequant_4x4(&c, qpy, true);
        let r = idct4x4(&d);

        let x_o = (bx * 4) as usize;
        let y_o = (by * 4) as usize;
        for i in 0..4usize {
            for j in 0..4usize {
                let p = i32::from(pred[y_o + i][x_o + j]);
                let sum = p + r[i * 4 + j];
                out[y_o + i][x_o + j] = sum.clamp(0, 255) as u8;
            }
        }
    }
    out
}

/// A whole picture's own luma sample buffer, plus the per-4x4-block
/// "has this been reconstructed yet" bitmap `Intra_4x4`'s own neighbour
/// derivation needs -- clause 6.4.7.3/6.4.8's combined effect, for frame
/// (non-MBAFF) pictures, reduces to exactly this: a global 4x4-block grid
/// addressed in absolute picture coordinates, where a position is
/// available iff its owning macroblock has already been fully
/// reconstructed, *or* it is the macroblock currently being reconstructed
/// and this specific 4x4 block was reconstructed earlier in *this*
/// macroblock's own z-order (clause 6.4.3) -- which is exactly what
/// catches clause 8.3.1.2's own "`x` is greater than 3 and `luma4x4BlkIdx`
/// is equal to 3 or 11" special case for free, rather than as a
/// hardcoded exception: block 3's top-right diagonal neighbour and block
/// 11's both resolve, via ordinary `blk_xy` z-order, to a *later* block
/// index in the same macroblock -- "not yet decoded", the general rule,
/// not a special one.
struct PictureBuffer {
    mbs_wide: u32,
    mbs_high: u32,
    /// Row-major, `mbs_wide * 16` wide.
    luma: Vec<u8>,
    /// One per global 4x4 luma block position, row-major,
    /// `mbs_wide * 4` wide.
    decoded_4x4: Vec<bool>,
}

impl PictureBuffer {
    fn new(mbs_wide: u32, mbs_high: u32) -> Self {
        let w = (mbs_wide * 16) as usize;
        let h = (mbs_high * 16) as usize;
        let bw = (mbs_wide * 4) as usize;
        let bh = (mbs_high * 4) as usize;
        Self {
            mbs_wide,
            mbs_high,
            luma: vec![128u8; w.saturating_mul(h)],
            decoded_4x4: vec![false; bw.saturating_mul(bh)],
        }
    }

    const fn width(&self) -> u32 {
        self.mbs_wide * 16
    }

    const fn height(&self) -> u32 {
        self.mbs_high * 16
    }

    /// `true` iff picture pixel `(x, y)` is in bounds *and* its owning 4x4
    /// block has already been written -- the single availability test
    /// every `Intra_4x4` neighbour sample and every `Intra_16x16`
    /// cross-macroblock neighbour row/column both reduce to.
    #[allow(
        clippy::integer_division,
        reason = "x/4, y/4 converts a pixel position to its owning 4x4 block position -- exact by construction (4x4 blocks), not a precision-loss bug"
    )]
    fn available(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return false;
        };
        if x >= self.width() || y >= self.height() {
            return false;
        }
        let (bx, by) = (x / 4, y / 4);
        let bw = self.mbs_wide * 4;
        self.decoded_4x4
            .get((by * bw + bx) as usize)
            .copied()
            .unwrap_or(false)
    }

    fn pixel(&self, x: i32, y: i32) -> u8 {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return 0;
        };
        if x >= self.width() || y >= self.height() {
            return 0;
        }
        self.luma
            .get((y * self.width() + x) as usize)
            .copied()
            .unwrap_or(0)
    }

    fn set_pixel(&mut self, x: u32, y: u32, v: u8) {
        let w = self.width();
        if let Some(slot) = self.luma.get_mut((y * w + x) as usize) {
            *slot = v;
        }
    }

    /// Marks the 4x4 block at picture-pixel upper-left `(x, y)` as
    /// reconstructed -- called once that block's own samples are already
    /// written, so a *later* block's neighbour lookup (same macroblock or
    /// a macroblock decoded after this one) sees it as available.
    #[allow(
        clippy::integer_division,
        reason = "x/4, y/4 converts a pixel position to its owning 4x4 block position -- exact by construction (4x4 blocks), not a precision-loss bug"
    )]
    fn mark_block_decoded(&mut self, x: u32, y: u32) {
        let bw = self.mbs_wide * 4;
        let (bx, by) = (x / 4, y / 4);
        if let Some(slot) = self.decoded_4x4.get_mut((by * bw + bx) as usize) {
            *slot = true;
        }
    }

    fn write_block4(&mut self, x: u32, y: u32, block: [[u8; 4]; 4]) {
        for (i, row) in block.iter().enumerate() {
            for (j, &v) in row.iter().enumerate() {
                self.set_pixel(x + j as u32, y + i as u32, v);
            }
        }
        self.mark_block_decoded(x, y);
    }
}

/// Builds one `Intra_4x4` block's [`crate::intra::Neighbours4`] from real
/// picture state -- clause 8.3.1.2's own 13 neighbouring samples, plus
/// the substitution rule for an unavailable top-right when `p[3,-1]` is
/// itself available.
fn intra4x4_neighbours(buf: &PictureBuffer, x: i32, y: i32) -> Neighbours4 {
    let top_available = (0..4).all(|dx| buf.available(x + dx, y - 1));
    let top = core::array::from_fn(|dx| buf.pixel(x + dx as i32, y - 1));
    let left_available = (0..4).all(|dy| buf.available(x - 1, y + dy));
    let left = core::array::from_fn(|dy| buf.pixel(x - 1, y + dy as i32));
    let corner_available = buf.available(x - 1, y - 1);
    let corner = if corner_available {
        buf.pixel(x - 1, y - 1)
    } else {
        0
    };

    let top_right_available = (4..8).all(|dx| buf.available(x + dx, y - 1));
    let top_right = if top_right_available {
        core::array::from_fn(|dx| buf.pixel(x + 4 + dx as i32, y - 1))
    } else if top_available {
        // Clause 8.3.1.2's own substitution: p[3,-1]'s value stands in for
        // all four, and they are treated as available from here on.
        [top[3]; 4]
    } else {
        [0; 4]
    };

    Neighbours4 {
        top_available,
        top,
        top_right,
        left_available,
        left,
        corner,
    }
}

/// Reconstructs one whole `Intra_4x4` macroblock's luma plane into `buf`
/// at macroblock origin `(mb_x, mb_y)` (macroblock units) -- clause
/// 8.3.1's own per-block interleaved predict/reconstruct order (the NOTE
/// under clause 8.3.1.2: "Each block is assumed to be constructed into a
/// frame prior to decoding of the next block"), not
/// [`reconstruct_intra16x16_luma`]'s predict-the-whole-macroblock-then-add
/// shape.
fn reconstruct_intra4x4_mb(
    buf: &mut PictureBuffer,
    mb_x: u32,
    mb_y: u32,
    qpy: i32,
    residual: &MbResidual,
) {
    for blk in 0..16u32 {
        let (bx, by) = blk_xy(blk);
        let x = mb_x * 16 + bx * 4;
        let y = mb_y * 16 + by * 4;
        let n = intra4x4_neighbours(buf, x as i32, y as i32);
        let mode = residual
            .intra4x4_pred_mode
            .get(blk as usize)
            .copied()
            .unwrap_or(2);
        let pred = predict_intra4x4(mode, n);

        // Clause 8.5.4's plain 16-position scan (no DC/AC split at all --
        // that split is `Intra_16x16`-only): position (0, 0) is a normal
        // coefficient like any other, so `dequant_4x4`'s own
        // `dc_already_scaled = false`.
        let ac = residual.luma_ac.get(blk as usize).and_then(Option::as_ref);
        let c = inverse_scan_luma_dc(ac);
        let d = dequant_4x4(&c, qpy, false);
        let r = idct4x4(&d);

        let mut block = [[0u8; 4]; 4];
        for (i, row) in block.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                let p = i32::from(pred.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0));
                let sum = p + r.get(i * 4 + j).copied().unwrap_or(0);
                *v = sum.clamp(0, 255) as u8;
            }
        }
        buf.write_block4(x, y, block);
    }
}

/// Reconstructs a whole picture's luma plane from one CABAC I-slice's
/// [`crate::mb::SliceStats::macroblocks`] -- `Intra_16x16` and `Intra_4x4`
/// macroblocks, in decode (raster) order, each drawing its own real
/// neighbour samples from macroblocks already reconstructed earlier in
/// that same order. `I_PCM` is refused (`Err`) rather than silently
/// producing wrong samples -- not attempted this round, and this crate's
/// oracle corpora do not use it.
///
/// Chroma is not reconstructed (see this module's own scope note) --
/// only the luma plane is returned, `mbs_wide * 16` wide by
/// `mbs_high * 16` tall, row-major.
///
/// # Errors
///
/// [`vaco_core::Error::Unsupported`] if any macroblock is `I_PCM` or
/// otherwise not one of `Intra_16x16`/`Intra_4x4` (e.g. an inter
/// macroblock reaching this function at all would itself be a scope
/// violation this crate's CABAC decode should have already refused
/// earlier).
pub(crate) fn reconstruct_picture_luma(
    macroblocks: &[MbSummary],
    mbs_wide: u32,
    mbs_high: u32,
) -> vaco_core::Result<Vec<u8>> {
    let mut buf = PictureBuffer::new(mbs_wide, mbs_high);
    for mb in macroblocks {
        if mb.is_ipcm {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-h264: I_PCM picture reconstruction is not implemented",
            ));
        }
        if mb.skipped {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-h264: skipped-macroblock reconstruction is not implemented (unreachable for I slices)",
            ));
        }
        if mb.is_intra16x16 {
            let x = mb.mb_x * 16;
            let y = mb.mb_y * 16;
            let top_available = (0..16).all(|dx| buf.available(x as i32 + dx, y as i32 - 1));
            let top = core::array::from_fn(|dx| buf.pixel(x as i32 + dx as i32, y as i32 - 1));
            let left_available = (0..16).all(|dy| buf.available(x as i32 - 1, y as i32 + dy));
            let left = core::array::from_fn(|dy| buf.pixel(x as i32 - 1, y as i32 + dy as i32));
            let neighbours = Neighbours16 {
                top_available,
                top,
                left_available,
                left,
            };
            let block = reconstruct_intra16x16_luma(
                mb.intra16x16_pred_mode,
                neighbours,
                mb.qpy,
                &mb.residual,
            );
            for (i, row) in block.iter().enumerate() {
                for (j, &v) in row.iter().enumerate() {
                    buf.set_pixel(x + j as u32, y + i as u32, v);
                }
            }
            for blk in 0..16u32 {
                let (bx, by) = blk_xy(blk);
                buf.mark_block_decoded(x + bx * 4, y + by * 4);
            }
        } else if mb.is_intra4x4 {
            reconstruct_intra4x4_mb(&mut buf, mb.mb_x, mb.mb_y, mb.qpy, &mb.residual);
        } else {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-h264: picture reconstruction only implements Intra_16x16/Intra_4x4 macroblocks",
            ));
        }
    }
    Ok(buf.luma)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::cabac_residual::CabacResidual;

    fn unavailable() -> Neighbours16 {
        Neighbours16 {
            top_available: false,
            top: [0; 16],
            left_available: false,
            left: [0; 16],
        }
    }

    /// No residual at all reduces to plain prediction -- the flat
    /// fixture's own case, re-checked here at this module's own level
    /// rather than only in `intra.rs`.
    #[test]
    fn zero_residual_is_pure_prediction() {
        let out = reconstruct_intra16x16_luma(2, unavailable(), 26, &MbResidual::default());
        assert!(out.iter().all(|row| row.iter().all(|&v| v == 128)));
    }

    /// A single luma DC coefficient, no AC at all, must shift every
    /// sample in the macroblock by the same amount (clause 8.5.2's dcY is
    /// shared by all 16 blocks; with no AC, `idct4x4` of a DC-only input
    /// is a flat block per clause 8.5.12.2's own separable sum).
    #[test]
    fn dc_only_residual_shifts_every_sample_uniformly() {
        let mut residual = MbResidual::default();
        residual.luma_dc = Some(CabacResidual {
            levels: vec![10],
            positions: vec![0],
        });
        let out = reconstruct_intra16x16_luma(2, unavailable(), 26, &residual);
        let first = out[0][0];
        assert!(
            out.iter().all(|row| row.iter().all(|&v| v == first)),
            "a DC-only residual must reconstruct to a single flat value, got {out:?}"
        );
        // Not a no-op: some real shift away from the pure-prediction 128
        // must have happened, or this test could not tell a DC add from a
        // dropped residual.
        assert_ne!(first, 128);
    }

    /// The first non-flat pixel comparison this investigation has had:
    /// `cabac_intra_oracle_gradient.264` (one macroblock, forced
    /// `Intra_16x16` DC with real, nonzero luma residual -- `partitions=none`
    /// alone was not enough to stop `libx264` choosing `Intra_4x4` on a
    /// smooth gradient; `preset=ultrafast` was, per its own restricted
    /// intra analysis) decodes end to end through the *live* CABAC path
    /// (`crate::mb::decode_slice_cabac`) into this module's
    /// `reconstruct_intra16x16_luma`, and is checked against `ffmpeg
    /// 8.1`'s own real decode of the same file, saved once as
    /// `cabac_intra_oracle_gradient_ref.yuv`
    /// (`ffmpeg -i ... -pix_fmt yuv420p -f rawvideo`). Chroma is not
    /// reconstructed here (this module's own scope line) -- confirmed
    /// separately, off the same live decode, that this fixture's own
    /// `CodedBlockPatternChroma == 0`, so the reference file's chroma
    /// planes (also checked below, directly, not just assumed flat) never
    /// needed anything past this crate's already-verified chroma DC
    /// prediction.
    ///
    /// Isolates residual correctness from prediction correctness on
    /// purpose: `Intra16x16PredMode` is DC, already covered by the flat
    /// fixture with zero residual -- the only new thing this test
    /// exercises is dequantisation + the inverse transform, on real,
    /// nonzero coefficients decoded off the actual bitstream.
    #[test]
    fn gradient_fixture_luma_matches_real_ffmpeg_byte_for_byte() {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_gradient.264");
        let reference: &[u8] =
            include_bytes!("../tests/fixtures/cabac_intra_oracle_gradient_ref.yuv");
        assert_eq!(
            reference.len(),
            384,
            "reference fixture: expected 16x16 4:2:0 (256 + 64 + 64 bytes)"
        );

        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();
        let mut stats = None;

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else {
                continue;
            };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget)
                            .unwrap();
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let s = crate::mb::decode_slice_cabac(
                        &mut cabac,
                        &mut budget,
                        sps,
                        pps,
                        &slice_header,
                    )
                    .unwrap_or_else(|e| {
                        panic!("gradient fixture: decode_slice_cabac failed: {e:?}")
                    });
                    assert!(
                        !cabac.malformed(),
                        "gradient fixture: CABAC engine reported malformed input"
                    );
                    stats = Some(s);
                }
                _ => {}
            }
        }

        let stats = stats.expect("gradient fixture: no slice NAL found");
        assert_eq!(
            stats.macroblock_count, 1,
            "gradient fixture: expected exactly one macroblock"
        );
        assert_eq!(
            stats.first_slice_mb_intra16x16_pred_mode,
            Some(2),
            "gradient fixture: expected Intra16x16PredMode == 2 (DC), same mode as the flat fixture"
        );
        let (cbp_luma, cbp_chroma) = stats
            .first_slice_mb_cbp
            .expect("gradient fixture: no coded_block_pattern recorded");
        assert_ne!(
            cbp_luma, 0,
            "gradient fixture: expected nonzero luma CBP -- this fixture exists to exercise residual decode"
        );
        assert_eq!(
            cbp_chroma, 0,
            "gradient fixture: expected zero chroma CBP -- this test does not reconstruct chroma residual"
        );
        let qpy = stats
            .first_slice_mb_qpy
            .expect("gradient fixture: no QPY recorded");
        let residual = stats
            .first_slice_mb_residual
            .expect("gradient fixture: no residual recorded");

        let luma = reconstruct_intra16x16_luma(2, unavailable(), qpy, &residual);
        let mut mismatches = Vec::new();
        for y in 0..16usize {
            for x in 0..16usize {
                let got = luma[y][x];
                let want = reference[y * 16 + x];
                if got != want {
                    mismatches.push((x, y, got, want));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "gradient fixture: luma reconstruction diverges from real ffmpeg at {} of 256 samples \
             (first few (x, y, got, want): {:?})",
            mismatches.len(),
            &mismatches[..mismatches.len().min(8)]
        );

        // Chroma is not reconstructed by this module (see its own doc) --
        // confirmed, not assumed, that the reference decode's chroma is
        // flat 128 throughout, matching this crate's already-verified
        // "zero CBP, DC prediction, no neighbours" case from the flat
        // fixture, so no chroma residual path is owed to this comparison.
        let chroma = &reference[256..384];
        assert!(
            chroma.iter().all(|&v| v == 128),
            "gradient fixture: expected flat 128 chroma in the reference decode (CodedBlockPatternChroma == 0)"
        );
    }

    /// Decodes every CABAC I-slice in `data` (each one, for this crate's
    /// oracle corpus, its own complete standalone picture -- confirmed
    /// structurally for every fixture used below, not assumed: every
    /// slice has `first_mb_in_slice == 0`) and reconstructs each one's
    /// luma plane. Panics (via `.expect`/`.unwrap`, this module's own
    /// test-code allow) on any parse or decode failure -- there is no
    /// "partial" result worth returning to a fixture-comparison test.
    fn decode_all_frames_luma(data: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
        decode_all_frames_luma_tolerant(data)
            .into_iter()
            .enumerate()
            .map(|(i, r)| r.unwrap_or_else(|e| panic!("frame {i}: {e}")))
            .collect()
    }

    /// Same as [`decode_all_frames_luma`], but never panics -- one
    /// slice's own decode/reconstruction failure (e.g. `malformed()`)
    /// becomes an `Err` for that one frame instead of aborting the whole
    /// file, so a corpus with one bad frame among many still reports
    /// every other frame's own comparison. Used where a fixture is not
    /// (yet) expected to decode cleanly end to end -- see
    /// `cabac_i_only_reconstructs_without_error_and_mostly_matches_ffmpeg`.
    fn decode_all_frames_luma_tolerant(data: &[u8]) -> Vec<Result<(u32, u32, Vec<u8>), String>> {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();
        let mut frames = Vec::new();

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else {
                continue;
            };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget)
                            .unwrap();
                    assert_eq!(
                        slice_header.first_mb_in_slice, 0,
                        "this helper assumes one slice == one whole picture"
                    );
                    let mbs_wide = sps.pic_width_in_mbs;
                    let mbs_high =
                        sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let result = crate::mb::decode_slice_cabac(
                        &mut cabac,
                        &mut budget,
                        sps,
                        pps,
                        &slice_header,
                    )
                    .map_err(|e| format!("decode_slice_cabac failed: {e:?}"))
                    .and_then(|stats| {
                        if cabac.malformed() {
                            return Err("CABAC engine reported malformed input".to_owned());
                        }
                        reconstruct_picture_luma(&stats.macroblocks, mbs_wide, mbs_high)
                            .map(|luma| (mbs_wide, mbs_high, luma))
                            .map_err(|e| format!("reconstruct_picture_luma failed: {e:?}"))
                    });
                    frames.push(result);
                }
                _ => {}
            }
        }
        frames
    }

    /// Compares one reconstructed luma plane against its reference,
    /// asserting a byte-exact match and reporting the first differing
    /// macroblock (not just the first differing byte) if it does not
    /// match -- the instrument this investigation has never had before
    /// this round.
    fn assert_luma_matches(
        name: &str,
        frame_idx: usize,
        ours: &[u8],
        reference: &[u8],
        mbs_wide: u32,
    ) {
        assert_eq!(
            ours.len(),
            reference.len(),
            "{name} frame {frame_idx}: luma plane size mismatch"
        );
        let mut first_mismatch = None;
        let mut mismatches = 0usize;
        for (i, (&a, &b)) in ours.iter().zip(reference.iter()).enumerate() {
            if a != b {
                mismatches += 1;
                if first_mismatch.is_none() {
                    let width = (mbs_wide * 16) as usize;
                    let (x, y) = (i % width, i / width);
                    let (mb_x, mb_y) = (x / 16, y / 16);
                    first_mismatch = Some((x, y, mb_x, mb_y, a, b));
                }
            }
        }
        assert!(
            mismatches == 0,
            "{name} frame {frame_idx}: {mismatches} of {} luma samples differ from ffmpeg; \
             first mismatch at pixel {:?} (x, y, mb_x, mb_y, ours, ffmpeg)",
            ours.len(),
            first_mismatch
        );
    }

    /// `cabac_intra_oracle_testsrc.264`: mixed `Intra_16x16`/`Intra_4x4`
    /// content (libx264's own log: 25%/75%), no deblocking -- the first
    /// clean (unconfounded by the loop filter this crate does not
    /// implement) multi-macroblock comparison exercising *both*
    /// prediction families and real cross-macroblock neighbour
    /// propagation in the same picture. Now byte-exact after two fixes:
    /// (1) `decode_residual_cabac`'s same-macroblock `coded_block_flag`
    /// neighbour lookups no longer route through `grids.mb_info_at`,
    /// which is (correctly) `None` until `set_mb_info` runs at the very
    /// end of `decode_macroblock_cabac`; and (2) luma DC's own
    /// `coded_block_flag` (`ctxBlockCat == 0`, one flag per macroblock)
    /// no longer aliases luma4x4BlkIdx 0's own AC `coded_block_flag`
    /// slot in the shared per-4x4-block `cbf_luma` grid -- that aliasing
    /// let an `Intra_16x16` macroblock's *AC block 0* flag silently
    /// stand in for its *DC* flag whenever a later `Intra_16x16`
    /// neighbour asked for it, invisible until the first
    /// `Intra_16x16`-to-`Intra_16x16` macroblock adjacency in a decode
    /// (macroblock (2, 1)'s own left neighbour, macroblock (1, 1), here).
    /// See `CabacGrids::cbf_luma_dc` for the dedicated, macroblock-
    /// granular storage that replaced the aliased lookup.
    #[test]
    fn testsrc_fixture_matches_ffmpeg_byte_for_byte() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_testsrc.264");
        let reference: &[u8] =
            include_bytes!("../tests/fixtures/cabac_intra_oracle_testsrc_ref.yuv");
        let frames = decode_all_frames_luma(data);
        assert_eq!(frames.len(), 1);
        let (mbs_wide, _mbs_high, luma) = &frames[0];
        assert_luma_matches("testsrc", 0, luma, &reference[..luma.len()], *mbs_wide);
    }

    /// `cabac_intra_oracle_noise.264`: independent random noise, almost
    /// entirely `Intra_4x4` (libx264's own log: `I16..4: 0.0% 0.0%
    /// 100.0%`), no deblocking -- the densest residual-decode stress case
    /// this crate's oracle corpus has, now reconstructable end to end.
    #[test]
    fn noise_fixture_matches_ffmpeg_byte_for_byte() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_noise.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_noise_ref.yuv");
        let frames = decode_all_frames_luma(data);
        assert_eq!(frames.len(), 1);
        let (mbs_wide, _mbs_high, luma) = &frames[0];
        assert_luma_matches("noise", 0, luma, &reference[..luma.len()], *mbs_wide);
    }

    /// `cabac_intra_oracle_multi.264`: five independent IDR pictures, one
    /// slice each, no deblocking -- checks the "each slice is decoded
    /// with entirely fresh neighbour state" assumption
    /// [`decode_all_frames_luma`] leans on holds across multiple pictures
    /// in one file, not just within a single one.
    #[test]
    fn multi_fixture_matches_ffmpeg_byte_for_byte_on_every_frame() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_multi.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_multi_ref.yuv");
        let frames = decode_all_frames_luma(data);
        assert_eq!(frames.len(), 5, "expected five independent IDR pictures");
        let frame_stride = 64 * 64 + 2 * 32 * 32;
        for (idx, (mbs_wide, _mbs_high, luma)) in frames.iter().enumerate() {
            let ref_frame = &reference[idx * frame_stride..idx * frame_stride + luma.len()];
            assert_luma_matches("multi", idx, luma, ref_frame, *mbs_wide);
        }
    }

    /// `cabac_i_only.264`: #418's own corpus, all `Intra_4x4`, 25
    /// independent IDR pictures -- **not** encoded with deblocking
    /// disabled (`disable_deblocking_filter_idc == 0` on every slice,
    /// confirmed structurally, not assumed), and this crate implements no
    /// deblocking filter at all, so a byte-exact match against `ffmpeg`'s
    /// real (deblocked) decode is not the achievable bar here the way it
    /// is for the four `no-deblock` fixtures above.
    ///
    /// The test below this one settles the question that used to hang
    /// over this corpus: decoded instead against `ffmpeg -skip_loop_filter
    /// all` (deblocking disabled at decode time, not re-encoded), this
    /// crate's own output is **byte-exact**, all 25 frames -- the
    /// previously-reported 63.77% mismatch against the real (deblocked)
    /// reference below was entirely the missing loop filter, not a
    /// decode defect. This crate's `Intra_4x4` reconstruction has no
    /// remaining known defect on this corpus at all.
    #[test]
    fn cabac_i_only_matches_ffmpeg_with_deblocking_skipped() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only_nodeblock_ref.yuv");
        let frames = decode_all_frames_luma(data);
        assert_eq!(frames.len(), 25);
        let frame_stride = 64 * 64 + 2 * 32 * 32;
        for (idx, (mbs_wide, _mbs_high, luma)) in frames.iter().enumerate() {
            let ref_frame = &reference[idx * frame_stride..idx * frame_stride + luma.len()];
            assert_luma_matches("cabac_i_only (no deblock)", idx, luma, ref_frame, *mbs_wide);
        }
    }

    /// The same corpus against `ffmpeg`'s own real, deblocked decode --
    /// kept as a standing, quantified record of the confound this crate's
    /// own missing loop filter costs, now that
    /// `cabac_i_only_matches_ffmpeg_with_deblocking_skipped` (above) has
    /// settled that the gap is entirely that filter and nothing else.
    /// Still `#[ignore]`d because implementing deblocking is out of this
    /// scope, not because anything here is in doubt.
    #[test]
    #[ignore = "not a decode defect: settled this round by comparing against ffmpeg with \
        -skip_loop_filter all instead of its default (deblocked) decode -- \
        cabac_i_only_matches_ffmpeg_with_deblocking_skipped is byte-exact, all 25 frames, 0 \
        mismatches, so this test's own 63.77% match against the real deblocked reference is \
        fully and exactly explained by this crate's well-known, out-of-scope missing loop \
        filter. Kept ignored (not deleted) as a standing quantified record of that filter's own \
        cost on real content, not as an open question. Does not retire \
        assert_slice_ends_at_rbsp_trailing_bits on its own -- that assertion's own remaining \
        relevance is a distinct question, now worth re-examining given the reconstruction \
        pipeline is independently confirmed correct on five corpora (noise, testsrc, multi, \
        gradient, and now cabac_i_only itself against a fair reference) -- but nothing here \
        argues for weakening it, and it was not touched."]
    fn cabac_i_only_reconstructs_without_error_and_mostly_matches_ffmpeg() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only_ref.yuv");
        let frames = decode_all_frames_luma_tolerant(data);
        assert_eq!(frames.len(), 25);
        let frame_stride = 64 * 64 + 2 * 32 * 32;
        let mut total = 0usize;
        let mut total_mismatch = 0usize;
        let mut failed_frames = 0usize;
        for (idx, frame) in frames.iter().enumerate() {
            let (mbs_wide, _mbs_high, luma) = match frame {
                Ok(f) => f,
                Err(e) => {
                    failed_frames += 1;
                    eprintln!("cabac_i_only frame {idx}: decode/reconstruct failed: {e}");
                    continue;
                }
            };
            let ref_frame = &reference[idx * frame_stride..idx * frame_stride + luma.len()];
            let width = (*mbs_wide * 16) as usize;
            let mut frame_mismatch = 0usize;
            let mut first = None;
            for (i, (&a, &b)) in luma.iter().zip(ref_frame.iter()).enumerate() {
                total += 1;
                if a != b {
                    total_mismatch += 1;
                    frame_mismatch += 1;
                    if first.is_none() {
                        let (x, y) = (i % width, i / width);
                        first = Some((x, y, x / 16, y / 16, a, b));
                    }
                }
            }
            eprintln!(
                "cabac_i_only frame {idx}: {frame_mismatch} / {} luma samples differ; first mismatch (x, y, mb_x, mb_y, ours, ffmpeg) = {:?}",
                luma.len(),
                first
            );
        }
        eprintln!(
            "cabac_i_only: {failed_frames} / {} frames failed to decode/reconstruct at all",
            frames.len()
        );
        let match_fraction = if total == 0 {
            0.0
        } else {
            1.0 - (total_mismatch as f64 / total as f64)
        };
        eprintln!(
            "cabac_i_only overall (successfully-decoded frames only): {total_mismatch} / {total} luma samples differ ({:.2}% match)",
            match_fraction * 100.0
        );
        assert_eq!(
            failed_frames,
            0,
            "cabac_i_only: {failed_frames} of {} frames failed to decode/reconstruct at all -- see stderr above",
            frames.len()
        );
        assert!(
            match_fraction >= 0.60,
            "cabac_i_only: only {:.2}% of luma samples match ffmpeg's real (deblocked) decode -- \
             ~63.77% (measured this round, once cabac_i_only_matches_ffmpeg_with_deblocking_skipped \
             confirmed the decode itself is byte-exact against a fair, un-deblocked reference) is \
             the expected floor here; a drop below it means a real decode regression, not just \
             the well-known missing loop filter",
            match_fraction * 100.0
        );
    }
}
