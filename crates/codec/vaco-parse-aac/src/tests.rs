//! Unit and property tests.
//!
//! # Where the expected values came from
//!
//! Every `// measured:` comment records an `ffprobe 8.1` observation. The
//! `AudioSpecificConfig` cases were produced by rewriting the `esds`
//! `DecoderSpecificInfo` of a real MP4 in place — the shortest path to the
//! configuration parser that exists, per plan 13 §1b — and reading back
//! `-show_streams`. The transcript and the harness are in
//! `docs/codec/vaco-parse-aac.md`.

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
use vaco_codec_core::Parser;
use vaco_core::Error;
use vaco_limits::Limits;

use crate::adts::{AdtsHeader, AdtsParser, MpegVersion};
use crate::asc::{AudioObjectType, AudioSpecificConfig, Signal};
use crate::latm::{LoasParser, StreamMuxConfig, SyncStreamHeader};
use crate::tables;

// ------------------------------------------------------------------ fixtures

/// The first two frames of an AAC-LC 44100 Hz stereo ADTS stream, as the
/// reference's own encoder produced them.
const ADTS_LC: [u8; 251] = [
    0xff, 0xf1, 0x50, 0x80, 0x07, 0x3f, 0xfc, 0x21, 0x10, 0x03, 0x40, 0x68, 0x1b, 0xc7, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x38, 0xff, 0xf1, 0x50, 0x80, 0x18, 0x5f, 0xfc,
    0x21, 0x19, 0xce, 0x00, 0x00, 0x00, 0x00, 0x08, 0x6b, 0x64, 0x0c, 0x90, 0x10, 0x7f, 0x8b, 0xdf,
    0xd7, 0x2c, 0x55, 0x37, 0xbb, 0xee, 0x48, 0xb5, 0xae, 0x54, 0xde, 0xfe, 0x41, 0xde, 0x22, 0x25,
    0x52, 0x25, 0x8b, 0x78, 0x9f, 0xb3, 0x39, 0xf1, 0x0f, 0x5a, 0xf9, 0x90, 0x9f, 0xb4, 0xfe, 0x3d,
    0x10, 0xf0, 0x9f, 0xcf, 0xe2, 0x3b, 0x1f, 0x88, 0x64, 0xbd, 0x0d, 0xf1, 0x68, 0x8f, 0xf0, 0xba,
    0xfe, 0x36, 0xe4, 0xff, 0xd0, 0x4b, 0xfe, 0x98, 0x52, 0x5f, 0xa9, 0xbf, 0xe0, 0xda, 0x43, 0xf9,
    0x8e, 0xfe, 0x68, 0xc9, 0xfd, 0xb3, 0xfc, 0x17, 0x10, 0xf3, 0x4f, 0xd6, 0xa2, 0x3c, 0x27, 0xe0,
    0x39, 0x2f, 0x15, 0xf9, 0xa8, 0x8f, 0x99, 0x3a, 0x69, 0x2f, 0x32, 0x3c, 0x87, 0x85, 0xf7, 0x04,
    0xfa, 0xff, 0x33, 0x21, 0xc1, 0x79, 0x89, 0x3b, 0x7c, 0xb4, 0x94, 0xbe, 0x2e, 0x47, 0x3f, 0xb3,
    0x25, 0xb9, 0xc2, 0xd0, 0x3c, 0x04, 0x9e, 0xe6, 0xc1, 0x0d, 0x26, 0x57, 0xe7, 0xe4, 0x6a, 0x72,
    0xa6, 0xdf, 0x88, 0xdd, 0x09, 0x72, 0x99, 0xd7, 0xd3, 0xd5, 0xb3, 0x8f, 0x76, 0x9e, 0x7a, 0x28,
    0xea, 0x9e, 0x2e, 0xed, 0x2c, 0xbf, 0xa8, 0xe2, 0x29, 0x1e, 0x7a, 0x98, 0x05, 0xff, 0x30, 0x00,
    0x00, 0x00, 0x00, 0x17, 0x2a, 0x6f, 0x7f, 0x20, 0xef, 0x11, 0x70,
];

/// One `AudioSyncStream` frame of the same content muxed as LOAS/LATM.
const LOAS_LC: [u8; 60] = [
    0x56, 0xe0, 0x39, 0x20, 0x00, 0x12, 0x10, 0x1f, 0xe1, 0x91, 0x08, 0x80, 0x1a, 0x03, 0x40, 0xde,
    0x38, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xc0,
];

fn parse(hex: &[u8]) -> AudioSpecificConfig {
    AudioSpecificConfig::parse(hex).unwrap_or_else(|e| panic!("{hex:02x?} did not parse: {e}"))
}

// -------------------------------------------------------- AudioSpecificConfig

#[test]
fn asc_plain_lc() {
    // measured: extradata 12 10 -> profile=LC sample_rate=44100 channels=2
    let cfg = parse(&[0x12, 0x10]);
    assert_eq!(cfg.object_type, AudioObjectType::AAC_LC);
    assert_eq!(cfg.sampling_frequency, 44100);
    assert_eq!(cfg.output_sample_rate(), 44100);
    assert_eq!(cfg.output_channels(), Some(2));
    assert_eq!(
        cfg.channel_layout(),
        Some(vaco_chlayout::ChannelLayout::STEREO)
    );
    assert_eq!(cfg.profile().map(|p| p.name), Some("LC"));
    assert_eq!(cfg.bits_read, 16);
}

#[test]
fn asc_explicit_sbr_reports_the_extension_rate() {
    // measured: 13 90 56 e5 a0 -> sample_rate=44100 channels=2
    // The real `esds` of an HE-AAC file the reference's own encoder produced.
    let cfg = parse(&[0x13, 0x90, 0x56, 0xe5, 0xa0]);
    assert_eq!(cfg.sampling_frequency, 22050);
    assert_eq!(cfg.extension_sampling_frequency, 44100);
    assert_eq!(cfg.sbr, Signal::Present);
    assert_eq!(cfg.output_sample_rate(), 44100);
    assert_eq!(cfg.output_channels(), Some(2));
    // The profile stays LC: it is the *core* object type, and the reference
    // prints `LC` too when it has only the configuration to go on.
    assert_eq!(cfg.profile().map(|p| p.name), Some("LC"));
}

#[test]
fn asc_sbr_is_not_a_doubling() {
    // measured: 12 10 56 e5 98 (sfi 4 = 44100, extension sfi 3 = 48000)
    //           -> sample_rate=48000, NOT 88200
    let cfg = parse(&[0x12, 0x10, 0x56, 0xe5, 0x98]);
    assert_eq!(cfg.sampling_frequency, 44100);
    assert_eq!(cfg.output_sample_rate(), 48000);
}

#[test]
fn asc_sbr_flag_zero_keeps_the_core_rate() {
    // measured: 13 90 56 e5 00 -> sample_rate=22050
    let cfg = parse(&[0x13, 0x90, 0x56, 0xe5, 0x00]);
    assert_eq!(cfg.sbr, Signal::Absent);
    assert_eq!(cfg.output_sample_rate(), 22050);
}

#[test]
fn asc_mono_with_sbr_and_no_ps_field_reports_stereo() {
    // measured: 13 88 56 e5 a0 -> channels=2
    // This is the case a two-valued `bool` gets wrong: PS is *unknown*, not
    // absent, and the reference assumes it.
    let cfg = parse(&[0x13, 0x88, 0x56, 0xe5, 0xa0]);
    assert_eq!(cfg.channel_configuration, 1);
    assert_eq!(cfg.ps, Signal::Unknown);
    assert_eq!(cfg.output_channels(), Some(2));
    assert_eq!(
        cfg.channel_layout(),
        Some(vaco_chlayout::ChannelLayout::STEREO)
    );
}

#[test]
fn asc_mono_with_explicit_ps_zero_reports_mono() {
    // measured: 13 88 56 e5 a5 48 00 -> channels=1 channel_layout=mono
    let cfg = parse(&[0x13, 0x88, 0x56, 0xe5, 0xa5, 0x48, 0x00]);
    assert_eq!(cfg.ps, Signal::Absent);
    assert_eq!(cfg.output_channels(), Some(1));
    assert_eq!(
        cfg.channel_layout(),
        Some(vaco_chlayout::ChannelLayout::MONO)
    );
}

#[test]
fn asc_he_aac_v2() {
    // measured: 13 88 56 e5 a5 48 80 -> sample_rate=44100 channels=2
    // The real `esds` of an HE-AACv2 file.
    let cfg = parse(&[0x13, 0x88, 0x56, 0xe5, 0xa5, 0x48, 0x80]);
    assert_eq!(cfg.ps, Signal::Present);
    assert_eq!(cfg.output_sample_rate(), 44100);
    assert_eq!(cfg.output_channels(), Some(2));
}

#[test]
fn asc_stereo_core_is_not_doubled_by_ps() {
    // measured: 13 90 56 e5 a5 48 80 -> channels=2, unchanged
    let cfg = parse(&[0x13, 0x90, 0x56, 0xe5, 0xa5, 0x48, 0x80]);
    assert_eq!(cfg.ps, Signal::Present);
    assert_eq!(cfg.output_channels(), Some(2));
}

#[test]
fn asc_hierarchical_sbr() {
    // measured: 2b 92 08 00 (audioObjectType 5, sfi 7, extension sfi 4, core 2)
    //           -> profile=LC sample_rate=44100 channels=2
    let cfg = parse(&[0x2b, 0x92, 0x08, 0x00]);
    assert_eq!(cfg.object_type, AudioObjectType::AAC_LC);
    assert_eq!(cfg.extension_object_type, AudioObjectType::SBR);
    assert_eq!(cfg.sampling_frequency, 22050);
    assert_eq!(cfg.output_sample_rate(), 44100);
}

#[test]
fn asc_hierarchical_sbr_doubles_a_mono_core() {
    // measured: 2b 8a 08 00 -> channels=2
    // audioObjectType 5 leaves PS unknown, and unknown means assumed.
    let cfg = parse(&[0x2b, 0x8a, 0x08, 0x00]);
    assert_eq!(cfg.channel_configuration, 1);
    assert_eq!(cfg.ps, Signal::Unknown);
    assert_eq!(cfg.output_channels(), Some(2));
}

#[test]
fn asc_hierarchical_ps() {
    // measured: eb 8a 08 00 (audioObjectType 29) -> sample_rate=44100 channels=2
    let cfg = parse(&[0xeb, 0x8a, 0x08, 0x00]);
    assert_eq!(cfg.extension_object_type, AudioObjectType::SBR);
    assert_eq!(cfg.ps, Signal::Present);
    assert_eq!(cfg.output_channels(), Some(2));
}

/// # D17
///
/// The standard defines `samplingFrequencyIndex` 15 as an escape to an explicit
/// 24-bit rate. The reference rejects it in the core position and accepts it in
/// the extension position. Both halves are pinned; see
/// `crate::asc::read_core_frequency`.
#[test]
fn core_escape_index_is_rejected() {
    // measured: 17 80 56 22 10 -> `invalid sampling rate index 15`, and the
    // stream does not appear in -show_streams at all.
    let err = AudioSpecificConfig::parse(&[0x17, 0x80, 0x56, 0x22, 0x10]);
    assert!(matches!(err, Err(Error::InvalidData(_))), "{err:?}");
}

/// See [`core_escape_index_is_rejected`].
#[test]
fn extension_escape_index_is_accepted() {
    // measured: 13 90 56 e5 f8 05 62 20 -> sample_rate=44100
    let cfg = parse(&[0x13, 0x90, 0x56, 0xe5, 0xf8, 0x05, 0x62, 0x20]);
    assert_eq!(cfg.extension_sampling_frequency, 44100);
    assert_eq!(cfg.output_sample_rate(), 44100);

    // measured: ... f8 01 81 c8 -> sample_rate=12345, an arbitrary rate the
    // index table cannot express.
    let cfg = parse(&[0x13, 0x90, 0x56, 0xe5, 0xf8, 0x01, 0x81, 0xc8]);
    assert_eq!(cfg.output_sample_rate(), 12345);
}

#[test]
fn extension_rate_of_zero_falls_back_to_the_core_rate() {
    // measured: 13 90 56 e5 f8 00 00 00 -> sample_rate=22050
    let cfg = parse(&[0x13, 0x90, 0x56, 0xe5, 0xf8, 0x00, 0x00, 0x00]);
    assert_eq!(cfg.extension_sampling_frequency, 0);
    assert_eq!(cfg.output_sample_rate(), 22050);
}

#[test]
fn reserved_sampling_frequency_indices_are_rejected() {
    // measured: sfi 13 and 14 -> `invalid sampling rate index`
    for asc in [[0x16, 0x90], [0x17, 0x10]] {
        assert!(AudioSpecificConfig::parse(&asc).is_err(), "{asc:02x?}");
    }
}

#[test]
fn channel_configurations_match_the_reference() {
    // measured, by rewriting `channelConfiguration` in an `esds`:
    //   1 mono | 2 stereo | 3 3.0 | 4 4.0 | 5 5.0 | 6 5.1 | 7 7.1 (8ch)
    //   11 6.1(back) 7ch | 12 7.1 8ch | 13 22.2 24ch | 14 5.1.2(back) 8ch
    //   8, 9, 10 and 15 are rejected outright.
    let expected: [(u8, u32, &str); 11] = [
        (1, 1, "mono"),
        (2, 2, "stereo"),
        (3, 3, "3.0"),
        (4, 4, "4.0"),
        (5, 5, "5.0"),
        (6, 6, "5.1"),
        (7, 8, "7.1"),
        (11, 7, "6.1(back)"),
        (12, 8, "7.1"),
        (13, 24, "22.2"),
        (14, 8, "5.1.2(back)"),
    ];
    for (config, channels, name) in expected {
        assert_eq!(
            tables::channels_for_config(config),
            Some(channels),
            "{config}"
        );
        let layout = tables::layout_for_config(config);
        assert_eq!(
            layout.as_ref().and_then(vaco_chlayout::ChannelLayout::name),
            Some(name),
            "{config}"
        );
        assert_eq!(layout.map(|l| l.channels), Some(channels), "{config}");
    }
    for config in [0, 8, 9, 10, 15] {
        assert_eq!(tables::channels_for_config(config), None, "{config}");
    }
}

#[test]
fn sampling_frequency_table_matches_the_reference() {
    // measured: every index 0..=12, by rewriting `samplingFrequencyIndex`.
    let expected = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    for (index, hz) in expected.into_iter().enumerate() {
        let index = u8::try_from(index).unwrap_or(0);
        assert_eq!(tables::frequency_for_index(index), Some(hz));
        assert_eq!(tables::index_for_frequency(hz), index);
    }
    for index in [13, 14, 15] {
        assert_eq!(tables::frequency_for_index(index), None);
    }
}

#[test]
fn object_type_escape() {
    // `GetAudioObjectType` escapes at 31: 5 bits of `11111` then 6 more.
    // ER AAC ELD is 39, so 31 + 8.
    let cfg = parse(&[0xf8, 0xe8, 0x40]);
    assert_eq!(cfg.object_type, AudioObjectType::ER_AAC_ELD);
    assert_eq!(cfg.profile().map(|p| p.name), Some("ELD"));
}

#[test]
fn frame_length_flag_selects_the_shorter_frame() {
    assert_eq!(parse(&[0x12, 0x10]).frame_length(), 1024);
    assert_eq!(parse(&[0x12, 0x14]).frame_length(), 960);
    // ER AAC LD (23) halves both.
    assert_eq!(parse(&[0xba, 0x10]).frame_length(), 512);
    assert_eq!(parse(&[0xba, 0x14]).frame_length(), 480);
}

#[test]
fn truncated_configs_never_panic() {
    let full = [0x13u8, 0x88, 0x56, 0xe5, 0xa5, 0x48, 0x80];
    for n in 0..full.len() {
        // Either it parses or it errors; it must not panic and must not hang.
        let _ = AudioSpecificConfig::parse(&full[..n]);
    }
}

// ------------------------------------------------------------------ ADTS

#[test]
fn adts_header_fields() {
    let header = AdtsHeader::parse(&ADTS_LC).expect("fixture header");
    assert_eq!(header.version, MpegVersion::Mpeg4);
    assert!(header.protection_absent);
    assert_eq!(header.object_type, AudioObjectType::AAC_LC);
    assert_eq!(header.sampling_frequency, 44100);
    assert_eq!(header.channel_configuration, 2);
    assert_eq!(header.frame_length, 57);
    assert_eq!(header.header_len(), 7);
    assert_eq!(header.payload_len(), 50);
    assert!(header.is_vbr());
    assert_eq!(header.raw_data_blocks, 1);
    // measured: the same stream in MP4 carries `12 10` in its `esds`.
    assert_eq!(header.to_audio_specific_config(), [0x12, 0x10]);
}

#[test]
fn adts_layer_must_be_zero() {
    // measured: flipping the layer bits makes the reference fail to recognise
    // the file as AAC at all.
    let mut data = ADTS_LC;
    if let Some(b) = data.get_mut(1) {
        *b |= 0x02;
    }
    assert!(AdtsHeader::parse(&data).is_err());
}

#[test]
fn adts_parser_splits_whole_frames() {
    let mut parser = AdtsParser::new(Limits::strict());
    let (first, used) = parser.parse(&ADTS_LC).expect("first frame");
    // measured: `ffprobe -show_packets` reports pos=0 size=57 then pos=57
    // size=194, so a packet is the whole frame, header included.
    assert_eq!(used, 57);
    assert_eq!(first.map(|p| p.len), Some(57));

    let (second, used) = parser.parse(&ADTS_LC[57..]).expect("second frame");
    assert_eq!(used, 194);
    assert_eq!(second.map(|p| p.len), Some(194));

    let params = parser.parameters().and_then(|p| p.audio.clone());
    assert_eq!(params.as_ref().map(|a| a.sample_rate), Some(44100));
    assert_eq!(params.and_then(|a| a.layout).map(|l| l.channels), Some(2));
}

#[test]
fn adts_parser_skips_leading_garbage() {
    let mut data = vec![0x00; 40];
    data.extend_from_slice(&ADTS_LC);
    let mut parser = AdtsParser::new(Limits::strict());
    let mut offset = 0usize;
    let mut got = None;
    // The parser is allowed to consume the garbage over several calls.
    for _ in 0..8 {
        let (packet, used) = parser.parse(&data[offset..]).expect("parse");
        offset += used;
        if packet.is_some() {
            got = packet;
            break;
        }
        if used == 0 {
            break;
        }
    }
    assert_eq!(got.map(|p| p.len), Some(57));
    assert_eq!(offset, 40 + 57);
}

#[test]
fn a_single_adts_frame_is_emitted_at_end_of_stream() {
    // measured: `ffprobe -f aac` on a file containing exactly one ADTS frame
    // reports the stream, so a parser that needs a following sync word to
    // confirm the frame must still emit it when the stream ends.
    let mut parser = AdtsParser::new(Limits::strict());
    let (nothing, used) = parser.parse(&ADTS_LC[..57]).expect("single frame");
    assert!(nothing.is_none());
    assert_eq!(used, 0);
    let (packet, used) = parser.parse(&[]).expect("flush");
    assert_eq!(used, 0);
    assert_eq!(packet.map(|p| p.len), Some(57));
    // Draining is idempotent: a second flush yields nothing.
    let (again, _) = parser.parse(&[]).expect("second flush");
    assert!(again.is_none());
}

/// # Regression: `parse_aac_adts` crash-0441e23d
///
/// Sixteen bytes in which a valid-looking header sits at offset 1 and declares a
/// fifteen-byte frame that ends exactly at the end of the buffer — so there is
/// no following sync word to confirm it with. The parser must defer the frame
/// and hand it over at end of stream, and both halves of the fuzz target's
/// chunk-invariance comparison must agree on that.
///
/// The finding was in the *harness* — the whole-buffer side never sent the
/// end-of-stream signal — but the input is exactly the boundary case this
/// crate's framing rule turns on, so it is pinned here rather than only fixed.
#[test]
fn regression_frame_ending_at_the_buffer_end_survives_to_the_flush() {
    const INPUT: [u8; 16] = [
        0x00, 0xff, 0xf9, 0x01, 0x00, 0x01, 0xfd, 0xff, 0x01, 0xff, 0x3f, 0xfe, 0xff, 0xff, 0xc6,
        0xff,
    ];
    let mut parser = AdtsParser::new(Limits::strict());
    let (nothing, used) = parser.parse(&INPUT).expect("scan");
    assert!(nothing.is_none());
    assert_eq!(used, 1, "the leading garbage byte should be consumed");

    let (nothing, used) = parser.parse(&INPUT[1..]).expect("candidate");
    assert!(nothing.is_none(), "the frame cannot be confirmed yet");
    assert_eq!(used, 0);

    let (packet, used) = parser.parse(&[]).expect("flush");
    assert_eq!(used, 0);
    let packet = packet.expect("the deferred frame");
    assert_eq!(packet.len, 15);
    let header = AdtsHeader::parse(packet.payload()).expect("re-parse");
    assert_eq!(usize::from(header.frame_length), packet.len);
}

/// # Regression: `parse_aac_adts` crash-115059e2
///
/// Eighty-nine bytes whose header declares an eighty-eight-byte frame, again
/// running to the end of the buffer. Driven through [`ParserDriver`] one byte at
/// a time, the frame needs eighty-odd `NeedMoreInput` returns before it can be
/// confirmed — and the driver's [`vaco_limits::ProgressGuard`] counts every one
/// of those as a stall and gives up at sixty-four.
///
/// **That limit was `vaco-codec-core`'s, not this crate's**, and it is now
/// fixed: `ParserDriver::push` resets the guard when it actually adds bytes,
/// because new bytes are progress even when the parser consumes none of them.
/// This test was written to fail once that landed, and it did.
#[test]
fn regression_small_chunks_hit_the_driver_progress_guard() {
    const INPUT: [u8; 89] = [
        0x00, 0xff, 0xf9, 0x00, 0x00, 0x0b, 0x01, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0x00, 0xfe, 0x01, 0x24, 0x0a, 0xff, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xfa, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfd, 0x00, 0x0e,
    ];

    // Fed whole, the frame comes out at the flush.
    let mut parser = AdtsParser::new(Limits::strict());
    let mut offset = 0usize;
    let mut frames = Vec::new();
    while offset < INPUT.len() {
        let (packet, used) = parser.parse(&INPUT[offset..]).expect("parse");
        if let Some(packet) = packet {
            frames.push(packet.len);
        } else if used == 0 {
            break;
        }
        offset += used;
    }
    while let Ok((Some(packet), _)) = parser.parse(&[]) {
        frames.push(packet.len);
    }
    assert_eq!(frames, vec![88]);

    // Fed one byte at a time through the driver, the frame still arrives.
    let mut driver =
        vaco_codec_core::ParserDriver::new(AdtsParser::new(Limits::strict()), Limits::permissive());
    let mut driven = Vec::new();
    for byte in INPUT {
        driver
            .push(&[byte])
            .expect("a one-byte push must never fail");
        match driver.next_unit() {
            Ok(packet) => driven.push(packet.payload().len()),
            Err(Error::NeedMoreInput) => {}
            Err(e) => panic!("the driver gave up on a one-byte feed: {e}"),
        }
    }
    driver.finish();
    while let Ok(packet) = driver.next_unit() {
        driven.push(packet.payload().len());
    }
    assert_eq!(
        driven, frames,
        "chunk size must not change what the parser finds"
    );
}

#[test]
fn adts_parser_rejects_a_lone_false_sync() {
    // 0xFFF followed by plausible-looking bytes, but no frame after it. The
    // resynchroniser must not emit a packet for it.
    let mut data = vec![0xff, 0xf1, 0x50, 0x80, 0x00, 0x1f, 0xfc];
    data.extend_from_slice(&[0x11; 300]);
    let mut parser = AdtsParser::new(Limits::strict());
    let (packet, _) = parser.parse(&data).expect("parse");
    assert!(packet.is_none());
}

// ------------------------------------------------------------------ LATM

#[test]
fn loas_frame_header() {
    let header = SyncStreamHeader::parse(&LOAS_LC).expect("LOAS header");
    // measured: `ffprobe -show_packets` on the LOAS file reports size=60.
    assert_eq!(header.mux_length, 57);
    assert_eq!(header.frame_len(), 60);
}

#[test]
fn loas_stream_mux_config_carries_the_asc() {
    let mut parser = LoasParser::new(Limits::strict());
    // The fixture is a single frame, so nothing follows it to confirm the
    // sync: the parser defers it and the end-of-stream flush emits it. That is
    // the path a one-frame file takes, and the reference accepts such a file
    // too (probed with `ffprobe -f loas`).
    let (deferred, used) = parser.parse(&LOAS_LC).expect("LOAS frame");
    assert!(deferred.is_none());
    assert_eq!(used, 0);
    let (packet, used) = parser.parse(&[]).expect("LOAS flush");
    assert_eq!(used, 0);
    assert_eq!(packet.map(|p| p.len), Some(60));

    let config = parser.config().cloned().expect("StreamMuxConfig");
    assert_eq!(config.version, 0);
    assert!(config.all_streams_same_time_framing);
    assert_eq!(config.sub_frames, 1);
    assert_eq!(config.programs, 1);
    let asc = config.primary_config().expect("AudioSpecificConfig");
    // measured: the reference's LOAS demuxer sets extradata to `12 10`.
    assert_eq!(asc.object_type, AudioObjectType::AAC_LC);
    assert_eq!(asc.sampling_frequency, 44100);
    assert_eq!(asc.channel_configuration, 2);
    assert_eq!(
        parser
            .parameters()
            .and_then(|p| p.audio.as_ref())
            .map(|a| a.sample_rate),
        Some(44100)
    );
}

#[test]
fn loas_mux_version_a_is_unsupported_not_invalid() {
    // audioMuxVersion = 1, audioMuxVersionA = 1: reserved syntax, so the right
    // answer is "we do not implement this", not "your file is broken".
    let bits = [0b1100_0000u8, 0, 0, 0];
    let mut reader = vaco_bitstream::BitReader::new(&bits);
    assert!(matches!(
        StreamMuxConfig::read(&mut reader),
        Err(Error::Unsupported(_))
    ));
}

// -------------------------------------------------------------- properties

proptest! {
    /// No byte string, of any length, may panic either parser or make it claim
    /// to have consumed more than it was given.
    #[test]
    fn parsers_never_panic_and_never_overconsume(data: Vec<u8>) {
        let _ = AudioSpecificConfig::parse(&data);
        let _ = AdtsHeader::parse(&data);
        let _ = SyncStreamHeader::parse(&data);

        let mut adts = AdtsParser::new(Limits::strict());
        if let Ok((_, used)) = adts.parse(&data) {
            prop_assert!(used <= data.len());
        }
        let mut loas = LoasParser::new(Limits::strict());
        if let Ok((_, used)) = loas.parse(&data) {
            prop_assert!(used <= data.len());
        }
    }

    /// Every ADTS header the syntax can express round-trips through the parser.
    #[test]
    fn adts_headers_round_trip(
        id in 0u8..2,
        protection in 0u8..2,
        profile in 0u8..4,
        sfi in 0u8..13,
        channels in 0u8..8,
        frame_len in 9u16..8191,
        fullness in 0u16..2048,
        blocks in 0u8..4,
    ) {
        let mut bytes = [0u8; 7];
        bytes[0] = 0xff;
        bytes[1] = 0xf0 | (id << 3) | protection;
        bytes[2] = (profile << 6) | (sfi << 2) | (channels >> 2);
        bytes[3] = ((channels & 3) << 6) | ((frame_len >> 11) as u8 & 0x03);
        bytes[4] = (frame_len >> 3) as u8;
        bytes[5] = (((frame_len & 0x07) as u8) << 5) | ((fullness >> 6) as u8 & 0x1f);
        bytes[6] = (((fullness & 0x3f) as u8) << 2) | blocks;

        let header = AdtsHeader::parse(&bytes);
        prop_assert!(header.is_ok(), "{bytes:02x?} rejected: {header:?}");
        let Ok(header) = header else { return Ok(()) };
        prop_assert_eq!(header.protection_absent, protection == 1);
        prop_assert_eq!(header.object_type.0, profile + 1);
        prop_assert_eq!(header.sampling_frequency_index, sfi);
        prop_assert_eq!(header.channel_configuration, channels);
        prop_assert_eq!(header.frame_length, frame_len);
        prop_assert_eq!(header.buffer_fullness, fullness);
        prop_assert_eq!(header.raw_data_blocks, blocks + 1);
    }

    /// A parser fed a real stream one byte at a time must produce exactly the
    /// frames it produces when fed the whole thing at once. This is the
    /// reassembly bug `ParserDriver` exists to prevent, checked from the other
    /// side.
    #[test]
    fn adts_framing_is_independent_of_chunking(chunk in 1usize..64) {
        let whole = vec![57usize, 194];

        let mut driver = vaco_codec_core::ParserDriver::new(
            AdtsParser::new(Limits::strict()),
            Limits::permissive(),
        );
        let mut pieced = Vec::new();
        let mut offset = 0;
        while offset < ADTS_LC.len() {
            let end = (offset + chunk).min(ADTS_LC.len());
            prop_assert!(driver.push(&ADTS_LC[offset..end]).is_ok());
            offset = end;
            while let Ok(packet) = driver.next_unit() {
                pieced.push(packet.len);
            }
        }
        driver.finish();
        while let Ok(packet) = driver.next_unit() {
            pieced.push(packet.len);
        }
        prop_assert_eq!(&whole, &pieced);
    }
}
