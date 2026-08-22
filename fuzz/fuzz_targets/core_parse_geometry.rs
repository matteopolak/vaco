//! `-s 1920x1080` and `-r 30000/1001` against arbitrary text.
//! fuzz-crate: vaco-core
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_core::parse;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else { return };
    if let Some((w, h)) = parse::image_size(s) {
        // A parsed size is always usable as a frame allocation.
        assert!(w > 0 && h > 0, "image_size accepted a zero dimension from {s:?}");
    }
    if let Some(r) = parse::video_rate(s) {
        // A frame rate that parses must be usable as one: strictly positive and
        // finite. The reference rejects 0, 0/0, 0/5, -25 and 1/0 outright, and
        // returning our UNDEFINED sentinel here would be indistinguishable from
        // "rate unknown" downstream.
        assert!(r.num > 0, "video_rate accepted a non-positive rate from {s:?}: {r:?}");
        assert!(r.den > 0, "video_rate accepted an infinite rate from {s:?}: {r:?}");
    }
    let _ = parse::rational(s);
    let _ = parse::boolean(s);
});
