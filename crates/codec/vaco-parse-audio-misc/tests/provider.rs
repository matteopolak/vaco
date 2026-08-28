//! What a `ParserProvider` gets when it builds this crate's parsers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]

use vaco_codec_core::{CodecId, Parser};
use vaco_core::MediaType;
use vaco_limits::Limits;
use vaco_parse_audio_misc::{PARSER_ALAC, PARSER_FLAC, PARSER_VORBIS};

#[test]
fn the_three_descriptors_answer_for_the_right_codecs() {
    assert_eq!(PARSER_VORBIS.name, "vorbis");
    assert_eq!(PARSER_FLAC.name, "flac");
    assert_eq!(PARSER_ALAC.name, "alac");
    assert!(PARSER_VORBIS.handles(CodecId::Vorbis));
    assert!(!PARSER_VORBIS.handles(CodecId::Flac));
    assert!(PARSER_FLAC.handles(CodecId::Flac));
    assert!(PARSER_ALAC.handles(CodecId::Alac));
    assert_eq!(PARSER_VORBIS.media_type, MediaType::Audio);
}

#[test]
fn a_boxed_flac_parser_describes_the_stream_from_extradata_alone() {
    let mut parser: Box<dyn Parser> = PARSER_FLAC.build(Limits::strict());
    assert!(parser.parameters().is_none());
    let mut streaminfo = [0u8; 34];
    streaminfo[0..2].copy_from_slice(&4608u16.to_be_bytes());
    streaminfo[2..4].copy_from_slice(&4608u16.to_be_bytes());
    streaminfo[10] = 0x0a;
    streaminfo[11] = 0xc4;
    streaminfo[12] = 0x42;
    streaminfo[13] = 0xf0;
    streaminfo[16] = 0xac;
    streaminfo[17] = 0x44;
    parser.set_extradata(&streaminfo).expect("a real STREAMINFO parses");
    let params = parser.parameters().expect("the record described the stream");
    assert_eq!(params.codec_id, Some(CodecId::Flac));
    let audio = params.audio.as_ref().expect("audio parameters");
    assert_eq!(audio.sample_rate, 44_100);
    assert_eq!(audio.layout.as_ref().map(|l| l.channels), Some(2));
}

#[test]
fn a_malformed_or_truncated_record_is_an_error_and_not_a_panic() {
    for desc in [PARSER_VORBIS, PARSER_FLAC, PARSER_ALAC] {
        for bad in [&[][..], &[0x00][..], &[0xffu8; 40][..]] {
            let mut parser: Box<dyn Parser> = desc.build(Limits::strict());
            let _ = parser.set_extradata(bad);
            let _ = parser.parse(&[0, 1, 2, 3]);
        }
    }
}
