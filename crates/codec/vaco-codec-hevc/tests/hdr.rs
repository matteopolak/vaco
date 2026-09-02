//! Finding 22b (`planning/INTERFACE-GAPS.md`): `vaco_parse_hevc::sei`
//! parses `MasteringDisplay`/`ContentLightLevel` correctly, and nothing in
//! this crate ever read either -- so `FrameSideData::MasteringDisplay`/
//! `ContentLightLevel` (real types in `vaco-frame`, with a working
//! consumer in `vaco-filter-mm`'s `sidedata` filter) had zero producers
//! for HEVC.
//!
//! # Fixture and its generation
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc=size=32x32:rate=25:duration=0.04" -pix_fmt yuv420p \
//!        -c:v libx265 -x265-params \
//!        "colorprim=bt2020:colormatrix=bt2020nc:range=limited:keyint=1:hdr10=1:\
//!         master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):\
//!         max-cll=1000,400" \
//!        -frames:v 1 -f hevc tests/fixtures/hdr10_mastering_display.hevc
//! ```
//!
//! `yuv420p`, not the more realistic `yuv420p10le`: this crate's own scope
//! is 8-bit samples only (see `pic_to_frame`'s own `Error::Unsupported`),
//! and the SEI payload this test exercises carries the identical raw bytes
//! regardless of the picture's own bit depth.
//!
//! Measured with real `ffprobe 9.0.1 -show_frames` on the same file:
//! `red_x=34000/50000 red_y=16000/50000 green_x=13250/50000
//! green_y=34500/50000 blue_x=7500/50000 blue_y=3000/50000
//! white_point_x=15635/50000 white_point_y=16450/50000 min_luminance=1/10000
//! max_luminance=10000000/10000` and `max_content=1000 max_average=400` --
//! the exact values this test asserts on the decoded `Frame`'s side data.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code over a fixed, checked-in fixture"
)]

use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

#[test]
fn a_real_hdr10_stream_attaches_the_measured_mastering_display_and_cll() {
    let data: &[u8] = include_bytes!("fixtures/hdr10_mastering_display.hevc");
    let mut d = HevcDecoder::new(Limits::default());
    let mut budget = Budget::new(Limits::default());
    let pkt = Packet::from_slice(&mut budget, data).unwrap();
    d.send_packet(Some(&pkt)).unwrap();
    d.send_packet(None).unwrap();
    let frame = d.receive_frame().unwrap();

    let Some(mastering) = frame.side_data.iter().find_map(|sd| match sd {
        vaco_frame::FrameSideData::MasteringDisplay(m) => Some(m.as_ref()),
        _ => None,
    }) else {
        panic!("frame should carry MasteringDisplay side data");
    };
    // red, green, blue -- see `vaco_frame::MasteringDisplay`'s own doc for
    // why this is not the bitstream's green/blue/red order.
    assert_eq!(
        mastering.primaries[0][0],
        vaco_core::Rational::new(34_000, 50_000),
        "red_x"
    );
    assert_eq!(
        mastering.primaries[0][1],
        vaco_core::Rational::new(16_000, 50_000),
        "red_y"
    );
    assert_eq!(
        mastering.primaries[1][0],
        vaco_core::Rational::new(13_250, 50_000),
        "green_x"
    );
    assert_eq!(
        mastering.primaries[1][1],
        vaco_core::Rational::new(34_500, 50_000),
        "green_y"
    );
    assert_eq!(
        mastering.primaries[2][0],
        vaco_core::Rational::new(7_500, 50_000),
        "blue_x"
    );
    assert_eq!(
        mastering.primaries[2][1],
        vaco_core::Rational::new(3_000, 50_000),
        "blue_y"
    );
    assert_eq!(
        mastering.white_point[0],
        vaco_core::Rational::new(15_635, 50_000),
        "white_point_x"
    );
    assert_eq!(
        mastering.white_point[1],
        vaco_core::Rational::new(16_450, 50_000),
        "white_point_y"
    );
    assert_eq!(
        mastering.min_luminance,
        vaco_core::Rational::new(1, 10_000),
        "min_luminance"
    );
    assert_eq!(
        mastering.max_luminance,
        vaco_core::Rational::new(10_000_000, 10_000),
        "max_luminance"
    );

    let Some(cll) = frame.side_data.iter().find_map(|sd| match sd {
        vaco_frame::FrameSideData::ContentLightLevel { max_cll, max_fall } => {
            Some((*max_cll, *max_fall))
        }
        _ => None,
    }) else {
        panic!("frame should carry ContentLightLevel side data");
    };
    assert_eq!(cll, (1000, 400));
}
