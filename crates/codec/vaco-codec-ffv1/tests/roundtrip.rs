//! Integration tests: this crate's own encode/decode round trip (the
//! primary correctness signal for a lossless codec — see the crate's
//! top-level docs) and a cross-check against a real `ffmpeg`-produced FFV1
//! stream.
//!
//! The real-`ffmpeg` fixtures under `tests/fixtures/` were captured once with
//! a locally installed `ffmpeg 8.1` (`ffmpeg -f lavfi -i
//! "testsrc=size=176x144:rate=5:duration=1" -pix_fmt yuv420p -c:v ffv1
//! out.mkv`), then pulled apart by hand (a small Matroska EBML walk, not
//! ffmpeg itself) into the Configuration Record, two consecutive frames'
//! coded bytes, and `ffmpeg`'s own raw `YUV420p` decode of the same file — so
//! this test needs no `ffmpeg` binary at run time, only what was captured.
//! `provenance/vaco-codec-ffv1.toml` records the exact command as a
//! `blackbox` source.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    reason = "test code exercising the crate, not the untrusted-input surface the lints protect; the integer division here is exact (even-area yuv420p buffer sizes) test-fixture arithmetic, not a precision-sensitive computation"
)]

use vaco_codec_core::SendReceive;
use vaco_core::Timestamp;
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketSideData, PacketSideDataKind};
use vaco_pixfmt::PixFmt;

use vaco_codec_ffv1::{Ffv1Decoder, Ffv1Encoder};

/// A deterministic, non-uniform pattern so every plane exercises a mix of
/// flat regions, gradients and noise-like variation rather than only runs.
fn fill_plane(w: usize, h: usize, seed: usize, row_bytes: usize) -> Vec<u8> {
    let mut out = vec![0u8; row_bytes * h.max(1)];
    for y in 0..h {
        for x in 0..w {
            let v = ((x
                .wrapping_mul(41)
                .wrapping_add(y.wrapping_mul(97))
                .wrapping_add(seed.wrapping_mul(211)))
                % 256) as u8;
            if let Some(slot) = out.get_mut(y * row_bytes + x) {
                *slot = v;
            }
        }
    }
    out
}

fn make_frame(format: PixFmt, w: u32, h: u32) -> Frame {
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_video(&mut budget, format, w, h).expect("alloc");
    let plane_count = frame.plane_count();
    for pi in 0..plane_count {
        let mut plane = frame.plane_mut(pi).expect("plane");
        let rows = plane.rows();
        let row_bytes = plane.row_bytes();
        let stride = plane.stride();
        let pixels = fill_plane(row_bytes, rows, pi + 1, stride.max(1));
        for y in 0..rows {
            if let Some(row) = plane.row_mut(y) {
                let start = y * stride;
                if let Some(src) = pixels.get(start..start + row_bytes) {
                    row[..row_bytes].copy_from_slice(src);
                }
            }
        }
    }
    frame.pts = Timestamp::new(0);
    frame.flags = FrameFlags::KEY;
    frame
}

fn frame_bytes(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    for pi in 0..frame.plane_count() {
        let plane = frame.plane(pi).expect("plane");
        for row in plane.rows_iter() {
            out.extend_from_slice(row);
        }
    }
    out
}

/// Encode `frame` through [`Ffv1Encoder`] and decode it back through
/// [`Ffv1Decoder`], asserting every sample matches exactly.
fn assert_round_trips(frame: &Frame) {
    let FrameData::Video { width, height, .. } = &frame.data else {
        panic!("video frame");
    };
    let (w, h) = (*width, *height);

    let mut enc = Ffv1Encoder::new(Limits::permissive());
    enc.send(Some(frame)).expect("send frame");
    let packet = enc.receive().expect("receive packet");
    enc.send(None).expect("drain");
    let _ = enc.receive();

    let record = match packet.side_data(PacketSideDataKind::NewExtradata) {
        Some(PacketSideData::NewExtradata(buf)) => buf.as_slice().to_vec(),
        other => panic!("expected NewExtradata, got {other:?}"),
    };
    let mut dec = Ffv1Decoder::new(Limits::permissive());
    dec.set_extradata(&record).expect("set_extradata");
    dec.prime_video(w, h);
    dec.send(Some(&packet)).expect("send packet");
    let decoded = dec.receive().expect("receive frame");

    assert_eq!(
        frame_bytes(frame),
        frame_bytes(&decoded),
        "{w}x{h} {:?}",
        frame.pixel_format()
    );
}

#[test]
fn round_trips_yuv420p_various_sizes() {
    for &(w, h) in &[
        (2, 2),
        (4, 4),
        (16, 16),
        (64, 48),
        (7, 5),
        (9, 3),
        (1, 1),
        (3, 1),
        (1, 8),
    ] {
        let frame = make_frame(PixFmt::Yuv420p, w, h);
        assert_round_trips(&frame);
    }
}

#[test]
fn round_trips_yuv444p_various_sizes() {
    for &(w, h) in &[(2, 2), (16, 16), (7, 5), (33, 17), (1, 1)] {
        let frame = make_frame(PixFmt::Yuv444p, w, h);
        assert_round_trips(&frame);
    }
}

#[test]
fn round_trips_yuv422p_various_sizes() {
    for &(w, h) in &[(2, 2), (16, 8), (9, 7)] {
        let frame = make_frame(PixFmt::Yuv422p, w, h);
        assert_round_trips(&frame);
    }
}

#[test]
fn round_trips_gbrp_various_sizes() {
    for &(w, h) in &[(2, 2), (16, 16), (7, 5), (33, 17), (1, 1), (8, 1)] {
        let frame = make_frame(PixFmt::Gbrp, w, h);
        assert_round_trips(&frame);
    }
}

#[test]
fn round_trips_solid_colour_yuv420p() {
    // Exercises the "context is 0 almost everywhere" path hard, the same way
    // the QOI crate's run-length test does for its own format.
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 32, 32).expect("alloc");
    for pi in 0..frame.plane_count() {
        let mut plane = frame.plane_mut(pi).expect("plane");
        plane.fill(if pi == 0 { 200 } else { 128 });
    }
    frame.pts = Timestamp::new(0);
    frame.flags = FrameFlags::KEY;
    assert_round_trips(&frame);
}

#[test]
fn round_trips_multiple_frames_in_one_session() {
    // Exercises the keyframe/SliceHeader state across multiple frames — see
    // codec::fresh_keyframe_state's docs for why a single-frame test cannot
    // catch a reset-vs-persist mistake there.
    let mut enc = Ffv1Encoder::new(Limits::permissive());
    let mut frames = Vec::new();
    let mut packets = Vec::new();
    for _ in 0..4u32 {
        let frame = make_frame(PixFmt::Yuv420p, 12, 10);
        enc.send(Some(&frame)).expect("send");
        packets.push(enc.receive().expect("receive"));
        frames.push(frame);
    }
    enc.send(None).expect("drain");

    let record = match packets[0].side_data(PacketSideDataKind::NewExtradata) {
        Some(PacketSideData::NewExtradata(buf)) => buf.as_slice().to_vec(),
        other => panic!("expected NewExtradata, got {other:?}"),
    };
    let mut dec = Ffv1Decoder::new(Limits::permissive());
    dec.set_extradata(&record).expect("set_extradata");
    dec.prime_video(12, 10);
    for (i, (frame, packet)) in frames.iter().zip(packets.iter()).enumerate() {
        dec.send(Some(packet)).expect("send");
        let decoded = dec.receive().expect("receive");
        assert_eq!(frame_bytes(frame), frame_bytes(&decoded), "frame {i}");
    }
}

/// Same bug class as `vaco-codec-vp8`/`vaco-codec-vp9`'s encoders:
/// `send` used to never set `Packet::duration`, which a container deriving
/// a track's total length from summed packet durations (Matroska/AVI/NUT,
/// the containers FFV1 actually reaches) silently undercounts by. Checked
/// with two *different* per-frame durations, not one fixed value, so a
/// constant-per-frame implementation could not pass by accident.
#[test]
fn send_propagates_the_input_frames_real_duration() {
    let mut enc = Ffv1Encoder::new(Limits::permissive());

    let mut first = make_frame(PixFmt::Yuv420p, 12, 10);
    first.duration = vaco_core::Duration::from_micros(33_367);
    enc.send(Some(&first)).expect("send");
    let p0 = enc.receive().expect("receive");
    assert_eq!(p0.duration, vaco_core::Duration::from_micros(33_367));
    assert_ne!(p0.duration, vaco_core::Duration::ZERO);

    let mut second = make_frame(PixFmt::Yuv420p, 12, 10);
    second.duration = vaco_core::Duration::from_micros(16_683);
    enc.send(Some(&second)).expect("send");
    let p1 = enc.receive().expect("receive");
    assert_eq!(p1.duration, vaco_core::Duration::from_micros(16_683));
}

// ------------------------------------------------------------------ proptest

proptest::proptest! {
    /// Random pixel data at random small dimensions (including odd
    /// widths/heights that don't divide evenly for subsampled chroma
    /// planes) round-trips exactly through this crate's own encoder/decoder.
    #[test]
    fn proptest_round_trips_yuv420p(
        w in 1u32..40,
        h in 1u32..40,
        seed in 0usize..10_000,
    ) {
        let format = PixFmt::Yuv420p;
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, format, w, h).expect("alloc");
        for pi in 0..frame.plane_count() {
            let mut plane = frame.plane_mut(pi).expect("plane");
            let rows = plane.rows();
            let row_bytes = plane.row_bytes();
            for y in 0..rows {
                if let Some(row) = plane.row_mut(y) {
                    for (x, slot) in row.iter_mut().enumerate().take(row_bytes) {
                        *slot = ((x.wrapping_mul(31).wrapping_add(y.wrapping_mul(17)).wrapping_add(seed).wrapping_add(pi.wrapping_mul(97))) % 256) as u8;
                    }
                }
            }
        }
        frame.pts = Timestamp::new(0);
        frame.flags = FrameFlags::KEY;
        assert_round_trips(&frame);
    }
}

// --------------------------------------------------------------- real ffmpeg
//
// The tests below, not the round trips above, are this crate's evidence that
// it agrees with the *format*. A round trip only shows the encoder and decoder
// agree with each other, which they did for months while both disagreed with
// FFV1: `decodes_real_ffmpeg_golomb_rice_stream_pixel_exact` was `#[ignore]`d
// as a known bug that the whole rest of this file could not see.
//
// Every fixture was captured by running the `ffmpeg` binary as a black box and
// pulling the result apart by hand (a small Matroska EBML walk, never ffmpeg's
// own demuxer or decoder) into a Configuration Record, per-frame coded bytes,
// and ffmpeg's own raw YUV420p decode of the same file. Commands are recorded
// in `provenance/vaco-codec-ffv1.toml`; nothing here needs an `ffmpeg` binary
// at run time.
//
// - `rice160_*` / `rice160.raw`: 160x120, `-coder rice`, three consecutive
//   frames of which only the first is a keyframe (ffmpeg 9.0.1).
// - `yuv420_*` / `yuv420.raw`: 176x144, ffmpeg's default coder, frame 0
//   (ffmpeg 8.1).
// - `yuv420_range_*` / `yuv420_range.raw`: 64x64, `-coder range_def`, a 2x2
//   slice grid despite the frame being nowhere near RFC 9043 §5's
//   multi-slice threshold (ffmpeg 8.1).

const RANGE_EXTRADATA: &[u8] = include_bytes!("fixtures/yuv420_range_extradata.bin");
const RANGE_FRAME0: &[u8] = include_bytes!("fixtures/yuv420_range_frame0.bin");
const RANGE_RAW: &[u8] = include_bytes!("fixtures/yuv420_range.raw");
const RANGE_W: u32 = 64;
const RANGE_H: u32 = 64;

/// The crate's primary real-`ffmpeg` cross-check: decode a real 4-slice,
/// range-coder FFV1 frame and compare pixel-for-pixel (Y, Cb, and Cr all)
/// against `ffmpeg`'s own raw decode of the same file.
#[test]
fn decodes_real_ffmpeg_range_coder_stream_pixel_exact() {
    let mut dec = Ffv1Decoder::new(Limits::permissive());
    dec.set_extradata(RANGE_EXTRADATA).expect("set_extradata");
    dec.prime_video(RANGE_W, RANGE_H);

    let mut budget = Budget::new(Limits::permissive());
    let pkt = Packet::from_slice(&mut budget, RANGE_FRAME0).expect("packet");
    dec.send(Some(&pkt)).expect("send frame");
    let frame = dec.receive().expect("receive frame");
    let frame_bytes_len = (RANGE_W as usize) * (RANGE_H as usize) * 3 / 2; // yuv420p
    assert_eq!(frame_bytes(&frame), &RANGE_RAW[..frame_bytes_len]);
}

const GOLOMB_EXTRADATA: &[u8] = include_bytes!("fixtures/yuv420_extradata.bin");
const GOLOMB_FRAME0: &[u8] = include_bytes!("fixtures/yuv420_frame0.bin");
const GOLOMB_RAW: &[u8] = include_bytes!("fixtures/yuv420.raw");
const GOLOMB_W: u32 = 176;
const GOLOMB_H: u32 = 144;

const RICE160_EXTRADATA: &[u8] = include_bytes!("fixtures/rice160_extradata.bin");
const RICE160_FRAMES: [&[u8]; 3] = [
    include_bytes!("fixtures/rice160_frame0.bin"),
    include_bytes!("fixtures/rice160_frame1.bin"),
    include_bytes!("fixtures/rice160_frame2.bin"),
];
const RICE160_RAW: &[u8] = include_bytes!("fixtures/rice160.raw");
const RICE160_W: u32 = 160;
const RICE160_H: u32 = 120;

/// The crate's primary conformance evidence for `coder_type = 0` — the coder a
/// plain `ffmpeg -c:v ffv1` actually writes.
///
/// Three consecutive frames, not one: the fixture's `intra` is 0, so only
/// frame 0 carries `keyframe = 1` and frames 1-2 require the Golomb VLC
/// contexts to keep adapting from where the previous frame left them. A
/// single-frame test passes with that wrong either way.
#[test]
fn decodes_real_ffmpeg_multi_frame_golomb_rice_stream_pixel_exact() {
    let mut dec = Ffv1Decoder::new(Limits::permissive());
    dec.set_extradata(RICE160_EXTRADATA).expect("set_extradata");
    dec.prime_video(RICE160_W, RICE160_H);

    let frame_len = (RICE160_W as usize) * (RICE160_H as usize) * 3 / 2;
    for (i, coded) in RICE160_FRAMES.iter().enumerate() {
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, coded).expect("packet");
        dec.send(Some(&pkt)).expect("send frame");
        let frame = dec.receive().expect("receive frame");
        assert_eq!(
            frame_bytes(&frame),
            &RICE160_RAW[i * frame_len..(i + 1) * frame_len],
            "frame {i}"
        );
    }
}

/// The Golomb-Rice (`coder_type = 0`) twin of the range-coder cross-check
/// above, at a second geometry (176x144, a 2x2 slice grid whose slices are odd
/// in neither axis) and from a different `ffmpeg` build than the fixture above.
#[test]
fn decodes_real_ffmpeg_golomb_rice_stream_pixel_exact() {
    let mut dec = Ffv1Decoder::new(Limits::permissive());
    dec.set_extradata(GOLOMB_EXTRADATA).expect("set_extradata");
    dec.prime_video(GOLOMB_W, GOLOMB_H);

    let mut budget = Budget::new(Limits::permissive());
    let pkt = Packet::from_slice(&mut budget, GOLOMB_FRAME0).expect("packet");
    dec.send(Some(&pkt)).expect("send frame");
    let frame = dec.receive().expect("receive frame");
    let frame_bytes_len = (GOLOMB_W as usize) * (GOLOMB_H as usize) * 3 / 2; // yuv420p
    assert_eq!(frame_bytes(&frame), &GOLOMB_RAW[..frame_bytes_len]);
}
