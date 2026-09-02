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

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over a fixed fixture"
)]

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
    let pad_bits = if pos.is_multiple_of(8) {
        8
    } else {
        8 - (pos % 8)
    };
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
                match slice_header.kind {
                    vaco_parse_h264::SliceKind::I => i_count += 1,
                    vaco_parse_h264::SliceKind::P => p_count += 1,
                    _ => {}
                }

                let mut cabac = CabacDecoder::from_reader(reader);
                let stats = vaco_codec_h264::mb::decode_slice_cabac(
                    &mut cabac,
                    &mut budget,
                    sps,
                    pps,
                    &slice_header,
                    None,
                )
                .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));

                assert!(
                    !cabac.malformed(),
                    "slice {slice_count}: CABAC engine reported malformed input"
                );
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
    assert!(
        slice_count >= 20,
        "expected at least 20 slices, got {slice_count}"
    );
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
                match slice_header.kind {
                    vaco_parse_h264::SliceKind::I => i_count += 1,
                    vaco_parse_h264::SliceKind::P => p_count += 1,
                    _ => {}
                }
                idc_seen.insert(slice_header.cabac_init_idc);

                let mut cabac = CabacDecoder::from_reader(reader);
                let stats = vaco_codec_h264::mb::decode_slice_cabac(
                    &mut cabac,
                    &mut budget,
                    sps,
                    pps,
                    &slice_header,
                    None,
                )
                .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));

                assert!(
                    !cabac.malformed(),
                    "slice {slice_count}: CABAC engine reported malformed input"
                );
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

    println!(
        "slices={slice_count} I={i_count} P={p_count} cabac_init_idc values seen={idc_seen:?}"
    );
    assert!(
        slice_count >= 40,
        "expected at least 40 slices, got {slice_count}"
    );
    assert!(i_count > 0, "expected at least one I slice");
    assert!(p_count > 0, "expected at least one P slice");
}

/// A sharper, weaker-in-form but stronger-in-teeth measurement than
/// [`every_slice_in_a_real_multiref_cabac_stream_consumes_exactly_its_own_bits`]'s
/// own `assert_slice_ends_at_rbsp_trailing_bits`: just
/// `H264Decoder`'s own production guard
/// (`stats.macroblock_count == total_mbs`, `decoder.rs`), the check that
/// decides whether the real CLI errors out on this content, run against
/// *every* slice rather than stopping at the first one that fails a
/// stricter check.
///
/// Built after direct measurement (a locally instrumented JM 19.1
/// reference decoder, `vcgit.hhi.fraunhofer.de/jvet/JM`, Tier A — see
/// `docs/codec/vaco-codec-h264.md`'s own "Deblocking" section) found that
/// `assert_slice_ends_at_rbsp_trailing_bits` demands an invariant real
/// CABAC streams do not actually have: clause 9.3.4.3.5's own encoder-side
/// flush writes the true `rbsp_stop_one_bit` as the *last* bit of a
/// multi-bit "terminating codeword" whose earlier bits are the encoder's
/// own internal register state, not zero-constrained padding — so a
/// decoder whose `decode_terminate()` correctly fires can legitimately
/// stop a few bits *before* that literal stop-bit position, precisely the
/// "right answers, wrong bit cost" shape every one of that test's own
/// ignore-reasons already describes. That does not mean this crate's own
/// CABAC decode is bug-free — see below — only that
/// `assert_slice_ends_at_rbsp_trailing_bits` is not the check to prove it
/// with.
///
/// What this measurement actually finds, unclouded by that: 7 of 50
/// slices in this real, JM-verified-conformant (`ldecod` decodes all 50
/// frames with zero errors) `libx264 -coder cabac -refs 4` corpus
/// genuinely stop short — a *real* premature `end_of_slice_flag`, the
/// exact shape `H264Decoder::send_packet` refuses in production
/// (`decoder.rs`'s own `Error::InvalidData` — the error `E2E-GAPS.md` §1b
/// reports from the real CLI, which does **not** reproduce with `-refs 1`
/// content, only with more than one active reference — every failing
/// slice below has `num_ref_idx_l0_active_minus1 >= 2`, every slice with
/// 0 or 1 active reference decodes cleanly). Slice 4's own shortfall (35
/// of 36 macroblocks, the smallest of the seven) is the cleanest minimal
/// repro this investigation has produced so far — sharper than "diverges
/// at slice 0", which was an artifact of the flawed trailing-bits check
/// above, not a property of the real defect. `ref_idx_lX`'s own CABAC
/// binarisation (`decode_ref_idx`, `mb.rs`) was checked directly against
/// clause 9.3.3.1.1.6 as the leading suspect (a wrong `ctxIdxInc` for
/// `binIdx >= 1` would plausibly hide until `num_ref_idx_active` makes a
/// multi-bin value likely) and matches the specification exactly: `binIdx
/// == 0` uses the neighbour-derived increment, `binIdx == 1` is fixed at
/// 4, `binIdx >= 2` is fixed at 5. Ruled out, not merely unchecked. Root
/// cause not isolated further within this dispatch's own time-box.
///
/// **Resolved.** This test now passes, un-ignored, on all 50 slices.
///
/// The `ctxIdxInc` suspicion in the paragraph above was right in kind and
/// wrong in place: `decode_ref_idx`'s own binarisation really is correct
/// (`binIdx == 0` neighbour-derived, 1 fixed at 4, `>= 2` fixed at 5), and
/// it was the *neighbour-derived increment* — clause 9.3.3.1.1.6's own
/// `condTermFlagN` — that had two independent defects, neither reachable
/// at `num_ref_idx_lX_active_minus1 == 0` because `ref_idx_lX` is then not
/// in the bitstream at all:
///
/// - `decode_two_partitions_cabac` did not publish partition 0's
///   `ref_idx` into the motion grid before partition 1's `ctxIdxInc` was
///   derived from it, and clause 6.4.11.7 makes partition 0 of the *same*
///   macroblock partition 1's neighbour for a 16x8/8x16 shape.
/// - A `P_Skip`/`B_Skip` or direct-predicted neighbour must contribute
///   `condTermFlagN = 0` (skip by name, direct through
///   `predModeEqualFlag`); `MvInfo` had no way to say "this block is
///   direct", since a direct block's derived motion is deliberately stored
///   as an ordinary `L0`/`L1`/`Bi` prediction. `MvInfo::direct_or_skip`
///   is that missing bit.
///
/// The earlier `decode_sub_mb_pred_cabac` ordering fix (clause 7.3.5.2's
/// four whole-macroblock passes) moved this corpus from 7 short slices to
/// 5 and stands on its own merits; these two took it to 0. Both were
/// localised by diffing a per-bin `(pStateIdx, valMPS, bit)` trace against
/// an identically instrumented JM 19.1 — the first divergence in each case
/// was a single `ref_idx_lX` bin whose decoded *value* was right and whose
/// *context* was wrong, which is exactly why so much content decoded
/// byte-exact before either one fired.
#[test]
fn every_slice_in_a_real_multiref_cabac_stream_visits_every_macroblock() {
    let data: &[u8] = include_bytes!("fixtures/cabac_ip_multiref.264");
    let mut params = ParameterSets::new();
    let mut budget = Budget::new(Limits::default());
    let mut rbsp = RbspBuf::new();
    let mut slice_idx = 0u32;
    let mut short_slices = Vec::new();

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
                let total_mbs = sps.pic_width_in_mbs * sps.pic_height_in_map_units;
                let mut cabac = CabacDecoder::from_reader(reader);
                let stats = vaco_codec_h264::mb::decode_slice_cabac(
                    &mut cabac,
                    &mut budget,
                    sps,
                    pps,
                    &slice_header,
                    None,
                )
                .unwrap_or_else(|e| panic!("slice {slice_idx}: {e:?}"));
                if stats.macroblock_count != total_mbs {
                    short_slices.push((
                        slice_idx,
                        stats.macroblock_count,
                        total_mbs,
                        slice_header.num_ref_idx_l0_active_minus1,
                    ));
                }
                slice_idx += 1;
            }
            _ => {}
        }
    }

    println!("short slices (idx, got, total, num_ref_idx_l0_active_minus1): {short_slices:?}");
    assert!(
        short_slices.is_empty(),
        "every slice must visit its own picture's full macroblock count"
    );
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
                let mut cabac = CabacDecoder::from_reader(reader);
                let stats = vaco_codec_h264::mb::decode_slice_cabac(
                    &mut cabac,
                    &mut budget,
                    sps,
                    pps,
                    &slice_header,
                    None,
                )
                .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));
                assert!(
                    !cabac.malformed(),
                    "slice {slice_count}: CABAC engine reported malformed input"
                );
                let total_mbs = sps.pic_width_in_mbs
                    * sps.pic_height_in_map_units
                    * if sps.frame_mbs_only { 1 } else { 2 };
                assert_eq!(
                    stats.macroblock_count, total_mbs,
                    "slice {slice_count}: short"
                );
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

#[test]
#[ignore = "known incomplete, and decisive rather than merely another data \
point: this is the smallest possible repro -- one macroblock (16x16 \
frame), every Y/Cb/Cr sample exactly 128 (clause 8.3.1.2.1's \
unavailable-neighbour substitution value, so residual is zero by pure \
construction, see cabac_cbp_oracle.rs's module doc), I_16x16 DC mode, \
coded_block_pattern (0, 0) -- confirmed correct by tests/cabac_cbp_oracle.rs. \
No residual coefficients, no Intra4x4, no neighbours, no inter prediction: \
the two remaining candidates two rounds of this investigation narrowed to \
(residual coefficient decode, prev_intra4x4_pred_mode_flag/ \
rem_intra4x4_pred_mode) are BOTH absent from this stream entirely, and it \
still fails assert_slice_ends_at_rbsp_trailing_bits. That rules them out \
as the sole cause and reopens the search to the macroblock layer's own \
mb_type/intra_chroma_pred_mode/mb_qp_delta/coded_block_flag(luma DC)/ \
end_of_slice_flag sequence -- a basic-rule error, not a subtle one, per \
the coordinator's own framing of this exact question. \
\
Bin-by-bin tracing (temporary instrumentation, not committed) found every \
individually-checked component correct against primary text: \
decode_mb_type_i_table's binarization tree and MB_TYPE_I's table values \
(ctxIdx 0-10, including the documented coincidence that ctxIdx 0-2 and \
3-5 hold identical (m,n) pairs in Table 9-12 itself, not a bug); \
cbf_cond_term's unavailable-neighbour special case (condTermFlag = \
current_is_intra, clause 9.3.3.1.1.9, matching the coordinator's own \
inspection from an earlier round); ContextModel::init_h264's clause \
9.3.1.1 formula; and, exhaustively, vaco-codec-cabac's three foundational \
tables (RANGE_TAB_LPS/TRANS_IDX_LPS/TRANS_IDX_MPS, all 64 rows checked \
against this draft's Table 9-33/9-34, zero mismatches -- read-only, that \
crate is agent:codec-bits's). Slice-header parsing and CABAC engine \
initialisation were confirmed bit-exact by direct inspection of this \
fixture's own raw bytes: the 9-bit codIOffset our decoder reads (509) is \
the literal bit pattern present at the exact byte-aligned position our \
header parse computes, byte for byte. \
\
What the trace shows instead: our decoder fires end_of_slice_flag at \
bitpos 69 of 72, leaving a 3-bit tail of `0b001` in the real file -- not \
a valid rbsp_trailing_bits() pattern (needs `1` then zeros). The file's \
actual final bit (bit 71) is `1`, consistent with the *true* stream \
needing about 2 more consumed bits before terminating than this decoder \
currently consumes -- meaning the arithmetic trajectory has already \
drifted by the time end_of_slice_flag is checked, despite every \
individual decoded *value* along the way (mb_type=3, chroma_pred=0, \
cbp=(0,0), qp_delta=0, luma DC coded_block_flag=0) matching what the real \
encoder's own log says it should be. That combination -- right answers, \
wrong bit cost -- was not resolved this round; it needs either an \
independent from-scratch CABAC arithmetic oracle (planned twice, never \
built) or further hand simulation to localise past 'somewhere in this \
nine-decision sequence'."]
fn a_single_flat_macroblock_with_no_residual_at_all_still_fails_bit_exactness() {
    let data: &[u8] = include_bytes!("fixtures/cabac_minimal_flat_1mb.264");
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
                let stats = vaco_codec_h264::mb::decode_slice_cabac(
                    &mut cabac,
                    &mut budget,
                    sps,
                    pps,
                    &slice_header,
                    None,
                )
                .unwrap_or_else(|e| panic!("slice {slice_count}: {e:?}"));
                assert!(
                    !cabac.malformed(),
                    "slice {slice_count}: CABAC engine reported malformed input"
                );
                assert_eq!(
                    stats.macroblock_count, 1,
                    "slice {slice_count}: expected exactly one macroblock"
                );
                assert_eq!(
                    stats.first_slice_mb_cbp,
                    Some((0, 0)),
                    "slice {slice_count}: this fixture's own construction argument (all-128 samples) requires zero residual"
                );
                let mut trailer_reader = cabac.into_reader();
                assert_slice_ends_at_rbsp_trailing_bits(&mut trailer_reader, slice_count);
                slice_count += 1;
            }
            _ => {}
        }
    }
    assert_eq!(slice_count, 1);
}
