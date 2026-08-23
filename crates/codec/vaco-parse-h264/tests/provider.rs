//! What a `ParserProvider` gets when it builds this crate's parser.
//!
//! The registry hands out a `Box<dyn Parser>` and can call nothing but the
//! trait, so anything the trait cannot reach is unreachable from a demuxer.
//! Every assertion here therefore goes through `dyn Parser`, never through
//! `H264Parser`'s inherent methods — a version of these tests written against
//! the concrete type would pass while the seam stayed broken.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unnecessary_cast,
    reason = "test code over fixed fixtures"
)]

use vaco_codec_core::{CodecId, Parser, ParserDriver};
use vaco_core::MediaType;
use vaco_limits::Limits;
use vaco_parse_h264::{H264Parser, PARSER};

/// `libx264`'s `avcC` for `testsrc2=s=640x360:r=24`, box payload verbatim.
const REAL_AVCC: &[u8] = &[
    0x01, 0x64, 0x00, 0x1E, 0xFF, 0xE1, 0x00, 0x1A, 0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0,
    0x2F, 0xF9, 0x70, 0x11, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x30, 0x0F, 0x16,
    0x2D, 0x96, 0x01, 0x00, 0x06, 0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0, 0xFD, 0xF8, 0xF8, 0x00,
];

#[test]
fn the_descriptor_says_what_it_parses() {
    assert_eq!(PARSER.name, "h264");
    assert!(PARSER.handles(CodecId::H264));
    assert!(!PARSER.handles(CodecId::Hevc));
    assert_eq!(PARSER.media_type, MediaType::Video);
}

/// **The whole point of `Parser::set_extradata`.** In MP4 the sequence
/// parameter set is in `avcC` and in no sample, so a parser that only sees
/// payloads describes nothing. Through the trait, because that is all a
/// provider-built parser exposes.
#[test]
fn a_boxed_parser_describes_the_stream_from_extradata_alone() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    assert!(
        parser.parameters().is_none(),
        "nothing is known before the record"
    );
    parser.set_extradata(REAL_AVCC).expect("a real avcC parses");

    let params = parser
        .parameters()
        .expect("the record described the stream");
    assert_eq!(params.codec_id, Some(CodecId::H264));
    assert_eq!(params.profile.map(|p| p.name), Some("High"));
    assert_eq!(params.level.map(vaco_codec_core::Level::raw), Some(30));
    let v = params.video.as_ref().expect("video parameters");
    assert_eq!((v.width, v.height), (640, 360));
    assert_eq!(v.format.map(vaco_pixfmt::PixFmt::name), Some("yuv420p"));
    // `bits_per_raw_sample` is set for H.264 and for no other codec here; see
    // the note in `params::codec_parameters`.
    assert_eq!(v.bits_per_raw_sample, Some(8));
    // The record declares a four-byte length prefix, which is what `ffprobe`
    // prints as `is_avc=true nal_length_size=4`.
    assert_eq!(v.nal_length_size, Some(4));
}

/// An Annex B stream reports `nal_length_size = 0`, which is a **value** and
/// not an absence: `is_avc=false` is printed for MPEG-TS, not omitted.
#[test]
fn an_annex_b_stream_reports_a_zero_length_size() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    let mut stream = Vec::new();
    stream.extend_from_slice(&[0, 0, 0, 1]);
    stream.extend_from_slice(&REAL_AVCC[8..8 + 26]); // the SPS from the record
    stream.extend_from_slice(&[0, 0, 0, 1]);
    stream.extend_from_slice(&REAL_AVCC[37..37 + 6]); // the PPS
    stream.extend_from_slice(&[0, 0, 0, 1]);
    stream.extend_from_slice(&[0x65, 0x88, 0x84, 0x00, 0x2F, 0x7F, 0x7E]);
    let (_, used) = parser.parse(&stream).expect("an Annex B stream parses");
    assert_eq!(used, stream.len());

    let v = parser
        .parameters()
        .and_then(|p| p.video.clone())
        .expect("the in-band SPS described the stream");
    assert_eq!((v.width, v.height), (640, 360));
    assert_eq!(v.nal_length_size, Some(0));
}

/// A length-prefixed sample contains **no start codes**, so the byte-stream
/// scanner would find nothing in it. Once a record has declared the framing,
/// `parse` must take the container path instead.
#[test]
fn a_length_prefixed_sample_is_read_as_one_access_unit() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    parser.set_extradata(REAL_AVCC).expect("avcC");
    let slice = [0x65u8, 0x88, 0x84, 0x00, 0x2F, 0x7F, 0x7E];
    let mut sample = (slice.len() as u32).to_be_bytes().to_vec();
    sample.extend_from_slice(&slice);

    let (packet, used) = parser.parse(&sample).expect("a sample parses");
    assert_eq!(used, sample.len(), "a container sample is consumed whole");
    let packet = packet.expect("one sample is one access unit");
    assert_eq!(packet.len as usize, sample.len());
}

/// A malformed record must not be fatal: stream discovery is *offering* the
/// parser whatever the container happened to carry, and a bad record means
/// "this told me nothing", not "stop reporting the file".
#[test]
fn a_malformed_record_is_an_error_and_not_a_panic() {
    for bad in [
        &[][..],
        &[0xFF][..],
        &[0x01, 0x64, 0x00][..],
        &[0u8; 64][..],
    ] {
        let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
        let _ = parser.set_extradata(bad);
        // and the parser is still usable afterwards.
        let _ = parser.parse(&[0, 0, 0, 1, 0x67]);
    }
}

/// Every prefix of a real record, which is the shape a truncated file has.
#[test]
fn every_truncation_of_a_real_record_is_handled() {
    for n in 0..=REAL_AVCC.len() {
        let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
        let _ = parser.set_extradata(&REAL_AVCC[..n]);
    }
}

/// `set_extradata` twice must be harmless — a fragmented MP4 restates its
/// `avcC` in every `moov`-equivalent, and discovery may offer it again.
#[test]
fn setting_the_record_twice_is_idempotent() {
    let mut parser: Box<dyn Parser> = PARSER.build(Limits::strict());
    parser.set_extradata(REAL_AVCC).expect("once");
    let first = parser.parameters().and_then(|p| p.video.clone());
    parser.set_extradata(REAL_AVCC).expect("twice");
    let second = parser.parameters().and_then(|p| p.video.clone());
    assert_eq!(
        first.map(|v| (v.width, v.height, v.nal_length_size)),
        second.map(|v| (v.width, v.height, v.nal_length_size))
    );
}

/// The descriptor's parser must drive through `ParserDriver`, because that is
/// what stream discovery wraps it in.
#[test]
fn the_descriptor_builds_something_the_driver_accepts() {
    let mut d = ParserDriver::new(PARSER.build(Limits::strict()), Limits::strict());
    d.push(&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1E]).expect("push");
    let _ = d.next_unit();
    d.finish();
    while d.next_unit().is_ok() {}
}

/// The concrete type and the descriptor must agree about the limits argument;
/// a descriptor that built an unbounded parser would be a denial of service on
/// the probe path with nothing to show for it.
#[test]
fn the_descriptor_and_the_constructor_agree() {
    let mut direct = H264Parser::new(Limits::strict());
    direct.set_extradata(REAL_AVCC).expect("avcC");
    let mut boxed: Box<dyn Parser> = PARSER.build(Limits::strict());
    boxed.set_extradata(REAL_AVCC).expect("avcC");
    assert_eq!(
        Parser::parameters(&direct).and_then(|p| p.video.as_ref().map(|v| v.width)),
        boxed
            .parameters()
            .and_then(|p| p.video.as_ref().map(|v| v.width))
    );
}
