//! WPP-B's mixed dependent and independent partial-row segments against the
//! JCT-VC `WPP_B_ericsson_MAIN_2` conformance oracle.
//!
//! This 416x240 stream has thirteen CTUs per row and exercises arbitrary
//! partial-row boundaries across dependent and independent segments. The
//! 68,895-byte stream has SHA-256
//! `f1d18f737a9380f6ce58b6aeae158af458c5b557e06dd24977b7a5c6e095b9b2`.
//! Its 48 archive pictures contain 7,188,480 yuv420p bytes with MD5
//! `e37c7e561a1226640a7bf98e81df78b1`.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code over one fixed, checked-in conformance vector"
)]

use vaco_codec_core::{Decoder, ParserDriver};
use vaco_codec_hevc::HevcDecoder;
use vaco_hash::HashAlgo;
use vaco_limits::{Budget, Limits};
use vaco_parse_hevc::HevcParser;

const HEVC: &[u8] = include_bytes!("fixtures/wpp_b_416x240.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/wpp_b_416x240.yuv");
const WIDTH: usize = 416;
const HEIGHT: usize = 240;
const FRAMES: usize = 48;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;
const OFFICIAL_MD5: &str = "e37c7e561a1226640a7bf98e81df78b1";

#[test]
fn mixed_wpp_partial_row_segments_match_archive() {
    let limits = Limits::permissive();
    let mut parser = ParserDriver::new(HevcParser::new(limits.clone()), limits.clone());
    parser.push(HEVC).expect("parse stream");
    parser.finish();

    let mut decoder = HevcDecoder::new(limits);
    let mut output_budget = Budget::new(Limits::permissive());
    let mut got = output_budget
        .alloc(FRAMES * FRAME_SIZE)
        .expect("output allocation");
    got.clear();
    for picture in 0..FRAMES {
        let packet = parser.next_unit().expect("picture access unit");
        decoder
            .send_packet(Some(&packet))
            .unwrap_or_else(|error| panic!("picture {picture} decode error: {error:?}"));
        drain_frames(&mut decoder, &mut got);
    }
    assert!(matches!(parser.next_unit(), Err(vaco_core::Error::Eof)));
    decoder.send_packet(None).expect("drain");
    drain_frames(&mut decoder, &mut got);

    assert_eq!(got.len(), REF_YUV.len());
    assert_eq!(got.len(), FRAMES * FRAME_SIZE);
    assert_eq!(
        HashAlgo::Md5.digest_hex(&got).as_deref(),
        Some(OFFICIAL_MD5)
    );
    assert_eq!(got, REF_YUV);
}

fn drain_frames(decoder: &mut HevcDecoder, out: &mut Vec<u8>) {
    while let Ok(frame) = decoder.receive_frame() {
        let vaco_frame::FrameData::Video { width, height, .. } = &frame.data else {
            panic!("expected video frame");
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
