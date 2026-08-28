//! Arbitrary filtergraph text against every filter this crate registers —
//! plan 16 §4.4's multimedia/T1-plumbing row, plus the T1 graph-plumbing
//! filters it carried under its previous name (`vaco-filter-plumbing`).
//!
//! # Where a length or count could turn option text into a big allocation
//!
//! `concat`'s `n`/`v`/`a` combine multiplicatively into a pad count
//! (`Concat::create` guards this with `checked_mul` plus a `pads::MAX`
//! ceiling). `interleave`/`ainterleave`/`streamselect`/`astreamselect`
//! take `inputs`/`nb_inputs` directly from the option text and size a
//! `Vec<bool>`/`Vec<FormatSet>` from it — guarded the same way, through
//! `vaco_filter_graph::registry::pads::of` *before* any allocation sized by
//! that count runs. `segment`/`asegment`'s boundary list and
//! `sendcmd`/`asendcmd`'s parsed interval list both grow one small element
//! per token, proportional to the text actually supplied rather than to a
//! numeric value multiplied into something larger — the `cellauto`
//! `size=WxH` shape does not apply to either. `loop`/`aloop`'s `size` (up
//! to 32767 frames, or `INT_MAX` samples for `aloop`) and `reverse`/
//! `areverse` (unbounded by design — buffers the entire clip) both retain
//! real, already-allocated frames rather than pre-sizing a buffer from an
//! option, and are charged against a `vaco_limits::Budget` as they
//! accumulate; `cue`/`acue`'s `buffer` duration is the same shape. This
//! target's job is confirming none of that amplifies into an
//! out-of-proportion allocation or a panic, for any option text at all.
//!
//! Routed through the real `vaco_filter_graph::ast::parse`/`arguments()`
//! pipeline rather than a hand-built `Instantiate`, matching the pattern in
//! `filter_audio_options.rs` (see that target's doc for why going through
//! the actual graph-string grammar matters, not just each filter's own
//! parser in isolation).
//!
//! Property: for any byte string, for any of this crate's registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-mm

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_mm::registry::MmRegistry;

const NAMES: &[&str] = &[
    "acopy",
    "ainterleave",
    "aloop",
    "ametadata",
    "anull",
    "anullsink",
    "anullsrc",
    "areverse",
    "abench",
    "acue",
    "alatency",
    "aperms",
    "arealtime",
    "asegment",
    "aselect",
    "asendcmd",
    "asidedata",
    "astreamselect",
    "asettb",
    "asetpts",
    "asplit",
    "atrim",
    "color",
    "concat",
    "copy",
    "interleave",
    "loop",
    "metadata",
    "null",
    "nullsink",
    "nullsrc",
    "reverse",
    "bench",
    "cue",
    "latency",
    "perms",
    "realtime",
    "segment",
    "select",
    "sendcmd",
    "sidedata",
    "streamselect",
    "settb",
    "setpts",
    "split",
    "trim",
];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, exactly the
    // bound `filter_audio_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = MmRegistry;
    for &name in NAMES {
        // Route through the real graph-string grammar so the filter's parser
        // sees text the way it actually would from `-filter_complex`,
        // escaping included, rather than a hand-assembled `Instantiate`.
        let text = format!("{name}={args}");
        let Ok(ast) = vaco_filter_graph::parse(&text) else {
            continue;
        };
        let Some(spec) = ast.chains.first().and_then(|c| c.filters.first()) else {
            continue;
        };
        let Ok(arguments) = spec.arguments() else {
            continue;
        };
        let req = Instantiate {
            name: &spec.name,
            instance: spec.instance.as_deref().unwrap_or(&spec.name),
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        // The result is not inspected: a clean `Err` and a working `Instance`
        // are both fine outcomes. Only a panic or a hang is a finding.
        let _ = registry.create(&req);
    }
});
