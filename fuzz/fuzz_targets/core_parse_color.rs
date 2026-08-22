//! `-fill_color red@0.5` against arbitrary text.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_core::parse;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else { return };
    // `random` is deliberately non-deterministic, so it cannot round-trip.
    if s.eq_ignore_ascii_case("random") { return }
    if let Some(c) = parse::color(s) {
        let rendered = parse::format_color(c);
        assert_eq!(parse::color(&rendered), Some(c), "colour round-trip failed for {s:?}");
    }
});
