//! [`vaco_codec_h264::mb::decode_slice_cavlc`] against a real `ffmpeg
//! 8.1`/`libx264` elementary stream — the bit-exact-consumption
//! measurement #419 asked for: I/P/B slices, two slices per picture,
//! Main profile (no 8x8 transform, out of scope — see `mb.rs`'s module
//! doc), `-coder cavlc`.
//!
//! # What this proves, and what it does not
//!
//! For every slice in the corpus, the macroblock loop consumes bits until
//! `CurrMbAddr` reaches the picture's own macroblock count, and at that
//! point nothing but `rbsp_trailing_bits()` remains — checked explicitly,
//! not merely inferred from "no error was returned". That is the real,
//! whole-slice bit-consumption measurement the previous two dispatches
//! established was unreachable without a macroblock layer to drive it.
//!
//! It does not prove any *decoded value* is correct — no motion vector,
//! reference index, or reconstructed pixel is ever produced or checked,
//! only that this crate reads exactly as many bits as `libx264` wrote for
//! every syntax element on the path real content exercises. See `mb.rs`'s
//! own module doc for exactly what is and is not in scope.
//!
//! Generator:
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=s=64x64:r=25:d=1" -pix_fmt yuv420p \
//!        -c:v libx264 -profile:v main -coder cavlc -bf 2 -b_strategy 0 \
//!        -sc_threshold 0 -g 10 -keyint_min 10 -slices 2 \
//!        -x264opts "slices=2:no-8x8dct" -f h264 cavlc_ipb.264
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over a fixed fixture"
)]

use vaco_bitstream::{BitReader, annexb};
use vaco_format_nalu::RbspBuf;
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

#[test]
fn every_slice_in_a_real_ipb_two_slice_cavlc_stream_consumes_exactly_its_own_bits() {
    let data: &[u8] = include_bytes!("fixtures/cavlc_ipb.264");
    let mut params = ParameterSets::new();
    let mut budget = Budget::new(Limits::default());
    let mut rbsp = RbspBuf::new();

    let mut slice_count = 0u32;
    let mut i_count = 0u32;
    let mut p_count = 0u32;
    let mut b_count = 0u32;
    let mut total_mbs = 0u32;
    let mut total_skipped = 0u32;

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
                // Peek pic_parameter_set_id the same minimal way
                // `H264Decoder::send_packet` does, to resolve SPS/PPS
                // before the full slice header parse needs them.
                let pps_id = peek_pps_id(payload, &mut budget);
                let Some((pps, sps)) = params.sps_for_pps(pps_id) else {
                    panic!("slice references PPS {pps_id} before it was seen");
                };
                let slice_header =
                    SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget).unwrap();

                match slice_header.kind {
                    vaco_parse_h264::SliceKind::I => i_count += 1,
                    vaco_parse_h264::SliceKind::P => p_count += 1,
                    vaco_parse_h264::SliceKind::B => b_count += 1,
                    _ => {}
                }

                let stats = vaco_codec_h264::mb::decode_slice_cavlc(
                    &mut reader,
                    &mut budget,
                    sps,
                    pps,
                    &slice_header,
                    None,
                )
                .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));
                assert!(
                    !more_rbsp_data(&reader),
                    "slice {slice_count}: macroblock loop ended with real data still \
                     unconsumed — {} macroblocks decoded",
                    stats.macroblock_count
                );
                total_mbs += stats.macroblock_count;
                total_skipped += stats.skipped_count;
                slice_count += 1;
            }
            _ => {}
        }
    }

    assert!(
        slice_count >= 20,
        "expected at least 20 slices, got {slice_count}"
    );
    assert!(i_count > 0, "expected at least one I slice");
    assert!(p_count > 0, "expected at least one P slice");
    assert!(b_count > 0, "expected at least one B slice");
    assert!(total_mbs > 0);
    println!(
        "slices={slice_count} I={i_count} P={p_count} B={b_count} \
         total_mbs={total_mbs} skipped={total_skipped}"
    );
}

/// The same minimal `first_mb_in_slice`/`slice_type`/`pic_parameter_set_id`
/// pre-scan `H264Decoder::send_packet` uses, duplicated here rather than
/// exposed from that module, since it is three `ue(v)` reads on a
/// throwaway reader — not worth a shared helper for one test.
fn peek_pps_id(nal: &[u8], budget: &mut Budget) -> u8 {
    let mut r = BitReader::new(nal);
    r.skip(8);
    let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r, budget);
    let _first_mb_in_slice = g.ue_v(u32::MAX).unwrap();
    let _slice_type = g.ue_v(9).unwrap();
    g.ue_v(255).unwrap() as u8
}

fn more_rbsp_data(r: &BitReader<'_>) -> bool {
    let remaining = r.remaining_bytes();
    let Some(idx) = remaining.iter().rposition(|&b| b != 0) else {
        return false;
    };
    if idx > 0 {
        return true;
    }
    remaining[idx].count_ones() > 1
}
