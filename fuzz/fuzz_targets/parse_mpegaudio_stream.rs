//! [`MpegAudioParser`] and [`Ac3Parser`]'s resync loops against arbitrary
//! bytes.
//!
//! `vaco-format-mpegaudio` and `vaco-format-ac3`'s own header parsers are
//! fuzzed directly (`parse_mpegaudio`, `ac3_bsi_parse`); this target is the
//! byte-stream framing this crate adds on top — the part that can be made to
//! loop, resynchronise forever, or over-consume, which a single-header fuzz
//! target cannot reach because it never calls `parse` twice.
//!
//! Properties, for both parsers:
//!
//! 1. A `parse` call never consumes more bytes than it was given.
//! 2. A call that emits no packet and consumes no bytes may not repeat
//!    forever on a buffer that does not grow.
//! 3. Feeding the same bytes one byte-defined-chunk-size at a time through
//!    the driver agrees with feeding them whole — the same chunk-invariance
//!    property `parse_aac_adts` checks for ADTS.
//!
//! fuzz-crate: vaco-parse-mpegaudio

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::{Parser, ParserDriver};
use vaco_core::Error;
use vaco_limits::Limits;
use vaco_parse_mpegaudio::{Ac3Parser, MpegAudioParser};

const MAX_CALLS: usize = 4096;

fn drain_whole<P: Parser>(mut parser: P, data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for _ in 0..MAX_CALLS {
        let Some(rest) = data.get(offset..) else { break };
        if rest.is_empty() {
            break;
        }
        let Ok((packet, used)) = parser.parse(rest) else {
            break;
        };
        assert!(used <= rest.len(), "parser over-consumed: {used} > {}", rest.len());
        if let Some(packet) = packet {
            out.push(packet.payload().to_vec());
        } else if used == 0 {
            break;
        }
        offset += used;
    }
    for _ in 0..MAX_CALLS {
        let Ok((packet, used)) = parser.parse(&[]) else { break };
        assert_eq!(used, 0, "the parser consumed bytes from an empty slice");
        match packet {
            Some(packet) => out.push(packet.payload().to_vec()),
            None => break,
        }
    }
    out
}

fn drain<P: Parser>(driver: &mut ParserDriver<P>, out: &mut Vec<Vec<u8>>) -> bool {
    loop {
        match driver.next_unit() {
            Ok(packet) => out.push(packet.payload().to_vec()),
            Err(Error::NeedMoreInput | Error::Eof) => return true,
            Err(_) => return false,
        }
        if out.len() > MAX_CALLS {
            return false;
        }
    }
}

fn check_chunked<P: Parser>(make: impl Fn() -> P, data: &[u8], whole: &[Vec<u8>]) {
    let chunk = usize::from(data.first().copied().unwrap_or(1)).max(1);
    let mut driver = ParserDriver::new(make(), Limits::permissive());
    let mut pieced: Vec<Vec<u8>> = Vec::new();
    let mut offset = 0usize;
    let mut calls = 0usize;
    while offset < data.len() && calls < MAX_CALLS {
        calls += 1;
        let end = offset.saturating_add(chunk).min(data.len());
        let Some(slice) = data.get(offset..end) else { break };
        if driver.push(slice).is_err() {
            return;
        }
        offset = end;
        if !drain(&mut driver, &mut pieced) {
            return;
        }
    }
    driver.finish();
    if !drain(&mut driver, &mut pieced) {
        return;
    }
    if calls < MAX_CALLS {
        assert_eq!(
            whole, pieced,
            "framing changed when the same bytes arrived in {chunk}-byte chunks"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let whole = drain_whole(MpegAudioParser::new(Limits::strict()), data);
    check_chunked(|| MpegAudioParser::new(Limits::strict()), data, &whole);

    let whole = drain_whole(Ac3Parser::new(Limits::strict()), data);
    check_chunked(|| Ac3Parser::new(Limits::strict()), data, &whole);
});
