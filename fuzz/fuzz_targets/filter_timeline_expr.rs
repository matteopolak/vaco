//! Arbitrary text as a filter's `enable=` expression.
//!
//! This is the one place `vaco-filter-core` touches untrusted input directly.
//! `-vf 'hflip=enable=<here>'` hands it whatever the user typed, and a
//! filtergraph pulled from a playlist, a preset file or a web request is not
//! trustworthy at all. The property is totality: no panic, no unbounded
//! allocation, no hang, on any byte string, followed by any number of
//! evaluations against any frame.
//!
//! The evaluation half matters as much as the parse half. A `Timeline` keeps
//! `st`/`ld` registers *between* frames, so a fuzzer-constructed expression
//! that accumulates state runs against a live register file rather than a fresh
//! one — which is exactly how a real graph runs it.
//! fuzz-crate: vaco-filter-core
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Rational, Timestamp};
use vaco_filter_core::mock::{audio_frame, gray_frame};
use vaco_filter_core::Timeline;

#[derive(Debug, arbitrary::Arbitrary)]
struct Input<'a> {
    expression: &'a str,
    /// A second expression, installed mid-stream the way the `enable` runtime
    /// command does. A rejected one must leave the first in place.
    replacement: Option<&'a str>,
    /// Timestamps to evaluate at, some of them absent.
    stamps: Vec<Option<i64>>,
    audio: bool,
}

fuzz_target!(|input: Input| {
    // 8 KiB is well past any real filter argument; beyond it the fuzzer only
    // measures how fast the parser can scan whitespace.
    if input.expression.len() > 8192 || input.stamps.len() > 256 {
        return;
    }

    let Ok(mut timeline) = Timeline::parse(input.expression) else {
        return;
    };
    let gated = timeline.is_gated();

    for (i, stamp) in input.stamps.iter().enumerate() {
        let mut frame = if input.audio {
            audio_frame(48_000, 64, 0)
        } else {
            gray_frame(8, 8, 0, 0)
        };
        frame.pts = stamp.map_or(Timestamp::NONE, Timestamp::new);
        frame.time_base = Rational::new(1, 25);
        let enabled = timeline.evaluate(&frame, i as u64);
        // The cached result must agree with what was just returned; a filter
        // reads one and the framework reads the other.
        assert_eq!(enabled, timeline.enabled());

        if let Some(replacement) = input.replacement {
            if i == input.stamps.len() / 2 && replacement.len() <= 8192 {
                let before = timeline.is_gated();
                if timeline.set_expression(replacement).is_err() {
                    // A rejected command leaves the object unmodified. If this
                    // ever fails, a bad `sendcmd` has silently disabled a filter
                    // for the rest of the stream.
                    assert_eq!(timeline.is_gated(), before);
                }
            }
        }
    }

    // Parsing is a pure function of the text.
    if let Ok(again) = Timeline::parse(input.expression) {
        assert_eq!(again.is_gated(), gated);
    }
});
