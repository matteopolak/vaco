//! Dependent-slice assembly against JCT-VC `DSLICE_A_HHI_5`.
//!
//! The ITU JCT-VC conformance package describes this as a 50-frame 1920x1080
//! stream that exercises both independent and dependent slice segments. The
//! checked-in Annex-B stream is 373,328 bytes with SHA-256
//! `8398fb23c814a197bba497ad2c6103f81ca8003434fa40ec347d4d0a07c9468a`.
//! Its visible yuv420p output is 155,520,000 bytes and has MD5
//! `c7caf3164b0a316549ac7244f66f1294`, matching both the package's `.md5`
//! file and a local black-box `ffmpeg` decode.

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

const HEVC: &[u8] = include_bytes!("fixtures/dslice_a_1920x1080.hevc");
const WIDTH: usize = 1920;
const HEIGHT: usize = 1080;
const FRAMES: usize = 50;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;
const OFFICIAL_MD5: &str = "c7caf3164b0a316549ac7244f66f1294";

#[test]
fn dslice_a_dependent_segments_match_official_md5() {
    let limits = Limits::permissive();
    let mut parser = ParserDriver::new(HevcParser::new(limits.clone()), limits.clone());
    parser.push(HEVC).unwrap();
    parser.finish();

    let mut decoder = HevcDecoder::new(limits);
    let mut output_budget = Budget::new(Limits::permissive());
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
