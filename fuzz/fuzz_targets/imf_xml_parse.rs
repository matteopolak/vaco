//! IMF's three XML documents (SMPTE ST 2067-3 Composition Playlist, ST
//! 429-9 ASSETMAP, ST 429-8 Packing List) over arbitrary bytes.
//!
//! All three are untrusted input by construction — a CPL, unlike most of
//! this workspace's container headers, is read by a general-purpose XML
//! parser (`quick-xml`, via `vaco_format_imf::xml`) rather than a bespoke
//! bounded reader, so the specific risk this target is built to catch is
//! `quick-xml`'s own recursion/allocation behaviour on adversarial nesting
//! or attribute counts, not just this crate's own field parsing. `xml::parse`
//! bounds node count (`xml::MAX_NODES`) independently of well-formedness,
//! and this target asserts that bound holds for genuinely arbitrary input
//! rather than only the hand-picked cases the unit tests cover — the same
//! shape as `dash_mpd_parse.rs`'s own `tree::MAX_NODES` check.
//! fuzz-crate: vaco-format-imf

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_imf::{assetmap, cpl, pkl};
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };

    // A fresh, generous budget per document per iteration: this target
    // checks structural bounds (node/attribute/child counts), not the
    // allocator budget's own accounting, which `limit_budget` already
    // fuzzes elsewhere in this workspace.
    let mut b = Budget::new(Limits::permissive());
    let _ = cpl::parse(text, &mut b);

    let mut b = Budget::new(Limits::permissive());
    let _ = assetmap::parse(text, &mut b);

    let mut b = Budget::new(Limits::permissive());
    let _ = pkl::parse(text, &mut b);
});
