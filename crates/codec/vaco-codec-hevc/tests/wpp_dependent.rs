//! Dependent WPP slice-segment assembly against JCT-VC `WPP_D_ericsson_MAIN_2`.
//!
//! The conformance stream is one CTU wide (64x240), uses WPP with varying
//! independent/dependent segments and slice-header extensions, and contains
//! 48 pictures. Its 22,474-byte stream has SHA-256
//! `30eec63f2324aa982fb91bd4c1c551c833c253ba711ee131aa9cc4d322398caf`.
//! The archive reference is 1,105,920 yuv420p bytes with MD5
//! `f710612103f386c415be3e6300693451`.

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

const HEVC: &[u8] = include_bytes!("fixtures/wpp_d_64x240.hevc");
const REF_YUV: &[u8] = include_bytes!("fixtures/wpp_d_64x240.yuv");
const WIDTH: usize = 64;
const HEIGHT: usize = 240;
const FRAMES: usize = 48;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;
const OFFICIAL_MD5: &str = "f710612103f386c415be3e6300693451";

#[test]
fn dependent_wpp_one_ctu_rows_match_archive() {
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
