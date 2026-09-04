//! Scaling-list resolution and dequantisation against the first two coded
//! pictures of JCT-VC `SLIST_C_Sony_4`.
//!
//! The repository corpus entry `jctvc-hevc-slist-c-sony-4` has archive
//! SHA-256 `0af7bda8eae57ab56e96e6d9796c5dbe449036229fd177657147eb5ee6be1369`.
//! Its conformance note and `ffmpeg 9.0.1`'s `trace_headers` agree on the
//! intended switch: the SPS enables scaling lists but carries no list data;
//! picture 0's PPS also carries none, selecting the specification defaults,
//! while picture 1 replaces the same PPS id with explicit custom list data.
//! Those pictures have POC 0 and 8 in the stream's hierarchical-B decode
//! order. The checked-in reference is `ffmpeg 9.0.1`'s direct yuv420p output
//! frames 0 and 8: 1,198,080 bytes, SHA-256
//! `bef0aacb39148117f740fa53af9714c32092196dfef6bb126e7b45dea827f535`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    reason = "test code over a fixed, checked-in conformance vector"
)]

use vaco_codec_core::{Decoder, ParserDriver};
use vaco_codec_hevc::HevcDecoder;
use vaco_core::Error;
use vaco_limits::Limits;
use vaco_parse_hevc::HevcParser;

const HEVC: &[u8] = include_bytes!("fixtures/slist_c_832x480.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/slist_c_first_2_832x480.yuv");
const WIDTH: usize = 832;
const HEIGHT: usize = 480;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;

#[test]
fn default_then_custom_scaling_lists_are_byte_exact() {
    let limits = Limits::default();
    let mut parser = ParserDriver::new(HevcParser::new(limits.clone()), limits.clone());
    parser.push(HEVC).unwrap();
    parser.finish();

    let mut decoder = HevcDecoder::new(limits);
    for picture in 0..2 {
        let packet = parser.next_unit().expect("picture access unit");
        match decoder.send_packet(Some(&packet)) {
            Ok(()) => {}
            Err(Error::Unsupported(message)) => {
                panic!("picture {picture} refused as unsupported: {message}")
            }
            Err(error) => panic!("picture {picture} decode error: {error:?}"),
        }
    }
    decoder.send_packet(None).unwrap();

    let mut got = Vec::new();
    while let Ok(frame) = decoder.receive_frame() {
        append_planes(&frame, &mut got);
    }

    assert_eq!(REF_YUV.len(), 2 * FRAME_SIZE, "two reference frames");
    assert_eq!(
        got.len(),
        REF_YUV.len(),
        "decoded exactly two complete frames"
    );
    let exact = got
        .iter()
        .zip(REF_YUV)
        .filter(|(left, right)| left == right)
        .count();
    assert_eq!(
        exact,
        REF_YUV.len(),
        "{exact} of {} bytes exact",
        REF_YUV.len()
    );
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
