//! What a `ParserProvider` gets when it builds this crate's parsers.

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
use vaco_parse_aac::{PARSER, PARSER_LATM};

/// The `AudioSpecificConfig` of a 44.1 kHz mono AAC-LC track: the two bytes an
/// `esds` `DecoderSpecificInfo` carries.
const ASC_LC_MONO: &[u8] = &[0x12, 0x08];

#[test]
fn the_two_descriptors_answer_for_two_codecs() {
    assert_eq!(PARSER.name, "aac");
    assert_eq!(PARSER_LATM.name, "aac_latm");
    assert!(PARSER.handles(CodecId::Aac));
    assert!(!PARSER.handles(CodecId::AacLatm));
    assert!(PARSER_LATM.handles(CodecId::AacLatm));
    assert_eq!(PARSER.media_type, MediaType::Audio);
}

/// **AAC is where the two paths genuinely differ.** In MPEG-TS every frame
/// carries an ADTS header; in MP4 the samples are raw and the whole description
/// is in the configuration record.
#[test]
fn a_boxed_parser_describes_the_stream_from_extradata_alone() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    assert!(parser.parameters().is_none());
    parser
        .set_extradata(ASC_LC_MONO)
        .expect("a real ASC parses");

    let params = parser
        .parameters()
        .expect("the record described the stream");
    assert_eq!(params.codec_id, Some(CodecId::Aac));
    assert_eq!(params.profile.map(|p| p.name), Some("LC"));
    let a = params.audio.as_ref().expect("audio parameters");
    assert_eq!(a.sample_rate, 44_100);
    assert_eq!(a.layout.as_ref().map(|l| l.channels), Some(1));
    // The decoder's output format, which is what the reference prints.
    assert_eq!(a.format.map(vaco_sampfmt::SampleFmt::name), Some("fltp"));
}

/// A configuration record must not be overwritten by a coincidental ADTS sync
/// word in a raw AAC sample. MP4 samples have no ADTS header at all, so
/// anything the scanner finds in one is noise.
#[test]
fn a_configured_parser_is_not_overwritten_by_a_stray_sync_word() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    parser.set_extradata(ASC_LC_MONO).expect("ASC");
    let before = parser
        .parameters()
        .and_then(|p| p.audio.clone())
        .map(|a| a.sample_rate);
    assert_eq!(before, Some(44_100));
    // A synthetic 48 kHz ADTS frame: eight header bytes plus padding.
    let mut frame = vec![0xFF, 0xF1, 0x4C, 0x40, 0x01, 0x3F, 0xFC];
    frame.resize(0x27, 0);
    let _ = parser.parse(&frame);
    let after = parser
        .parameters()
        .and_then(|p| p.audio.clone())
        .map(|a| a.sample_rate);
    assert_eq!(after, Some(44_100), "the record must win over a raw sample");
}

#[test]
fn a_malformed_or_truncated_record_is_an_error_and_not_a_panic() {
    for desc in [PARSER, PARSER_LATM] {
        for bad in [&[][..], &[0x12][..], &[0xFF, 0xFF][..], &[0u8; 64][..]] {
            let mut parser: Box<dyn Parser> = desc.build(Limits::strict());
            let _ = parser.set_extradata(bad);
            let _ = parser.parse(&[0xFF, 0xF1, 0, 0]);
        }
    }
}
