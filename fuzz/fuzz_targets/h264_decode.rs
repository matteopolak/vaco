//! H.264 decode against arbitrary bytes: the whole registered decoder, from
//! `set_extradata`/`send_packet` through NAL splitting, parameter-set
//! bookkeeping, slice-header parsing (`vaco-parse-h264`), CABAC
//! (`vaco-codec-cabac`), the macroblock layer, intra prediction,
//! dequantisation and inverse transform (`vaco-codec-dsp-idct`), motion
//! compensation, weighted prediction, the DPB, and the in-loop deblocking
//! filter (`vaco-codec-dsp-deblock`) — `H264Decoder::send_packet`/
//! `receive_frame` as the CLI itself calls them.
//!
//! **Why this exists separately from `h264_entropy`.** That target drives
//! `residual_block_cavlc`/`residual_block_cabac` directly: it never reaches
//! `crate::mb`'s macroblock layer, `crate::reconstruct`, `crate::intra`,
//! `crate::motion`, `crate::deblock`, the DPB, or the budget accounting
//! around any of them. Those are the majority of the crate, they read
//! attacker-controlled geometry (picture size in macroblocks, `ref_idx`,
//! motion vectors that point outside the reference picture,
//! `mb_qp_delta` accumulating across a slice, `pred_weight_table` weights
//! and offsets), and until this target they had no direct fuzz coverage at
//! all.
//!
//! Both framings are exercised: Annex-B start codes and length-prefixed
//! `avcC`, since `H264Decoder` detects which applies from the extradata
//! shape and walks them through different code paths.
//!
//! fuzz-crate: vaco-codec-h264

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_h264::H264Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// Drives one decoder to completion over `data`, treating the first
/// `split` bytes as extradata (parameter sets) and the rest as one access
/// unit — the shape a real demuxer hands the decoder, and the only way to
/// reach slice decoding at all, since a slice referencing an unseen PPS is
/// skipped by design.
fn run(extradata: &[u8], payload: &[u8]) {
    let mut budget = Budget::new(Limits::strict());
    let Ok(packet) = Packet::from_slice(&mut budget, payload) else {
        return;
    };
    let mut decoder = H264Decoder::new(Limits::strict());
    let _ = decoder.set_extradata(extradata);
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while let Ok(frame) = decoder.receive_frame() {
        // Every plane the decoder claims to have written must be
        // addressable at the frame's own reported dimensions: a cropping
        // or stride mistake shows up here without needing a pixel oracle
        // (that is `tests/decoder_output_matches_ffmpeg.rs`'s job).
        for idx in 0..3 {
            if let Some(plane) = frame.plane(idx) {
                let _ = plane.row(0);
            }
        }
    }
    // A second, identical access unit exercises what the decoder carries
    // *between* pictures rather than within one: the DPB and its budget
    // charges (a reference picture pushed, later evicted and released),
    // the parameter-set maps, and inter prediction against a real
    // reference rather than against an empty list.
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while decoder.receive_frame().is_ok() {}
    let _ = decoder.send_packet(None);
    while decoder.receive_frame().is_ok() {}
    decoder.flush();
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    // One attacker-controlled byte chooses where parameter sets end and the
    // access unit begins, so the fuzzer can build a coherent
    // extradata/slice pair out of one input rather than having to guess a
    // fixed split point.
    let split = usize::from(data[0]).min(data.len() - 1);
    let (head, tail) = data[1..].split_at(split.min(data.len() - 1));
    run(head, tail);
    // ... and the whole input as a single self-framed Annex-B stream, which
    // is what an elementary-stream demuxer produces and what carries in-band
    // SPS/PPS ahead of the slice.
    run(&[], &data[1..]);
});
