//! The harness's TOML subset reader against arbitrary bytes.
//!
//! Manifests and the divergence register are repository content, not hostile
//! input — but this is a hand-written parser with a byte cursor, and D6's rule
//! is that a parser without a fuzz target is not done. What this proves is the
//! property the workspace lints demand everywhere: it terminates and it does
//! not panic, whatever it is handed.
//! fuzz-crate: vaco-conformance
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_conformance::toml;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else { return };
    // A parse either succeeds or reports an error. Neither may panic, and a
    // successful parse must survive being walked.
    if let Ok(table) = toml::parse(s) {
        for (_key, value) in &table {
            let _ = value.kind();
            let _ = value.as_str();
            let _ = value.as_int();
            let _ = value.as_f64();
            let _ = value.as_bool();
            let _ = value.as_str_array();
            if let Some(items) = value.as_array() {
                for item in items {
                    let _ = item.kind();
                }
            }
            if let Some(inner) = value.as_table() {
                for (_k, v) in inner {
                    let _ = v.kind();
                }
            }
        }
    }
});
