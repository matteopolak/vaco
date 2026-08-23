//! `tee::grammar::parse` over arbitrary bytes.
//!
//! The `[opt=val:opt2=val2]path|[opt=val]path2` grammar is exactly the kind
//! of hand-rolled mini-language a `-f tee "..."` argument hands straight to
//! this crate from the command line — untrusted in the sense that D6 cares
//! about (a user can paste anything there), and layered three separator
//! levels deep (`|`, then `:`, then `=`, each quote/backslash-aware via
//! [`vaco_core::escape`]).
//!
//! Properties asserted:
//!
//! * Parsing never panics on any byte sequence (decoded lossily to `&str`
//!   first).
//! * Parsing is deterministic.
//! * Every successfully parsed output list round-trips through
//!   [`vaco_mux_stream::tee::grammar::format`] and back to the identical
//!   list — the same invariant the crate's own proptest checks against a
//!   generator's distribution, exercised here against whatever the fuzzer's
//!   corpus mutation finds instead.
//!
//! fuzz-crate: vaco-mux-stream

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_mux_stream::tee::grammar;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);

    let parsed = grammar::parse(&text);
    let parsed_again = grammar::parse(&text);
    assert_eq!(parsed, parsed_again, "parse is not deterministic");

    if let Ok(outputs) = parsed {
        let rendered = grammar::format(&outputs);
        let reparsed = grammar::parse(&rendered).unwrap_or_else(|e| {
            panic!("re-parsing this crate's own rendering failed: {e} (rendered={rendered:?})")
        });
        assert_eq!(
            outputs, reparsed,
            "round trip changed the parsed outputs (rendered={rendered:?})"
        );
    }
});
