//! `vaco-packet` construction, sub-packets and truncation on arbitrary input.
//!
//! The padding invariant is what `vaco-bitstream`'s unchecked-body fast path
//! rests on, so a packet that comes out of any of these paths without at least
//! 64 zero bytes past its payload is a soundness-adjacent finding, not a
//! cosmetic one.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_bitstream::BitReader;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pool::{ALIGN, BITSTREAM_PADDING, BufferPool};

#[derive(Arbitrary, Debug)]
struct Input {
    payload: Vec<u8>,
    range: (u32, u32),
    truncate_to: u32,
    pool_class: u16,
    pooled_len: u16,
    read_widths: Vec<u8>,
}

fuzz_target!(|input: Input| {
    let mut budget = Budget::new(Limits::permissive());
    let Ok(mut pkt) = Packet::from_slice(&mut budget, &input.payload) else {
        return;
    };

    assert_eq!(pkt.payload(), &input.payload[..]);
    assert_eq!(pkt.data.len(), input.payload.len() + BITSTREAM_PADDING);
    assert_eq!(pkt.data.as_slice().as_ptr().addr() % ALIGN, 0);
    let padded = pkt.payload_padded().expect("constructors allocate padded");
    assert_eq!(padded.logical_len(), input.payload.len());

    // The fast reader and the slow one must agree bit for bit, including on
    // where they overrun.
    let mut fast = BitReader::new_padded(padded);
    let mut slow = BitReader::new(&input.payload);
    for w in input.read_widths.iter().take(256) {
        let n = u32::from(*w % 33);
        assert_eq!(fast.get(n), slow.get(n));
        assert_eq!(fast.overrun(), slow.overrun());
    }

    // Sub-packets: in range they copy exactly, out of range they refuse.
    let lo = input.range.0 as usize;
    let hi = input.range.1 as usize;
    match pkt.sub_packet(&mut budget, lo..hi) {
        Ok(sub) => {
            assert!(lo <= hi && hi <= input.payload.len());
            assert_eq!(sub.payload(), &input.payload[lo..hi]);
            assert!(sub.payload_padded().is_some());
            assert!(!sub.data.ptr_eq(&pkt.data), "sub-packet aliases its parent");
        }
        Err(_) => assert!(lo > hi || hi > input.payload.len()),
    }

    // Truncation must never leave the padding invariant broken, and must never
    // disturb a clone.
    let clone = pkt.clone();
    pkt.truncate(input.truncate_to as usize);
    assert!(pkt.len <= input.payload.len());
    assert_eq!(pkt.payload(), &input.payload[..pkt.len]);
    assert!(pkt.payload_padded().is_some());
    assert_eq!(clone.payload(), &input.payload[..]);
    assert!(clone.payload_padded().is_some());

    // The pooled path re-zeroes only the tail, so its invariant needs its own
    // check on recycled storage.
    let pool = BufferPool::new(input.pool_class as usize);
    if let Ok(mut pooled) = Packet::alloc_pooled(&pool, input.pooled_len as usize) {
        pooled.payload_mut().fill(0xA5);
        assert!(pooled.payload_padded().is_some());
        drop(pooled);
        if let Ok(again) = Packet::alloc_pooled(&pool, input.pooled_len as usize / 2) {
            assert!(again.payload_padded().is_some(), "recycled tail not restored");
        }
    }
});
