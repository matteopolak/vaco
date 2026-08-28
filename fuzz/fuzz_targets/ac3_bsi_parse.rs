//! `syncinfo()`/`bsi()` parsing over arbitrary bytes, isolated from the
//! decode pipeline that also exercises it (`ac3_decode`) — this is the
//! narrower target for `vaco-format-ac3` itself, so a regression here is
//! attributed to the header parser rather than diagnosed by re-deriving it
//! from a decode-pipeline crash.
//!
//! fuzz-crate: vaco-format-ac3

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_ac3::bsi::Bsi;
use vaco_format_ac3::syncinfo;

fuzz_target!(|data: &[u8]| {
    let Some(info) = syncinfo::parse(data) else {
        return;
    };
    assert!(info.frame_size > 0);
    assert!(info.sample_rate > 0);
    let _ = Bsi::parse(data, &info);
});
