//! Finding 22b (`planning/INTERFACE-GAPS.md`): `vaco_parse_av1::metadata`
//! parses `metadata_hdr_mdcv()`/`metadata_hdr_cll()` (§5.8.3/§5.8.4)
//! correctly, and nothing in this crate ever read either -- `decode_temporal_unit`
//! silently dropped every `ObuType::METADATA` OBU through its own `_ => {}`
//! catch-all, so `FrameSideData::MasteringDisplay`/`ContentLightLevel` had
//! zero producers for AV1.
//!
//! # Fixture and its generation
//!
//! A flat 64x64 grey keyframe -- the same shape as `fixtures/flat128.obu`
//! (see `oracle.rs`'s own module doc for why flat content is used: this
//! crate's known content gaps, per that file, are tied to `SMOOTH_PRED`/
//! CFL/ADST reconstruction, not to metadata OBU handling, so nothing about
//! *this* finding needs busier content) -- with mastering-display/CLL
//! metadata added via SVT-AV1's own encoder option:
//!
//! ```text
//! ffmpeg -y -f rawvideo -pix_fmt yuv420p -s 64x64 -i flat128.yuv -frames:v 1 \
//!        -c:v libsvtav1 -qp 36 -svtav1-params \
//!        "enable-cdef=0:enable-restoration=0:enable-tf=0:film-grain=0:scm=0:\
//!         mastering-display=G(0.170,0.797)B(0.131,0.046)R(0.708,0.292)\
//!         WP(0.3127,0.3290)L(1000,0.005):content-light=1000,400" \
//!        -f obu tests/fixtures/flat128_hdr.obu
//! ```
//!
//! Measured with real `ffprobe 9.0.1 -show_frames` on the same file:
//! `red_x=46399/65536 red_y=19137/65536 green_x=11141/65536
//! green_y=52232/65536 blue_x=8585/65536 blue_y=3015/65536
//! white_point_x=20493/65536 white_point_y=21561/65536 min_luminance=82/16384
//! max_luminance=256000/256`, `max_content=1000 max_average=400` -- the
//! exact values this test asserts on the decoded `Frame`'s side data.

#![allow(clippy::unwrap_used, clippy::panic, reason = "test code over a fixed, checked-in fixture")]

use vaco_codec_av1::Av1Decoder;
use vaco_codec_core::Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

#[test]
fn a_real_svt_av1_stream_attaches_the_measured_mastering_display_and_cll() {
    let fixture: &[u8] = include_bytes!("fixtures/flat128_hdr.obu");
    let mut decoder = Av1Decoder::new(Limits::default());
    let mut budget = Budget::new(Limits::default());
    let pkt = Packet::from_slice(&mut budget, fixture).unwrap();
    decoder.send_packet(Some(&pkt)).unwrap();
    let frame = decoder.receive_frame().unwrap();

    let Some(mastering) = frame.side_data.iter().find_map(|sd| match sd {
        vaco_frame::FrameSideData::MasteringDisplay(m) => Some(m.as_ref()),
        _ => None,
    }) else {
        panic!("frame should carry MasteringDisplay side data");
    };
    // red, green, blue -- AV1's own bitstream order, unlike H.264/HEVC's
    // green/blue/red (see `mastering_display_from_mdcv`'s own doc for the
    // black-box measurement that caught this the first time).
    assert_eq!(mastering.primaries[0][0], vaco_core::Rational::new(46_399, 65_536), "red_x");
    assert_eq!(mastering.primaries[0][1], vaco_core::Rational::new(19_137, 65_536), "red_y");
    assert_eq!(mastering.primaries[1][0], vaco_core::Rational::new(11_141, 65_536), "green_x");
    assert_eq!(mastering.primaries[1][1], vaco_core::Rational::new(52_232, 65_536), "green_y");
    assert_eq!(mastering.primaries[2][0], vaco_core::Rational::new(8_585, 65_536), "blue_x");
    assert_eq!(mastering.primaries[2][1], vaco_core::Rational::new(3_015, 65_536), "blue_y");
    assert_eq!(mastering.white_point[0], vaco_core::Rational::new(20_493, 65_536), "white_point_x");
    assert_eq!(mastering.white_point[1], vaco_core::Rational::new(21_561, 65_536), "white_point_y");
    assert_eq!(mastering.min_luminance, vaco_core::Rational::new(82, 16_384), "min_luminance");
    assert_eq!(mastering.max_luminance, vaco_core::Rational::new(256_000, 256), "max_luminance");

    let Some(cll) = frame.side_data.iter().find_map(|sd| match sd {
        vaco_frame::FrameSideData::ContentLightLevel { max_cll, max_fall } => Some((*max_cll, *max_fall)),
        _ => None,
    }) else {
        panic!("frame should carry ContentLightLevel side data");
    };
    assert_eq!(cll, (1000, 400));
}
