//! Fixture-based tests for the resync loops in [`crate::mpegaudio`] and
//! [`crate::ac3`].

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]

use vaco_codec_core::{CodecId, Parser};
use vaco_limits::Limits;

use crate::{Ac3Parser, MpegAudioParser};

/// MPEG-1 Layer III, 44.1 kHz, stereo, 128 kbps, no CRC.
///
/// `0xFF 0xFB` is the sync word plus `version=11 layer=01 protection=1`;
/// `0x90` is `bitrate_index=1001(9->128kbps) sample_rate_index=00`;
/// `0x00` leaves padding/private/mode at their zero defaults (stereo).
/// `frame_len()` for this header is 417 bytes.
fn mp3_frame(fill: u8) -> Vec<u8> {
    let mut f = vec![fill; 417];
    f[0] = 0xff;
    f[1] = 0xfb;
    f[2] = 0x90;
    f[3] = 0x00;
    f
}

#[test]
fn mp3_two_frames_confirm_each_other() {
    let mut data = mp3_frame(0x11);
    data.extend(mp3_frame(0x22));
    let mut parser = MpegAudioParser::new(Limits::permissive());
    let (packet, used) = parser.parse(&data).unwrap();
    let packet = packet.expect("first frame confirmed by the second's sync word");
    assert_eq!(used, 417);
    assert_eq!(packet.len, 417);
    let params = parser.parameters().expect("header parsed");
    assert_eq!(params.codec_id, Some(CodecId::Mp3));
    let audio = params.audio.as_ref().unwrap();
    assert_eq!(audio.sample_rate, 44_100);
    assert_eq!(audio.layout.as_ref().map(|l| l.channels), Some(2));
    assert_eq!(params.bit_rate, Some(128_000));
}

#[test]
fn mp3_last_frame_of_a_stream_is_still_emitted() {
    let data = mp3_frame(0x33);
    let mut parser = MpegAudioParser::new(Limits::permissive());
    // No second sync word follows: the driver would call this at end of
    // stream once the reassembly buffer is exhausted.
    let (packet, used) = parser.parse(&data).unwrap();
    assert!(packet.is_none());
    assert_eq!(used, 0);
    let (packet, used) = parser.parse(&[]).unwrap();
    assert!(packet.is_some());
    assert_eq!(used, 0);
    assert_eq!(parser.frames(), 1);
}

#[test]
fn mp3_resyncs_past_garbage() {
    let mut data = vec![0u8; 64];
    data.extend(mp3_frame(0x44));
    data.extend(mp3_frame(0x55));
    let mut parser = MpegAudioParser::new(Limits::permissive());
    let mut offset = 0usize;
    let mut frames = 0usize;
    loop {
        let (packet, used) = parser.parse(&data[offset..]).unwrap();
        if used == 0 && packet.is_none() {
            break;
        }
        offset += used;
        if packet.is_some() {
            frames += 1;
        }
        if offset >= data.len() {
            break;
        }
    }
    assert_eq!(frames, 2);
    assert!(parser.resyncs() >= 1);
}

#[test]
fn mp3_never_panics_on_arbitrary_bytes() {
    let mut parser = MpegAudioParser::new(Limits::permissive());
    for len in [0usize, 1, 3, 4, 5, 64, 4096] {
        let data = vec![0xffu8; len];
        let _ = parser.parse(&data);
    }
    let _ = parser.parse(&[]);
}

/// Classic AC-3, 48 kHz, 5.1, `frmsizecod=20` (192 kbps -> frame 768 bytes at
/// 48 kHz — see `vaco-format-ac3::syncinfo`'s own fixture, which this mirrors
/// exactly since it is the one measured against `BITRATES_KBPS`/`SAMPLE_RATES`).
///
/// `acmod=7` (3/2, `L C R SL SR`) plus `lfeon=1` is 5.1: `bsi()` packs
/// `acmod(3) cmixlev(2) surmixlev(2) lfeon(1)` into byte 6 for this `acmod`
/// (`has_center`/`has_surround` both true, `dsurmod` only applies to
/// `acmod==2`), so `0b111_00_00_1 == 0xE1` sets `acmod=7`, both mix-level
/// fields to their zero default, and `lfeon=1` in the one byte.
fn ac3_frame(fill: u8) -> Vec<u8> {
    let mut f = vec![fill; 768];
    f[0] = 0x0b;
    f[1] = 0x77;
    f[4] = 20; // fscod=0 (48kHz), frmsizecod=20
    f[5] = 8 << 3; // bsid=8
    f[6] = 0xe1; // acmod=7, cmixlev=0, surmixlev=0, lfeon=1
    f
}

#[test]
fn ac3_two_frames_confirm_each_other() {
    let mut data = ac3_frame(0x11);
    data.extend(ac3_frame(0x22));
    let mut parser = Ac3Parser::new(Limits::permissive());
    let (packet, used) = parser.parse(&data).unwrap();
    let packet = packet.expect("first frame confirmed by the second's sync word");
    assert_eq!(used, 768);
    assert_eq!(packet.len, 768);
    let params = parser.parameters().expect("header parsed");
    assert_eq!(params.codec_id, Some(CodecId::Ac3));
    let audio = params.audio.as_ref().unwrap();
    assert_eq!(audio.sample_rate, 48_000);
    assert_eq!(audio.layout.as_ref().map(|l| l.channels), Some(6));
}

#[test]
fn ac3_last_frame_of_a_stream_is_still_emitted() {
    let data = ac3_frame(0x33);
    let mut parser = Ac3Parser::new(Limits::permissive());
    let (packet, used) = parser.parse(&data).unwrap();
    assert!(packet.is_none());
    assert_eq!(used, 0);
    let (packet, _) = parser.parse(&[]).unwrap();
    assert!(packet.is_some());
}

#[test]
fn ac3_never_panics_on_arbitrary_bytes() {
    let mut parser = Ac3Parser::new(Limits::permissive());
    for len in [0usize, 1, 2, 5, 6, 64, 4096] {
        let data = vec![0x0bu8; len];
        let _ = parser.parse(&data);
    }
    let _ = parser.parse(&[]);
}

/// Regression for a real fuzz finding: feeding a stream one byte at a time
/// through [`vaco_codec_core::ParserDriver`] used to silently drop every
/// frame whose sync word landed on a chunk boundary, because the resync scan
/// searched for the whole two-byte `0x0B77` pair and discarded a trailing
/// `0x0B` as "no match" when the `0x77` had not arrived yet.
#[test]
fn ac3_finds_a_frame_fed_one_byte_at_a_time() {
    let mut garbage = vec![0u8; 23];
    garbage.extend(ac3_frame(0x11));
    let mut driver = vaco_codec_core::ParserDriver::new(
        Ac3Parser::new(Limits::permissive()),
        Limits::permissive(),
    );
    let mut units = Vec::new();
    for byte in &garbage {
        driver.push(std::slice::from_ref(byte)).unwrap();
        loop {
            match driver.next_unit() {
                Ok(packet) => units.push(packet.len),
                Err(vaco_core::Error::NeedMoreInput) => break,
                Err(e) => panic!("unexpected driver error: {e:?}"),
            }
        }
    }
    driver.finish();
    loop {
        match driver.next_unit() {
            Ok(packet) => units.push(packet.len),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected driver error at eof: {e:?}"),
        }
    }
    assert_eq!(
        units,
        vec![768],
        "the frame was lost across a byte-at-a-time feed"
    );
}

#[test]
fn the_three_descriptors_answer_for_the_right_codecs() {
    assert!(crate::PARSER_MPEGAUDIO.handles(CodecId::Mp3));
    assert!(crate::PARSER_MPEGAUDIO.handles(CodecId::Mp2));
    assert!(crate::PARSER_MPEGAUDIO.handles(CodecId::Mp1));
    assert!(!crate::PARSER_MPEGAUDIO.handles(CodecId::Ac3));
    assert!(crate::PARSER_AC3.handles(CodecId::Ac3));
    assert!(!crate::PARSER_AC3.handles(CodecId::Eac3));
    assert!(crate::PARSER_EAC3.handles(CodecId::Eac3));
}
