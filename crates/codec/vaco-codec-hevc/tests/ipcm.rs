//! I_PCM syntax and reconstruction against JCT-VC `ipcm_A_NEC_3`.
//!
//! The vector is the repository corpus entry
//! `jctvc-hevc-ipcm-a-nec-3` (archive SHA-256
//! `89a71f4b9b7e22c481378f83bacc5aac6c8f204999691d3fa21f2804337e84c9`).
//! Its accompanying conformance note says it contains one 416x240 intra
//! picture, uses 8-bit luma/chroma PCM samples, permits 8x8 through 32x32 PCM
//! coding blocks, and leaves `pcm_loop_filter_disabled_flag` clear. The
//! checked-in reference is `ffmpeg 9.0.1`'s direct yuv420p decode; its MD5
//! `8049988c383486e076ea2494edda3831` matches the archive's own published MD5.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    reason = "test code over a fixed, checked-in conformance vector"
)]

use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_core::Error;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const HEVC: &[u8] = include_bytes!("fixtures/ipcm_a_416x240.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/ipcm_a_416x240.yuv");
const WIDTH: usize = 416;
const HEIGHT: usize = 240;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;

#[test]
fn ipcm_a_is_byte_exact() {
    let mut budget = Budget::new(Limits::default());
    let packet = Packet::from_slice(&mut budget, HEVC).unwrap();
    let mut decoder = HevcDecoder::new(Limits::default());

    match decoder.send_packet(Some(&packet)) {
        Ok(()) => {}
        Err(Error::Unsupported(msg)) => panic!("decode refused as unsupported: {msg}"),
        Err(e) => panic!("unexpected decode error: {e:?}"),
    }
    decoder.send_packet(None).unwrap();

    let mut got = Vec::new();
    while let Ok(frame) = decoder.receive_frame() {
        append_planes(&frame, &mut got);
    }

    assert_eq!(REF_YUV.len(), FRAME_SIZE, "reference frame size");
    assert_eq!(got.len(), FRAME_SIZE, "decoded exactly one complete frame");

    let y_size = WIDTH * HEIGHT;
    let c_size = WIDTH / 2 * (HEIGHT / 2);
    let mut offset = 0;
    for (name, size) in [("Y", y_size), ("U", c_size), ("V", c_size)] {
        let ours = &got[offset..offset + size];
        let reference = &REF_YUV[offset..offset + size];
        offset += size;
        let exact = ours
            .iter()
            .zip(reference)
            .filter(|(left, right)| left == right)
            .count();
        assert_eq!(exact, size, "{name}: {exact} of {size} samples byte-exact");
    }
}

fn append_planes(frame: &vaco_frame::Frame, out: &mut Vec<u8>) {
    let vaco_frame::FrameData::Video { width, height, .. } = &frame.data else {
        panic!("expected a video frame");
    };
    assert_eq!((*width as usize, *height as usize), (WIDTH, HEIGHT));
    for (plane_index, (width, height)) in [
        (0, (WIDTH, HEIGHT)),
        (1, (WIDTH / 2, HEIGHT / 2)),
        (2, (WIDTH / 2, HEIGHT / 2)),
    ] {
        let plane = frame.plane(plane_index).expect("plane present");
        for y in 0..height {
            out.extend_from_slice(&plane.row(y).expect("row in range")[..width]);
        }
    }
}
