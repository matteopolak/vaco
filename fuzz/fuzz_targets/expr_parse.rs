//! Parse and evaluate arbitrary text as an expression.
//!
//! `vaco-expr` sits directly under the command line: `-vf scale=w='<here>'`
//! hands it whatever the user typed, and a filtergraph pulled from a playlist
//! or a script is not trustworthy input at all. So the property is simply
//! totality — no panic, no unbounded allocation, no hang — on any byte string.
//!
//! Evaluation runs with a small iteration budget so that a `while` the fuzzer
//! happens to construct is a finding about the budget rather than a libFuzzer
//! timeout. (The reference genuinely hangs here; we do not.)
//! fuzz-crate: vaco-expr
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_expr::{Bindings, Context, Expr, Limits, Registers};

const NAMES: &[&str] = &["w", "h", "t", "n", "a", "main_w", "x", "y"];
const VALUES: &[f64] = &[1920.0, 1080.0, 0.5, 3.0, 16.0 / 9.0, 640.0, 0.0, -1.0];

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    // 8 KiB is well past any real filter argument; beyond it the fuzzer only
    // measures how fast we can scan whitespace.
    if src.len() > 8192 {
        return;
    }
    let limits = Limits {
        max_iterations: 4096,
        ..Limits::default()
    };
    let Ok(expr) = Expr::parse_with(src, &Bindings::new(NAMES), limits) else {
        return;
    };

    let mut regs = Registers::new();
    let mut printed = 0u32;
    let mut sink = |_v: f64, _l: f64| printed += 1;
    let value = expr.eval_with(
        &mut Context::new(VALUES, &mut regs)
            .with_limits(limits)
            // Pinned so a `time(0)` in the corpus cannot make a crash
            // irreproducible.
            .with_time(1_700_000_000.0)
            .with_print(&mut sink),
    );
    // Force the result to be observed so nothing above can be optimised away.
    std::hint::black_box(value);
    std::hint::black_box(printed);

    // Evaluating twice with the same registers must not panic either: `st`,
    // `random` and `root` all write state that survives into the second call.
    std::hint::black_box(expr.eval_with(
        &mut Context::new(VALUES, &mut regs)
            .with_limits(limits)
            .with_time(1_700_000_000.0),
    ));
});
