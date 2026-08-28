#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code over a fixed fixture")]

use vaco_bitstream::{BitReader, annexb};
use vaco_format_nalu::RbspBuf;
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

#[test]
fn simple_ip_single_ref_single_slice() {
    let data: &[u8] = include_bytes!("fixtures/cavlc_ip_simple.264");
    let mut params = ParameterSets::new();
    let mut budget = Budget::new(Limits::default());
    let mut rbsp = RbspBuf::new();
    let mut slice_count = 0u32;

    for nal in annexb::nal_units(data) {
        let Some(header) = H264NalHeader::parse(nal) else {
            continue;
        };
        match header.nal_unit_type {
            NalUnitType::Sps => {
                rbsp.fill(nal, &mut budget).unwrap();
                let r = params.add_sps(rbsp.as_slice(), &mut budget);
                if let Err(e) = r {
                    println!("add_sps error: {e:?}");
                }
            }
            NalUnitType::Pps => {
                rbsp.fill(nal, &mut budget).unwrap();
                let r = params.add_pps(rbsp.as_slice(), &mut budget);
                if let Err(e) = r {
                    println!("add_pps error: {e:?}");
                }
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
                println!(
                    "slice {slice_count}: kind={:?} n0={} first_mb={}",
                    slice_header.kind,
                    slice_header.num_ref_idx_l0_active_minus1,
                    slice_header.first_mb_in_slice
                );
                let stats =
                    vaco_codec_h264::mb::decode_slice_cavlc(&mut reader, &mut budget, sps, pps, &slice_header)
                        .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));
                println!(
                    "  -> mbs={} skipped={}",
                    stats.macroblock_count, stats.skipped_count
                );
                slice_count += 1;
            }
            _ => {}
        }
    }
    println!("total slices decoded: {slice_count}");
    assert!(slice_count > 1, "expected at least an I and a P slice");
}
