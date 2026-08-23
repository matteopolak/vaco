//! What a `ParserProvider` gets when it builds this crate's parser.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unnecessary_cast,
    reason = "test code over fixed fixtures"
)]

use vaco_codec_core::{CodecId, Parser};
use vaco_core::MediaType;
use vaco_limits::Limits;
use vaco_parse_av1::PARSER;

/// The `av1C` payload of an `libsvtav1` MP4: 642x358, yuv420p, level 2.1.
const REAL_AV1C: &[u8] = &[
    0x81, 0x01, 0x0c, 0x00, 0x0a, 0x0b, 0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00,
    0x10,
];

#[test]
fn the_descriptor_says_what_it_parses() {
    assert_eq!(PARSER.name, "av1");
    assert!(PARSER.handles(CodecId::Av1));
    assert_eq!(PARSER.media_type, MediaType::Video);
}

/// AV1 differs from H.264 and HEVC in three ways that were measured rather than
/// assumed, and all three are asserted here so a change shows up as a failure.
#[test]
fn a_boxed_parser_describes_the_stream_from_extradata_alone() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    assert!(parser.parameters().is_none());
    parser.set_extradata(REAL_AV1C).expect("a real av1C parses");

    let params = parser
        .parameters()
        .expect("the record described the stream");
    assert_eq!(params.codec_id, Some(CodecId::Av1));
    assert_eq!(params.profile.map(|p| p.name), Some("Main"));
    let v = params.video.as_ref().expect("video parameters");
    // 1. No coded/display split: `coded_width` is the frame width, full stop.
    assert_eq!(v.coded_width, v.width);
    assert_eq!(v.coded_height, v.height);
    // 2. No `yuvj` family, at any range.
    assert_eq!(v.format.map(vaco_pixfmt::PixFmt::name), Some("yuv420p"));
    // 3. Neither of H.264's two extra fields.
    assert_eq!(v.bits_per_raw_sample, None);
    assert_eq!(v.nal_length_size, None);
}

/// AV1's low-overhead bitstream format is the same OBU stream in MP4 as in a
/// raw file, so unlike H.264 and HEVC there is no framing switch to make.
#[test]
fn the_packet_path_needs_no_framing_switch() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    parser.set_extradata(REAL_AV1C).expect("av1C");
    let before = parser
        .parameters()
        .and_then(|p| p.video.clone())
        .map(|v| v.width);
    let (_, used) = parser.parse(&[0x12, 0x00]).expect("a temporal delimiter");
    assert_eq!(used, 2);
    let after = parser
        .parameters()
        .and_then(|p| p.video.clone())
        .map(|v| v.width);
    assert_eq!(before, after);
}

#[test]
fn a_malformed_or_truncated_record_is_an_error_and_not_a_panic() {
    for n in 0..=REAL_AV1C.len() {
        let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
        let _ = parser.set_extradata(&REAL_AV1C[..n]);
        let _ = parser.parse(&[0x12, 0x00]);
    }
    for bad in [&[0xFFu8][..], &[0u8; 64][..]] {
        let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
        let _ = parser.set_extradata(bad);
    }
}
