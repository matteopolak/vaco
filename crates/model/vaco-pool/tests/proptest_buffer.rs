//! Property tests for the three invariants everything else rests on:
//! alignment, copy-on-write isolation, and the padding contract.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use proptest::prelude::*;
use vaco_bitstream::Padded;
use vaco_limits::{Budget, Limits};
use vaco_pool::{ALIGN, BITSTREAM_PADDING, Buffer, BufferPool, PoolConfig};

proptest! {
    /// Alignment holds for every size, not just the round ones.
    #[test]
    fn alignment_holds_for_every_size(len in 0usize..70_000) {
        let mut b = Budget::new(Limits::permissive());
        let buf = Buffer::alloc(&mut b, len).unwrap();
        prop_assert_eq!(buf.len(), len);
        prop_assert_eq!(buf.as_slice().as_ptr().addr() % ALIGN, 0);
        prop_assert!(buf.as_slice().iter().all(|&x| x == 0));
    }

    /// The padding is present and zero for every size, and `Padded` accepts it.
    #[test]
    fn padding_holds_for_every_size(src in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut b = Budget::new(Limits::permissive());
        let buf = Buffer::from_slice_padded(&mut b, &src).unwrap();
        prop_assert_eq!(buf.len(), src.len() + BITSTREAM_PADDING);
        prop_assert_eq!(buf.as_slice().as_ptr().addr() % ALIGN, 0);
        prop_assert_eq!(&buf.as_slice()[..src.len()], &src[..]);
        prop_assert!(buf.as_slice()[src.len()..].iter().all(|&x| x == 0));

        let padded = Padded::new(buf.as_slice(), src.len());
        prop_assert!(padded.is_some());
        prop_assert_eq!(padded.unwrap().logical_bytes(), &src[..]);
    }

    /// Copy-on-write never aliases: mutating one clone leaves every other
    /// holder byte-identical to the original.
    #[test]
    fn cow_never_aliases(
        src in prop::collection::vec(any::<u8>(), 1..512),
        clones in 1usize..4,
        fill in any::<u8>(),
    ) {
        let mut b = Budget::new(Limits::permissive());
        let mut writer = Buffer::from_slice(&mut b, &src).unwrap();
        let readers: Vec<Buffer> = (0..clones).map(|_| writer.clone()).collect();

        writer.make_mut().fill(fill);

        let expected = vec![fill; src.len()];
        prop_assert_eq!(writer.as_slice(), expected.as_slice());
        for r in &readers {
            prop_assert_eq!(r.as_slice(), &src[..]);
            prop_assert!(!writer.ptr_eq(r));
        }
        // Every reader still shares one allocation with the others.
        for r in &readers {
            prop_assert!(r.ptr_eq(&readers[0]));
        }
    }

    /// A unique buffer is never copied — the whole point of the seam.
    #[test]
    fn unique_write_is_in_place(src in prop::collection::vec(any::<u8>(), 1..512)) {
        let mut b = Budget::new(Limits::permissive());
        let mut buf = Buffer::from_slice(&mut b, &src).unwrap();
        let before = buf.as_slice().as_ptr().addr();
        buf.make_mut()[0] ^= 0xFF;
        prop_assert_eq!(buf.as_slice().as_ptr().addr(), before);
    }

    /// Whatever the acquire/drop schedule, the pool never exceeds its bounds and
    /// its accounting stays consistent.
    #[test]
    fn pool_accounting_is_bounded(
        ops in prop::collection::vec(prop::bool::ANY, 0..200),
        size in 0usize..2048,
        retain in 0usize..8,
    ) {
        let cfg = PoolConfig {
            max_live_bytes: 1 << 20,
            max_live_buffers: 24,
            max_retained_buffers: retain,
        };
        let pool = BufferPool::with_config(size, cfg);
        let mut live: Vec<Buffer> = Vec::new();
        for take in ops {
            if take {
                if let Ok(b) = pool.get() {
                    live.push(b);
                }
            } else {
                live.pop();
            }
            let s = pool.stats();
            prop_assert!(s.retained_buffers <= retain);
            prop_assert!(s.live_buffers <= 24);
            prop_assert!(s.live_bytes <= 1 << 20);
            prop_assert!(s.live_buffers >= live.len());
        }
        drop(live);
        let s = pool.stats();
        prop_assert_eq!(s.live_buffers, s.retained_buffers);
        prop_assert!(s.allocations >= s.recycled.min(1));
    }

    /// Recycled storage is reused rather than reallocated, for any buffer size.
    #[test]
    fn recycling_reuses_storage(size in 0usize..8192) {
        let pool = BufferPool::new(size);
        let a = pool.get().unwrap();
        let addr = a.as_slice().as_ptr().addr();
        drop(a);
        let b = pool.get().unwrap();
        prop_assert_eq!(b.as_slice().as_ptr().addr(), addr);
        prop_assert_eq!(pool.stats().allocations, 1);
        prop_assert_eq!(pool.stats().hits, 1);
    }
}
