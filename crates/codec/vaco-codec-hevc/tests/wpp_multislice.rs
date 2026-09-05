//! Independent row-slice WPP assembly against a real `libx265` stream.
//!
//! The fixture is a two-frame 256x256 all-intra Main 4:2:0 stream encoded with
//! `wpp=1:slices=2:no-sao=1:no-deblock=1`. Each picture has two independent
//! slices, each covering two complete CTB rows. The checked-in stream is
//! 11,927 bytes with SHA-256
//! `75dbd6e7e7659e26de62c96ff03b8e219ea2fc8107e69e5895a7df5adaba9354`.
//! A black-box `ffmpeg` decode produces 196,608 visible yuv420p bytes with
//! MD5 `138d30492cca3f85709c514b8b4d9bac`.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code over one fixed, checked-in conformance-shaped fixture"
)]

use vaco_codec_core::{Decoder, ParserDriver};
use vaco_codec_hevc::HevcDecoder;
use vaco_hash::HashAlgo;
use vaco_limits::{Budget, Limits};
use vaco_parse_hevc::HevcParser;

const HEVC: &[u8] = include_bytes!("fixtures/wpp_multislice_256x256.hevc");
const WIDTH: usize = 256;
const HEIGHT: usize = 256;
const FRAMES: usize = 2;
const FRAME_SIZE: usize = WIDTH * HEIGHT * 3 / 2;
const OFFICIAL_MD5: &str = "138d30492cca3f85709c514b8b4d9bac";

#[test]
fn independent_wpp_row_slices_match_ffmpeg_md5() {
    let limits = Limits::permissive();
    let mut parser = ParserDriver::new(HevcParser::new(limits.clone()), limits.clone());
    parser.push(HEVC).expect("fixture parser input");
    parser.finish();

    let mut decoder = HevcDecoder::new(limits);
    let mut output_budget = Budget::new(Limits::permissive());
    let mut got: Vec<u8> = output_budget
        .alloc(FRAMES * FRAME_SIZE)
        .expect("output allocation");
    got.clear();
    for picture in 0..FRAMES {
        let packet = parser.next_unit().expect("picture access unit");
        decoder
            .send_packet(Some(&packet))
            .unwrap_or_else(|error| panic!("picture {picture} decode failed: {error:?}"));
        drain_frames(&mut decoder, &mut got);
    }
    assert!(matches!(parser.next_unit(), Err(vaco_core::Error::Eof)));

    decoder.send_packet(None).expect("decoder drain");
    drain_frames(&mut decoder, &mut got);

    assert_eq!(got.len(), FRAMES * FRAME_SIZE);
    assert_eq!(
        HashAlgo::Md5.digest_hex(&got).as_deref(),
        Some(OFFICIAL_MD5)
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
