//! Unit tests for packet construction, padding, copy-on-write and rescaling.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_bitstream::{BitReader, GolombRead};
use vaco_core::{Duration, Rational, Rounding, Timestamp};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags, PacketSideData, PacketSideDataKind};
use vaco_pool::{ALIGN, BITSTREAM_PADDING, Buffer, BufferPool};

fn budget() -> Budget {
    Budget::new(Limits::permissive())
}

#[test]
fn from_slice_allocates_padded_and_aligned() {
    let mut b = budget();
    for len in [0usize, 1, 3, 63, 64, 65, 1500] {
        let src: Vec<u8> = (0..len).map(|i| (i % 255) as u8 + 1).collect();
        let pkt = Packet::from_slice(&mut b, &src).unwrap();
        assert_eq!(pkt.len, len);
        assert_eq!(pkt.payload(), &src[..]);
        assert_eq!(pkt.data.len(), len + BITSTREAM_PADDING);
        assert_eq!(pkt.data.as_slice().as_ptr().addr() % ALIGN, 0);
        let padded = pkt.payload_padded().expect("must be padded");
        assert_eq!(padded.logical_len(), len);
        assert_eq!(padded.logical_bytes(), &src[..]);
    }
}

#[test]
fn a_parser_gets_the_fast_reader_path() {
    let mut b = budget();
    // A tiny "parameter set": exactly the short-buffer shape the padding buys.
    let pkt = Packet::from_slice(&mut b, &[0x67, 0x42, 0xC0, 0x1E]).unwrap();
    let padded = pkt.payload_padded().unwrap();
    let mut fast = BitReader::new_padded(padded);
    let mut slow = BitReader::new(pkt.payload());
    assert_eq!(fast.get(8), slow.get(8));
    assert_eq!(fast.ue(), slow.ue());
    assert_eq!(fast.bits_left(), slow.bits_left());
    assert_eq!(fast.overrun(), slow.overrun());
}

#[test]
fn empty_packet_is_usable() {
    let pkt = Packet::empty();
    assert!(pkt.is_empty());
    assert_eq!(pkt.payload(), b"");
    assert_eq!(Packet::default().len, 0);
}

#[test]
fn payload_len_cannot_exceed_the_buffer() {
    let mut b = budget();
    let buf = Buffer::alloc(&mut b, 10).unwrap();
    let pkt = Packet::new(buf, 1000);
    assert_eq!(pkt.len, 10);
    assert_eq!(pkt.payload().len(), 10);
}

#[test]
fn cow_isolates_a_shared_payload() {
    let mut b = budget();
    let mut a = Packet::from_slice(&mut b, b"abcdef").unwrap();
    let c = a.clone();
    assert!(!a.is_writable());

    a.payload_mut().fill(b'z');

    assert_eq!(c.payload(), b"abcdef");
    assert_eq!(a.payload(), b"zzzzzz");
    assert!(a.is_writable());
    assert!(c.is_writable());
    // Both are still padded after the split.
    assert!(a.payload_padded().is_some());
    assert!(c.payload_padded().is_some());
}

#[test]
fn payload_mut_cannot_reach_the_padding() {
    let mut b = budget();
    let mut pkt = Packet::from_slice(&mut b, b"abc").unwrap();
    assert_eq!(pkt.payload_mut().len(), 3);
    pkt.payload_mut().fill(0xFF);
    assert!(pkt.payload_padded().is_some(), "padding was disturbed");
}

#[test]
fn truncate_preserves_the_padding_invariant() {
    let mut b = budget();
    let mut pkt = Packet::from_slice(&mut b, &[0xAA; 200]).unwrap();
    pkt.truncate(50);
    assert_eq!(pkt.len, 50);
    assert_eq!(pkt.payload(), &[0xAA; 50]);
    let padded = pkt.payload_padded().expect("still padded");
    assert_eq!(padded.logical_len(), 50);
    // Growing back is not possible through `truncate`.
    pkt.truncate(100);
    assert_eq!(pkt.len, 50);
}

#[test]
fn truncate_on_a_shared_packet_does_not_disturb_the_other() {
    let mut b = budget();
    let mut a = Packet::from_slice(&mut b, &[0x11; 100]).unwrap();
    let c = a.clone();
    a.truncate(10);
    assert_eq!(c.len, 100);
    assert_eq!(c.payload(), &[0x11; 100]);
}

#[test]
fn pooled_packets_recycle_and_stay_padded() {
    let pool = BufferPool::new_padded(1024);
    let mut first = Packet::alloc_pooled(&pool, 1024).unwrap();
    first.payload_mut().fill(0xCD);
    let addr = first.data.as_slice().as_ptr().addr();
    drop(first);

    let second = Packet::alloc_pooled(&pool, 300).unwrap();
    assert_eq!(second.data.as_slice().as_ptr().addr(), addr, "not recycled");
    assert_eq!(pool.stats().allocations, 1);
    // The previous packet's bytes are still there — documented — but the
    // padding for the *new* logical length has been restored.
    assert!(second.payload_padded().is_some());
    assert_eq!(second.len, 300);
}

#[test]
fn pooled_packet_rejects_an_undersized_class() {
    let pool = BufferPool::new(100);
    assert!(Packet::alloc_pooled(&pool, 100).is_err());
    assert!(Packet::alloc_pooled(&pool, 36).is_ok());
}

#[test]
fn sub_packet_copies_and_carries_metadata() {
    let mut b = budget();
    let mut pkt = Packet::from_slice(&mut b, b"0123456789").unwrap();
    pkt.stream_index = 3;
    pkt.pts = Timestamp::new(900);
    pkt.dts = Timestamp::new(800);
    pkt.duration = Duration::from_micros(1000);
    pkt.pos = Some(42);
    pkt.flags = PacketFlags::KEY;
    pkt.set_side_data(PacketSideData::SkipSamples { start: 1, end: 2 });

    let sub = pkt.sub_packet(&mut b, 2..6).unwrap();
    assert_eq!(sub.payload(), b"2345");
    assert_eq!(sub.stream_index, 3);
    assert_eq!(sub.pts, Timestamp::new(900));
    assert_eq!(sub.pos, Some(42));
    assert_eq!(sub.flags, PacketFlags::KEY);
    assert!(sub.side_data(PacketSideDataKind::SkipSamples).is_some());
    assert!(sub.payload_padded().is_some());
    assert!(!sub.data.ptr_eq(&pkt.data), "sub-packet aliases the parent");

    assert!(pkt.sub_packet(&mut b, 5..50).is_err());
    let (lo, hi) = (8usize, 2usize);
    assert!(
        pkt.sub_packet(&mut b, lo..hi).is_err(),
        "reversed range accepted"
    );
}

#[test]
fn rescale_moves_pts_and_dts_together() {
    let mut pkt = Packet::empty();
    pkt.pts = Timestamp::new(90_000);
    pkt.dts = Timestamp::new(87_000);
    pkt.duration = Duration::from_micros(40_000);

    pkt.rescale_ts(
        Rational::new(1, 90_000),
        Rational::new(1, 1000),
        Rounding::default(),
    );
    assert_eq!(pkt.pts, Timestamp::new(1000));
    assert_eq!(pkt.dts.ticks(), Some(967));
    // Duration is microseconds, so it is base-independent and untouched.
    assert_eq!(pkt.duration, Duration::from_micros(40_000));
}

#[test]
fn rescaling_an_absent_timestamp_stays_absent() {
    let mut pkt = Packet::empty();
    pkt.rescale_ts(
        Rational::new(1, 90_000),
        Rational::new(1, 1000),
        Rounding::default(),
    );
    assert_eq!(pkt.pts, Timestamp::NONE);
    assert_eq!(pkt.dts, Timestamp::NONE);
}

#[test]
fn side_data_set_get_replace_remove() {
    let mut pkt = Packet::empty();
    assert!(pkt.side_data(PacketSideDataKind::DisplayMatrix).is_none());
    pkt.set_side_data(PacketSideData::DisplayMatrix([1; 9]));
    pkt.set_side_data(PacketSideData::SkipSamples { start: 0, end: 5 });
    assert_eq!(pkt.side_data.len(), 2);
    pkt.set_side_data(PacketSideData::DisplayMatrix([7; 9]));
    assert_eq!(pkt.side_data.len(), 2);
    assert!(matches!(
        pkt.side_data(PacketSideDataKind::DisplayMatrix),
        Some(PacketSideData::DisplayMatrix(m)) if m[0] == 7
    ));
    assert!(
        pkt.remove_side_data(PacketSideDataKind::DisplayMatrix)
            .is_some()
    );
    assert!(
        pkt.remove_side_data(PacketSideDataKind::DisplayMatrix)
            .is_none()
    );
}

#[test]
fn oversized_allocation_is_an_error_not_a_wrap() {
    let mut b = budget();
    assert!(Packet::alloc(&mut b, usize::MAX).is_err());
    let pool = BufferPool::new(128);
    assert!(Packet::alloc_pooled(&pool, usize::MAX).is_err());
}

#[test]
fn cloning_a_packet_shares_the_payload() {
    let mut b = budget();
    let a = Packet::from_slice(&mut b, &[0x42; 4096]).unwrap();
    let c = a.clone();
    assert!(a.data.ptr_eq(&c.data));
}
