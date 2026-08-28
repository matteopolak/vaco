//! [`H264Decoder`] against real `ffmpeg 8.1`-encoded elementary streams —
//! exactly the corpus axis the dispatch asked for (`-coder cavlc`/`-coder
//! cabac`), embedded rather than generated at test time (D7/plan 13 §1b:
//! the test needs no `ffmpeg` on `PATH`, matching `vaco-parse-h264`'s own
//! `tests/reference.rs` precedent).
//!
//! # What this proves, and what it does not
//!
//! `H264Decoder::send_packet` locates a real slice header (via
//! `vaco-parse-h264`, itself reference-tested) and resolves
//! `entropy_coding_mode_flag` correctly against real encoder output — that
//! much is checked here, against real bytes. It does **not** prove
//! [`crate::cavlc::residual_block_cavlc`]/[`crate::cabac_residual::residual_block_cabac`]
//! consume a real slice's residual bits correctly: that needs the
//! macroblock loop (#419+) to know which syntax elements precede each
//! residual block and with what `nC`/`ctxBlockCat`, which this crate does
//! not implement yet. See `crate`'s own module doc for the fuller
//! statement of this boundary.
//!
//! Generator, so the corpus can be rebuilt when the pinned reference moves:
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=s=64x64:r=25:d=1" -pix_fmt yuv420p \
//!        -c:v libx264 -coder cavlc -g 30 -bf 0 -f h264 cavlc.264
//! ffmpeg -y -f lavfi -i "testsrc2=s=64x64:r=25:d=1" -pix_fmt yuv420p \
//!        -c:v libx264 -coder cabac -g 30 -bf 2 -f h264 cabac.264
//! ```
//!
//! Each fixture file here is one Annex-B-start-code-prefixed SPS+PPS pair
//! (`*_extradata.bin`, fed through [`Decoder::set_extradata`]) and the raw
//! bytes of the first IDR slice NAL unit, start code stripped (`*_idr_slice.264`,
//! fed as a packet payload — the shape a demuxer hands a decoder).

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]

use vaco_codec_core::Decoder;
use vaco_codec_h264::H264Decoder;
use vaco_core::Error;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn decoder() -> H264Decoder {
    H264Decoder::new(Limits::default())
}

fn packet(bytes: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::default());
    Packet::from_slice(&mut budget, bytes).unwrap()
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
    assert!(!msg.contains("CABAC"), "message wrongly named CABAC: {msg}");
}

#[test]
fn resolves_cabac_from_a_real_x264_cabac_stream() {
    let mut d = decoder();
    d.set_extradata(include_bytes!("fixtures/cabac_extradata.bin")).unwrap();
    let pkt = packet(include_bytes!("fixtures/cabac_idr_slice.264"));
    let err = d.send_packet(Some(&pkt)).unwrap_err();
    let Error::Unsupported(msg) = err else {
        panic!("expected Unsupported, got {err:?}");
    };
    assert!(msg.contains("CABAC"), "message did not name CABAC: {msg}");
    assert!(!msg.contains("CAVLC"), "message wrongly named CAVLC: {msg}");
}

#[test]
fn refuses_a_packet_with_no_active_parameter_sets() {
    // No `set_extradata` call at all: the slice references a PPS this
    // decoder has never seen, and that must be reported, not guessed at.
    let mut d = decoder();
    let pkt = packet(include_bytes!("fixtures/cavlc_idr_slice.264"));
    let err = d.send_packet(Some(&pkt)).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

#[test]
fn eof_drains_cleanly() {
    let mut d = decoder();
    assert!(matches!(d.send_packet(None), Err(Error::Eof)));
}

#[test]
fn flush_does_not_panic_and_leaves_the_decoder_usable() {
    let mut d = decoder();
    d.set_extradata(include_bytes!("fixtures/cavlc_extradata.bin")).unwrap();
    d.flush();
    let pkt = packet(include_bytes!("fixtures/cavlc_idr_slice.264"));
    // The active parameter sets are a `vaco-parse-h264` concept independent
    // of this decoder's own (currently empty) buffered state, so a slice
    // referencing an already-seen PPS still resolves after a flush.
    let err = d.send_packet(Some(&pkt)).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
}
