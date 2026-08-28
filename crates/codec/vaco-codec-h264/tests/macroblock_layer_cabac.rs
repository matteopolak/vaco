//! [`vaco_codec_h264::mb::decode_slice_cabac`] against a real `ffmpeg
//! 8.1`/`libx264 -coder cabac` elementary stream — the CABAC half of #419's
//! bit-exact-consumption measurement, I and P slices (B is out of scope for
//! this dispatch; see `mb.rs`'s own module doc for why).
//!
//! Generator:
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=s=64x64:r=25:d=1" -pix_fmt yuv420p \
//!        -c:v libx264 -profile:v main -coder cabac -bf 0 -refs 1 -g 30 \
//!        -x264opts "no-8x8dct" -f h264 cabac_ip_simple.264
//! ```
//!
//! Unlike the CAVLC macroblock loop, CABAC has an explicit
//! `end_of_slice_flag` at the end of every iteration (clause 7.3.4), so
//! there is no `more_rbsp_data`-style inference to get wrong the way the
//! CAVLC side's two real bugs were — the assertion here is instead that
//! `CabacDecoder::malformed()` stays false and that every macroblock
//! address in the picture was actually visited (`stats.macroblock_count`
//! matches `pic_width_in_mbs * pic_height_in_map_units`).

#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code over a fixed fixture")]

use vaco_bitstream::{BitReader, annexb};
use vaco_codec_cabac::CabacDecoder;
use vaco_format_nalu::RbspBuf;
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

/// Clause 7.3.2.10's `rbsp_slice_trailing_bits()`: exactly one
/// `rbsp_stop_one_bit` (value 1) padding the current byte with zeros, then
/// zero or more `cabac_zero_word`s (two all-zero bytes each). Checking
/// only `!cabac.malformed()` and the visited macroblock count (as this
/// test originally did) is not the same measurement the CAVLC test makes
/// -- CABAC's arithmetic engine can decode wrong values throughout a slice
/// and still have `end_of_slice_flag` fire at a macroblock-count-plausible
/// point purely by coincidence, since neither check depends on what was
/// actually decoded. This closes that gap the same way
/// `more_rbsp_data()` closes it for CAVLC.
fn assert_slice_ends_at_rbsp_trailing_bits(reader: &mut BitReader<'_>, slice_count: u32) {
    let pos = reader.bit_pos();
    let pad_bits = if pos % 8 == 0 { 8 } else { 8 - (pos % 8) };
    let stop_pattern = reader.get(u32::try_from(pad_bits).unwrap());
    let expected = 1u32 << (pad_bits - 1);
    assert_eq!(
        stop_pattern, expected,
        "slice {slice_count}: the bits right after end_of_slice_flag are not \
         rbsp_trailing_bits() (expected a lone stop bit then zero padding, \
         {expected:#010b}, found {stop_pattern:#010b})"
    );
    let trailer = reader.remaining_bytes();
    assert!(
        trailer.iter().all(|&b| b == 0),
        "slice {slice_count}: bytes after rbsp_trailing_bits() are not all-zero \
         cabac_zero_word padding: {trailer:?}"
    );
}


#[test]
#[ignore = "known incomplete, and this round found the earlier repros were \
measured with too weak a check: macroblock_count==total_mbs and \
!malformed() (this test's original assertions) can both hold even when \
CABAC decodes wrong values throughout, since end_of_slice_flag's fixed, \
non-adapting context can plausibly fire at a macroblock-count-correct \
point regardless of what was actually decoded before it. Adding \
assert_slice_ends_at_rbsp_trailing_bits (this round, matching the rigor \
tests/macroblock_layer.rs already applies to CAVLC via more_rbsp_data) \
found the real divergence starts at slice 0, not slice 10 as previously \
reported — slice 10 was simply the first point the old, weaker check \
happened to notice. Cross-checked address-by-address against `ffmpeg \
-debug mb_type` (letter meanings confirmed by reading \
get_type_mv_char/get_segmentation_char in FFmpeg's own \
libavcodec/mpegutils.c, not assumed): even macroblocks whose mb_type \
classification (I_4x4 vs I_16x16 vs skip vs partition shape) matches the \
reference exactly still leave the arithmetic engine a bit or two short of \
rbsp_trailing_bits() by the slice's end, which is not explained by any \
ctxIdxInc/context-table bug — every mb_type/mb_skip_flag/cbp/qp_delta/ \
intra-pred-mode table and formula reachable in an all-Intra4x4 slice has \
now been re-verified against primary text (Table 9-12's MB_TYPE_I, Table \
9-13's SKIP_P/MB_TYPE_P, Table 9-17's PREV_INTRA4X4/REM_INTRA4X4/ \
INTRA_CHROMA_PRED_MODE/QP_DELTA, Table 9-18's CBP_LUMA/CBP_CHROMA, plus \
cbf_cond_term/cbp_luma_cond_term/cbp_chroma_cond_term/mvd_abs_term) and \
all matched. That leaves residual_block_cabac itself — exercised here \
against real encoder output for the first time ever, since no prior \
measurement drove it through a real macroblock loop — as the most likely \
remaining location, not yet isolated further within this round's time \
budget. I_PCM is no longer the blocker on any corpus (see below)."]
fn every_slice_in_a_real_ip_cabac_stream_consumes_exactly_its_own_bits() {
    let data: &[u8] = include_bytes!("fixtures/cabac_ip_simple.264");
    let mut params = ParameterSets::new();
    let mut budget = Budget::new(Limits::default());
    let mut rbsp = RbspBuf::new();

    let mut slice_count = 0u32;
    let mut i_count = 0u32;
    let mut p_count = 0u32;

    for nal in annexb::nal_units(data) {
        let Some(header) = H264NalHeader::parse(nal) else { continue };
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
                match slice_header.kind {
                    vaco_parse_h264::SliceKind::I => i_count += 1,
                    vaco_parse_h264::SliceKind::P => p_count += 1,
                    _ => {}
                }

                let mut cabac = CabacDecoder::from_reader(reader);
                let stats =
                    vaco_codec_h264::mb::decode_slice_cabac(&mut cabac, &mut budget, sps, pps, &slice_header)
                        .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));

                assert!(!cabac.malformed(), "slice {slice_count}: CABAC engine reported malformed input");
                let total_mbs = sps.pic_width_in_mbs
                    * sps.pic_height_in_map_units
                    * if sps.frame_mbs_only { 1 } else { 2 };
                assert_eq!(
                    stats.macroblock_count, total_mbs,
                    "slice {slice_count}: macroblock loop stopped short of the picture's own macroblock count"
                );
                let mut trailer_reader = cabac.into_reader();
                assert_slice_ends_at_rbsp_trailing_bits(&mut trailer_reader, slice_count);
                slice_count += 1;
            }
            _ => {}
        }
    }

    println!("slices={slice_count} I={i_count} P={p_count}");
    assert!(slice_count >= 20, "expected at least 20 slices, got {slice_count}");
    assert!(i_count > 0, "expected at least one I slice");
    assert!(p_count > 0, "expected at least one P slice");
}

#[test]
#[ignore = "same finding as the ip_simple test's ignore reason: the \
earlier \"all 36 macroblocks visited, malformed() trips at the end\" \
report was measured against a too-weak assertion. With \
assert_slice_ends_at_rbsp_trailing_bits added this round, this corpus \
also fails at slice 0 — the divergence was never actually isolated to \
end-of-slice bookkeeping, that was just where the old, weaker check first \
noticed something wrong. See the ip_simple test's ignore reason for what \
this round ruled out (every mb_type/mb_skip_flag/cbp/qp_delta/intra-pred \
context table and ctxIdxInc formula reachable before residual decode) and \
what remains the leading suspect (residual_block_cabac itself)."]
fn every_slice_in_a_real_multiref_cabac_stream_consumes_exactly_its_own_bits() {
    let data: &[u8] = include_bytes!("fixtures/cabac_ip_multiref.264");
    let mut params = ParameterSets::new();
    let mut budget = Budget::new(Limits::default());
    let mut rbsp = RbspBuf::new();

    let mut slice_count = 0u32;
    let mut i_count = 0u32;
    let mut p_count = 0u32;
    let mut idc_seen = std::collections::HashSet::new();

    for nal in annexb::nal_units(data) {
        let Some(header) = H264NalHeader::parse(nal) else { continue };
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
                match slice_header.kind {
                    vaco_parse_h264::SliceKind::I => i_count += 1,
                    vaco_parse_h264::SliceKind::P => p_count += 1,
                    _ => {}
                }
                idc_seen.insert(slice_header.cabac_init_idc);

                let mut cabac = CabacDecoder::from_reader(reader);
                let stats =
                    vaco_codec_h264::mb::decode_slice_cabac(&mut cabac, &mut budget, sps, pps, &slice_header)
                        .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));

                assert!(!cabac.malformed(), "slice {slice_count}: CABAC engine reported malformed input");
                let total_mbs = sps.pic_width_in_mbs
                    * sps.pic_height_in_map_units
                    * if sps.frame_mbs_only { 1 } else { 2 };
                assert_eq!(
                    stats.macroblock_count, total_mbs,
                    "slice {slice_count}: macroblock loop stopped short of the picture's own macroblock count"
                );
                let mut trailer_reader = cabac.into_reader();
                assert_slice_ends_at_rbsp_trailing_bits(&mut trailer_reader, slice_count);
                slice_count += 1;
            }
            _ => {}
        }
    }

    println!("slices={slice_count} I={i_count} P={p_count} cabac_init_idc values seen={idc_seen:?}");
    assert!(slice_count >= 40, "expected at least 40 slices, got {slice_count}");
    assert!(i_count > 0, "expected at least one I slice");
    assert!(p_count > 0, "expected at least one P slice");
}

#[test]
#[ignore = "I_PCM is no longer this corpus's blocker: decode_slice_cabac \
now handles it (byte-align, skip 256*ChromaFormatFactor=384 raw \
pcm_byte[i] u(8) reads per the 2002 draft's clause 7.3.5 — no bit-depth \
dependency, that extension postdates this edition — then re-initialise \
just the arithmetic engine per clause 9.3.1.2, leaving context models \
untouched per 9.3.1.1 not being re-invoked). That was genuinely cheap, as \
expected: `CabacDecoder`'s own renorm() consumes exactly one bit at a \
time with no read-ahead, so into_reader() already hands back a BitReader \
positioned exactly where the raw bytes start. But this round's \
assert_slice_ends_at_rbsp_trailing_bits addition (see the ip_simple \
test's ignore reason for why the old assertions could not have caught \
this) found this corpus, like the other two, actually diverges at slice \
0 — the earlier \"decodes through slice 5\" report was real progress on a \
different, now-fixed bug (intra_chroma_pred_mode storage) but was never \
actually bit-exact even for slice 0, the weaker check just could not see \
it. Cross-checked address-by-address against `ffmpeg -debug mb_type` \
(confirmed against FFmpeg's own libavcodec/mpegutils.c source, not \
assumed): mb_type classification matches the reference throughout slice \
0 (an all-Intra4x4 slice), yet the arithmetic engine ends a bit or two \
short of rbsp_trailing_bits(). See the ip_simple test's ignore reason for \
the full list of tables and formulas this round ruled out and the \
remaining leading suspect (residual_block_cabac, never before exercised \
against real encoder output)."]
fn every_slice_in_a_real_i_only_cabac_stream_consumes_exactly_its_own_bits() {
    let data: &[u8] = include_bytes!("fixtures/cabac_i_only.264");
    let mut params = ParameterSets::new();
    let mut budget = Budget::new(Limits::default());
    let mut rbsp = RbspBuf::new();
    let mut slice_count = 0u32;

    for nal in annexb::nal_units(data) {
        let Some(header) = H264NalHeader::parse(nal) else { continue };
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
                let mut cabac = CabacDecoder::from_reader(reader);
                let stats =
                    vaco_codec_h264::mb::decode_slice_cabac(&mut cabac, &mut budget, sps, pps, &slice_header)
                        .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));
                assert!(!cabac.malformed(), "slice {slice_count}: CABAC engine reported malformed input");
                let total_mbs = sps.pic_width_in_mbs
                    * sps.pic_height_in_map_units
                    * if sps.frame_mbs_only { 1 } else { 2 };
                assert_eq!(stats.macroblock_count, total_mbs, "slice {slice_count}: short");
                let mut trailer_reader = cabac.into_reader();
                assert_slice_ends_at_rbsp_trailing_bits(&mut trailer_reader, slice_count);
                slice_count += 1;
            }
            _ => {}
        }
    }
    println!("I-only slices={slice_count}");
    assert!(slice_count >= 20);
}
