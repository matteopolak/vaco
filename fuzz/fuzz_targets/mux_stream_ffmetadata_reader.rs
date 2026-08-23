//! `ffmetadata::parse` over arbitrary bytes.
//!
//! This is the one reader in `vaco-mux-stream` that takes fully untrusted
//! text: a `;FFMETADATA1` document handed to `vaco -i script.ffmeta` would
//! reach this exact function. The escaping rules (backslash removes the
//! next character's special meaning, whatever it is; a `[SECTION]` line
//! switches which list following `key=value` lines land in) are exactly the
//! kind of "looks simple, has a corner" grammar D6 wants fuzzed rather than
//! only unit-tested.
//!
//! Properties asserted:
//!
//! * Parsing never panics on any byte sequence (lossily decoded to `&str`
//!   first, since [`vaco_mux_stream::ffmetadata::parse`] takes one — the
//!   lossy substitution still exercises every branch the escaping and
//!   section-tracking state machine has, since replacement characters are
//!   ordinary, non-special text to this grammar).
//! * Parsing is deterministic: the same input parses to the same result
//!   twice.
//! * Every value [`vaco_mux_stream::ffmetadata::write`] re-escapes for a
//!   parsed document's global metadata reads back byte-identical through
//!   [`vaco_mux_stream::ffmetadata::parse`] on its own — the same round-trip
//!   invariant `ffmetadata`'s proptest checks, exercised here against
//!   whatever the corpus mutates into rather than a generator's own
//!   distribution.
//!
//! fuzz-crate: vaco-mux-stream

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_mux_stream::ffmetadata;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);

    let parsed = ffmetadata::parse(&text);
    let parsed_again = ffmetadata::parse(&text);
    assert_eq!(parsed, parsed_again, "parse is not deterministic");

    // Every global (key, value) pair, written back out on its own, must
    // parse back to exactly that pair — this is what makes the escaping
    // safe to feed a differential comparison downstream.
    for (key, value) in &parsed.global {
        let rendered = ffmetadata::write(std::slice::from_ref(&(key.clone(), value.clone())), &[], &[]);
        let reparsed = ffmetadata::parse(&rendered);
        assert!(
            reparsed.global.contains(&(key.clone(), value.clone())),
            "round trip lost a global pair: key={key:?} value={value:?} rendered={rendered:?}"
        );
    }
});
