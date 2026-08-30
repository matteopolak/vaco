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

#[test]
fn resolves_cavlc_from_a_real_x264_cavlc_stream() {
    let mut d = decoder();
    d.set_extradata(include_bytes!("fixtures/cavlc_extradata.bin")).unwrap();
    let pkt = packet(include_bytes!("fixtures/cavlc_idr_slice.264"));
    let err = d.send_packet(Some(&pkt)).unwrap_err();
    let Error::Unsupported(msg) = err else {
        panic!("expected Unsupported, got {err:?}");
    };
    assert!(msg.contains("CAVLC"), "message did not name CAVLC: {msg}");
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
    d.set_extradata(include_bytes!("fixtures/cabac_extradata.bin")).unwrap();
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
    let FrameData::Video { format, width, height, planes } = &frame.data else {
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
    let (extradata, slice) = split_extradata_and_first_slice(include_bytes!("fixtures/cabac_i_only.264"));
    d.set_extradata(&extradata).unwrap();
    let pkt = packet(&slice);
    d.send_packet(Some(&pkt)).unwrap();
    // See the sibling High-profile test's own comment: `Caps::DELAY`
    // means one packet is not enough on its own to guarantee a frame is
    // ready, so this test signals end of stream first.
    d.send_packet(None).unwrap();
    let frame = d.receive_frame().unwrap();
    let FrameData::Video { format, width, height, planes } = &frame.data else {
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
    let (extradata, slice) = split_extradata_and_first_slice(include_bytes!("fixtures/cabac_i_only.264"));
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
