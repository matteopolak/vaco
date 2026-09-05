//! Slice-header short-term RPS derivation against JCT-VC `RPS_A_docomo_5`.
//!
//! The archive is SHA-256
//! `45427f66400a033e7a47cc6ca9b21d456e9e06450db6ca8d8fa92c438125324b`.
//! Its 64,948-byte Annex-B stream is SHA-256
//! `e7a90335952dc5718d931adb461d90049eb558b4d08c90ff1706612f8bca4439`.
//! The package documents 44 416x240 yuv420p frames, with short-term
//! slice-header RPS inter-prediction in its final three frames. Its published
//! visible-byte MD5, independently reproduced by local black-box `ffmpeg`, is
//! `7f4ad6c6b3de54558b0db59629b87db9`.

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
use vaco_hash::HashAlgo;
use vaco_limits::{Budget, Limits};
use vaco_parse_hevc::HevcParser;

const HEVC: &[u8] = include_bytes!("fixtures/rps_a_416x240.hevc");
const WIDTH: usize = 416;
const HEIGHT: usize = 240;
const FRAMES: usize = 44;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;
const OFFICIAL_MD5: &str = "7f4ad6c6b3de54558b0db59629b87db9";

#[test]
fn rps_a_slice_header_rps_matches_official_md5() {
    let limits = Limits::default();
    let mut parser = ParserDriver::new(HevcParser::new(limits.clone()), limits.clone());
    parser.push(HEVC).unwrap();
    parser.finish();

    let mut decoder = HevcDecoder::new(limits);
    let mut output_budget = Budget::new(Limits::default());
    let mut got: Vec<u8> = output_budget.alloc(FRAMES * FRAME_SIZE).unwrap();
    got.clear();
    for picture in 0..FRAMES {
        let packet = parser.next_unit().expect("picture access unit");
        match decoder.send_packet(Some(&packet)) {
            Ok(()) => {}
            Err(Error::Unsupported(message)) => {
                panic!("picture {picture} refused as unsupported: {message}")
            }
            Err(error) => panic!("picture {picture} decode error: {error:?}"),
        }
        drain_frames(&mut decoder, &mut got);
    }
    assert!(matches!(parser.next_unit(), Err(Error::Eof)));

    decoder.send_packet(None).unwrap();
    drain_frames(&mut decoder, &mut got);

    assert_eq!(
        got.len(),
        FRAMES * FRAME_SIZE,
        "decoded exactly {FRAMES} complete visible frames"
    );
    assert_eq!(
        HashAlgo::Md5.digest_hex(&got).as_deref(),
        Some(OFFICIAL_MD5),
        "whole-stream visible-byte digest"
    );
}

fn drain_frames(decoder: &mut HevcDecoder, out: &mut Vec<u8>) {
    while let Ok(frame) = decoder.receive_frame() {
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
}
