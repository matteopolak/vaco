//! Tile-picture scope boundary against a real HM 18.0 Annex-B stream.
//!
//! The fixture is one 512x64 IDR picture with two uniform tile columns. Its
//! PPS must carry `tiles_enabled_flag = 1`; the decoder must then refuse the
//! picture by the named scope error before reading tile-partitioned CABAC.
//! The 1,813-byte HM 18.0 stream has SHA-256
//! `e7ede7ded9e07974097809c4bacda3492a6634a216a8d8b5c8920a3ceb3c91f2`.
//! A black-box `ffmpeg` decode is 49,152 visible yuv420p bytes with MD5
//! `6ccc33b0cd92240a275d30a05de031cc`.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code over one fixed, checked-in conformance-shaped fixture"
)]

use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_core::Error;
use vaco_format_nalu::Framing;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_hevc::HevcParser;

const HEVC: &[u8] = include_bytes!("fixtures/tiles_512x64.hevc");

fn packet(bytes: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::default());
    Packet::from_slice(&mut budget, bytes).expect("fixture packet allocation")
}

#[test]
fn tile_pps_is_rejected_by_name_before_cabac_decode() {
    let limits = Limits::default();
    let mut parser = HevcParser::new(limits.clone());
    let info = parser
        .push_access_unit(HEVC, Framing::AnnexB)
        .expect("valid tile fixture parses");
    assert_eq!(info.picture_type, Some('I'));

    let pps = parser
        .parameter_sets()
        .get_pps(0)
        .expect("fixture references PPS zero");
    let tiles = pps.tiles.as_ref().expect("fixture PPS enables tiles");
    assert_eq!((tiles.num_columns, tiles.num_rows), (2, 1));
    assert!(tiles.uniform_spacing);

    let mut decoder = HevcDecoder::new(limits);
    let error = decoder
        .send_packet(Some(&packet(HEVC)))
        .expect_err("tile pictures must remain out of scope");
    assert!(matches!(
        error,
        Error::Unsupported("vaco-codec-hevc: tiles are not supported")
    ));
}
