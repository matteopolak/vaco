//! Dependent WPP slice-segment assembly against JCT-VC `WPP_F_ericsson_MAIN_2`.
//!
//! This conformance stream is three CTUs wide (192x240), and splits the first
//! row after CTU zero before continuing with complete rows. A dependent
//! segment that starts a new row must use the prior row's §9.3.2.3 snapshot,
//! while a segment that continues a row uses §9.3.2.5's final context state.
//!
//! The 31,461-byte stream has SHA-256
//! `e8566e0e48509592dfaf7d314b7e292f51cede045559e3c32b447a9822a8b949`.
//! Its 48 archive pictures contain 3,317,760 yuv420p bytes with MD5
//! `2aaf16274fe8e799d72fa08a4963850d`.

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

const HEVC: &[u8] = include_bytes!("fixtures/wpp_f_192x240.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/wpp_f_192x240.yuv");
const WIDTH: usize = 192;
const HEIGHT: usize = 240;
const FRAMES: usize = 48;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;
const OFFICIAL_MD5: &str = "2aaf16274fe8e799d72fa08a4963850d";

#[test]
fn dependent_wpp_three_ctu_partial_row_segments_match_archive() {
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
