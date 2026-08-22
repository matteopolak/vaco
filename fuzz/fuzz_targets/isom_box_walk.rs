//! The box grammar alone: headers, iteration, the depth-capped walk, and the
//! descriptor tree inside `esds`.
//!
//! Separate from `isom_file` because it is *fast*. Every execution exercises
//! header parsing and the walker directly instead of spending its budget
//! getting a `moov` to parse, so the shapes this finds are the ones that need
//! many executions to reach: a `largesize` that overflows, a `uuid` whose
//! extended type straddles the end, a `size == 0` box that is not last, a
//! `stsd` whose entry count and payload disagree.
//!
//! The two properties asserted are the ones the design rests on:
//!
//! * **Iteration terminates.** `BoxHeader::parse` guarantees
//!   `size >= header_len >= 8`, so every step advances. If that guarantee ever
//!   breaks, this hangs rather than quietly producing an infinite stream.
//! * **Depth is bounded by a constant, not by the input.** The walker uses an
//!   explicit worklist capped at `MAX_DEPTH`, so a file nested a million deep
//!   costs a bounded walk. The unit tests check this for one construction; the
//!   fuzzer checks it for every construction.
//! fuzz-crate: vaco-format-isom

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_isom::boxes::{self, BoxIter, MAX_DEPTH};
use vaco_format_isom::esds;
use vaco_limits::{Budget, Limits};

/// Boxes visited before the target concludes iteration is not terminating.
/// The input is bounded by libFuzzer's `-max_len`, and every box costs at
/// least eight bytes, so a real walk cannot approach this.
const RUNAWAY: usize = 1 << 22;

fuzz_target!(|data: &[u8]| {
    // 1. Flat iteration must terminate and must never produce a box whose
    //    payload is not inside the buffer it came from.
    let mut seen = 0usize;
    for item in BoxIter::new(data, 0) {
        seen += 1;
        assert!(seen < RUNAWAY, "box iteration did not terminate");
        let Ok(b) = item else { break };
        assert!(
            b.payload.len() as u64 <= b.header.size,
            "a payload outgrew its box"
        );
        assert_eq!(
            b.payload_offset(),
            b.offset + b.header.header_len,
            "payload offset is not header-relative"
        );
        // Full-box decoding must be total.
        if let Ok(f) = b.full() {
            assert!(f.flags <= 0x00FF_FFFF, "flags overflowed 24 bits");
            assert!(f.body.len() < data.len().max(1));
        }
        // Children of an arbitrary box must also terminate.
        let mut nested = 0usize;
        for c in b.children() {
            nested += 1;
            assert!(nested < RUNAWAY, "child iteration did not terminate");
            if c.is_err() {
                break;
            }
        }
    }

    // 2. The depth-capped walk. Fuel is generous so the cap, not the budget,
    //    is what is under test.
    let mut budget = Budget::new(Limits::permissive().with_fuel(1 << 20));
    let mut deepest = 0usize;
    let _ = boxes::walk(BoxIter::new(data, 0), &mut budget, |_, depth| {
        deepest = deepest.max(depth);
        true
    });
    assert!(deepest < MAX_DEPTH, "the walk reached depth {deepest}");

    // 3. A tiny fuel budget must fail cleanly rather than doing the work.
    let mut starved = Budget::new(Limits::strict().with_fuel(4));
    let mut visits = 0usize;
    let _ = boxes::walk(BoxIter::new(data, 0), &mut starved, |_, _| {
        visits += 1;
        true
    });
    assert!(visits <= 4, "the walk did {visits} visits on 4 fuel");

    // 4. The MPEG-4 descriptor tree, whose expandable length encoding is its
    //    own parser with its own overflow surface.
    let mut rest = data;
    let mut descriptors = 0usize;
    while let Some((_, _, used)) = esds::read_descriptor(rest) {
        descriptors += 1;
        assert!(descriptors < RUNAWAY, "descriptor walk did not terminate");
        assert!(used > 0, "a descriptor consumed nothing");
        let Some(next) = rest.get(used..) else { break };
        rest = next;
    }
    let _ = esds::EsDescriptor::parse_es(data);
});
