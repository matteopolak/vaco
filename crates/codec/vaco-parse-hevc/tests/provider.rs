//! What a `ParserProvider` gets when it builds this crate's parser.
//!
//! Everything goes through `dyn Parser`, because that is all the registry
//! hands a demuxer — a test written against `HevcParser` directly would pass
//! while the seam stayed broken.

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
use vaco_parse_hevc::PARSER;

/// An `x265` `hvcC`, box payload verbatim, with the prefix-SEI array trimmed.
const REAL_HVCC: &[u8] = &[
    0x01, 0x01, 0x60, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f, 0xf0, 0x00, 0xfc,
    0xfd, 0xf8, 0xf8, 0x00, 0x00, 0x0f, 0x03, //
    0xa0, 0x00, 0x01, 0x00, 0x18, //
    0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03,
    0x00, 0x00, 0x03, 0x00, 0x3f, 0x95, 0x98, 0x09, //
    0xa1, 0x00, 0x01, 0x00, 0x2a, //
    0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03,
    0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32, 0xbc, 0x05, 0xa0, 0x20,
    0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x01, //
    0xa2, 0x00, 0x01, 0x00, 0x07, //
    0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40,
];

#[test]
fn the_descriptor_says_what_it_parses() {
    assert_eq!(PARSER.name, "hevc");
    assert!(PARSER.handles(CodecId::Hevc));
    assert!(!PARSER.handles(CodecId::H264));
    assert_eq!(PARSER.media_type, MediaType::Video);
}

#[test]
fn a_boxed_parser_describes_the_stream_from_extradata_alone() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    assert!(parser.parameters().is_none());
    parser.set_extradata(REAL_HVCC).expect("a real hvcC parses");

    let params = parser
        .parameters()
        .expect("the record described the stream");
    assert_eq!(params.codec_id, Some(CodecId::Hevc));
    assert_eq!(params.profile.map(|p| p.name), Some("Main"));
    let v = params.video.as_ref().expect("video parameters");
    assert!(v.width > 0 && v.height > 0);
    assert_eq!(v.format.map(vaco_pixfmt::PixFmt::name), Some("yuv420p"));
    // D17: **not** set, unlike H.264's. Probed on the same 1918x1080 source
    // encoded twice: `bits_per_raw_sample="8"` for H.264, `"N/A"` for HEVC.
    assert_eq!(v.bits_per_raw_sample, None);
    // D17: `is_avc`/`nal_length_size` are H.264 private options and the
    // reference prints neither for HEVC, even from an `hvcC` that declares a
    // four-byte prefix. `None` is what keeps them out of the output.
    assert_eq!(v.nal_length_size, None);
}

/// A length-prefixed sample contains no start codes, so once the record has
/// declared the framing, `parse` must take the container path.
#[test]
fn a_length_prefixed_sample_is_read_as_one_access_unit() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    parser.set_extradata(REAL_HVCC).expect("hvcC");
    let slice = [0x26u8, 0x01, 0xaf, 0x06, 0x30, 0x40];
    let mut sample = (slice.len() as u32).to_be_bytes().to_vec();
    sample.extend_from_slice(&slice);
    let (packet, used) = parser.parse(&sample).expect("a sample parses");
    assert_eq!(used, sample.len());
    assert!(packet.is_some(), "one sample is one access unit");
}

#[test]
fn a_malformed_or_truncated_record_is_an_error_and_not_a_panic() {
    for n in 0..=REAL_HVCC.len() {
        let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
        let _ = parser.set_extradata(&REAL_HVCC[..n]);
        let _ = parser.parse(&[0, 0, 0, 1, 0x42]);
    }
    for bad in [&[0xFFu8][..], &[0u8; 64][..]] {
        let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
        let _ = parser.set_extradata(bad);
    }
}
