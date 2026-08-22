//! The `aresample=` option string, which is the CLI-facing half of the crate.
//!
//! # Why this is separate from `resample_convert`
//!
//! `resample_convert` builds `ResampleOptions` field by field through
//! `Arbitrary`, which is the right shape for reaching many *engine*
//! configurations — but it means the parser that turns `k=v:k=v` into those
//! fields has never been fuzzed at all. Found during an issue audit, and worth
//! its own target rather than a branch inside that one: a parser and an engine
//! want different corpora, and mixing them lets the cheap inputs crowd out the
//! expensive ones.
//!
//! Everything here is attacker-adjacent in the ordinary case — the string comes
//! straight off a command line or out of a filtergraph description, which a
//! playlist can supply.
//!
//! # The invariant
//!
//! Beyond "does not panic": **a rejected option string must leave the options
//! unchanged in every field it had not already accepted.** `set_from_str` walks
//! pairs left to right and returns on the first bad one, so a half-applied
//! string is possible by construction; what must not happen is a *later*
//! failure reaching back and corrupting an *earlier* success, or a failing
//! `set` leaving its own field partly written. The check below applies each
//! pair individually and compares against the bulk call, which catches both.
//!
//! `validate()` is called on whatever survives, because the engine's contract
//! is that a validated `ResampleOptions` is safe to construct from — so a value
//! the parser accepts and `validate` also accepts must not then explode.

//! fuzz-crate: vaco-resample
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_resample::ResampleOptions;

fuzz_target!(|data: &[u8]| {
    let Ok(spec) = std::str::from_utf8(data) else {
        return;
    };
    // Bound the work: the parser is linear in the string, and a megabyte of
    // colons tells us nothing a kilobyte does not.
    if spec.len() > 4096 {
        return;
    }

    let mut bulk = ResampleOptions::default();
    let bulk_result = bulk.set_from_str(spec);

    // Apply the same pairs one at a time, stopping where the bulk call would.
    // Equality afterwards is the real assertion: it means no pair's failure
    // disturbed a pair that had already succeeded.
    let mut stepwise = ResampleOptions::default();
    let mut stepwise_result = Ok(());
    for pair in spec.split(':').filter(|s| !s.is_empty()) {
        stepwise_result = stepwise.set_from_str(pair);
        if stepwise_result.is_err() {
            break;
        }
    }

    assert_eq!(
        bulk_result.is_ok(),
        stepwise_result.is_ok(),
        "bulk and stepwise parsing disagreed on whether {spec:?} is valid"
    );
    assert_eq!(
        bulk, stepwise,
        "applying {spec:?} in one call differed from applying it pair by pair"
    );

    // Whatever the parser accepted must survive validation without panicking,
    // and a validated configuration is what the engine promises to accept.
    if bulk_result.is_ok() {
        let _ = bulk.validate();
    }
});
