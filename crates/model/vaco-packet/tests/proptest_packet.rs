//! Property tests: the padding invariant and copy-on-write isolation must hold
//! for every payload, because `vaco-bitstream`'s fast path depends on them.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use proptest::prelude::*;
use vaco_bitstream::BitReader;
use vaco_core::{Rational, Rounding, Timestamp};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pool::{ALIGN, BITSTREAM_PADDING, BufferPool};

proptest! {
    /// Every packet carries at least PAD zero bytes past its payload, whatever
    /// the length — the invariant `Padded` is a typestate for.
    #[test]
    fn padding_holds_for_every_payload(src in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut b = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut b, &src).unwrap();
        prop_assert_eq!(pkt.payload(), &src[..]);
        prop_assert_eq!(pkt.data.len(), src.len() + BITSTREAM_PADDING);
        prop_assert_eq!(pkt.data.as_slice().as_ptr().addr() % ALIGN, 0);
        prop_assert!(pkt.data.as_slice()[src.len()..].iter().all(|&x| x == 0));
        prop_assert!(pkt.payload_padded().is_some());
    }

    /// The padded and unpadded readers agree bit for bit, including on where
    /// they overrun. If they ever diverge the fast path is a bug, not a
    /// speed-up.
    #[test]
    fn padded_and_unpadded_readers_agree(
        src in prop::collection::vec(any::<u8>(), 0..256),
        widths in prop::collection::vec(1u32..25, 1..40),
    ) {
        let mut b = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut b, &src).unwrap();
        let mut fast = BitReader::new_padded(pkt.payload_padded().unwrap());
        let mut slow = BitReader::new(&src);
        for n in widths {
            prop_assert_eq!(fast.get(n), slow.get(n));
            prop_assert_eq!(fast.overrun(), slow.overrun());
        }
    }

    /// Copy-on-write never aliases: writing one clone leaves the other's
    /// payload and padding exactly as they were.
    #[test]
    fn cow_never_aliases(src in prop::collection::vec(any::<u8>(), 1..1024), fill in any::<u8>()) {
        let mut b = Budget::new(Limits::permissive());
        let mut a = Packet::from_slice(&mut b, &src).unwrap();
        let c = a.clone();
        a.payload_mut().fill(fill);
        prop_assert_eq!(c.payload(), &src[..]);
        prop_assert!(c.payload_padded().is_some());
        prop_assert!(a.payload_padded().is_some());
        prop_assert!(!a.data.ptr_eq(&c.data));
    }

    /// Truncation to any length keeps the padding invariant.
    #[test]
    fn truncation_preserves_padding(
        src in prop::collection::vec(any::<u8>(), 0..512),
        to in 0usize..600,
    ) {
        let mut b = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut b, &src).unwrap();
        pkt.truncate(to);
        prop_assert_eq!(pkt.len, to.min(src.len()));
        prop_assert_eq!(pkt.payload(), &src[..pkt.len]);
        prop_assert!(pkt.payload_padded().is_some());
    }

    /// A sub-packet is a real copy with a real padding tail, for any range.
    #[test]
    fn sub_packets_are_well_formed(
        src in prop::collection::vec(any::<u8>(), 1..512),
        a in 0usize..512,
        b2 in 0usize..512,
    ) {
        let mut b = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut b, &src).unwrap();
        let (lo, hi) = (a.min(b2), a.max(b2));
        match pkt.sub_packet(&mut b, lo..hi) {
            Ok(sub) => {
                prop_assert!(hi <= src.len());
                prop_assert_eq!(sub.payload(), &src[lo..hi]);
                prop_assert!(sub.payload_padded().is_some());
            }
            Err(_) => prop_assert!(hi > src.len()),
        }
    }

    /// A pooled packet is padded for its own logical length however the pool
    /// recycled the storage underneath it.
    #[test]
    fn pooled_packets_are_padded(lens in prop::collection::vec(0usize..512, 1..20)) {
        let pool = BufferPool::new_padded(512);
        for len in lens {
            let mut pkt = Packet::alloc_pooled(&pool, len).unwrap();
            pkt.payload_mut().fill(0xEE);
            prop_assert_eq!(pkt.len, len);
            prop_assert!(pkt.payload_padded().is_some());
        }
        prop_assert!(pool.stats().allocations <= 1);
    }

    /// `rescale_ts` moves pts and dts by the same factor, so their difference
    /// scales rather than drifting.
    #[test]
    fn rescale_keeps_pts_and_dts_consistent(
        pts in -1_000_000i64..1_000_000,
        delta in 0i64..10_000,
        from_den in 1i32..100_000,
        to_den in 1i32..100_000,
    ) {
        let mut pkt = Packet::empty();
        pkt.pts = Timestamp::new(pts);
        pkt.dts = Timestamp::new(pts - delta);
        let (from, to) = (Rational::new(1, from_den), Rational::new(1, to_den));
        pkt.rescale_ts(from, to, Rounding::default());

        let expect_pts = Timestamp::new(pts).rescale(from, to, Rounding::default());
        let expect_dts = Timestamp::new(pts - delta).rescale(from, to, Rounding::default());
        prop_assert_eq!(pkt.pts, expect_pts);
        prop_assert_eq!(pkt.dts, expect_dts);
    }
}
