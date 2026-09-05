//! Independent-slice assembly against JCT-VC `HRD_A_Fujitsu_3`.
//!
//! The repository corpus entry `jctvc-hevc-hrd-a-fujitsu-3` has archive
//! SHA-256 `7241c304d8326ae182ae245b83eb4f95e299d0f4fe254e0e8844c56ee5d37aee`.
//! The checked-in Annex-B elementary stream is 149,080 bytes with SHA-256
//! `61af83565147e9dc23842847111e748d32d95830d88cd3855a66143ea08d6915`.
//! Its 96 416x240 yuv420p frames comprise 14,376,960 visible bytes and have
//! the conformance package's published MD5 `f6d04dba2ef09bcadbea7b8ab5c8c917`.
//! Each picture is four independent, raster-contiguous one-CTB-row slices.

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

const HEVC: &[u8] = include_bytes!("fixtures/hrd_a_416x240.hevc");
const WIDTH: usize = 416;
const HEIGHT: usize = 240;
const FRAMES: usize = 96;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;
const OFFICIAL_MD5: &str = "f6d04dba2ef09bcadbea7b8ab5c8c917";

#[test]
fn hrd_a_independent_row_slices_match_official_md5() {
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
