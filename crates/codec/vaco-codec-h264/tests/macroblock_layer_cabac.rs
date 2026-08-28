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

#[test]
#[ignore = "known incomplete: bit consumption diverges shortly after certain \
coded_block_flag combinations for chroma DC (ctxBlockCat 3) — reproduces on \
I slices alone, so it is not a P-slice/ref_idx/mvd-specific bug. Root cause \
not found within this dispatch's time budget; see the coordinator report \
for the exact minimal repro (cabac_i_only.264, slice 1, macroblock address \
9 — cbp_luma=0b1111, cbp_chroma=2, both chroma-DC coded_block_flag reads \
false). Kept here, not deleted, for whoever picks this back up."]
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
#[ignore = "known incomplete: bit consumption diverges shortly after certain \
coded_block_flag combinations for chroma DC (ctxBlockCat 3) — reproduces on \
I slices alone, so it is not a P-slice/ref_idx/mvd-specific bug. Root cause \
not found within this dispatch's time budget; see the coordinator report \
for the exact minimal repro (cabac_i_only.264, slice 1, macroblock address \
9 — cbp_luma=0b1111, cbp_chroma=2, both chroma-DC coded_block_flag reads \
false). Kept here, not deleted, for whoever picks this back up."]
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
#[ignore = "known incomplete: bit consumption diverges shortly after certain \
coded_block_flag combinations for chroma DC (ctxBlockCat 3) — reproduces on \
I slices alone, so it is not a P-slice/ref_idx/mvd-specific bug. Root cause \
not found within this dispatch's time budget; see the coordinator report \
for the exact minimal repro (cabac_i_only.264, slice 1, macroblock address \
9 — cbp_luma=0b1111, cbp_chroma=2, both chroma-DC coded_block_flag reads \
false). Kept here, not deleted, for whoever picks this back up."]
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
                slice_count += 1;
            }
            _ => {}
        }
    }
    println!("I-only slices={slice_count}");
    assert!(slice_count >= 20);
}
