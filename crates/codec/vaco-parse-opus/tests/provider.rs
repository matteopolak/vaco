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
use vaco_parse_opus::PARSER;

/// The `OpusHead` of a 48 kHz stereo file, exactly as `libopus` wrote it.
const HEAD_STEREO: &[u8] = &[
    0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x02, 0x38, 0x01, 0x80, 0xbb, 0x00, 0x00,
    0x00, 0x00, 0x00,
];

#[test]
fn the_descriptor_says_what_it_parses() {
    assert_eq!(PARSER.name, "opus");
    assert!(PARSER.handles(CodecId::Opus));
    assert_eq!(PARSER.media_type, MediaType::Audio);
}

/// **Opus has no in-band configuration at all.** The channel count, the
/// pre-skip and the mapping live only in the identification header the
/// container carries, so for this codec `set_extradata` is not an optimisation
/// — it is the only way a parser can describe the stream.
#[test]
fn every_reported_field_comes_from_the_identification_header() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    // A valid Opus packet on its own establishes nothing.
    let (packet, _) = parser.parse(&[0xfc, 0xff, 0xfe]).expect("a packet");
    assert!(packet.is_some());
    assert!(
        parser.parameters().is_none(),
        "a packet cannot describe an Opus stream"
    );

    parser.set_extradata(HEAD_STEREO).expect("a real OpusHead");
    let params = parser
        .parameters()
        .expect("the header described the stream");
    assert_eq!(params.codec_id, Some(CodecId::Opus));
    let a = params.audio.as_ref().expect("audio parameters");
    assert_eq!(a.sample_rate, 48_000);
    assert_eq!(a.layout.as_ref().map(|l| l.channels), Some(2));
    assert_eq!(a.format.map(vaco_sampfmt::SampleFmt::name), Some("fltp"));
    // `initial_padding` is the `pre_skip`, and stream discovery uses it to
    // derive `start_time` — the field the reference reports as 0 rather than
    // as the first packet's negative pts.
    assert_eq!(a.initial_padding, 312);
}

#[test]
fn a_malformed_or_truncated_header_is_an_error_and_not_a_panic() {
    for n in 0..=HEAD_STEREO.len() {
        let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
        let _ = parser.set_extradata(&HEAD_STEREO[..n]);
        let _ = parser.parse(&[0xfc, 0xff, 0xfe]);
    }
    for bad in [&[0xFFu8][..], &[0u8; 64][..]] {
        let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
        let _ = parser.set_extradata(bad);
    }
}
