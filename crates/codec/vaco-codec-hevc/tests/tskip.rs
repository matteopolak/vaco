//! `transform_skip_flag` (§7.3.8.11 syntax, §8.6.4.2 residual) against a real
//! `libx265 --tskip` encode, measured plane by plane against `ffmpeg`'s own
//! decode.
//!
//! # Fixture and its generation
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc2=size=64x64:rate=25:duration=1" -pix_fmt yuv420p \
//!        -c:v libx265 \
//!        -x265-params "tskip=1:qp=34:keyint=1:wpp=0:no-sao=1:no-deblock=1" \
//!        -frames:v 2 tests/fixtures/tskip_64x64.hevc
//! ffmpeg -y -skip_loop_filter all -i tests/fixtures/tskip_64x64.hevc \
//!        -pix_fmt yuv420p -f rawvideo tests/fixtures/tskip_64x64.yuv
//! ```
//!
//! `tskip=1` is `libx265`'s own switch and is **off** by default, which is why
//! no fixture in `oracle.rs`/`flat.rs` reaches this path: measured, by making
//! `read_transform_skip_flag` refuse on a decoded `1` and re-running, this
//! fixture refuses on the very first access unit while every other fixture in
//! this crate decodes unchanged. So a regression that silently stopped
//! honouring the flag would fail *here* and nowhere else.
//!
//! The in-loop filters are switched off at the encoder (not papered over with
//! `-skip_loop_filter all`, which is passed as well only as the second line of
//! defence `AGENT-CONSTRAINTS.md` asks for) so that a residual-path regression
//! cannot hide behind a filtering one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    reason = "test code over a fixed, checked-in fixture: WIDTH/HEIGHT are compile-time-known \
              even powers of two, and a missing plane/row/frame-shape here is itself the test failing"
)]

use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_core::Error;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const HEVC: &[u8] = include_bytes!("fixtures/tskip_64x64.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/tskip_64x64.yuv");
const WIDTH: usize = 64;
const HEIGHT: usize = 64;
const FRAMES: usize = 2;

/// Every access unit in this fixture is one IDR (`keyint=1`), so splitting on
/// the Annex-B start code that precedes each `first_slice_segment_in_pic_flag`
/// slice is exactly "split on IRAP NAL units" here.
fn access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            // 19/20 = IDR_W_RADL / IDR_N_LP.
            let nut = (data[i + 3] >> 1) & 0x3f;
            if starts.is_empty() || nut == 19 || nut == 20 {
                starts.push(i);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.push(data.len());
    starts.windows(2).map(|w| &data[w[0]..w[1]]).collect()
}

#[test]
fn transform_skip_blocks_are_byte_exact() {
    let mut budget = Budget::new(Limits::default());
    let mut decoder = HevcDecoder::new(Limits::default());
    let y_size = WIDTH * HEIGHT;
    let c_size = (WIDTH / 2) * (HEIGHT / 2);
    let frame_size = y_size + 2 * c_size;

    let mut got = Vec::new();
    for au in access_units(HEVC) {
        let pkt = Packet::from_slice(&mut budget, au).unwrap();
        match decoder.send_packet(Some(&pkt)) {
            Ok(()) => {}
            Err(Error::Unsupported(msg)) => panic!("decode refused as unsupported: {msg}"),
            Err(e) => panic!("unexpected decode error: {e:?}"),
        }
        while let Ok(frame) = decoder.receive_frame() {
            append_planes(&frame, &mut got);
        }
    }
    decoder.send_packet(None).unwrap();
    while let Ok(frame) = decoder.receive_frame() {
        append_planes(&frame, &mut got);
    }

    assert_eq!(
        got.len(),
        FRAMES * frame_size,
        "frame count: decoded {} frames, fixture has {FRAMES}",
        got.len() / frame_size
    );
    assert_eq!(REF_YUV.len(), FRAMES * frame_size, "fixture ref YUV size");

    for (f, (ours, theirs)) in got
        .chunks_exact(frame_size)
        .zip(REF_YUV.chunks_exact(frame_size))
        .enumerate()
    {
        let mut offset = 0;
        for (name, size) in [("Y", y_size), ("U", c_size), ("V", c_size)] {
            let a = &ours[offset..offset + size];
            let b = &theirs[offset..offset + size];
            offset += size;
            let exact = a.iter().zip(b).filter(|(x, y)| x == y).count();
            assert_eq!(
                exact, size,
                "frame {f} plane {name}: {exact} of {size} samples byte-exact"
            );
        }
    }
}

fn append_planes(frame: &vaco_frame::Frame, out: &mut Vec<u8>) {
    let vaco_frame::FrameData::Video { width, height, .. } = &frame.data else {
        panic!("expected a video frame");
    };
    assert_eq!(*width as usize, WIDTH);
    assert_eq!(*height as usize, HEIGHT);
    for (plane_index, (w, h)) in [
        (0, (WIDTH, HEIGHT)),
        (1, (WIDTH / 2, HEIGHT / 2)),
        (2, (WIDTH / 2, HEIGHT / 2)),
    ] {
        let plane = frame.plane(plane_index).expect("plane present");
        for y in 0..h {
            out.extend_from_slice(&plane.row(y).expect("row in range")[..w]);
        }
    }
}
