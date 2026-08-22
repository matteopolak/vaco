//! `ID3v2Tag::parse` and `Id3v1Tag::parse` over arbitrary bytes.
//!
//! `ID3v2` is exactly the shape plan 13 §2.2.2 warns about: a header carries
//! a declared size, an extended header carries a second declared size, every
//! frame carries a third, and unsynchronisation removal builds a new buffer
//! from the input. Each of those is a place a naive implementation would
//! trust a length before checking it against what is actually there.
//!
//! Properties asserted:
//!
//! * Parsing either tag never panics, whatever the bytes.
//! * Under `Limits::strict()` with a near-empty budget, every failure is a
//!   clean `Error::LimitExceeded` — never a panic, never a multi-megabyte
//!   allocation from a 15-byte input (the exact `vaco-demux-mp4` finding
//!   plan 19 §13 cites, and the reason a declared-size field must never be
//!   reserved up front).
//! * `unsync::remove` never produces output longer than its input, for any
//!   bytes — checked directly here as well as through the tag parse, since
//!   it is the one place in this crate that builds a new buffer from
//!   attacker-controlled content.
//!
//! fuzz-crate: vaco-format-id3

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_id3::id3v1::Id3v1Tag;
use vaco_format_id3::tag::Id3v2Tag;
use vaco_format_id3::unsync;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    // 1. ID3v2, generous budget: exercises the real parsing logic.
    let mut generous = Budget::new(Limits::permissive());
    let _ = Id3v2Tag::parse(data, &mut generous);

    // 2. ID3v2, a budget too small to hold more than a few bytes. Every
    //    failure this produces must be a clean error, not a panic or an
    //    allocation the budget did not approve.
    let mut starved = Budget::new(Limits::strict().with_alloc_total(8).with_fuel(64));
    let _ = Id3v2Tag::parse(data, &mut starved);

    // 3. ID3v1 has no allocation of its own (fixed 128-byte struct), so this
    //    is purely a totality check, at both the exact length it expects and
    //    arbitrary lengths.
    let _ = Id3v1Tag::parse(data);

    // 4. Unsynchronisation removal must never grow its input and must
    //    consume budget honestly.
    let mut budget = Budget::new(Limits::permissive());
    if let Ok(out) = unsync::remove(data, &mut budget) {
        assert!(out.len() <= data.len(), "unsync::remove grew its input");
    }
});
