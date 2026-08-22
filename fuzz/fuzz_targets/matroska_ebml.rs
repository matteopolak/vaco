//! The EBML layer alone: VINTs, the child walker, and the unknown-size
//! termination rule.
//!
//! Separate from `matroska_demux` because it runs at a different depth. The
//! whole-file target spends most of its budget getting past the header; this one
//! feeds arbitrary bytes straight into the grammar, which is where EBML's
//! pathological inputs live — a nested variable-length structure where both the
//! ID and the size are attacker-chosen, and where one size field can claim
//! 72 petabytes.
//!
//! Properties asserted:
//!
//! * **The walker terminates and stays inside its buffer.** Every child is a
//!   disjoint in-order sub-slice, and the cursor advances by at least the
//!   header length each step.
//! * **VINT decode and encode agree.** Whatever `read_size` accepts,
//!   `synth::vint_min` re-encodes to the same value.
//! * **Termination is monotone.** `Stack::terminations_for` never claims to
//!   close more frames than are open, and closing is idempotent: after popping
//!   what it asked for, the same ID needs no further closes.
//!
//! fuzz-crate: vaco-demux-matroska

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_demux_matroska::ebml::{self, Caps, Size};
use vaco_demux_matroska::synth;

/// Children walked before the run is treated as non-terminating.
const MAX_CHILDREN: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    let caps = Caps::default();

    // --- the child walker
    let mut end = 0usize;
    let mut count = 0usize;
    for child in ebml::Slice::new(data, caps).children() {
        assert!(child.offset >= end, "children overlap or go backwards");
        assert!(
            child.data_offset >= child.offset,
            "data precedes its header"
        );
        let child_end = child
            .data_offset
            .checked_add(child.data.len())
            .expect("child extent overflows a usize");
        assert!(child_end <= data.len(), "child runs past its parent");
        end = child_end;
        count += 1;
        assert!(count < MAX_CHILDREN, "the walker did not terminate");
    }

    // --- VINT round trip, over whatever prefix the input happens to hold
    if let Ok((size, used)) = ebml::read_size(data, ebml::MAX_SIZE_LEN) {
        assert!((1..=8).contains(&used));
        if let Size::Known(v) = size {
            // The shortest encoding of a value decodes back to that value.
            let re = synth::vint_min(v);
            assert_eq!(
                ebml::read_size(&re, ebml::MAX_SIZE_LEN)
                    .ok()
                    .map(|(s, _)| s),
                Some(Size::Known(v)),
                "vint_min({v}) did not round trip"
            );
        }
    }
    if let Ok((id, used)) = ebml::read_id(data, ebml::MAX_ID_LEN) {
        assert!((1..=4).contains(&used));
        assert!(id != 0, "a zero element id is not decodable");
    }
    if let Ok((v, used)) = ebml::read_signed_vint(data) {
        assert!((1..=8).contains(&used));
        let re = synth::signed_vint(v);
        assert_eq!(
            ebml::read_signed_vint(&re).ok().map(|(x, _)| x),
            Some(v),
            "signed_vint({v}) did not round trip"
        );
    }

    // --- RFC 8794 section 6.2, driven by IDs the input chose
    let mut stack = ebml::Stack::new();
    // Build a stack out of the input's own bytes, alternating known and unknown
    // sizes so both branches of the rule are exercised.
    for (i, chunk) in data.chunks(4).take(ebml::Stack::MAX_FRAMES).enumerate() {
        let mut id = 0u32;
        for &b in chunk {
            id = (id << 8) | u32::from(b);
        }
        let end = if i % 2 == 0 {
            None
        } else {
            Some(i as u64 * 97)
        };
        if stack.push(id, end).is_err() {
            break;
        }
    }
    let depth = stack.depth();
    for chunk in data.chunks(4).take(64) {
        let mut id = 0u32;
        for &b in chunk {
            id = (id << 8) | u32::from(b);
        }
        if let Some(n) = stack.terminations_for(id) {
            assert!(n <= depth, "claimed to close {n} of {depth} frames");
            let mut probe = stack.clone();
            probe.truncate_by(n);
            // Idempotent: after closing what it asked for, nothing more closes.
            assert_eq!(
                probe.terminations_for(id),
                Some(0),
                "termination is not a fixed point"
            );
        }
    }
});
