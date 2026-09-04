//! I_PCM syntax and reconstruction against JCT-VC `ipcm_A_NEC_3`,
//! `ipcm_C_NEC_3`, and `ipcm_D_NEC_3`.
//!
//! The vector is the repository corpus entry
//! `jctvc-hevc-ipcm-a-nec-3` (archive SHA-256
//! `89a71f4b9b7e22c481378f83bacc5aac6c8f204999691d3fa21f2804337e84c9`) and
//! `jctvc-hevc-ipcm-c-nec-3` (archive SHA-256
//! `12da972f8fcf2d75825535022b7a437d95b1c8272af0b738dc651f34e94c2175`),
//! and `jctvc-hevc-ipcm-d-nec-3` (archive SHA-256
//! `4ef6cdc8fd330d339e1e9e9e04487ba84a21e40a02d111b2ee10c28549bddc94`).
//! Their accompanying conformance notes describe one 416x240 intra picture
//! with 8-bit luma/chroma PCM samples and 8x8 through 32x32 PCM coding blocks.
//! A leaves `pcm_loop_filter_disabled_flag` clear; C sets it and specifically
//! tests skipping loop filtering on samples belonging to PCM CUs. D enables
//! transquant bypass and tests both direct residual reconstruction and loop
//! filter suppression for lossless CUs while leaving the PCM-specific loop
//! filter flag clear. The
//! checked-in references are `ffmpeg 9.0.1`'s direct yuv420p decodes; their
//! MD5 values `8049988c383486e076ea2494edda3831` and
//! `c3e74c399b73a5ab2dbd20523f583464`, and
//! `aa64a16240064bc2a90fadf979a62a7b` match the archives' published MD5 files.

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

const A_HEVC: &[u8] = include_bytes!("fixtures/ipcm_a_416x240.hevc");
const A_REF_YUV: &[u8] = include_bytes!("fixtures/ipcm_a_416x240.yuv");
const C_HEVC: &[u8] = include_bytes!("fixtures/ipcm_c_416x240.hevc");
const C_REF_YUV: &[u8] = include_bytes!("fixtures/ipcm_c_416x240.yuv");
const D_HEVC: &[u8] = include_bytes!("fixtures/ipcm_d_416x240.hevc");
const D_REF_YUV: &[u8] = include_bytes!("fixtures/ipcm_d_416x240.yuv");
const WIDTH: usize = 416;
const HEIGHT: usize = 240;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;

#[test]
fn ipcm_a_is_byte_exact() {
    assert_byte_exact(A_HEVC, A_REF_YUV);
}

#[test]
fn ipcm_c_filter_suppression_is_byte_exact() {
    assert_byte_exact(C_HEVC, C_REF_YUV);
}

#[test]
fn ipcm_d_transquant_bypass_is_byte_exact() {
    assert_byte_exact(D_HEVC, D_REF_YUV);
}

fn assert_byte_exact(hevc: &[u8], reference_yuv: &[u8]) {
    let mut budget = Budget::new(Limits::default());
    let packet = Packet::from_slice(&mut budget, hevc).unwrap();
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

    assert_eq!(reference_yuv.len(), FRAME_SIZE, "reference frame size");
    assert_eq!(got.len(), FRAME_SIZE, "decoded exactly one complete frame");

    let y_size = WIDTH * HEIGHT;
    let c_size = WIDTH / 2 * (HEIGHT / 2);
    let mut offset = 0;
    for (name, size) in [("Y", y_size), ("U", c_size), ("V", c_size)] {
        let ours = &got[offset..offset + size];
        let reference = &reference_yuv[offset..offset + size];
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
