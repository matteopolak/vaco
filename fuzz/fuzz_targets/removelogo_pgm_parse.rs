//! `removelogo`'s PGM (P5) mask parser over arbitrary bytes — the one
//! genuinely untrusted-input surface in `vaco-filter-artistic`: every
//! other filter in that crate only ever sees decoded frames from a trusted
//! pipeline stage, but this parser reads a user-supplied file whose header
//! declares its own width/height.
//!
//! fuzz-crate: vaco-filter-artistic

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_artistic::removelogo::parse_pgm;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    if let Ok(mask) = parse_pgm(data, &mut budget) {
        assert!(mask.width > 0);
        assert!(mask.height > 0);
    }
});
