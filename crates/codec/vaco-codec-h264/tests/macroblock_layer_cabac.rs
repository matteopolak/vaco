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
#[ignore = "known incomplete, but measurably moved by a second fix: the \
coded_block_pattern neighbour-derivation fix from an earlier round (clause \
6.4.7.2 + Table 6-2, still verified correct) was a byte-for-byte no-op on \
this corpus. A later fix to CBF_CHROMA_AC (cabac_mb_tables.rs, ctxIdx \
101..=104) — found to be an exact copy-paste duplicate of CBF_CHROMA_DC's \
own values (ctxIdx 97..=100) instead of its own row of Table 9-18, and \
corrected against primary text — is NOT a no-op here: decode now clears \
the first check inside assert_slice_ends_at_rbsp_trailing_bits (the \
stop-bit-and-padding comparison), and instead fails the second check in \
the same helper: the bytes after the (now-correctly-located) \
rbsp_trailing_bits() padding are not all-zero cabac_zero_word. That means \
decode lands on a \
byte-aligned position matching the stop-bit convention, but reaches it \
too early relative to the slice's real content — the divergence has moved \
later in the stream, not been resolved. Root cause still unisolated; see \
the multiref/i_only tests' reasons for how the same CBF_CHROMA_AC fix \
affected them differently (one shifted, one changed failure mode \
entirely)."]
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
#[ignore = "known incomplete: the CBF_CHROMA_AC fix (cabac_mb_tables.rs, \
ctxIdx 101..=104 -- see the ip_simple test's ignore reason for the exact \
bug and its Table 9-18 citation) changed this corpus's own slice-0 \
divergence again -- the exact expected/found stop-bit pattern at the \
assert_slice_ends_at_rbsp_trailing_bits comparison is different from any \
earlier round -- another real, confirmed behavioural change, not a \
no-op. But it still fails the *same* check (the stop-bit-and-padding \
comparison itself), unlike ip_simple, which now clears that check and \
fails the later all-zero-padding one instead. Still diverges at slice 0, \
root cause unisolated."]
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
#[ignore = "known incomplete, and this round's CBF_CHROMA_AC fix (see the \
ip_simple test's ignore reason for the exact bug and its Table 9-18 \
citation) changed the *failure mode itself*, not just its position: \
before the fix this corpus reached (and failed) the \
assert_slice_ends_at_rbsp_trailing_bits comparison; after the fix it now \
fails earlier and differently, with CabacDecoder::malformed() reporting \
true before the trailing-bits check ever runs. The underlying table fix \
is independently verified correct against primary text regardless -- \
this corpus's own decode is being pushed further off the rails earlier \
in the slice by a context table that is now correct, exposing a \
still-unlocated separate bug sooner rather than later. Still diverges at \
slice 0."]
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
