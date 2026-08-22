//! Unit tests for [`Buffer`]: alignment, copy-on-write and padding.
//!
//! The workspace denies indexing and `unwrap` because a panic reachable from
//! untrusted input is a decoder vulnerability. In a test a panic *is* the
//! failure report, so both are allowed here.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_bitstream::{BitReader, Padded};
use vaco_limits::{Budget, Limits};
use vaco_pool::{ALIGN, BITSTREAM_PADDING, Buffer, BufferPool, PoolConfig};

fn budget() -> Budget {
    Budget::new(Limits::permissive())
}

#[test]
fn padding_constant_matches_bitstream() {
    // The compile-time assertion in lib.rs is the real guard; this makes the
    // contract visible in the test output too.
    assert_eq!(BITSTREAM_PADDING, Padded::PAD);
    assert_eq!(ALIGN, 64);
}

#[test]
fn alignment_holds_for_a_size_sweep() {
    let mut b = budget();
    for len in (0..=512).chain([1023, 1024, 1025, 4096, 65_537]) {
        let buf = Buffer::alloc(&mut b, len).unwrap();
        assert_eq!(buf.len(), len, "len {len}");
        assert!(buf.is_aligned(), "len {len} not 64-byte aligned");
        assert_eq!(buf.as_slice().as_ptr().addr() % ALIGN, 0);
        assert!(
            buf.as_slice().iter().all(|&x| x == 0),
            "len {len} not zeroed"
        );
    }
}

#[test]
fn zero_length_buffer_is_still_aligned() {
    let mut b = budget();
    let buf = Buffer::alloc(&mut b, 0).unwrap();
    assert!(buf.is_empty());
    assert!(buf.is_aligned());
    assert!(Buffer::empty().is_aligned());
}

#[test]
fn cow_does_not_alias() {
    let mut b = budget();
    let mut a = Buffer::from_slice(&mut b, b"original").unwrap();
    let c = a.clone();
    assert!(a.ptr_eq(&c));
    assert!(!a.is_unique());

    a.make_mut().fill(b'x');

    assert!(!a.ptr_eq(&c));
    assert_eq!(c.as_slice(), b"original");
    assert_eq!(a.as_slice(), b"xxxxxxxx");
    assert!(a.is_unique());
    assert!(c.is_unique());
}

#[test]
fn unique_buffer_mutates_in_place() {
    let mut b = budget();
    let mut a = Buffer::from_slice(&mut b, b"abc").unwrap();
    let before = a.as_slice().as_ptr().addr();
    a.make_mut()[0] = b'z';
    assert_eq!(a.as_slice().as_ptr().addr(), before, "unique write copied");
}

#[test]
fn cow_copy_is_also_aligned() {
    let mut b = budget();
    let mut a = Buffer::alloc(&mut b, 300).unwrap();
    let _shared = a.clone();
    a.make_writable();
    assert!(a.is_aligned());
}

#[test]
fn padded_buffer_satisfies_the_bitstream_contract() {
    let mut b = budget();
    for len in [0usize, 1, 7, 8, 63, 64, 65, 1000] {
        let src: Vec<u8> = (0..len).map(|i| (i % 251) as u8 + 1).collect();
        let buf = Buffer::from_slice_padded(&mut b, &src).unwrap();
        assert_eq!(buf.len(), len + BITSTREAM_PADDING);
        assert!(buf.is_aligned());
        let padded = buf.padded(len).expect("padding must validate");
        assert_eq!(padded.logical_len(), len);
        assert_eq!(padded.logical_bytes(), &src[..]);
        assert!(buf.as_slice()[len..].iter().all(|&x| x == 0));

        // The payoff: a reader built from it agrees with the unpadded one.
        let mut fast = BitReader::new_padded(padded);
        let mut slow = BitReader::new(&src);
        for _ in 0..(len * 8 + 40) {
            assert_eq!(fast.get_bit(), slow.get_bit());
        }
        assert_eq!(fast.overrun(), slow.overrun());
    }
}

#[test]
fn unpadded_buffer_is_not_padded() {
    let mut b = budget();
    let buf = Buffer::from_slice(&mut b, b"payload").unwrap();
    assert!(buf.padded(7).is_none());
}

#[test]
fn allocation_is_charged_to_the_budget() {
    let mut b = Budget::new(Limits::tiny());
    assert!(Buffer::alloc(&mut b, 1 << 20).is_err());
    assert!(Buffer::alloc(&mut b, 16).is_ok());
}

#[test]
fn overflowing_length_is_an_error_not_a_wrap() {
    let mut b = budget();
    assert!(Buffer::alloc(&mut b, usize::MAX).is_err());
    assert!(Buffer::alloc_padded(&mut b, usize::MAX).is_err());
}

#[test]
fn pool_recycles_on_last_drop() {
    let pool = BufferPool::new(4096);
    let a = pool.get().unwrap();
    let addr = a.as_slice().as_ptr().addr();
    assert!(a.is_pooled());
    drop(a);
    assert_eq!(pool.stats().retained_buffers, 1);

    let b = pool.get().unwrap();
    assert_eq!(b.as_slice().as_ptr().addr(), addr, "not the same storage");
    let s = pool.stats();
    assert_eq!(s.allocations, 1);
    assert_eq!(s.hits, 1);
    assert_eq!(s.live_buffers, 1);
}

#[test]
fn steady_state_does_not_allocate() {
    let pool = BufferPool::new(64 * 1024);
    // Warm up with four concurrently-live buffers.
    let warm: Vec<_> = (0..4).map(|_| pool.get().unwrap()).collect();
    drop(warm);
    let after_warmup = pool.stats().allocations;
    assert_eq!(after_warmup, 4);

    for _ in 0..100 {
        let live: Vec<_> = (0..4).map(|_| pool.get().unwrap()).collect();
        drop(live);
    }
    assert_eq!(
        pool.stats().allocations,
        after_warmup,
        "steady state allocated"
    );
    assert_eq!(pool.stats().hits, 400);
}

#[test]
fn shared_buffer_only_returns_when_the_last_clone_drops() {
    let pool = BufferPool::new(1024);
    let a = pool.get().unwrap();
    let b = a.clone();
    drop(a);
    assert_eq!(pool.stats().retained_buffers, 0, "returned while shared");
    drop(b);
    assert_eq!(pool.stats().retained_buffers, 1);
}

#[test]
fn cow_copy_comes_from_the_pool() {
    let pool = BufferPool::new(1024);
    // Warm the free list so the copy has somewhere to come from.
    let seed: Vec<_> = (0..2).map(|_| pool.get().unwrap()).collect();
    drop(seed);
    assert_eq!(pool.stats().allocations, 2);

    let mut a = pool.get().unwrap();
    let b = a.clone();
    a.make_writable();
    assert!(a.is_pooled(), "copy-on-write copy fell out of the pool");
    assert!(!a.ptr_eq(&b));
    assert_eq!(pool.stats().allocations, 2, "copy allocated");
    assert_eq!(pool.stats().hits, 2, "copy did not come from the free list");
}

#[test]
fn retention_bound_is_respected() {
    let cfg = PoolConfig {
        max_retained_buffers: 2,
        ..PoolConfig::default()
    };
    let pool = BufferPool::with_config(256, cfg);
    let live: Vec<_> = (0..5).map(|_| pool.get().unwrap()).collect();
    drop(live);
    let s = pool.stats();
    assert_eq!(s.retained_buffers, 2);
    assert_eq!(s.live_buffers, 2, "released buffers still counted as live");
}

#[test]
fn live_bound_is_enforced() {
    let cfg = PoolConfig {
        max_live_bytes: 4096,
        ..PoolConfig::default()
    };
    let pool = BufferPool::with_config(1024, cfg);
    let mut live = Vec::new();
    for _ in 0..3 {
        live.push(pool.get().unwrap());
    }
    // Each buffer's footprint is 1024 + 63, so the fourth does not fit.
    assert!(pool.get().is_err());
    drop(live);
    assert!(pool.get().is_ok(), "recycled buffer should be available");
}

#[test]
fn live_buffer_bound_is_enforced_for_zero_size() {
    let cfg = PoolConfig {
        max_live_buffers: 2,
        ..PoolConfig::default()
    };
    let pool = BufferPool::with_config(0, cfg);
    let _a = pool.get().unwrap();
    let _b = pool.get().unwrap();
    assert!(pool.get().is_err());
}

#[test]
fn buffer_outlives_its_pool() {
    let buf = {
        let pool = BufferPool::new(128);
        let b = pool.get().unwrap();
        assert!(b.is_pooled());
        b
    };
    // The `Weak` is dead; the buffer is still perfectly usable and is freed
    // normally when it drops.
    assert_eq!(buf.len(), 128);
    assert!(buf.is_aligned());
    drop(buf);
}

#[test]
fn clear_drops_the_free_list() {
    let pool = BufferPool::new(1024);
    let live: Vec<_> = (0..3).map(|_| pool.get().unwrap()).collect();
    drop(live);
    assert_eq!(pool.stats().retained_buffers, 3);
    pool.clear();
    let s = pool.stats();
    assert_eq!(s.retained_buffers, 0);
    assert_eq!(s.live_buffers, 0);
    assert_eq!(s.live_bytes, 0);
}

#[test]
fn get_zeroed_scrubs_recycled_contents() {
    let pool = BufferPool::new(64);
    let mut a = pool.get().unwrap();
    a.make_mut().fill(0xAB);
    drop(a);
    let b = pool.get_zeroed().unwrap();
    assert!(b.as_slice().iter().all(|&x| x == 0));
}

#[test]
fn padded_pool_class_includes_the_padding() {
    let pool = BufferPool::new_padded(100);
    assert_eq!(pool.buffer_size(), 100 + BITSTREAM_PADDING);
    let mut buf = pool.get_zeroed().unwrap();
    buf.make_mut()[..100].fill(0xFF);
    assert!(buf.padded(100).is_some());
}

#[test]
fn concurrent_acquire_and_drop_is_sound() {
    let pool = BufferPool::new(4096);
    std::thread::scope(|s| {
        for _ in 0..16 {
            let pool = pool.clone();
            s.spawn(move || {
                for i in 0..200u32 {
                    let Ok(mut buf) = pool.get() else { continue };
                    buf.make_mut().fill(i as u8);
                    let shared = buf.clone();
                    assert_eq!(shared.as_slice()[0], i as u8);
                }
            });
        }
    });
    let s = pool.stats();
    assert_eq!(s.live_buffers, s.retained_buffers);
    assert!(s.hits > 0, "16 threads never reused a buffer");
}
