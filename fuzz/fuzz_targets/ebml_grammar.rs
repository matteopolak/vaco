//! The generic EBML grammar alone: VINTs, the in-memory child walker, the
//! streaming header reader, and the open-element stack — everything in
//! `vaco-format-ebml`, fuzzed without any concrete schema on top of it.
//!
//! `vaco-demux-matroska`'s own `matroska_ebml` target already fuzzes this
//! grammar with the *Matroska* schema layered on; this target exists to keep
//! `vaco-format-ebml` itself honestly covered as a standalone crate; a small
//! synthetic two-level schema built from the input's own bytes stands in for
//! Matroska's, so a bug in the generic mechanism cannot hide behind "well,
//! the Matroska table happens to avoid it".
//!
//! Properties asserted:
//!
//! * **The child walker terminates and stays inside its buffer.** Every
//!   child is a disjoint in-order sub-slice, and the cursor advances by at
//!   least the header length each step.
//! * **VINT encode and decode agree.** Whatever `read_size`/`read_id`
//!   accepts, `vint_min`/`id_bytes` re-encode to the same value, and
//!   `read_header` over an `IoContext` agrees with `read_id`/`read_size` over
//!   the same bytes read from a slice.
//! * **`Stack::terminations_for` is monotone and a fixed point.** It never
//!   claims to close more frames than are open, and closing exactly what it
//!   asked for leaves nothing more to close for the same ID.
//!
//! fuzz-crate: vaco-format-ebml

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_ebml::{
    Caps, Size, Slice, Stack, id_bytes, read_header, read_id, read_signed_vint, read_size,
    signed_vint, vint_min,
};
use vaco_io::{IoContext, IoOptions, MemorySource};

/// Children/terminations walked before the run is treated as non-terminating.
const RUNAWAY: usize = 1 << 20;

/// A tiny two-level schema, independent of Matroska's: `ROOT(0) -> A(1) ->
/// B(2)`, plus a global `G(3)` legal everywhere — enough shape to exercise
/// every branch of `terminations_for` (sibling closes, root closes
/// everything, a known-size frame is skipped rather than closed, an unknown
/// ID closes nothing) without depending on any real element tree.
fn is_child_of(child: u32, parent: u32) -> bool {
    match child {
        1 => parent == 0, // A is a child of ROOT
        2 => parent == 1, // B is a child of A
        3 => true,        // G is global
        _ => false,
    }
}

fn is_root(id: u32) -> bool {
    id == 1
}

fuzz_target!(|data: &[u8]| {
    let caps = Caps::default();

    // --- the child walker
    let mut end = 0usize;
    let mut count = 0usize;
    for child in Slice::new(data, caps).children() {
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
        assert!(count < RUNAWAY, "the walker did not terminate");
    }

    // --- VINT round trip, over whatever prefix the input happens to hold
    if let Ok((size, used)) = read_size(data, 8) {
        assert!((1..=8).contains(&used));
        if let Size::Known(v) = size {
            let re = vint_min(v);
            assert_eq!(
                read_size(&re, 8).ok().map(|(s, _)| s),
                Some(Size::Known(v)),
                "vint_min({v}) did not round trip"
            );
        }
    }
    if let Ok((id, used)) = read_id(data, 4) {
        assert!((1..=4).contains(&used));
        assert_ne!(id, 0, "a zero element id is not decodable");
        // `id_bytes` is only guaranteed to re-encode a value that already
        // carries a correctly placed marker bit for its width (see
        // `vaco-format-ebml`'s own docs on this) — which is exactly what a
        // value `read_id` just accepted has, since `read_id` requires the
        // marker to decode the length in the first place.
        let re = id_bytes(id);
        assert_eq!(
            read_id(&re, 4).ok(),
            Some((id, re.len())),
            "id_bytes({id:#x}) did not round trip"
        );
    }
    if let Ok((v, used)) = read_signed_vint(data) {
        assert!((1..=8).contains(&used));
        let re = signed_vint(v);
        assert_eq!(
            read_signed_vint(&re).ok().map(|(x, _)| x),
            Some(v),
            "signed_vint({v}) did not round trip"
        );
    }

    // --- the streaming header reader agrees with the slice-based decoders
    let src: Box<dyn vaco_io::MediaSource> = Box::new(MemorySource::new(data.to_vec()));
    if let Ok(mut io) = IoContext::new(src, &IoOptions::default())
        && let Ok(Some(header)) = read_header(&mut io, caps)
        && let Ok((slice_id, id_len)) = read_id(data, caps.max_id_len)
    {
        assert_eq!(header.id, slice_id, "stream and slice readers disagree on the id");
        if let Some(rest) = data.get(id_len..)
            && let Ok((slice_size, _)) = read_size(rest, caps.max_size_len)
        {
            assert_eq!(
                header.size, slice_size,
                "stream and slice readers disagree on the size"
            );
        }
    }

    // --- RFC 8794 section 6.2, driven by IDs the input chose, against the
    // small synthetic schema above rather than Matroska's.
    let mut stack = Stack::new();
    for (i, chunk) in data.chunks(4).take(Stack::MAX_FRAMES).enumerate() {
        let mut id = 0u32;
        for &b in chunk {
            id = (id << 8) | u32::from(b);
        }
        // Keep ids inside the tiny schema's alphabet plus a couple of
        // out-of-schema values, so the walk exercises real transitions
        // instead of "unknown id, no-op" almost every time.
        let id = id % 6;
        let end = if i % 2 == 0 { None } else { Some(i as u64 * 97) };
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
        let id = id % 6;
        if let Some(n) = stack.terminations_for(id, 0, is_child_of, is_root) {
            assert!(n <= depth, "claimed to close {n} of {depth} frames");
            let mut probe = stack.clone();
            probe.truncate_by(n);
            assert_eq!(
                probe.terminations_for(id, 0, is_child_of, is_root),
                Some(0),
                "termination is not a fixed point"
            );
        }
    }
});
