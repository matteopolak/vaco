//! Arbitrary filtergraph text against every T1 audio filter's option parser.
//!
//! `pan`, `channelmap` and `join` in particular hand-roll a mini-grammar
//! (`|`/`,`/`-`/`.`-separated lists, `LAYOUT|OUTSPEC|...`) on top of
//! `vaco_opts`'s own `k=v:k2=v2` splitter, and `pan`'s output feeds a second
//! parser (`vaco_expr::Expr::parse`). All of it runs on `-filter_complex`
//! text, which is exactly the untrusted, attacker-shaped input D6 asks every
//! option surface to be fuzzed against.
//!
//! Routed through the *real* `vaco_filter_graph::ast::parse` pipeline rather
//! than a hand-built `Instantiate`, so this also exercises level-1/level-0
//! escaping (`vaco-filter-graph`'s own doc: the option scanner then the
//! `|`-list scanner) ahead of each filter's own parser — plan 13 §1b's
//! standing warning that probing a parser *through* a filtergraph measures
//! the filtergraph's unescaping unless the whole path is exercised together.
//!
//! Property: for any byte string, for any of the eleven registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-audio

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_audio::registry::AudioRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "aformat",
    "amerge",
    "amix",
    "aresample",
    "asetnsamples",
    "asetrate",
    "channelmap",
    "channelsplit",
    "join",
    "pan",
    "volume",
];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, exactly the
    // bound `filter_timeline_expr` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = AudioRegistry;
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
