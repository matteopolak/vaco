//! Arbitrary bytes through every text-subtitle decoder.
//!
//! This crate parses untrusted input by definition — a subtitle payload is
//! attacker-controlled in any file — and its five text decoders are all
//! hand-written scanners over `<`/`>`/`&` with index arithmetic, which is the
//! shape that produces slicing panics. `mov_text` is the harder half: it is
//! binary, its text length and its `styl` entry count are both declared by
//! the input, and a declared count larger than the box it sits in must
//! truncate rather than allocate or panic.
//!
//! Properties asserted here, beyond "does not panic":
//!
//! - No decoder emits a raw CR or LF. Line breaks must become `\N`; one that
//!   survives would split a single event into two dialogue lines downstream.
//! - Output is bounded by a small multiple of input, so a cue cannot be an
//!   amplification vector.
//!
//! fuzz-crate: vaco-codec-subtitle-text

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_subtitle_text::{TextCodec, decode};

const CODECS: [TextCodec; 6] = [
    TextCodec::SubRip,
    TextCodec::Ass,
    TextCodec::WebVtt,
    TextCodec::MovText,
    TextCodec::Text,
    TextCodec::Ttml,
];

fuzz_target!(|data: &[u8]| {
    for codec in CODECS {
        let Some(out) = decode(codec, data) else {
            continue;
        };
        assert!(
            !out.contains('\n') && !out.contains('\r'),
            "{} leaked a raw line break",
            codec.name()
        );
        // TTML resolves XML entities, so one input byte can become several
        // output bytes; 16x plus a constant is far above any real expansion
        // and still catches an unbounded one.
        assert!(
            out.len() <= data.len().saturating_mul(16).saturating_add(64),
            "{} expanded {} bytes to {}",
            codec.name(),
            data.len(),
            out.len()
        );
    }
});
