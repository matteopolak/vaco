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
use vaco_parse_mpegaudio::{PARSER_AC3, PARSER_EAC3, PARSER_MPEGAUDIO};

#[test]
fn the_three_descriptors_answer_for_the_right_codecs() {
    assert_eq!(PARSER_MPEGAUDIO.name, "mp3");
    assert_eq!(PARSER_AC3.name, "ac3");
    assert_eq!(PARSER_EAC3.name, "eac3");
    assert!(PARSER_MPEGAUDIO.handles(CodecId::Mp3));
    assert!(PARSER_MPEGAUDIO.handles(CodecId::Mp2));
    assert!(PARSER_MPEGAUDIO.handles(CodecId::Mp1));
    assert!(PARSER_AC3.handles(CodecId::Ac3));
    assert!(!PARSER_AC3.handles(CodecId::Eac3));
    assert!(PARSER_EAC3.handles(CodecId::Eac3));
    assert_eq!(PARSER_MPEGAUDIO.media_type, MediaType::Audio);
}

#[test]
fn a_boxed_mp3_parser_describes_a_frame_it_parses() {
    let mut parser: Box<dyn Parser> = PARSER_MPEGAUDIO.build(Limits::strict());
    assert!(parser.parameters().is_none());
    let mut frame = vec![0u8; 417];
    frame[0] = 0xff;
    frame[1] = 0xfb;
    frame[2] = 0x90;
    frame[3] = 0x00;
    let mut two = frame.clone();
    two.extend_from_slice(&frame);
    let (packet, used) = parser.parse(&two).expect("valid frame pair");
    assert!(packet.is_some());
    assert_eq!(used, 417);
    let params = parser.parameters().expect("header described the stream");
    assert_eq!(params.codec_id, Some(CodecId::Mp3));
}

#[test]
fn a_boxed_ac3_parser_describes_a_frame_it_parses() {
    let mut parser: Box<dyn Parser> = PARSER_AC3.build(Limits::strict());
    let mut frame = vec![0u8; 768];
    frame[0] = 0x0b;
    frame[1] = 0x77;
    frame[4] = 20;
    frame[5] = 8 << 3;
    frame[6] = 0xe1;
    let mut two = frame.clone();
    two.extend_from_slice(&frame);
    let (packet, used) = parser.parse(&two).expect("valid frame pair");
    assert!(packet.is_some());
    assert_eq!(used, 768);
    let params = parser.parameters().expect("header described the stream");
    assert_eq!(params.codec_id, Some(CodecId::Ac3));
}

#[test]
fn a_malformed_stream_is_an_error_and_not_a_panic() {
    for desc in [PARSER_MPEGAUDIO, PARSER_AC3, PARSER_EAC3] {
        for bad in [&[][..], &[0xff][..], &[0u8; 4][..], &[0xffu8; 4096][..]] {
            let mut parser: Box<dyn Parser> = desc.build(Limits::strict());
            let _ = parser.parse(bad);
            let _ = parser.parse(&[]);
        }
    }
}
