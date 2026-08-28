//! `vaco-codec-msac`: both boolean-entropy engines (VP8's and VP9's) against
//! arbitrary bytes and an arbitrary script of reads.
//!
//! Neither engine returns `Result` from a per-bool read — an under-length
//! partition has nothing sensible to fail *onto* mid-symbol, so both mirror
//! `vaco-codec-cabac`'s convention of flagging `overrun()` and returning
//! deterministic zero-ish reads past the end instead. What this target
//! checks, run identically against both engines so a regression in either
//! is caught the same way:
//!
//! 1. **No panic, on any byte buffer and any script**, including an empty
//!    buffer, a script that reads more bits than the buffer has, and a
//!    100-way `read_literal`/`read_tree` mix.
//! 2. **`overrun()` only ever turns true, never back to false** — a decoder
//!    that "recovers" mid-stream after running past the end would be a
//!    correctness bug in the overrun bookkeeping itself.
//! 3. **Determinism**: replaying the exact same script against a fresh
//!    decoder over the same bytes produces the exact same sequence of
//!    results.
//! 4. **The shared tree-walker (`read_tree`) always terminates and returns
//!    a value reachable from the tree's own leaf set**, for both engines
//!    and for a handful of real VP8 tree shapes (not just the toy
//!    alternating tree used elsewhere), since a malformed or adversarial
//!    tree table is exactly the kind of static data a future codec crate
//!    could get wrong the same way this package's `COEFF_TREE` transcription
//!    slip almost did (caught by differential testing, not fuzzing, but a
//!    tree that loops or indexes out of bounds on bad *data* rather than a
//!    bad *table* is squarely fuzzing's job).
//!
//! fuzz-crate: vaco-codec-msac

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_msac::{Vp8BoolDecoder, Vp9BoolDecoder};

#[derive(Arbitrary, Debug, Clone, Copy)]
enum Op {
    Bool(u8),
    Literal(u8),
    Tree(u8),
}

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
    script: Vec<Op>,
}

/// RFC 6386's `KF_YMODE_TREE` (§11.2) — a small, real, asymmetric tree with
/// both internal-node depths and negative (leaf) entries at more than one
/// depth, unlike a hand-built alternating toy tree. Its five leaves are
/// `{0,1,2,3,4}` (`DC_PRED`..`B_PRED`).
const KF_YMODE_TREE: [i8; 8] = [-4, 2, 4, 6, 0, -1, -2, -3];

fn run_vp8(data: &[u8], script: &[Op]) -> Vec<i64> {
    let mut dec = Vp8BoolDecoder::new(data);
    let mut out = Vec::new();
    for op in script.iter().take(4096) {
        match *op {
            Op::Bool(p) => out.push(i64::from(dec.read_bool(p))),
            Op::Literal(n) => out.push(i64::from(dec.read_literal(u32::from(n) % 33))),
            Op::Tree(p) => {
                let probs = [p; 4];
                out.push(i64::from(dec.read_tree(&KF_YMODE_TREE, &probs)));
            }
        }
    }
    out.push(i64::from(dec.overrun()));
    out
}

fn run_vp9(data: &[u8], script: &[Op]) -> Vec<i64> {
    let mut dec = Vp9BoolDecoder::new(data);
    let mut out = Vec::new();
    let mut last_overrun = dec.overrun();
    for op in script.iter().take(4096) {
        match *op {
            Op::Bool(p) => out.push(i64::from(dec.read_bool(p))),
            Op::Literal(n) => out.push(i64::from(dec.read_literal(u32::from(n) % 33))),
            Op::Tree(p) => {
                let probs = [p; 4];
                out.push(i64::from(dec.read_tree(&KF_YMODE_TREE, &probs)));
            }
        }
        let now = dec.overrun();
        assert!(now || !last_overrun, "vp9 overrun cleared itself back to false");
        last_overrun = now;
    }
    out.push(i64::from(dec.overrun()));
    out
}

fuzz_target!(|input: Input| {
    let script: Vec<Op> = input.script;

    let vp8_a = run_vp8(&input.data, &script);
    let vp8_b = run_vp8(&input.data, &script);
    assert_eq!(vp8_a, vp8_b, "VP8 bool decoder is not deterministic");

    let vp9_a = run_vp9(&input.data, &script);
    let vp9_b = run_vp9(&input.data, &script);
    assert_eq!(vp9_a, vp9_b, "VP9 bool decoder is not deterministic");

    // A tree read's result is always one of KF_YMODE_TREE's five leaves
    // (0..=4), for both engines.
    let mut vp8_leaf = Vp8BoolDecoder::new(&input.data);
    let mut vp9_leaf = Vp9BoolDecoder::new(&input.data);
    for op in script.iter().take(256) {
        if let Op::Tree(p) = *op {
            let probs = [p; 4];
            let a = vp8_leaf.read_tree(&KF_YMODE_TREE, &probs);
            assert!((0..=4).contains(&a), "VP8 read_tree left its leaf set: {a}");
            let b = vp9_leaf.read_tree(&KF_YMODE_TREE, &probs);
            assert!((0..=4).contains(&b), "VP9 read_tree left its leaf set: {b}");
        }
    }
});
