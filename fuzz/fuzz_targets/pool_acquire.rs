//! `vaco-pool` under an arbitrary acquire/drop/copy-on-write schedule.
//!
//! Every size here derives from fuzzer input, which is exactly the shape of a
//! demuxer sizing a buffer from a length field. The findings are: a panic, an
//! arithmetic overflow, a buffer that is not 64-byte aligned, or accounting that
//! grows past the pool's configured bound. `Error::LimitExceeded` is success.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};
use vaco_pool::{ALIGN, BITSTREAM_PADDING, Buffer, BufferPool, PoolConfig};

#[derive(Arbitrary, Debug)]
enum Op {
    /// Take from the pool.
    Get,
    /// Take from the pool, zeroed.
    GetZeroed,
    /// Drop the buffer at this index.
    Drop(u8),
    /// Clone the buffer at this index, so the next write must copy.
    Share(u8),
    /// Write through the buffer at this index, exercising the CoW path.
    Write(u8, u8),
    /// Allocate a standalone buffer of an arbitrary length.
    Alloc(u32),
    /// Allocate a standalone padded buffer of an arbitrary length.
    AllocPadded(u32),
    /// Copy an arbitrary slice into a padded buffer.
    FromSlicePadded(Vec<u8>),
    /// Drop everything the pool retained.
    Clear,
}

#[derive(Arbitrary, Debug)]
struct Input {
    buffer_size: u16,
    max_live_bytes: u32,
    max_live_buffers: u8,
    max_retained: u8,
    ops: Vec<Op>,
}

const MAX_LIVE: usize = 64;

fuzz_target!(|input: Input| {
    let config = PoolConfig {
        max_live_bytes: input.max_live_bytes as usize,
        max_live_buffers: input.max_live_buffers as usize,
        max_retained_buffers: input.max_retained as usize,
    };
    let pool = BufferPool::with_config(input.buffer_size as usize, config);
    // `tiny` keeps the standalone path from actually allocating gigabytes while
    // still exercising every size computation on the way there.
    let mut budget = Budget::new(Limits::tiny());
    let mut live: Vec<Buffer> = Vec::new();

    let check = |buf: &Buffer| {
        // The invariant the whole crate exists to provide.
        assert_eq!(buf.as_slice().as_ptr().addr() % ALIGN, 0, "misaligned");
    };

    for op in input.ops.iter().take(512) {
        match op {
            Op::Get | Op::GetZeroed => {
                let got = if matches!(op, Op::Get) {
                    pool.get()
                } else {
                    pool.get_zeroed()
                };
                if let Ok(buf) = got {
                    check(&buf);
                    assert_eq!(buf.len(), pool.buffer_size());
                    if live.len() < MAX_LIVE {
                        live.push(buf);
                    }
                }
            }
            Op::Drop(i) => {
                if !live.is_empty() {
                    live.remove(*i as usize % live.len());
                }
            }
            Op::Share(i) => {
                if !live.is_empty() {
                    let at = *i as usize % live.len();
                    let dup = live[at].clone();
                    assert!(dup.ptr_eq(&live[at]));
                    if live.len() < MAX_LIVE {
                        live.push(dup);
                    }
                }
            }
            Op::Write(i, byte) => {
                if !live.is_empty() {
                    let at = *i as usize % live.len();
                    live[at].make_mut().fill(*byte);
                    check(&live[at]);
                    assert!(live[at].as_slice().iter().all(|&b| b == *byte));
                }
            }
            Op::Alloc(len) => {
                if let Ok(buf) = Buffer::alloc(&mut budget, *len as usize) {
                    check(&buf);
                    assert_eq!(buf.len(), *len as usize);
                }
            }
            Op::AllocPadded(len) => {
                let len = *len as usize;
                if let Ok(buf) = Buffer::alloc_padded(&mut budget, len) {
                    check(&buf);
                    assert_eq!(buf.len(), len + BITSTREAM_PADDING);
                    assert!(buf.padded(len).is_some(), "padding not established");
                }
            }
            Op::FromSlicePadded(src) => {
                if let Ok(buf) = Buffer::from_slice_padded(&mut budget, src) {
                    check(&buf);
                    assert!(buf.padded(src.len()).is_some());
                }
            }
            Op::Clear => pool.clear(),
        }

        // The bound is a correctness property, not a tuning knob: a pool that
        // grows past it is a memory leak on a resolution-switching stream.
        let s = pool.stats();
        assert!(s.retained_buffers <= config.max_retained_buffers);
        assert!(s.live_buffers <= config.max_live_buffers);
        assert!(s.live_bytes <= config.max_live_bytes);
    }

    drop(live);
    let s = pool.stats();
    assert_eq!(s.live_buffers, s.retained_buffers);
});
