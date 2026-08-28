//! The pixel-comparison corpus #420/#424's own work will need: four
//! `libx264 -coder cabac` streams, all-intra, all encoded with
//! `disable_deblocking_filter_idc = 1` -- so a from-scratch reconstruction
//! that never implements deblocking at all is nonetheless *exactly*
//! comparable against plain black-box `ffmpeg` output, since clause 8.7
//! makes every conformant decoder skip the loop filter for such a stream,
//! `ffmpeg` included.
//!
//! # Why this needed its own corpus, not the existing one
//!
//! `cabac_i_only.264` (used throughout #418's own investigation) has
//! `disable_deblocking_filter_idc == 0` on every slice -- `ffmpeg` filters
//! it. An intra-only reconstruction that (correctly, per this dispatch's
//! own scope) never applies deblocking would never match that fixture's
//! reference decode, regardless of how correct the reconstruction is. This
//! file's own fixtures exist so that gap doesn't cost #420 a round the way
//! it could have cost this one.
//!
//! # Why `no-deblock` is trustworthy here, not merely assumed
//!
//! Byte-identical decodes between a deblocked and a non-deblocked encode of
//! the same content would be equally consistent with "the flag works" and
//! "`ffmpeg` ignores the flag" -- a non-discriminating check. Confirmed the
//! discriminating case instead: encoded each source twice, once with
//! `no-deblock` and once without, decoded both with real `ffmpeg` to raw
//! YUV, and diffed:
//!
//! | fixture | QP | bytes differing (of total) | max |Δ| |
//! |---|---|---|---|
//! | `cabac_intra_oracle_flat.264` | 23 | not checked -- flat content has no block edges for deblocking to act on | -- |
//! | `cabac_intra_oracle_testsrc.264` | 33 | 2327 / 6144 | 12 |
//! | `cabac_intra_oracle_noise.264` | 33 | 2 / 6144 | 2 |
//! | `cabac_intra_oracle_multi.264` | 33 | 11450 / 30720 | 14 |
//!
//! Every non-flat fixture shows a real, substantial, nonzero difference --
//! deblocking would have visibly mattered at this QP had it been left on,
//! which is exactly what makes "it's off" a claim worth having confirmed
//! rather than assumed. `disable_deblocking_filter_idc == 1` is also
//! confirmed structurally below, by parsing each fixture's own slice
//! header with the already-tested [`SliceHeader`] parser -- not inferred
//! from the encoder invocation alone.
//!
//! # The four fixtures
//!
//! `cabac_intra_oracle_flat.264` -- one macroblock (16x16), every Y/Cb/Cr
//! sample exactly 128, the same construction argument
//! `cabac_cbp_oracle.rs` uses: any prediction mode against an already-128
//! neighbourhood (real or clause 8.3.1.2.1-substituted) predicts 128
//! again, so the reconstructed output is `128` everywhere, by hand, with
//! no reconstruction code needed to check it.
//!
//! ```text
//! ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 16x16 -i flat128_1mb.yuv -frames:v 1 \
//!        -c:v libx264 -profile:v main -coder cabac -bf 0 -refs 1 -g 1 -qp 26 \
//!        -x264opts "no-8x8dct:no-deblock" -f h264 cabac_intra_oracle_flat.264
//! ```
//!
//! `cabac_intra_oracle_testsrc.264` -- `ffmpeg`'s own `testsrc2` pattern
//! (real edges and gradients, deterministic), 64x64, one frame. `libx264`'s
//! own log: mixed `I_16x16`/`I_NxN` (25%/75%), a realistic macroblock-type
//! spread rather than a degenerate all-one-type corpus.
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=s=64x64:r=25:d=1" -pix_fmt yuv420p \
//!        -frames:v 1 testsrc64.yuv
//! ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 64x64 -i testsrc64.yuv -frames:v 1 \
//!        -c:v libx264 -profile:v main -coder cabac -bf 0 -refs 1 -g 1 -qp 36 \
//!        -x264opts "no-8x8dct:no-deblock" -f h264 cabac_intra_oracle_testsrc.264
//! ```
//!
//! `cabac_intra_oracle_noise.264` -- independent random noise
//! (`random.seed(11)`), 64x64, one frame. Almost entirely `I_NxN`
//! (`I16..4: 0.0% 0.0% 100.0%`), dense high-frequency residual -- the
//! shape that exercises `residual_block_cabac`'s coefficient decode
//! hardest.
//!
//! ```text
//! python3 -c "
//! import random
//! random.seed(11)
//! w, h = 64, 64
//! y = bytearray(random.randrange(0,256) for _ in range(w*h))
//! cw, ch = w//2, h//2
//! cb = bytearray(random.randrange(0,256) for _ in range(cw*ch))
//! cr = bytearray(random.randrange(0,256) for _ in range(cw*ch))
//! open('noise64.yuv','wb').write(bytes(y)+bytes(cb)+bytes(cr))
//! "
//! ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 64x64 -i noise64.yuv -frames:v 1 \
//!        -c:v libx264 -profile:v main -coder cabac -bf 0 -refs 1 -g 1 -qp 36 \
//!        -x264opts "no-8x8dct:no-deblock" -f h264 cabac_intra_oracle_noise.264
//! ```
//!
//! `cabac_intra_oracle_multi.264` -- the same `testsrc2` source, five
//! frames, `-g 1 -keyint_min 1` so every frame is its own IDR slice (five
//! independent all-intra slices in one file, mirroring `cabac_i_only.264`'s
//! own shape but with deblocking off).
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=s=64x64:r=25:d=1" -pix_fmt yuv420p \
//!        -frames:v 5 testsrc64_5f.yuv
//! ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 64x64 -i testsrc64_5f.yuv -frames:v 5 \
//!        -c:v libx264 -profile:v main -coder cabac -bf 0 -refs 1 -g 1 \
//!        -keyint_min 1 -qp 36 -x264opts "no-8x8dct:no-deblock" \
//!        -f h264 cabac_intra_oracle_multi.264
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]

use vaco_bitstream::{BitReader, annexb};
use vaco_format_nalu::RbspBuf;
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

/// Every slice's `disable_deblocking_filter_idc`, in file order.
fn disable_deblocking_filter_idcs(data: &[u8]) -> Vec<u32> {
    let mut params = ParameterSets::new();
    let mut budget = Budget::new(Limits::default());
    let mut rbsp = RbspBuf::new();
    let mut out = Vec::new();

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
                    SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget).unwrap();
                out.push(slice_header.disable_deblocking_filter_idc);
            }
            _ => {}
        }
    }
    out
}

macro_rules! deblock_off_test {
    ($name:ident, $fixture:literal, $expected_slices:literal) => {
        #[test]
        fn $name() {
            let data: &[u8] = include_bytes!(concat!("fixtures/", $fixture));
            let idcs = disable_deblocking_filter_idcs(data);
            assert_eq!(
                idcs.len(),
                $expected_slices,
                "unexpected slice count in {}",
                $fixture
            );
            assert!(
                idcs.iter().all(|&idc| idc == 1),
                "{}: expected disable_deblocking_filter_idc == 1 on every slice, got {idcs:?}",
                $fixture
            );
        }
    };
}

deblock_off_test!(
    flat_fixture_has_deblocking_disabled,
    "cabac_intra_oracle_flat.264",
    1
);
deblock_off_test!(
    testsrc_fixture_has_deblocking_disabled,
    "cabac_intra_oracle_testsrc.264",
    1
);
deblock_off_test!(
    noise_fixture_has_deblocking_disabled,
    "cabac_intra_oracle_noise.264",
    1
);
deblock_off_test!(
    multi_fixture_has_deblocking_disabled,
    "cabac_intra_oracle_multi.264",
    5
);
