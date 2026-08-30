//! The **whole-sequence, per-plane, byte-for-byte** acceptance check this
//! decoder went a long time without.
//!
//! `planning/E2E-GAPS.md` §7 records why this file exists: the H.264
//! decoder was reported as passing "FULL 25/25" by a harness that compared
//! output *file sizes*, and a 13.70%-of-all-bytes error survived as a green
//! result. The correct bar, stated there and enforced here, is: every frame,
//! every plane, every byte, against a real reference decoder's output --
//! and, when it fails, a message naming the **first differing frame** and
//! the magnitude of its difference, never a bare pass/fail.
//!
//! Deliberately driven through [`H264Decoder`]'s own public [`Decoder`]
//! surface (`set_extradata`/`send_packet`/`receive_frame`), not through
//! `crate::reconstruct`'s `pub(crate)` internals: the same §1 finding
//! ("the implementation had no production caller, and the tests hid that by
//! driving it directly") applies to correctness as much as to reachability.
//!
//! The reference (`fixtures/cabac_ip_simple_deblocked_ref.yuv`) is real
//! `ffmpeg` output for `fixtures/cabac_ip_simple.264` at its default
//! settings -- deblocking on -- captured once and embedded, so this test
//! needs no `ffmpeg` on `PATH` (D7 / plan 13 §1b, the same precedent
//! `tests/decoder.rs` follows). It was independently confirmed byte-exact
//! against a locally built JM 19.1 `ldecod` decode of the same fixture,
//! so the bar this file holds the decoder to is two independent reference
//! decoders agreeing, not one.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    clippy::format_collect,
    clippy::cast_precision_loss,
    reason = "test code over fixed fixtures: every division is by the constant 2 (4:2:0 \
              chroma subsampling) over compile-time constants, and the failure message is \
              built once, only on the failing path"
)]

use vaco_bitstream::annexb;
use vaco_codec_core::Decoder;
use vaco_codec_h264::H264Decoder;
use vaco_core::Error;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// `cabac_ip_simple.264`: 64x64, Main profile, CABAC, I + P slices, one
/// reference, one slice per picture, 25 frames.
const WIDTH: usize = 64;
const HEIGHT: usize = 64;
const LUMA: usize = WIDTH * HEIGHT;
const CHROMA: usize = (WIDTH / 2) * (HEIGHT / 2);
const FRAME: usize = LUMA + 2 * CHROMA;
const FRAMES: usize = 25;

/// One decoded plane's meaningful bytes, stride padding dropped, so the
/// comparison below is against the reference's own packed layout rather
/// than against whatever alignment the allocator happened to choose.
fn packed(plane: &vaco_frame::Plane, row_bytes: usize, rows: usize) -> Vec<u8> {
    let data = plane.data.as_slice();
    let mut out = Vec::new();
    for r in 0..rows {
        let start = r * plane.stride;
        out.extend_from_slice(&data[start..start + row_bytes]);
    }
    out
}

#[test]
fn every_frame_of_a_real_ip_stream_is_byte_exact_against_ffmpeg() {
    let stream: &[u8] = include_bytes!("fixtures/cabac_ip_simple.264");
    let reference: &[u8] = include_bytes!("fixtures/cabac_ip_simple_deblocked_ref.yuv");
    assert_eq!(reference.len(), FRAMES * FRAME, "reference fixture is not 25 whole frames");

    let mut d = H264Decoder::new(Limits::default());
    let mut budget = Budget::new(Limits::default());

    // Split the elementary stream into extradata (SPS + PPS) and one
    // packet per slice NAL, the framing a real demuxer hands `send_packet`.
    let mut extradata = Vec::new();
    let mut slices: Vec<Vec<u8>> = Vec::new();
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
    assert_eq!(slices.len(), FRAMES, "fixture should carry one slice per picture, 25 pictures");
    d.set_extradata(&extradata).unwrap();

    // `H264Decoder` declares `Caps::DELAY` (B-slice output reordering
    // needs it -- `crate::decoder`'s own module doc), so a frame is no
    // longer guaranteed one-to-one and immediately after the packet that
    // produced it: a picture can be held back until a later one proves
    // it is safe to emit in display order. This fixture is P-only (no B
    // slices at all), so decode order and display order coincide and
    // every frame's own reordering delay is bounded by the SPS's own
    // small reorder window -- but the *test* still has to drive the real
    // send/receive protocol (`Error::OutputPending` on backpressure,
    // `Error::NeedMoreInput` when nothing is ready yet, an explicit EOF
    // to flush whatever is still held) rather than assume strict
    // packet-for-frame lockstep, which no `Caps::DELAY` decoder can
    // promise in general.
    let mut frames: Vec<vaco_frame::Frame> = Vec::new();
    for slice in &slices {
        let pkt = Packet::from_slice(&mut budget, slice).unwrap();
        loop {
            match d.send_packet(Some(&pkt)) {
                Ok(()) => break,
                Err(Error::OutputPending) => frames.push(d.receive_frame().unwrap()),
                Err(e) => panic!("send_packet failed: {e:?}"),
            }
        }
        // Opportunistically drain whatever is already available --
        // `Error::NeedMoreInput` here just means nothing is ready yet
        // (the common case, given the reorder window), not a failure.
        while let Ok(frame) = d.receive_frame() {
            frames.push(frame);
        }
    }
    d.send_packet(None).unwrap();
    loop {
        match d.receive_frame() {
            Ok(frame) => frames.push(frame),
            Err(Error::Eof) => break,
            Err(e) => panic!("receive_frame failed while draining end of stream: {e:?}"),
        }
    }
    assert_eq!(frames.len(), FRAMES, "expected {FRAMES} frames out, got {}", frames.len());

    // Per-frame, per-plane difference counts and maxima -- collected for
    // every frame first, so the failure message can report the *first*
    // differing frame and the shape of the whole sequence, not just abort
    // at the first mismatching byte.
    let mut report: Vec<(usize, [(usize, u8); 3])> = Vec::new();
    for (idx, frame) in frames.iter().enumerate() {
        let FrameData::Video { format, planes, .. } = &frame.data else {
            panic!("frame {idx}: expected video, got {:?}", frame.data);
        };
        assert_eq!(format.name(), "yuv420p", "frame {idx}");
        assert_eq!(planes.len(), 3, "frame {idx}");

        let got = [
            packed(&planes[0], WIDTH, HEIGHT),
            packed(&planes[1], WIDTH / 2, HEIGHT / 2),
            packed(&planes[2], WIDTH / 2, HEIGHT / 2),
        ];
        let base = idx * FRAME;
        let want: [&[u8]; 3] = [
            &reference[base..base + LUMA],
            &reference[base + LUMA..base + LUMA + CHROMA],
            &reference[base + LUMA + CHROMA..base + FRAME],
        ];

        let mut per_plane = [(0usize, 0u8); 3];
        for p in 0..3 {
            assert_eq!(got[p].len(), want[p].len(), "frame {idx} plane {p}: wrong size");
            for (g, w) in got[p].iter().zip(want[p].iter()) {
                if g != w {
                    per_plane[p].0 += 1;
                    per_plane[p].1 = per_plane[p].1.max(g.abs_diff(*w));
                }
            }
        }
        report.push((idx, per_plane));
    }

    let first_bad = report.iter().find(|(_, p)| p.iter().any(|(n, _)| *n > 0));
    if let Some((idx, per_plane)) = first_bad {
        let total: usize = report.iter().map(|(_, p)| p.iter().map(|(n, _)| n).sum::<usize>()).sum();
        let summary: String = report
            .iter()
            .filter(|(_, p)| p.iter().any(|(n, _)| *n > 0))
            .map(|(i, p)| {
                format!(
                    "\n  frame {i}: Y {} bytes (max delta {}), Cb {} (max {}), Cr {} (max {})",
                    p[0].0, p[0].1, p[1].0, p[1].1, p[2].0, p[2].1
                )
            })
            .collect();
        panic!(
            "first differing frame is {idx}: Y {} bytes differ (max delta {}), \
             Cb {} (max {}), Cr {} (max {}). {total} of {} bytes differ across the \
             whole sequence ({:.4}%).{summary}",
            per_plane[0].0,
            per_plane[0].1,
            per_plane[1].0,
            per_plane[1].1,
            per_plane[2].0,
            per_plane[2].1,
            FRAMES * FRAME,
            100.0 * total as f64 / (FRAMES * FRAME) as f64,
        );
    }
}
