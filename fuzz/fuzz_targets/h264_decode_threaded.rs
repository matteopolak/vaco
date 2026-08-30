//! `H264Decoder::set_thread_count` against arbitrary bytes: the row-progress
//! machinery `h264_decode.rs` never touches at all.
//!
//! That target never calls `set_thread_count`, so every line of
//! `ProgressPicture`'s band publication, `TaskCtx::wait_rows`'s per-row waits,
//! `PlaneView::block`'s refusal of an unpublished read and the `Banded` arm of
//! `crate::reconstruct::RefPlane` had zero fuzz coverage before this target
//! existed -- see `docs/codec/frame-threading.md` for the design and
//! `planning/E2E-GAPS.md` §21 for the row-granularity mechanism this exercises.
//!
//! # Design
//!
//! **Determinism is the property worth asserting, not "does not panic".**
//! `docs/codec/frame-threading.md`'s whole claim is that `-threads N` changes
//! *when* work happens and never *what is computed* -- the same input decoded
//! at one thread and at several must produce byte-identical output. So this
//! target decodes the same bytes twice, once with threading off and once with
//! a small thread count drawn from the input itself, and asserts the two
//! outputs are exactly equal. A plain "both runs complete without panicking"
//! target would have missed every one of `docs/codec/frame-threading.md`'s
//! three pinned boundary conditions (the filter's one-row lag, the
//! final-after-the-next-row watermark, the per-row reference reach) failing in
//! a way that still produces *plausible*, merely wrong, pixels -- exactly the
//! failure mode `planning/AGENT-CONSTRAINTS.md` calls out under "a test that
//! asserts well-formedness does not assert correctness".
//!
//! The thread count is derived from the input (`1 + data[0] % 4`, so 1..=4)
//! rather than fixed, so libFuzzer's own coverage feedback explores both the
//! flat and the banded `RefPlane` arm from one corpus, and stays small enough
//! that spawning threads does not dominate each iteration -- the brief this
//! target was written against measured spawning 64 threads per input as
//! "destroying execution throughput and finding nothing extra".
//!
//! A mismatch here is a race: the row-progress machinery published, waited on
//! or read something in an order that depends on wall-clock scheduling rather
//! than on the bitstream, which is precisely what row-level frame threading's
//! determinism argument says cannot happen.
//!
//! fuzz-crate: vaco-codec-h264

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_h264::H264Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// Decode `extradata`/`payload` at `threads` threads and reduce every emitted
/// frame to its meaningful bytes (plane data only, row by row -- stride
/// padding is an allocator choice, not part of what was decoded).
///
/// `None` means "this input never reached a picture" (extradata rejected, or
/// the first `send_packet` failed) -- not interesting to compare, since
/// neither run got far enough to disagree.
fn decode_at(threads: usize, extradata: &[u8], payload: &[u8]) -> Option<Vec<u8>> {
    let mut budget = Budget::new(Limits::strict());
    let packet = Packet::from_slice(&mut budget, payload).ok()?;
    let mut decoder = H264Decoder::new(Limits::strict());
    let _ = decoder.set_extradata(extradata);
    let _ = decoder.set_thread_count(threads);
    decoder.send_packet(Some(&packet)).ok()?;

    let mut out = Vec::new();
    let mut collect = |decoder: &mut H264Decoder| {
        while let Ok(frame) = decoder.receive_frame() {
            for idx in 0..3 {
                if let Some(plane) = frame.plane(idx) {
                    for r in 0..plane.rows() {
                        if let Some(row) = plane.row(r) {
                            out.extend_from_slice(row);
                        }
                    }
                }
            }
        }
    };
    collect(&mut decoder);
    // A second, identical access unit: the interesting reference-picture
    // reads (a task waiting on a predecessor's banded rows) only happen once
    // the DPB is non-empty, which the very first picture of a stream never
    // exercises.
    if decoder.send_packet(Some(&packet)).is_ok() {
        collect(&mut decoder);
    }
    let _ = decoder.send_packet(None);
    collect(&mut decoder);
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    // One byte picks a small worker count (1..=4, per this target's own doc)
    // so libFuzzer explores both the flat and the banded `RefPlane` arm from
    // the same corpus without spawning enough threads to tank exec/s.
    let threads = 1 + usize::from(data[0] % 4);
    let split = usize::from(data[1]).min(data.len() - 2);
    let (extradata, payload) = data[2..].split_at(split.min(data.len() - 2));

    let serial = decode_at(1, extradata, payload);
    let parallel = decode_at(threads, extradata, payload);
    assert_eq!(
        serial, parallel,
        "H264Decoder disagreed between -threads 1 and -threads {threads} on the same input \
         -- a race in the row-progress machinery, not a decode error (both runs used the same \
         bytes; a genuinely malformed input fails identically at every thread count)"
    );
});
