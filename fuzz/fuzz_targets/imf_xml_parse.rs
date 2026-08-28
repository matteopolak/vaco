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
//!
//! That assertion was only ever described here, not written: `cpl`/
//! `assetmap`/`pkl::parse` build and discard their own internal
//! `xml::XmlNode` tree, so calling them alone leaves nothing to count
//! against `MAX_NODES`. Fixed by calling `xml::parse` directly too, exactly
//! as `dash_mpd_parse.rs` calls `tree::parse` directly, and asserting a
//! successfully parsed tree never exceeds it.
//! fuzz-crate: vaco-format-imf

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_imf::xml::{self, MAX_NODES, XmlNode};
use vaco_format_imf::{assetmap, cpl, pkl};
use vaco_limits::{Budget, Limits};

fn node_count(node: &XmlNode) -> u64 {
    let mut total = 1u64;
    for child in &node.children {
        total = total.saturating_add(node_count(child));
    }
    total
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };

    // A fresh, generous budget per document per iteration: this target
    // checks structural bounds (node/attribute/child counts), not the
    // allocator budget's own accounting, which `limit_budget` already
    // fuzzes elsewhere in this workspace.
    let mut b = Budget::new(Limits::permissive());
    if let Ok(root) = xml::parse(text, &mut b) {
        let count = node_count(&root);
        assert!(
            count <= MAX_NODES,
            "{count} nodes parsed successfully — xml::MAX_NODES ({MAX_NODES}) was lost"
        );
    }

    let mut b = Budget::new(Limits::permissive());
    let _ = cpl::parse(text, &mut b);

    let mut b = Budget::new(Limits::permissive());
    let _ = assetmap::parse(text, &mut b);

    let mut b = Budget::new(Limits::permissive());
    let _ = pkl::parse(text, &mut b);
});
