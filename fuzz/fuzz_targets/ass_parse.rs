//! Arbitrary bytes as an ASS/SSA script: `vaco_ass::parse` must never
//! panic, and `plan_event` must never panic on any event it produces —
//! both parsing and override-tag interpretation run on attacker-controlled
//! subtitle files (`planning/AGENT-CONSTRAINTS.md`'s "Fonts and ASS
//! drawing commands are attacker-controlled: bound everything").
//!
//! Property: for any byte string interpreted as UTF-8 (lossily, matching
//! what a real filter does with a file whose encoding it does not fully
//! trust), `parse` returns a `Script`, and `plan_event` against every
//! event that script contains completes without panicking, looping
//! forever, or allocating unboundedly relative to the input size.
//! fuzz-crate: vaco-ass

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let script = vaco_ass::parse(&text);
    for event in &script.events {
        let _ = vaco_ass::plan_event(&script, event);
    }
});
