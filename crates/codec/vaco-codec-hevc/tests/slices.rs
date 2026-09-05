//! Mixed-type independent slice segments against JCT-VC `SLICES_A_Rovi_3`.
//!
//! The stream deliberately alternates I and P slices within one picture, as
//! permitted by ITU-T H.265 §7.4.7.1. It is 65,943 bytes, nine 640x480
//! yuv420p frames, and the reference output is 4,147,200 bytes with MD5
//! `c2d63a4d145a5713afecd822032ec271` (the archive's published checksum).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code over a fixed, checked-in conformance vector"
)]

use vaco_codec_core::{Decoder, ParserDriver};
use vaco_codec_hevc::HevcDecoder;
use vaco_hash::HashAlgo;
use vaco_limits::{Budget, Limits};
use vaco_parse_hevc::HevcParser;

const HEVC: &[u8] = include_bytes!("fixtures/slices_a_640x480.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/slices_a_640x480.yuv");
const WIDTH: usize = 640;
const HEIGHT: usize = 480;
const FRAMES: usize = 9;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;
const OFFICIAL_MD5: &str = "c2d63a4d145a5713afecd822032ec271";

#[test]
fn mixed_slice_types_match_published_md5() {
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
