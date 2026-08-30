//! `IoContext` against a scripted sequence of reads, peeks and seeks.
//!
//! The buffer is the one piece of `vaco-io` with real state: a head, a tail, a
//! base offset, a sticky EOF and a short-seek path that silently substitutes
//! read-and-discard for a seek. Every one of those is an opportunity to return
//! the wrong bytes, and none of it is visible to a caller — which is why it is
//! checked against a trivial model rather than against itself.
//!
//! Invariants asserted:
//!
//! * position tracks the model exactly, after every operation;
//! * bytes read are the bytes at that position in the source;
//! * `peek` returns the bytes at the current position and does not move it;
//! * a forward-only source refuses backward seeks rather than lying about them;
//! * a seek past the end is legal (a file allows it too) and leaves the
//!   position wherever it actually landed, which can be past `data.len()` —
//!   the model's cursor is **not** bounded by the source's length, only by
//!   what a later read is allowed to hand back;
//! * once the position is at or past the end, a read or peek reports EOF
//!   (zero bytes) rather than returning phantom data or panicking;
//! * `skip(n)` is all-or-nothing: it either lands exactly `n` bytes ahead of
//!   where it started, or fails leaving the model to resynchronise from
//!   `io.pos()` — never a silent short skip.
//! fuzz-crate: vaco-io
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_io::{IoContext, IoOptions, MemorySource, Seekability};

#[derive(Arbitrary, Debug)]
enum Op {
    Read(u16),
    ReadExact(u16),
    Peek(u16),
    Seek(u32),
    Skip(u16),
    R8,
    Rb32,
    Tag,
    Str(u16),
}

#[derive(Arbitrary, Debug)]
struct Script {
    data: Vec<u8>,
    block: u16,
    kind: u8,
    direct: bool,
    ops: Vec<Op>,
}

/// Keep the fuzzer honest about memory: none of these bounds affect the logic
/// under test, they only stop a single input from asking for a gigabyte.
const MAX_DATA: usize = 1 << 16;
const MAX_LEN: usize = 1 << 14;

fuzz_target!(|script: Script| {
    let mut data = script.data;
    data.truncate(MAX_DATA);

    let seekability = match script.kind % 3 {
        0 => Seekability::Cheap,
        1 => Seekability::Expensive,
        _ => Seekability::None,
    };
    let source = match seekability {
        Seekability::Cheap => MemorySource::new(data.clone()),
        Seekability::Expensive => MemorySource::expensive(data.clone()),
        Seekability::None => MemorySource::forward_only(data.clone()),
    };

    let opts = IoOptions::default()
        .with_block_size(usize::from(script.block).clamp(64, 8192))
        .with_direct(script.direct);
    let Ok(mut io) = IoContext::new(Box::new(source), &opts) else {
        return;
    };

    // The model: one cursor into `data`.
    let mut pos: usize = 0;

    for op in script.ops.iter().take(256) {
        match op {
            Op::Read(n) | Op::ReadExact(n) => {
                let want = usize::from(*n).min(MAX_LEN);
                let mut buf = vec![0u8; want];
                let exact = matches!(op, Op::ReadExact(_));
                let got = if exact {
                    match io.read_exact(&mut buf) {
                        Ok(()) => Some(want),
                        Err(_) => None,
                    }
                } else {
                    io.read_partial(&mut buf).ok()
                };
                match got {
                    Some(0) => {
                        // A zero-length read is always fine, including at a
                        // position past `data.len()` reached by a past-EOF
                        // seek/skip — there is nothing to compare against.
                    }
                    Some(n) => {
                        assert!(pos + n <= data.len(), "read past the end of the source");
                        assert_eq!(&buf[..n], &data[pos..pos + n], "wrong bytes at {pos}");
                        pos += n;
                    }
                    None => {
                        // A failed `read_exact` consumed an unknown amount, so
                        // resynchronise the model from the context.
                        pos = io.pos() as usize;
                    }
                }
            }
            Op::Peek(n) => {
                let want = usize::from(*n).min(MAX_LEN);
                let before = io.pos();
                if let Ok(seen) = io.peek(want) {
                    // `pos` can be past `data.len()` after a past-EOF
                    // seek/skip; clamp both ends so the range stays valid and
                    // degenerates to empty exactly where a real peek would.
                    let start = pos.min(data.len());
                    let end = (start + want).min(data.len());
                    assert_eq!(seen, &data[start..end], "peek returned the wrong bytes");
                }
                assert_eq!(io.pos(), before, "peek moved the position");
            }
            Op::Seek(target) => {
                // Deliberately *not* clamped to `data.len()`: a seek past the
                // end must be legal (that is Defect 1 this target exists to
                // catch), so exercising only in-bounds targets would leave
                // that path permanently unfuzzed.
                let target = u64::from(*target);
                match io.seek(target) {
                    Ok(at) => pos = at as usize,
                    Err(_) => {
                        // Two ways a seek can legitimately fail:
                        // - a backward seek on a forward-only source, or
                        // - a forward hop under the short-seek threshold that
                        //   the buffer serves as read-and-discard, which hits
                        //   real EOF before covering the distance (possible
                        //   for `Expensive`, whose threshold is generous, and
                        //   for `None`, which always uses this path). `Cheap`
                        //   never takes it: its threshold is zero, so any
                        //   forward hop goes straight to a transport seek,
                        //   which this source never fails.
                        let past_real_eof = target > data.len() as u64;
                        assert!(
                            (seekability == Seekability::None && target < pos as u64)
                                || past_real_eof
                                || io.error().is_some(),
                            "seek to {target} refused unexpectedly (pos={pos}, kind={seekability:?})"
                        );
                        pos = io.pos() as usize;
                    }
                }
            }
            Op::Skip(n) => {
                if io.skip(u64::from(*n)).is_ok() {
                    // `skip`'s contract is exactly `n` or an error (Defect 2):
                    // no clamping, so the model's cursor can legitimately end
                    // up past `data.len()` here, same as after `Op::Seek`.
                    pos += usize::from(*n);
                } else {
                    pos = io.pos() as usize;
                }
            }
            Op::R8 => {
                if let Ok(b) = io.r8() {
                    assert_eq!(b, data[pos]);
                    pos += 1;
                }
            }
            Op::Rb32 => {
                if let Ok(v) = io.rb32() {
                    let bytes: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
                    assert_eq!(v, u32::from_be_bytes(bytes));
                    pos += 4;
                } else {
                    pos = io.pos() as usize;
                }
            }
            Op::Tag => {
                if let Ok(t) = io.tag() {
                    assert_eq!(&t, &data[pos..pos + 4]);
                    pos += 4;
                } else {
                    pos = io.pos() as usize;
                }
            }
            Op::Str(max) => {
                if io.get_str(usize::from(*max).min(4096)).is_ok() {
                    pos = io.pos() as usize;
                }
            }
        }
        assert_eq!(io.pos(), pos as u64, "position diverged after {op:?}");
        // The model's cursor is allowed past `data.len()` now — a past-EOF
        // seek or skip is legal, exactly like `lseek` on a real file. What
        // must hold is the thing that actually matters: a read from there
        // reports EOF (zero bytes, no panic, no phantom data) rather than
        // whatever `assert!(pos <= data.len())` used to stand in for.
        if pos >= data.len() {
            let mut probe = [0u8; 1];
            if let Ok(n) = io.read_partial(&mut probe) {
                assert_eq!(n, 0, "read past EOF returned data that does not exist");
            }
            assert_eq!(
                io.pos(),
                pos as u64,
                "a zero-byte EOF read must not move the position"
            );
        }
    }
});
