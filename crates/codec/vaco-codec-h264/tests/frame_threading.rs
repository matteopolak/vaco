//! `-threads N` must change *when* work happens and nothing else.
//!
//! The claim frame threading makes is that its output is bit-identical to the
//! single-threaded decoder's, at every legal thread count, on every stream.
//! `planning/E2E-GAPS.md` §20 records the end-to-end verification of that claim
//! against ffmpeg's own rawvideo on five real files; this file is the part of it
//! that lives in the repository and runs on every `cargo test`, so a regression
//! is caught by CI rather than by somebody re-running a shell script.
//!
//! It compares against **the one-thread decode**, not against a stored
//! reference: `decoder_output_matches_ffmpeg.rs` is what pins the one-thread
//! decode to reality, and duplicating that here would only be able to fail in
//! the same way. What this asserts is the *invariance*, which is the property
//! threading can break and the other file cannot see.
//!
//! Three fixtures, chosen for their dependency shapes, because that is the axis
//! that matters: a P-only chain (nothing may overlap but the pipeline stages), a
//! stream with B slices (pictures genuinely decode out of display order and the
//! reorder buffer has to hold them), and a multi-reference stream (a task waits
//! on more than one predecessor).

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
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// One decoded frame reduced to what a caller can observe: its timestamp and
/// its planes' meaningful bytes, stride padding dropped so an allocator's
/// alignment choice cannot pass for a decode difference.
type Observable = (Option<i64>, Vec<Vec<u8>>);

fn observe(frame: &Frame) -> Observable {
    let FrameData::Video {
        width,
        height,
        planes,
        ..
    } = &frame.data
    else {
        panic!("expected a video frame");
    };
    let packed = planes
        .iter()
        .enumerate()
        .map(|(i, plane)| {
            let (w, h) = if i == 0 {
                (*width as usize, *height as usize)
            } else {
                (
                    (*width as usize).div_ceil(2),
                    (*height as usize).div_ceil(2),
                )
            };
            let data = plane.data.as_slice();
            let mut out = Vec::new();
            for r in 0..h {
                let start = r * plane.stride;
                out.extend_from_slice(&data[start..start + w]);
            }
            out
        })
        .collect();
    (frame.pts.ticks(), packed)
}

/// Split an Annex B elementary stream into extradata and one packet per slice,
/// the framing a real demuxer hands `send_packet`.
fn split(stream: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut extradata = Vec::new();
    let mut slices = Vec::new();
    for nal in annexb::nal_units(stream) {
        match nal.first().map(|b| b & 0x1F) {
            Some(7 | 8) => {
                extradata.extend_from_slice(&[0, 0, 0, 1]);
                extradata.extend_from_slice(nal);
            }
            Some(1 | 5) => {
                let mut framed = vec![0u8, 0, 0, 1];
                framed.extend_from_slice(nal);
                slices.push(framed);
            }
            _ => {}
        }
    }
    (extradata, slices)
}

fn decode(stream: &[u8], threads: usize) -> Vec<Observable> {
    let (extradata, slices) = split(stream);
    let mut d = H264Decoder::new(Limits::permissive());
    // Assert the path was actually taken. `set_thread_count` narrows what it
    // grants (a target without threads clamps to one), so a test that only
    // *asked* for eight threads and silently got a serial decoder would pass
    // this file while proving nothing at all.
    let granted = d.set_thread_count(threads);
    if threads > 1 {
        assert!(
            matches!(granted, vaco_codec_core::Threading::Frame { .. }),
            "asked for {threads} threads and was granted {granted:?}"
        );
        assert!(
            granted.max_frames() > 1,
            "asked for {threads} threads and was granted a single picture in flight"
        );
    }
    let mut budget = Budget::new(Limits::permissive());
    d.set_extradata(&extradata).unwrap();

    let mut out = Vec::new();
    for slice in &slices {
        let pkt = Packet::from_slice(&mut budget, slice).unwrap();
        loop {
            match d.send_packet(Some(&pkt)) {
                Ok(()) => break,
                Err(Error::OutputPending) => out.push(observe(&d.receive_frame().unwrap())),
                Err(e) => panic!("send_packet failed at {threads} threads: {e:?}"),
            }
        }
        while let Ok(frame) = d.receive_frame() {
            out.push(observe(&frame));
        }
    }
    d.send_packet(None).unwrap();
    loop {
        match d.receive_frame() {
            Ok(frame) => out.push(observe(&frame)),
            Err(Error::Eof) => break,
            Err(e) => panic!("drain failed at {threads} threads: {e:?}"),
        }
    }
    out
}

fn assert_invariant(name: &str, stream: &[u8]) {
    let serial = decode(stream, 1);
    assert!(
        !serial.is_empty(),
        "{name}: the serial decode produced no frames"
    );
    for threads in [2, 3, 4, 8] {
        let got = decode(stream, threads);
        assert_eq!(
            got.len(),
            serial.len(),
            "{name}: {threads} threads emitted {} frames, one thread emitted {}",
            got.len(),
            serial.len()
        );
        for (i, (a, b)) in serial.iter().zip(got.iter()).enumerate() {
            assert_eq!(
                a.0, b.0,
                "{name}: frame {i} came out with pts {:?} at {threads} threads, {:?} at one \
                 -- the reorder buffer saw a different order",
                b.0, a.0
            );
            assert_eq!(
                a.1.len(),
                b.1.len(),
                "{name}: frame {i} has a different plane count at {threads} threads"
            );
            for (p, (pa, pb)) in a.1.iter().zip(b.1.iter()).enumerate() {
                let differing = pa.iter().zip(pb.iter()).filter(|(x, y)| x != y).count();
                assert_eq!(
                    differing,
                    0,
                    "{name}: frame {i} plane {p} differs in {differing} of {} bytes at \
                     {threads} threads",
                    pa.len()
                );
            }
        }
    }
}

/// A `Budget` charge that is never released is invisible until the total runs
/// out, which is why `planning/E2E-GAPS.md` §8 caught the DPB's own leak only
/// as "1080p stops after exactly 10 frames". Frame threading adds two charges
/// per picture -- a DPB entry's samples, and an in-flight task's working set --
/// and multiplies the second by the thread count, so a missed `release` on
/// either is exactly the shape that was already found once here and four times
/// in the sibling HEVC decoder.
///
/// This decodes the same 25-picture fixture under a ceiling far too small to
/// hold 25 pictures' worth of anything, at every thread count. If either charge
/// leaks, the decode fails with `LimitExceeded` partway through instead of
/// producing all 25 frames.
#[test]
fn neither_per_picture_budget_charge_leaks_at_any_thread_count() {
    let stream: &[u8] = include_bytes!("fixtures/cabac_ip_simple.264");
    let (extradata, slices) = split(stream);
    // 64x64: a task's charge is ~42 KiB (two coded pictures at 6 KiB each,
    // plus 16 `MbSummary` at 1,888 bytes -- the macroblock array is the big
    // half, here as at 4K) and a DPB entry's is ~12 KiB. At eight threads the
    // legitimate live set is nine in-flight pictures plus the DPB, around
    // 410 KiB; 25 pictures' worth of the task charge left unreleased is over
    // 1 MiB, and of the DPB charge nearly 300 KiB on top of the legitimate
    // set. The ceiling sits between.
    //
    // **The number was chosen by checking the test fails, not by rounding.**
    // Each `Budget::release` was deleted in turn and this test re-run: both
    // deletions fail at 512 KiB. An earlier draft used 4 MiB, which caught the
    // DPB leak and silently passed the task leak, because 25 unreleased task
    // charges still fit underneath it -- a test passing for the wrong reason,
    // which is the failure `planning/AGENT-CONSTRAINTS.md` names as an oracle
    // that shares your misreading. Re-check both if these sizes change.
    let mut limits = Limits::permissive();
    limits.max_alloc_total = 512 * 1024;

    for threads in [1, 2, 4, 8] {
        let mut d = H264Decoder::new(limits.clone());
        d.set_thread_count(threads);
        let mut budget = Budget::new(Limits::permissive());
        d.set_extradata(&extradata).unwrap();
        let mut frames = 0usize;
        for slice in &slices {
            let pkt = Packet::from_slice(&mut budget, slice).unwrap();
            loop {
                match d.send_packet(Some(&pkt)) {
                    Ok(()) => break,
                    Err(Error::OutputPending) => {
                        d.receive_frame().unwrap();
                        frames += 1;
                    }
                    Err(e) => panic!(
                        "at {threads} threads, decode failed after {frames} frames \
                         under the ceiling -- a per-picture Budget charge is not \
                         being released: {e:?}"
                    ),
                }
            }
            while d.receive_frame().is_ok() {
                frames += 1;
            }
        }
        d.send_packet(None).unwrap();
        while d.receive_frame().is_ok() {
            frames += 1;
        }
        assert_eq!(frames, slices.len(), "at {threads} threads");
    }
}

/// A budget too small for `-threads N`'s whole in-flight window must cost
/// *speed*, not the decode.
///
/// `-threads 8` at 4K wants nine pictures in flight and about 756 MiB to hold
/// them, against `Limits::permissive`'s 1 GiB -- it fits by margin rather than
/// by design, and 8K or a tighter `-max_alloc` would not. The failure that
/// would produce is the wrong shape: a thread count that silently also means
/// "and do not decode large pictures". `split_packet` therefore finishes
/// pictures until the next one's charge fits before allocating anything for
/// it, so the window shrinks under pressure instead of the decode failing.
///
/// This asks for eight threads under a ceiling that holds barely two pictures
/// and asserts all 25 frames still come out, byte-identical to the unbounded
/// serial decode. If the drain were removed, this reports `LimitExceeded`.
#[test]
fn a_budget_too_small_for_the_thread_count_costs_speed_not_the_decode() {
    let stream: &[u8] = include_bytes!("fixtures/cabac_ip_simple.264");
    let want = decode(stream, 1);

    let mut limits = Limits::permissive();
    // One picture's charge is ~42 KiB and its DPB entry's ~12 KiB, so this
    // holds two in flight and change -- a quarter of what eight threads asks
    // for.
    limits.max_alloc_total = 128 * 1024;

    let (extradata, slices) = split(stream);
    let mut d = H264Decoder::new(limits);
    d.set_thread_count(8);
    let mut budget = Budget::new(Limits::permissive());
    d.set_extradata(&extradata).unwrap();
    let mut got = Vec::new();
    for slice in &slices {
        let pkt = Packet::from_slice(&mut budget, slice).unwrap();
        loop {
            match d.send_packet(Some(&pkt)) {
                Ok(()) => break,
                Err(Error::OutputPending) => got.push(observe(&d.receive_frame().unwrap())),
                Err(e) => panic!(
                    "a tight budget failed the decode after {} frames instead of \
                     narrowing the in-flight window: {e:?}",
                    got.len()
                ),
            }
        }
        while let Ok(frame) = d.receive_frame() {
            got.push(observe(&frame));
        }
    }
    d.send_packet(None).unwrap();
    while let Ok(frame) = d.receive_frame() {
        got.push(observe(&frame));
    }
    assert_eq!(got.len(), want.len(), "frames lost under a tight budget");
    assert!(got == want, "a tight budget changed the decoded output");
}

#[test]
fn a_p_only_chain_decodes_identically_at_every_thread_count() {
    // Nothing here may overlap except the serial stage against the parallel
    // one -- every picture predicts from the one before it. The interesting
    // case for correctness even so: it is the shape where a task spends most
    // of its life blocked in `wait_rows`.
    assert_invariant(
        "cabac_ip_simple",
        include_bytes!("fixtures/cabac_ip_simple.264"),
    );
}

#[test]
fn b_slices_decode_identically_at_every_thread_count() {
    // Decode order and display order genuinely differ here, so this is what
    // would catch the reorder buffer being driven in completion order rather
    // than in decode order.
    assert_invariant("cabac_ipbb", include_bytes!("fixtures/cabac_ipbb.264"));
}

#[test]
fn a_multi_reference_stream_decodes_identically_at_every_thread_count() {
    // A task that waits on more than one predecessor, and a sliding-window DPB
    // deep enough for an eviction to race a reader if the samples were not
    // reference-counted.
    assert_invariant(
        "cabac_ip_multiref",
        include_bytes!("fixtures/cabac_ip_multiref.264"),
    );
}
