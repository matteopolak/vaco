//! `key=value:key=value` option strings against arbitrary text.
//! fuzz-crate: vaco-core
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_core::dict::{Dict, DictFlags};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else { return };
    let mut d = Dict::new();
    if d.parse_string(s, "=", ":", DictFlags::default()).is_ok() {
        // Every key that parsed must be retrievable under its own name.
        let pairs: Vec<(String, String)> =
            d.iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        for (k, v) in &pairs {
            assert_eq!(d.get(k), Some(v.as_str()), "key {k:?} not retrievable after parse");
        }
        assert_eq!(d.len(), pairs.len());
    }
});
