//! #420's own seam: composing [`crate::intra`]'s prediction,
//! [`crate::dequant`]'s scaling, [`crate::scan`]'s inverse zig-zag, and
//! [`vaco_codec_dsp_idct::h264`]'s transforms into actual reconstructed
//! luma samples -- clause 8.5.2's own ordered steps for an `Intra_16x16`
//! macroblock, end to end.
//!
//! # Scope: `Intra_16x16` luma only, one macroblock
//!
//! This module reconstructs exactly what a single `Intra_16x16`
//! macroblock's luma plane needs (clause 8.5.1's `Clip1(pred + r)`, driven
//! by clause 8.5.2's DC-then-16-AC-blocks sequence). It does **not**
//! implement:
//!
//! - **Chroma reconstruction.** Every fixture this module is checked
//!   against so far has `CodedBlockPatternChroma == 0` (zero chroma
//!   residual), so chroma reconstruction is exactly [`crate::intra`]'s own
//!   already-verified prediction output with nothing added -- clause
//!   8.5.3's chroma residual path (`chroma4x4BlkIdx`'s own simpler raster
//!   block order, [`crate::scan::inverse_scan_chroma_dc`]'s already-tested
//!   raster-not-zigzag scan) is written but not yet composed into a
//!   `predC + r` sum here, since nothing on hand exercises it.
//! - **`Intra_4x4`.** A different macroblock prediction mode entirely
//!   (clause 8.3.1, clause 8.5's own per-4x4-block interleaved
//!   predict-then-reconstruct order, not this module's DC-then-16-blocks
//!   shape) -- #420's next piece, not this one's.
//! - **Multi-macroblock neighbour propagation.** [`crate::intra`]'s own
//!   `Neighbours16`/`NeighboursChroma` still take already-resolved
//!   availability and sample values; a real reconstructed-picture sample
//!   buffer is not built here. Every macroblock this module has been
//!   run against is macroblock 0 of its own slice, where clause 6.4.8 has
//!   nowhere to look for a neighbour anyway -- "unavailable" is correct by
//!   construction, not a simplification this module gets away with by
//!   accident.

#![allow(
    dead_code,
    reason = "exercised by this module's own tests, including the gradient-fixture end-to-end reconstruction; not yet wired into mb.rs's macroblock loop for the general multi-macroblock case"
)]

use vaco_codec_dsp_idct::h264::idct4x4;

use crate::dequant::{dequant_4x4, dequant_luma_dc_4x4};
use crate::intra::{Neighbours16, predict_intra16x16};
use crate::mb::{MbResidual, blk_xy};
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
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
}
