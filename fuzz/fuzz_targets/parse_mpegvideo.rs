//! MPEG-1/2/4 part 2 access-unit splitting against arbitrary bytes.
//!
//! Both parsers in this crate keep an internal copy of the in-progress
//! access unit (see the crate doc's "framing cost" note), so unlike
//! `parse_vpx` there genuinely is reassembly state to exercise across chunk
//! boundaries — the same chunking-invariance property `parse_h264` checks.
//!
//! Properties, beyond "does not panic":
//!
//! 1. **Access units partition the input.** Feeding the whole buffer at once
//!    and feeding it one byte at a time must consume the same total number
//!    of bytes and produce the same packet count.
//! 2. **Progress.** A parser that returns `(None, 0)` forever on growing
//!    input is a hang; `ParserDriver`'s own `ProgressGuard` already turns
//!    that into a bounded error rather than a fuzzer timeout, so this target
//!    only has to drive it, not re-implement the check.
//!
//! One property this target deliberately does *not* check: that a reported
//! `width`/`height` pair is both-set or both-unset together. Both
//! `sequence_header()` and `video_object_layer_width`/`_height` code the
//! dimension directly rather than a "value minus one" field, so a corrupt
//! bitstream can legally spell exactly one of the pair as `0` — an earlier
//! version of this target asserted the correlation anyway and found nothing
//! but its own wrong assumption, twice, once per parser.
//!
//! fuzz-crate: vaco-parse-mpegvideo
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::ParserDriver;
use vaco_limits::Limits;
use vaco_parse_mpegvideo::{Mpeg12Parser, Mpeg4Parser};

fn drive_whole<P: vaco_codec_core::Parser>(parser: P, data: &[u8]) -> (usize, u64) {
    let mut driver = ParserDriver::new(parser, Limits::strict());
    if driver.push(data).is_err() {
        return (0, 0);
    }
    driver.finish();
    let mut count = 0usize;
    while driver.next_unit().is_ok() {
        count += 1;
    }
    (count, driver.consumed())
}

fn drive_byte_at_a_time<P: vaco_codec_core::Parser>(parser: P, data: &[u8]) -> (usize, u64) {
    let mut driver = ParserDriver::new(parser, Limits::strict());
    let mut count = 0usize;
    for &b in data {
        if driver.push(&[b]).is_err() {
            return (count, driver.consumed());
        }
        while driver.next_unit().is_ok() {
            count += 1;
        }
    }
    driver.finish();
    while driver.next_unit().is_ok() {
        count += 1;
    }
    (count, driver.consumed())
}

fuzz_target!(|data: &[u8]| {
    let whole = drive_whole(Mpeg12Parser::new(Limits::strict()), data);
    let chunked = drive_byte_at_a_time(Mpeg12Parser::new(Limits::strict()), data);
    assert_eq!(whole, chunked, "mpeg12: chunking must not change the result");

    let whole4 = drive_whole(Mpeg4Parser::new(Limits::strict()), data);
    let chunked4 = drive_byte_at_a_time(Mpeg4Parser::new(Limits::strict()), data);
    assert_eq!(whole4, chunked4, "mpeg4: chunking must not change the result");
});
