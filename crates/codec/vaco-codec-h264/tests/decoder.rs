//! [`H264Decoder`] against real `ffmpeg 8.1`-encoded elementary streams —
//! exactly the corpus axis the dispatch asked for (`-coder cavlc`/`-coder
//! cabac`), embedded rather than generated at test time (D7/plan 13 §1b:
//! the test needs no `ffmpeg` on `PATH`, matching `vaco-parse-h264`'s own
//! `tests/reference.rs` precedent).
//!
//! # What this proves, and what it does not
//!
//! `H264Decoder::send_packet` now decodes a real CABAC I-slice end to
//! end — a real [`vaco_frame::Frame`] comes back from `receive_frame`,
//! not just a resolved `entropy_coding_mode_flag`. Byte-exactness against
//! `ffmpeg` is `crate::reconstruct`'s own, much more thorough corpus
//! (`cabac_i_only.264`, `cabac_ip_simple.264`, chroma checked
//! per-plane); this file's job is narrower and different: proving the
//! *production* [`Decoder`] trait surface — `send_packet`/`receive_frame`/
//! `set_extradata`/`flush`, with real AVCC-vs-Annex-B framing detection —
//! actually reaches that already-verified reconstruction, which is
//! exactly the gap this crate shipped with for a long time (see
//! `crate::decoder`'s own module doc).
//!
//! CAVLC is still refused, honestly — see `crate::decoder`'s own doc for
//! why (the entropy layer alone verifies bit consumption; motion vectors
//! and residual coefficients are never captured).
//!
//! `cabac_idr_slice.264`/`cabac_extradata.bin` (High profile,
//! `transform_8x8_mode` set — real x264 default-preset output) are kept
//! for [`resolves_cavlc_from_a_real_x264_cavlc_stream`]'s CAVLC-side
//! sibling and for the entropy-mode-resolution boundary alone: a real
//! *reconstruction* now needs a fixture inside this crate's own
//! implemented scope, so the actual decode tests below reuse
//! `cabac_i_only.264` (`crate::reconstruct`'s own #418 corpus,
//! `Intra_4x4`-only, no 8x8 transform) instead, splitting its own
//! in-band SPS/PPS from its first IDR slice at test time rather than
//! keeping a third pair of fixture files for the same content.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]

use vaco_bitstream::annexb;
use vaco_codec_core::Decoder;
use vaco_codec_h264::H264Decoder;
use vaco_core::Error;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn decoder() -> H264Decoder {
    H264Decoder::new(Limits::default())
}

/// Wraps a bare NAL unit's bytes (no start code, no length prefix — what
/// `fixtures/*_idr_slice.264` store) in an Annex-B start code, matching
/// this crate's Annex-B extradata fixtures and the framing a real
/// elementary stream actually carries.
fn packet(nal_bytes: &[u8]) -> Packet {
    let mut framed = vec![0u8, 0, 0, 1];
    framed.extend_from_slice(nal_bytes);
    let mut budget = Budget::new(Limits::default());
    Packet::from_slice(&mut budget, &framed).unwrap()
}

/// Splits a real Annex-B elementary stream into a start-code-prefixed
/// `(SPS + PPS)` extradata blob and the bare bytes of its first primary
/// coded slice — everything [`decoder`]/[`packet`] need from one `.264`
/// file, without keeping a separately captured `_extradata.bin` alongside
/// it.
fn split_extradata_and_first_slice(data: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut extradata = Vec::new();
    for nal in annexb::nal_units(data) {
        match nal.first().map(|b| b & 0x1F) {
            Some(7 | 8) => {
                extradata.extend_from_slice(&[0, 0, 0, 1]);
                extradata.extend_from_slice(nal);
            }
            Some(1 | 5) => return (extradata, nal.to_vec()),
            _ => {}
        }
    }
    panic!("fixture has no slice NAL");
}

/// [`split_extradata_and_first_slice`]'s sibling for a fixture whose *n*th
/// primary-coded-slice NAL (0-indexed, `first_mb_in_slice`/`slice_type`
/// order in the bitstream -- not display order) is the one a test needs,
/// alongside the same in-band SPS/PPS extradata. `cabac_ipbb.264`'s own
/// bitstream order is I, P, B, B, P, B, B, ... (a real `libx264 -bf 2
/// -refs 1` IBBP stream), confirmed by parsing every slice header's own
/// `first_mb_in_slice`/`slice_type` at fixture-authoring time -- slice
/// index 2 is the fixture's first real B slice.
fn nth_slice_and_extradata(data: &[u8], n: usize) -> (Vec<u8>, Vec<u8>) {
    let mut extradata = Vec::new();
    let mut seen = 0usize;
    for nal in annexb::nal_units(data) {
        match nal.first().map(|b| b & 0x1F) {
            Some(7 | 8) => {
                extradata.extend_from_slice(&[0, 0, 0, 1]);
                extradata.extend_from_slice(nal);
            }
            Some(1 | 5) => {
                if seen == n {
                    return (extradata, nal.to_vec());
                }
                seen += 1;
            }
            _ => {}
        }
    }
    panic!("fixture has fewer than {} slice NALs", n + 1);
}

#[test]
fn resolves_cavlc_from_a_real_x264_cavlc_stream() {
    let mut d = decoder();
    d.set_extradata(include_bytes!("fixtures/cavlc_extradata.bin"))
        .unwrap();
    let pkt = packet(include_bytes!("fixtures/cavlc_idr_slice.264"));
    let err = d.send_packet(Some(&pkt)).unwrap_err();
    let Error::Unsupported(msg) = err else {
        panic!("expected Unsupported, got {err:?}");
    };
    assert!(msg.contains("CAVLC"), "message did not name CAVLC: {msg}");
}

/// The B-slice gate is gone: a real `libx264` IBBP CABAC stream's first B
/// slice now decodes instead of being refused.
///
/// This test used to assert the *refusal*. The gate went in when every I
/// and P frame of a real `-bf 2 -refs 1` stream matched plain `ffmpeg`
/// byte for byte and every B frame carried a small residual (max
/// per-sample delta 3-5 over 1-2% of samples) --
/// `planning/AGENT-CONSTRAINTS.md`'s "registered-but-wrong is worse than
/// absent". The residual was a clause 8.7.2.1 boundary-strength input
/// (`MvInfo::ref_idx_l1`), with two `ctxIdxInc` defects behind it; the
/// end-to-end byte-exactness measurement that justified lifting the gate
/// lives in `crate::mb::decode_slice_cabac`'s own comment and
/// `docs/codec/vaco-codec-h264.md`, because it needs the `ffmpeg` binary
/// and 480 encoded clips and so cannot live in a unit test.
///
/// What this test still pins down is narrower and worth keeping: the
/// fixture's I and P slices decode, *and* its first B slice decodes too --
/// so a future regression that re-refuses B slices (or that breaks I/P
/// while "fixing" B) fails here rather than only in a sweep nobody runs.
#[test]
fn a_real_b_slice_now_decodes() {
    let data = include_bytes!("fixtures/cabac_ipbb.264");
    let (extradata, i_slice) = nth_slice_and_extradata(data, 0);
    let (_, p_slice) = nth_slice_and_extradata(data, 1);
    let (_, b_slice) = nth_slice_and_extradata(data, 2);

    let mut d = decoder();
    d.set_extradata(&extradata).unwrap();

    let i_pkt = packet(&i_slice);
    d.send_packet(Some(&i_pkt)).unwrap();
    let _ = d.receive_frame();

    // `unwrap`, not `expect` (this file's own `#![allow]` covers the
    // former, not the latter).
    let p_pkt = packet(&p_slice);
    d.send_packet(Some(&p_pkt)).unwrap();

    let b_pkt = packet(&b_slice);
    d.send_packet(Some(&b_pkt)).unwrap();
}

#[test]
fn a_high_profile_8x8_transform_stream_now_decodes() {
    // `cabac_idr_slice.264`/`cabac_extradata.bin`: real x264 output that
    // enables `transform_size_8x8_flag` (High profile's own default) --
    // this crate's CABAC path now supports `Intra_8x8`/the 8x8 transform
    // (`MbKind::Intra8x8`, `crate::intra::predict_intra8x8`,
    // `crate::dequant::dequant_8x8`, `ContextCategory::Luma8x8`), so this
    // real High-profile IDR slice now decodes instead of being refused.
    // This test used to assert the *refusal* -- `planning/AGENT-CONSTRAINTS.md`'s
    // own "never pin the absence of something the project is building"
    // rule names this exact shape of test as the one that costs the
    // agent who *fixes* the gap a debugging session; asserting the
    // mapping (a real decode) instead, not the emptiness.
    let mut d = decoder();
    d.set_extradata(include_bytes!("fixtures/cabac_extradata.bin"))
        .unwrap();
    let pkt = packet(include_bytes!("fixtures/cabac_idr_slice.264"));
    d.send_packet(Some(&pkt)).unwrap();
    // `H264Decoder` now declares `Caps::DELAY` (B-slice output reordering
    // needs it -- `crate::decoder`'s own module doc) and holds a decoded
    // picture until a future one proves it is safe to emit, or until end
    // of stream says nothing is coming that could reorder ahead of it.
    // One packet alone is never enough to prove that on its own, so the
    // real `Decoder` protocol's own EOF signal is what this test needs to
    // send before a frame is guaranteed available.
    d.send_packet(None).unwrap();
    let frame = d.receive_frame().unwrap();
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        panic!("expected a video frame, got {:?}", frame.data);
    };
    assert_eq!(format.name(), "yuv420p");
    assert!(*width > 0 && *height > 0, "width={width} height={height}");
    assert_eq!(planes.len(), 3, "yuv420p is three planes");
    let luma = &planes[0].data;
    assert!(
        luma.as_slice().windows(2).any(|w| w[0] != w[1]),
        "luma plane is perfectly flat -- looks like it was never written"
    );
}

#[test]
fn decodes_a_real_frame_from_a_real_x264_cabac_stream() {
    let mut d = decoder();
    let (extradata, slice) =
        split_extradata_and_first_slice(include_bytes!("fixtures/cabac_i_only.264"));
    d.set_extradata(&extradata).unwrap();
    let pkt = packet(&slice);
    d.send_packet(Some(&pkt)).unwrap();
    // See the sibling High-profile test's own comment: `Caps::DELAY`
    // means one packet is not enough on its own to guarantee a frame is
    // ready, so this test signals end of stream first.
    d.send_packet(None).unwrap();
    let frame = d.receive_frame().unwrap();
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        panic!("expected a video frame, got {:?}", frame.data);
    };
    assert_eq!(format.name(), "yuv420p");
    assert!(*width > 0 && *height > 0, "width={width} height={height}");
    assert_eq!(planes.len(), 3, "yuv420p is three planes");
    // A real decode, not a flat placeholder: some sample must differ from
    // its neighbour, i.e. this is not just an all-grey buffer nobody
    // actually wrote into.
    let luma = &planes[0].data;
    assert!(
        luma.as_slice().windows(2).any(|w| w[0] != w[1]),
        "luma plane is perfectly flat -- looks like it was never written"
    );
    // No second frame -- this stream only ever had one, and end of stream
    // was already signalled above, so the machine is draining/drained
    // rather than waiting for more input.
    assert!(matches!(d.receive_frame(), Err(Error::Eof)));
}

#[test]
fn a_packet_referencing_unseen_parameter_sets_is_skipped_not_erred() {
    // No `set_extradata` call at all: the slice references a PPS this
    // decoder has never seen. `H264Parser::push_access_unit`'s own
    // documented contract is "skip, don't error" for exactly this case
    // (a stream joined mid-flight, which is legal and common) --
    // `H264Decoder` reuses that contract rather than re-deriving a
    // stricter one of its own.
    let mut d = decoder();
    let pkt = packet(include_bytes!("fixtures/cavlc_idr_slice.264"));
    d.send_packet(Some(&pkt)).unwrap();
    assert!(matches!(d.receive_frame(), Err(Error::NeedMoreInput)));
}

#[test]
fn eof_drains_cleanly() {
    let mut d = decoder();
    d.send_packet(None).unwrap();
    assert!(matches!(d.receive_frame(), Err(Error::Eof)));
}

#[test]
fn flush_does_not_panic_and_leaves_the_decoder_usable() {
    let mut d = decoder();
    let (extradata, slice) =
        split_extradata_and_first_slice(include_bytes!("fixtures/cabac_i_only.264"));
    d.set_extradata(&extradata).unwrap();
    d.flush();
    let pkt = packet(&slice);
    // The active parameter sets are a `vaco-parse-h264` concept independent
    // of this decoder's own (now-cleared) DPB/output-queue state, so a
    // slice referencing an already-seen PPS still decodes after a flush.
    d.send_packet(Some(&pkt)).unwrap();
    // `Caps::DELAY` (see the other tests' own comments): signal end of
    // stream so the one decoded picture is guaranteed to have left the
    // reorder buffer.
    d.send_packet(None).unwrap();
    assert!(d.receive_frame().is_ok());
}

/// Finding 22a (`planning/INTERFACE-GAPS.md`): the VUI already reached
/// `CodecParameters::color` via `vaco_parse_h264::params`, which is what
/// `vaco-probe -show_streams` reads, but nothing wrote `Frame::color`, so
/// `-vf showinfo`-style tooling downstream of a real decode saw the
/// *default* `ColorInfo` on every H.264 frame regardless of what the
/// stream actually signalled.
///
/// `fixtures/vui_bt709.264` is real `ffmpeg 9.0.1`/`libx264` output
/// (`-x264-params colorprim=bt709:transfer=bt709:colormatrix=bt709:
/// fullrange=off`), captured once and embedded (D7/plan 13 §1b). Measured
/// against the same file with real `ffprobe`: `color_range=tv
/// color_space=bt709 color_transfer=bt709 color_primaries=bt709` — the
/// exact four values this test asserts on the decoded `Frame`, not just
/// "the box is present".
#[test]
fn a_real_bt709_stream_stamps_its_measured_colour_onto_the_decoded_frame() {
    let mut d = decoder();
    let (extradata, slice) =
        split_extradata_and_first_slice(include_bytes!("fixtures/vui_bt709.264"));
    d.set_extradata(&extradata).unwrap();
    let pkt = packet(&slice);
    d.send_packet(Some(&pkt)).unwrap();
    d.send_packet(None).unwrap();
    let frame = d.receive_frame().unwrap();
    assert_eq!(frame.color.primaries, vaco_color::ColorPrimaries::Bt709);
    assert_eq!(
        frame.color.transfer,
        vaco_color::TransferCharacteristic::Bt709
    );
    assert_eq!(frame.color.matrix, vaco_color::MatrixCoefficients::Bt709);
    assert_eq!(frame.color.range, vaco_color::ColorRange::Limited);
}

/// The regression case on the other side of the fix above: a stream with
/// no VUI at all (`cabac_i_only.264`, real `ffmpeg`-measured
/// `color_range=unknown color_space=unknown color_transfer=unknown
/// color_primaries=unknown`) must still land on §E.2.1's inference
/// rule — `Unspecified` primaries/transfer/matrix/range, *not*
/// `ChromaLocation::Unspecified`: `Sps::color_info`'s own doc records that
/// an absent `chroma_loc_info_present_flag` infers type 0 (left) per
/// §7.4.2.1.1, which is a real, spec-mandated value rather than "nothing
/// was signalled" — so this is deliberately not `ColorInfo::default()`,
/// caught by first writing that assertion and having it fail exactly this
/// way.
#[test]
fn a_stream_with_no_vui_still_decodes_to_the_unspecified_default() {
    let mut d = decoder();
    let (extradata, slice) =
        split_extradata_and_first_slice(include_bytes!("fixtures/cabac_i_only.264"));
    d.set_extradata(&extradata).unwrap();
    let pkt = packet(&slice);
    d.send_packet(Some(&pkt)).unwrap();
    d.send_packet(None).unwrap();
    let frame = d.receive_frame().unwrap();
    assert_eq!(
        frame.color.primaries,
        vaco_color::ColorPrimaries::Unspecified
    );
    assert_eq!(
        frame.color.transfer,
        vaco_color::TransferCharacteristic::Unspecified
    );
    assert_eq!(
        frame.color.matrix,
        vaco_color::MatrixCoefficients::Unspecified
    );
    assert_eq!(frame.color.range, vaco_color::ColorRange::Unspecified);
    assert_eq!(
        frame.color.chroma_location,
        vaco_color::ChromaLocation::Left
    );
}

/// [`split_extradata_and_first_slice`]'s sibling for an access unit whose
/// SEI messages matter: SPS/PPS still go to `extradata`, but every other
/// NAL up to and including the first primary-coded-slice NAL (SEI
/// included) is kept together in one packet, matching how a real demuxer
/// hands a whole access unit to `send_packet` in one call rather than
/// splitting SEI out the way [`split_extradata_and_first_slice`]'s
/// slice-only fixtures need it kept separate for.
fn split_extradata_and_first_access_unit(data: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut extradata = Vec::new();
    let mut access_unit = Vec::new();
    for nal in annexb::nal_units(data) {
        match nal.first().map(|b| b & 0x1F) {
            Some(7 | 8) => {
                extradata.extend_from_slice(&[0, 0, 0, 1]);
                extradata.extend_from_slice(nal);
            }
            Some(1 | 5) => {
                access_unit.extend_from_slice(&[0, 0, 0, 1]);
                access_unit.extend_from_slice(nal);
                return (extradata, access_unit);
            }
            _ => {
                access_unit.extend_from_slice(&[0, 0, 0, 1]);
                access_unit.extend_from_slice(nal);
            }
        }
    }
    panic!("fixture has no slice NAL");
}

/// Finding 22b (`planning/INTERFACE-GAPS.md`): `vaco_parse_h264::sei`
/// parses `MasteringDisplay`/`ContentLightLevel` correctly, and nothing in
/// this crate ever read either — so `FrameSideData::MasteringDisplay`/
/// `ContentLightLevel` (real types in `vaco-frame`, with a working
/// consumer in `vaco-filter-mm`'s `sidedata` filter) had zero producers.
///
/// `fixtures/hdr10_mastering_display.264` is real `ffmpeg 9.0.1`/`libx264`
/// output (`-x264-params mastering-display=G(13250,34500)B(7500,3000)
/// R(34000,16000)WP(15635,16450)L(10000000,1):cll=1000,400`), captured
/// once and embedded. Measured with real `ffprobe`'s own `-show_frames`
/// `[SIDE_DATA]` block on the same file: `red_x=34000/50000
/// red_y=16000/50000 green_x=13250/50000 green_y=34500/50000
/// blue_x=7500/50000 blue_y=3000/50000 white_point_x=15635/50000
/// white_point_y=16450/50000 min_luminance=1/10000
/// max_luminance=10000000/10000` and `max_content=1000 max_average=400` —
/// the exact values this test asserts on the decoded `Frame`'s side data,
/// not just "the SEI is present".
#[test]
fn a_real_hdr10_stream_attaches_the_measured_mastering_display_and_cll() {
    let mut d = decoder();
    let (extradata, access_unit) = split_extradata_and_first_access_unit(include_bytes!(
        "fixtures/hdr10_mastering_display.264"
    ));
    d.set_extradata(&extradata).unwrap();
    let mut budget = Budget::new(Limits::default());
    let pkt = Packet::from_slice(&mut budget, &access_unit).unwrap();
    d.send_packet(Some(&pkt)).unwrap();
    d.send_packet(None).unwrap();
    let frame = d.receive_frame().unwrap();

    let Some(mastering) = frame.side_data.iter().find_map(|sd| match sd {
        vaco_frame::FrameSideData::MasteringDisplay(m) => Some(m.as_ref()),
        _ => None,
    }) else {
        panic!("frame should carry MasteringDisplay side data");
    };
    // red, green, blue — see `vaco_frame::MasteringDisplay`'s own doc for
    // why this is not the bitstream's green/blue/red order.
    assert_eq!(
        mastering.primaries[0][0],
        vaco_core::Rational::new(34_000, 50_000),
        "red_x"
    );
    assert_eq!(
        mastering.primaries[0][1],
        vaco_core::Rational::new(16_000, 50_000),
        "red_y"
    );
    assert_eq!(
        mastering.primaries[1][0],
        vaco_core::Rational::new(13_250, 50_000),
        "green_x"
    );
    assert_eq!(
        mastering.primaries[1][1],
        vaco_core::Rational::new(34_500, 50_000),
        "green_y"
    );
    assert_eq!(
        mastering.primaries[2][0],
        vaco_core::Rational::new(7_500, 50_000),
        "blue_x"
    );
    assert_eq!(
        mastering.primaries[2][1],
        vaco_core::Rational::new(3_000, 50_000),
        "blue_y"
    );
    assert_eq!(
        mastering.white_point[0],
        vaco_core::Rational::new(15_635, 50_000),
        "white_point_x"
    );
    assert_eq!(
        mastering.white_point[1],
        vaco_core::Rational::new(16_450, 50_000),
        "white_point_y"
    );
    assert_eq!(
        mastering.min_luminance,
        vaco_core::Rational::new(1, 10_000),
        "min_luminance"
    );
    assert_eq!(
        mastering.max_luminance,
        vaco_core::Rational::new(10_000_000, 10_000),
        "max_luminance"
    );

    let Some(cll) = frame.side_data.iter().find_map(|sd| match sd {
        vaco_frame::FrameSideData::ContentLightLevel { max_cll, max_fall } => {
            Some((*max_cll, *max_fall))
        }
        _ => None,
    }) else {
        panic!("frame should carry ContentLightLevel side data");
    };
    assert_eq!(cll, (1000, 400));
}
