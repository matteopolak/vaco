//! Unit and property tests.
//!
//! # Where the expected values came from
//!
//! Every `// measured:` comment records an `ffprobe 8.1` observation. The
//! identification-header cases were produced by rewriting the `OpusHead` packet
//! of a real Ogg Opus file in place and recomputing the page CRC — the shortest
//! path to the header parser, per plan 13 §1b — then reading back
//! `-show_streams`. The harness is described in `docs/codec/vaco-parse-opus.md`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test that unwraps a None is a failing test, which is the \
              correct outcome; the lints exist to stop library code panicking \
              on hostile input"
)]

use proptest::prelude::*;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::Parser;
use vaco_core::Error;
use vaco_limits::Limits;

use crate::comment::CommentHeader;
use crate::head::{IdentificationHeader, MappingFamily, OUTPUT_SAMPLE_RATE, ambisonic_order};
use crate::packet::{Bandwidth, Mode, OpusPacket, Toc};
use crate::{OpusParser, split_streams};

// ------------------------------------------------------------------ fixtures

/// The `OpusHead` of a 48 kHz stereo file, exactly as `libopus` wrote it.
const HEAD_STEREO: [u8; 19] = [
    0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x02, 0x38, 0x01, 0x80, 0xbb, 0x00, 0x00,
    0x00, 0x00, 0x00,
];

/// The `OpusHead` of a 5.1 file: mapping family 1, four streams, two coupled.
const HEAD_51: [u8; 27] = [
    0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x06, 0x38, 0x01, 0x80, 0xbb, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x04, 0x02, 0x00, 0x04, 0x01, 0x02, 0x03, 0x05,
];

fn head(bytes: &[u8]) -> IdentificationHeader {
    IdentificationHeader::parse(bytes).unwrap_or_else(|e| panic!("{bytes:02x?}: {e}"))
}

/// Build an `OpusHead` with the given fields, so a test can vary one at a time.
fn build(channels: u8, family: u8, streams: u8, coupled: u8, mapping: &[u8]) -> Vec<u8> {
    let mut out = b"OpusHead".to_vec();
    out.push(1);
    out.push(channels);
    out.extend(312u16.to_le_bytes());
    out.extend(48000u32.to_le_bytes());
    out.extend(0i16.to_le_bytes());
    out.push(family);
    if family != 0 {
        out.push(streams);
        out.push(coupled);
        out.extend_from_slice(mapping);
    }
    out
}

fn identity(channels: u8) -> Vec<u8> {
    (0..channels).collect()
}

// ------------------------------------------------------- identification header

#[test]
fn stereo_head() {
    // measured: sample_rate=48000 channels=2 channel_layout=stereo
    //           initial_padding=312 extradata_size=19
    let h = head(&HEAD_STEREO);
    assert_eq!(h.version, 1);
    assert_eq!(h.channel_count, 2);
    assert_eq!(h.pre_skip, 312);
    assert_eq!(h.input_sample_rate, 48000);
    assert_eq!(h.output_gain_q8, 0);
    assert_eq!(h.mapping_family, MappingFamily::Rtp);
    assert_eq!(h.stream_count, 1);
    assert_eq!(h.coupled_count, 1);
    assert_eq!(h.channel_layout(), Some(ChannelLayout::STEREO));

    let params = h.to_codec_parameters();
    let audio = params.audio.expect("audio parameters");
    assert_eq!(audio.sample_rate, OUTPUT_SAMPLE_RATE);
    assert_eq!(audio.initial_padding, 312);
}

#[test]
fn multichannel_head() {
    // measured: channels=6 channel_layout=5.1 extradata_size=27
    let h = head(&HEAD_51);
    assert_eq!(h.mapping_family, MappingFamily::Vorbis);
    assert_eq!(h.stream_count, 4);
    assert_eq!(h.coupled_count, 2);
    assert_eq!(h.channel_mapping.as_slice(), &[0, 4, 1, 2, 3, 5]);
    assert_eq!(
        h.channel_layout().as_ref().and_then(ChannelLayout::name),
        Some("5.1")
    );
}

#[test]
fn input_sample_rate_is_informational() {
    // measured: an OpusHead declaring 8000 still reports sample_rate=48000.
    // RFC 7845 §5.1 — Opus always decodes at 48 kHz.
    let mut bytes = HEAD_STEREO;
    bytes[12..16].copy_from_slice(&8000u32.to_le_bytes());
    let h = head(&bytes);
    assert_eq!(h.input_sample_rate, 8000);
    assert_eq!(
        h.to_codec_parameters().audio.map(|a| a.sample_rate),
        Some(48000)
    );
}

#[test]
fn only_the_major_version_is_checked() {
    // measured: versions 0x00..=0x0f are accepted, 0x10 and above are rejected
    // with `Header processing failed` from the Ogg demuxer.
    for version in 0..=0x0fu8 {
        let mut bytes = HEAD_STEREO;
        bytes[8] = version;
        assert!(IdentificationHeader::parse(&bytes).is_ok(), "{version:#x}");
    }
    for version in [0x10u8, 0x20, 0xff] {
        let mut bytes = HEAD_STEREO;
        bytes[8] = version;
        assert!(IdentificationHeader::parse(&bytes).is_err(), "{version:#x}");
    }
}

#[test]
fn zero_channels_is_rejected() {
    // measured: `Zero channel count specified in the extradata`
    let mut bytes = HEAD_STEREO;
    bytes[9] = 0;
    assert!(IdentificationHeader::parse(&bytes).is_err());
}

#[test]
fn family_0_allows_at_most_two_channels() {
    // measured: `Channel mapping 0 is only specified for up to 2 channels`
    for channels in [1u8, 2] {
        assert!(IdentificationHeader::parse(&build(channels, 0, 0, 0, &[])).is_ok());
    }
    for channels in [3u8, 8, 255] {
        assert!(IdentificationHeader::parse(&build(channels, 0, 0, 0, &[])).is_err());
    }
}

#[test]
fn family_1_layouts_are_vorbis_order() {
    // measured, one channel count at a time:
    //   1 mono | 2 stereo | 3 3.0 | 4 quad | 5 5.0 | 6 5.1 | 7 6.1 | 8 7.1
    // Note 4 is `quad` and 7 is `6.1`, neither of which is what an AAC stream
    // of the same channel count reports.
    let expected = ["mono", "stereo", "3.0", "quad", "5.0", "5.1", "6.1", "7.1"];
    for (index, name) in expected.into_iter().enumerate() {
        let channels = u8::try_from(index + 1).unwrap();
        let bytes = build(channels, 1, channels, 0, &identity(channels));
        let h = head(&bytes);
        assert_eq!(
            h.channel_layout().as_ref().and_then(ChannelLayout::name),
            Some(name),
            "{channels} channels"
        );
    }
    // measured: `Channel mapping 1 is only specified for up to 8 channels`
    let bytes = build(9, 1, 9, 0, &identity(9));
    assert!(IdentificationHeader::parse(&bytes).is_err());
}

#[test]
fn family_255_has_no_layout() {
    // measured: `channel_layout` is absent from -show_streams for family 255,
    // which means the layout is unspecified rather than guessed.
    for channels in [1u8, 2, 3, 6, 8] {
        let bytes = build(channels, 255, channels, 0, &identity(channels));
        let h = head(&bytes);
        let layout = h.channel_layout().expect("layout");
        assert_eq!(layout.channels, u32::from(channels));
        assert_eq!(layout.name(), None, "{channels} channels");
    }
}

#[test]
fn family_2_is_ambisonic() {
    // measured: 1 -> `ambisonic 0`, 3 -> `ambisonic 0+stereo`,
    //           4 -> `ambisonic 1`, 6 -> `ambisonic 1+stereo`,
    //           9 -> `ambisonic 2`, 11 -> `ambisonic 2+stereo`,
    //           16 -> `ambisonic 3`, 25 -> `ambisonic 4`,
    //           27 -> `ambisonic 4+stereo`.
    let expected = [
        (1u8, "ambisonic 0"),
        (3, "ambisonic 0+stereo"),
        (4, "ambisonic 1"),
        (6, "ambisonic 1+stereo"),
        (9, "ambisonic 2"),
        (11, "ambisonic 2+stereo"),
        (16, "ambisonic 3"),
        (25, "ambisonic 4"),
        (27, "ambisonic 4+stereo"),
    ];
    for (channels, description) in expected {
        let bytes = build(channels, 2, channels, 0, &identity(channels));
        let h = head(&bytes);
        assert_eq!(
            h.channel_layout().map(|l| l.describe()),
            Some(description.to_owned()),
            "{channels} channels"
        );
    }
    // measured: `Channel mapping 2 is only specified for channel counts which
    // are (n + 1)^2 or (n + 1)^2 + 2`
    for channels in [2u8, 5, 7, 8, 10, 17, 26] {
        assert_eq!(ambisonic_order(channels), None, "{channels}");
        let bytes = build(channels, 2, channels, 0, &identity(channels));
        assert!(IdentificationHeader::parse(&bytes).is_err(), "{channels}");
    }
}

#[test]
fn family_3_is_unsupported_not_invalid() {
    // measured: `Mapping type 3 is not implemented.` — the file is fine, the
    // reference is not, and neither are we.
    let bytes = build(4, 3, 4, 0, &identity(4));
    assert!(matches!(
        IdentificationHeader::parse(&bytes),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn stream_counts_are_validated() {
    // measured: `Invalid stream/stereo stream count: 0/0` and `1/2`
    assert!(IdentificationHeader::parse(&build(2, 1, 0, 0, &[0, 1])).is_err());
    assert!(IdentificationHeader::parse(&build(2, 1, 1, 2, &[0, 1])).is_err());
    // measured: 255/1 is rejected because the pair would need 256 channels.
    assert!(IdentificationHeader::parse(&build(2, 255, 255, 1, &[0, 1])).is_err());
}

#[test]
fn mapping_indices_are_range_checked_except_255() {
    // measured: `Invalid channel map for output channel 0: 9`
    assert!(IdentificationHeader::parse(&build(2, 1, 1, 1, &[9, 9])).is_err());
    // measured: a mapping of 255 is accepted — RFC 7845 §5.1.1 makes it the
    // "this output channel is silent" escape.
    assert!(IdentificationHeader::parse(&build(2, 1, 1, 1, &[255, 255])).is_ok());
}

#[test]
fn dops_is_the_same_fields_big_endian() {
    // The MP4 carriage of the same stereo stream, from a real `dOps` box.
    let dops = [
        0x00u8, 0x02, 0x01, 0x38, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x00, 0x00,
    ];
    let h = IdentificationHeader::parse_dops(&dops).expect("dOps");
    assert_eq!(h.channel_count, 2);
    assert_eq!(h.pre_skip, 312);
    assert_eq!(h.input_sample_rate, 48000);
    // measured: the reference reports extradata_size=19 for an Opus track in
    // MP4, i.e. it converts `dOps` back into an `OpusHead`. Ours differs only
    // in the version octet, which `dOps` defines as 0 and `OpusHead` as 1.
    let mut expected = HEAD_STEREO;
    expected[8] = 0;
    assert_eq!(h.to_opus_head().as_slice(), expected.as_slice());
}

#[test]
fn heads_round_trip_through_opus_head() {
    for bytes in [HEAD_STEREO.as_slice(), HEAD_51.as_slice()] {
        let h = head(bytes);
        assert_eq!(h.to_opus_head().as_slice(), bytes);
    }
}

#[test]
fn truncated_heads_never_panic() {
    for n in 0..HEAD_51.len() {
        let _ = IdentificationHeader::parse(&HEAD_51[..n]);
        let _ = IdentificationHeader::parse_dops(&HEAD_51[..n]);
    }
}

// -------------------------------------------------------------- comment header

#[test]
fn comment_header() {
    let mut data = b"OpusTags".to_vec();
    data.extend(5u32.to_le_bytes());
    data.extend_from_slice(b"vendo");
    data.extend(2u32.to_le_bytes());
    data.extend(9u32.to_le_bytes());
    data.extend_from_slice(b"TITLE=abc");
    data.extend(20u32.to_le_bytes());
    data.extend_from_slice(b"R128_TRACK_GAIN=-256");

    let header = CommentHeader::parse(&data).expect("comment header");
    assert_eq!(header.vendor, "vendo");
    assert_eq!(header.len(), 2);
    assert_eq!(header.get("title"), Some("abc"));
    assert_eq!(header.r128_track_gain(), Some(-256));
    assert_eq!(header.iter().count(), 2);
    assert!(header.trailing.is_empty());
}

#[test]
fn comment_lengths_are_bounded_by_the_packet() {
    // A four-byte count can claim four billion comments in a twenty-byte
    // packet. It must error, not allocate and not spin.
    let mut data = b"OpusTags".to_vec();
    data.extend(0u32.to_le_bytes());
    data.extend(u32::MAX.to_le_bytes());
    assert!(CommentHeader::parse(&data).is_err());

    // A single comment claiming more bytes than exist.
    let mut data = b"OpusTags".to_vec();
    data.extend(0u32.to_le_bytes());
    data.extend(1u32.to_le_bytes());
    data.extend(1_000_000u32.to_le_bytes());
    data.extend_from_slice(b"short");
    assert!(CommentHeader::parse(&data).is_err());
}

#[test]
fn truncated_comment_headers_never_panic() {
    let mut data = b"OpusTags".to_vec();
    data.extend(3u32.to_le_bytes());
    data.extend_from_slice(b"abc");
    data.extend(1u32.to_le_bytes());
    data.extend(3u32.to_le_bytes());
    data.extend_from_slice(b"A=1");
    for n in 0..data.len() {
        let _ = CommentHeader::parse(&data[..n]);
    }
}

// ---------------------------------------------------------------------- TOC

#[test]
fn toc_frame_durations_match_the_reference() {
    // measured: encoding with -frame_duration 2.5/5/10/20/40/60 produces
    // packet durations of 120/240/480/960/1920/2880 at time_base 1/48000, with
    // TOC bytes 0xe4, 0xfc, 0xfd and 0xff.
    assert_eq!(Toc(0xe4).frame_samples(), 120); // config 28, CELT FB 2.5 ms
    assert_eq!(Toc(0xfc).frame_samples(), 960); // config 31, CELT FB 20 ms
    assert_eq!(Toc(0xfc).mode(), Mode::CeltOnly);
    assert_eq!(Toc(0xfc).bandwidth(), Bandwidth::Fullband);
    assert!(Toc(0xfc).is_stereo());

    // The whole of RFC 6716 Table 2, config by config.
    let silk = [480u32, 960, 1920, 2880];
    for config in 0u8..12 {
        assert_eq!(
            Toc(config << 3).frame_samples(),
            silk[usize::from(config % 4)]
        );
        assert_eq!(Toc(config << 3).mode(), Mode::SilkOnly);
    }
    for config in 12u8..16 {
        let expected = if config % 2 == 0 { 480 } else { 960 };
        assert_eq!(Toc(config << 3).frame_samples(), expected);
        assert_eq!(Toc(config << 3).mode(), Mode::Hybrid);
    }
    let celt = [120u32, 240, 480, 960];
    for config in 16u8..32 {
        assert_eq!(
            Toc(config << 3).frame_samples(),
            celt[usize::from(config % 4)]
        );
        assert_eq!(Toc(config << 3).mode(), Mode::CeltOnly);
    }

    let bandwidths = [
        (0u8, Bandwidth::Narrowband),
        (4, Bandwidth::Mediumband),
        (8, Bandwidth::Wideband),
        (12, Bandwidth::SuperWideband),
        (14, Bandwidth::Fullband),
        (16, Bandwidth::Narrowband),
        (20, Bandwidth::Wideband),
        (24, Bandwidth::SuperWideband),
        (28, Bandwidth::Fullband),
    ];
    for (config, bandwidth) in bandwidths {
        assert_eq!(Toc(config << 3).bandwidth(), bandwidth, "config {config}");
    }
}

// ------------------------------------------------------------------- framing

#[test]
fn code_0_is_one_frame() {
    let packet = OpusPacket::parse(&[0xfc, 1, 2, 3]).expect("code 0");
    assert_eq!(packet.frames.len(), 1);
    assert_eq!(packet.frames[0], &[1, 2, 3]);
    assert_eq!(packet.samples(), 960);
    assert_eq!(packet.len, 4);
}

#[test]
fn code_1_is_two_equal_frames() {
    let packet = OpusPacket::parse(&[0xfd, 1, 2, 3, 4]).expect("code 1");
    assert_eq!(packet.frames.len(), 2);
    assert_eq!(packet.frames[0], &[1, 2]);
    assert_eq!(packet.frames[1], &[3, 4]);
    assert_eq!(packet.samples(), 1920);
    // An odd payload cannot be split in two.
    assert!(OpusPacket::parse(&[0xfd, 1, 2, 3]).is_err());
}

#[test]
fn code_2_carries_the_first_frame_length() {
    let packet = OpusPacket::parse(&[0xfe, 2, 1, 2, 3, 4, 5]).expect("code 2");
    assert_eq!(packet.frames.len(), 2);
    assert_eq!(packet.frames[0], &[1, 2]);
    assert_eq!(packet.frames[1], &[3, 4, 5]);
    // A first length longer than the packet must not underflow.
    assert!(OpusPacket::parse(&[0xfe, 200, 1, 2]).is_err());
}

#[test]
fn code_3_cbr() {
    // measured: a 60 ms packet is TOC 0xff followed by 0x03 — CBR, no padding,
    // three 20 ms frames.
    let packet = OpusPacket::parse(&[0xff, 0x03, 1, 2, 3, 4, 5, 6]).expect("code 3 CBR");
    assert_eq!(packet.frames.len(), 3);
    assert_eq!(packet.samples(), 2880);
    assert_eq!(packet.frames[2], &[5, 6]);
    // A payload that does not divide by the frame count is malformed.
    assert!(OpusPacket::parse(&[0xff, 0x03, 1, 2, 3, 4, 5]).is_err());
}

#[test]
fn code_3_vbr() {
    // VBR, no padding, three frames: two explicit lengths then the remainder.
    let packet = OpusPacket::parse(&[0xff, 0x83, 1, 2, 9, 8, 7, 6, 5]).expect("code 3 VBR");
    assert_eq!(packet.frames.len(), 3);
    assert_eq!(packet.frames[0], &[9]);
    assert_eq!(packet.frames[1], &[8, 7]);
    assert_eq!(packet.frames[2], &[6, 5]);
    // Lengths that claim more than the packet holds must not underflow.
    assert!(OpusPacket::parse(&[0xff, 0x83, 200, 200, 1, 2]).is_err());
}

#[test]
fn code_3_padding() {
    // VBR + padding: 2 frames, one explicit length, 3 padding bytes.
    let packet =
        OpusPacket::parse(&[0xff, 0xc2, 3, 2, 1, 2, 3, 4, 0, 0, 0]).expect("code 3 padded");
    assert_eq!(packet.padding, 3);
    assert_eq!(packet.frames.len(), 2);
    assert_eq!(packet.frames[0], &[1, 2]);
    assert_eq!(packet.frames[1], &[3, 4]);
    assert_eq!(packet.len, 11);
    // Padding longer than the packet must be rejected, not subtracted.
    assert!(OpusPacket::parse(&[0xff, 0xc2, 200, 2, 1, 2]).is_err());
}

#[test]
fn code_3_padding_escape() {
    // A 255 length byte means 254 bytes of padding and "read another byte".
    let mut data = vec![0xff, 0xc1, 255, 1];
    data.extend(std::iter::repeat_n(0u8, 255));
    let packet = OpusPacket::parse(&data).expect("escaped padding");
    assert_eq!(packet.padding, 255);
    assert_eq!(packet.frames.len(), 1);
    assert!(packet.frames[0].is_empty());
}

#[test]
fn code_3_frame_count_is_bounded_to_120_ms() {
    // 63 frames of 20 ms would be 1260 ms. RFC 6716 §3.2.5 caps a packet at
    // 120 ms, which for 20 ms frames is six.
    assert!(OpusPacket::parse(&[0xff, 63]).is_err());
    assert!(OpusPacket::parse(&[0xff, 0]).is_err());
    // 48 frames of 2.5 ms is exactly 120 ms and is legal: config 28, stereo,
    // code 3 is TOC 0xe7.
    let data = {
        let mut d = vec![0xe7u8, 48];
        d.extend(std::iter::repeat_n(0u8, 48));
        d
    };
    let packet = OpusPacket::parse(&data).expect("48 x 2.5 ms");
    assert_eq!(packet.samples(), 5760);
}

#[test]
fn an_empty_packet_is_an_error() {
    assert!(matches!(OpusPacket::parse(&[]), Err(Error::UnexpectedEof)));
}

#[test]
fn multistream_packets_are_self_delimited() {
    // measured: a 5.1 file's Ogg packets begin `fc 02 ff fe fc 02 ff fe 78 ...`
    // — four streams, the first three self-delimited.
    let data = [
        0xfc, 0x02, 0xff, 0xfe, 0xfc, 0x02, 0xff, 0xfe, 0xfc, 0x02, 0xff, 0xfe, 0xfc, 0x11, 0x22,
    ];
    let packets = split_streams(&data, 4).expect("split");
    assert_eq!(packets.len(), 4);
    for packet in packets.iter().take(3) {
        assert_eq!(packet.len, 4);
        assert_eq!(packet.frames[0], &[0xff, 0xfe]);
    }
    assert_eq!(packets[3].frames[0], &[0x11, 0x22]);
}

#[test]
fn a_self_delimited_split_cannot_walk_off_the_end() {
    // Claiming more streams than the data holds is an error, not a panic.
    let data = [0xfc, 0x02, 0xff, 0xfe];
    assert!(split_streams(&data, 4).is_err());
}

// ---------------------------------------------------------------- the parser

#[test]
fn parser_passes_packets_through() {
    let mut parser = OpusParser::with_extradata(Limits::strict(), &HEAD_STEREO).expect("extradata");
    let (packet, used) = parser.parse(&[0xfc, 1, 2, 3]).expect("parse");
    assert_eq!(used, 4);
    assert_eq!(packet.map(|p| p.len), Some(4));
    assert_eq!(parser.samples(), 960);
    assert_eq!(
        parser
            .parameters()
            .and_then(|p| p.audio.as_ref())
            .map(|a| a.sample_rate),
        Some(48000)
    );
    // The end-of-stream convention: an empty slice, nothing to flush.
    let (flush, used) = parser.parse(&[]).expect("eos");
    assert!(flush.is_none());
    assert_eq!(used, 0);
}

// -------------------------------------------------------------- properties

// ------------------------------------------------ Parser::packet_duration

/// `seconds` in ticks of a `1/den` base, truncated towards zero exactly as
/// `vaco_format_core::time::quantise_duration` does.
fn ticks(seconds: vaco_core::Rational, den: i64) -> i64 {
    i64::from(seconds.num)
        .checked_mul(den)
        .and_then(|n| n.checked_div(i64::from(seconds.den)))
        .unwrap_or(0)
}

/// One TOC byte per `-frame_duration` libopus accepts, mono, code 0.
///
/// `config` is chosen so `frame_samples()` gives the stated size; the low three
/// bits are `s=0, code=0`.
const FRAME_SIZE_TOCS: &[(u8, u32, i32)] = &[
    // CELT fullband: configs 28..=31 step 2.5/5/10/20 ms.
    (28 << 3, 120, 2),  // 2.5 ms -> 2 ticks on a 1 ms base, NOT 3
    (29 << 3, 240, 5),  // 5 ms
    (30 << 3, 480, 10), // 10 ms
    (31 << 3, 960, 20), // 20 ms
    // SILK wideband: configs 8..=11 step 10/20/40/60 ms.
    (10 << 3, 1920, 40), // 40 ms
    (11 << 3, 2880, 60), // 60 ms
];

/// Every libopus frame size, in 48 kHz samples, straight off the TOC.
///
/// measured: `ffprobe 8.1` on six `-frame_duration` files, none of which
/// contains a `DefaultDuration` element — see the table on
/// `<OpusParser as Parser>::packet_duration`.
#[test]
fn packet_duration_is_frames_over_48000() {
    let parser = OpusParser::new(Limits::strict());
    for &(toc, samples, ticks_at_1ms) in FRAME_SIZE_TOCS {
        let packet = [toc, 0x11, 0x22, 0x33];
        let d = parser.packet_duration(&packet).expect("a duration");
        assert_eq!(d.num, i32::try_from(samples).unwrap(), "toc {toc:#04x}");
        assert_eq!(d.den, 48000);
        // And what the consumer makes of it on Matroska's base. The 2.5 ms row
        // is the one that distinguishes truncation from rounding.
        assert_eq!(ticks(d, 1000), i64::from(ticks_at_1ms), "toc {toc:#04x}");
    }
}

/// The denominator is the output rate, never the header's `input_sample_rate`.
#[test]
fn packet_duration_ignores_the_declared_input_rate() {
    let mut parser = OpusParser::new(Limits::strict());
    // `OpusHead` declaring 8000 Hz input. Opus still runs at 48 kHz.
    parser
        .set_extradata(b"OpusHead\x01\x01\x38\x01\x40\x1f\0\0\0\0\0")
        .expect("head");
    let d = parser
        .packet_duration(&[31 << 3, 0x11])
        .expect("a duration");
    assert_eq!((d.num, d.den), (960, 48000));
}

/// A code-3 CBR packet lasts frame count × frame size, up to the 120 ms cap.
#[test]
fn packet_duration_counts_the_frames_a_code_3_packet_declares() {
    let parser = OpusParser::new(Limits::strict());
    // config 31 (20 ms), code 3, CBR, no padding, three frames of one byte.
    let packet = [(31 << 3) | 3, 3, 0xaa, 0xbb, 0xcc];
    let d = parser.packet_duration(&packet).expect("a duration");
    assert_eq!((d.num, d.den), (2880, 48000));
}

/// A multi-stream packet is read in the self-delimiting framing, and every
/// stream in it codes the same duration (RFC 7845 §3), so the first answers.
///
/// measured: a 5.1 libopus track in Matroska (`stream_count=4`) reports
/// `duration=20` per packet in `ffprobe 8.1`, and so do we.
#[test]
fn packet_duration_reads_the_first_substream_of_a_multistream_packet() {
    let mut parser = OpusParser::new(Limits::strict());
    // Mapping family 1, two streams, one coupled, three channels.
    let head = [
        b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', //
        1,    // version
        3,    // channel_count
        0x38, 0x01, // pre_skip = 312
        0x80, 0xbb, 0x00, 0x00, // input_sample_rate = 48000
        0x00, 0x00, // output_gain
        1,    // mapping_family
        2,    // stream_count
        1,    // coupled_count
        0, 1, 2, // channel mapping
    ];
    parser.set_extradata(&head).expect("head");
    // Stream 0: TOC, self-delimited length 2, two bytes. Stream 1 takes the rest.
    let packet = [31 << 3, 2, 0xaa, 0xbb, 31 << 3, 0xcc, 0xdd];
    let d = parser.packet_duration(&packet).expect("a duration");
    assert_eq!((d.num, d.den), (960, 48000));
}

/// A packet that does not frame is reported as unmeasurable, not as an error.
#[test]
fn packet_duration_refuses_a_malformed_packet() {
    let parser = OpusParser::new(Limits::strict());
    assert_eq!(parser.packet_duration(&[]), None);
    // Code 1 demands an even payload; three bytes cannot split in two.
    assert_eq!(parser.packet_duration(&[(31 << 3) | 1, 1, 2, 3]), None);
    // Code 3 declaring zero frames.
    assert_eq!(parser.packet_duration(&[(31 << 3) | 3, 0]), None);
}

proptest! {
    /// No byte string may panic any of the parsers.
    /// `packet_duration` is total, bounded, and never longer than the 120 ms
    /// RFC 6716 §3.2.5 allows — over arbitrary bytes, with and without a
    /// header, and in both framings.
    #[test]
    fn packet_duration_is_total_and_bounded(data: Vec<u8>, head: Vec<u8>) {
        let mut parser = OpusParser::new(Limits::strict());
        let _ = parser.set_extradata(&head);
        if let Some(d) = parser.packet_duration(&data) {
            prop_assert!(d.num > 0);
            prop_assert_eq!(d.den, i32::try_from(OUTPUT_SAMPLE_RATE).unwrap());
            prop_assert!(d.num <= i32::try_from(crate::packet::MAX_PACKET_SAMPLES).unwrap());
        }
    }

    #[test]
    fn parsers_never_panic(data: Vec<u8>) {
        let _ = IdentificationHeader::parse(&data);
        let _ = IdentificationHeader::parse_dops(&data);
        let _ = CommentHeader::parse(&data);
        let _ = OpusPacket::parse(&data);
        let _ = OpusPacket::parse_self_delimited(&data);
        let _ = split_streams(&data, 8);
    }

    /// A packet that parses accounts for every byte it was given: frames,
    /// padding and framing overhead must sum to the input length, and no frame
    /// may point outside it.
    #[test]
    fn parsed_packets_account_for_every_byte(data: Vec<u8>) {
        let Ok(packet) = OpusPacket::parse(&data) else { return Ok(()) };
        prop_assert_eq!(packet.len, data.len());
        prop_assert!(packet.payload_bytes() + packet.padding < data.len().max(1));
        prop_assert!(packet.samples() <= crate::packet::MAX_PACKET_SAMPLES);
        prop_assert!(!packet.frames.is_empty());
    }

    /// A self-delimited packet never claims more bytes than it was given.
    #[test]
    fn self_delimited_packets_stay_in_bounds(data: Vec<u8>) {
        let Ok(packet) = OpusPacket::parse_self_delimited(&data) else { return Ok(()) };
        prop_assert!(packet.len <= data.len());
    }

    /// Every identification header we accept re-serialises to the bytes it came
    /// from, which is what makes `dOps` → `OpusHead` conversion lossless.
    #[test]
    fn accepted_heads_round_trip(data: Vec<u8>) {
        let Ok(header) = IdentificationHeader::parse(&data) else { return Ok(()) };
        let re = header.to_opus_head();
        prop_assert_eq!(re.as_slice(), &data[..re.len()]);
        let again = IdentificationHeader::parse(re.as_slice()).ok();
        prop_assert_eq!(again.as_ref(), Some(&header));
    }
}
