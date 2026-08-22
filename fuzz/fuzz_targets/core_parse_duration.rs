//! `-t 00:01:30.5` against arbitrary text.
//!
//! Property: never panics, and anything that parses formats back to something
//! that parses to the same value.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_core::parse;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else { return };
    if let Some(d) = parse::duration(s) {
        let rendered = parse::format_duration(d);
        assert_eq!(parse::duration(&rendered), Some(d), "duration round-trip failed for {s:?}");
    }
});
