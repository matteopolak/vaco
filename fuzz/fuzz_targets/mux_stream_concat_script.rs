//! `concat::script::parse` over arbitrary bytes.
//!
//! A concat script is the one input in this pair of crates that is *always*
//! attacker-shaped by construction: `vaco -f concat -safe 0 -i list.txt`
//! feeds this parser a file the caller does not control the contents of,
//! and its quote/backslash grammar ([`vaco_core::escape::split_raw`]) is
//! shared with `tee`'s, so a bug the tee fuzz target's corpus does not
//! happen to hit here is still worth finding independently.
//!
//! Properties asserted:
//!
//! * Parsing never panics on any byte sequence, decoded lossily to `&str`
//!   first, under both `safe` settings (the `option` directive's
//!   accept/reject path only exercises with `safe=false`, so both are
//!   fuzzed rather than only one).
//! * Parsing is deterministic.
//! * [`vaco_mux_stream::concat::resolve_entries`] never panics on whatever
//!   [`vaco_mux_stream::concat::script::parse`] accepts — the layer that
//!   turns directives into [`vaco_mux_stream::concat::FileEntry`] values is
//!   exercised too, not just tokenising.
//!
//! fuzz-crate: vaco-mux-stream

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_mux_stream::concat::{resolve_entries, script};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);

    for &safe in &[true, false] {
        let parsed = script::parse(&text, safe);
        let parsed_again = script::parse(&text, safe);
        assert_eq!(parsed, parsed_again, "parse is not deterministic (safe={safe})");

        if let Ok(script) = parsed {
            let _ = resolve_entries(&script);
        }
    }
});
