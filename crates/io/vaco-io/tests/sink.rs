//! Writer and dynamic-buffer behaviour.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use proptest::prelude::*;
use vaco_core::Error;
use vaco_io::{
    DataMarker, DynBuf, IoContext, IoOptions, IoWriter, MediaSink, MemorySource, SharedDynBuf,
};

fn writer(sink: SharedDynBuf, block: usize) -> IoWriter {
    IoWriter::new(Box::new(sink), &IoOptions::default().with_block_size(block)).unwrap()
}

#[test]
fn write_measure_patch() {
    // The pattern every ISO-BMFF muxer needs: emit a placeholder size, write the
    // payload, come back and fill the size in.
    let out = SharedDynBuf::new();
    let mut w = writer(out.clone(), 64);
    w.wb32(0).unwrap();
    w.write_tag(b"moov").unwrap();
    w.write(&[0xAA; 40]).unwrap();
    let size = w.pos();
    w.seek(0).unwrap();
    w.wb32(size as u32).unwrap();
    w.flush().unwrap();
    drop(w);

    let bytes = out.take();
    assert_eq!(bytes.len(), 48);
    assert_eq!(&bytes[..4], &48u32.to_be_bytes());
    assert_eq!(&bytes[4..8], b"moov");
}

#[test]
fn dynbuf_is_seekable_and_zero_fills_holes() {
    let mut b = DynBuf::new();
    b.write(&[1, 2, 3]).unwrap();
    b.seek(6).unwrap();
    b.write(&[9]).unwrap();
    assert_eq!(b.as_slice(), &[1, 2, 3, 0, 0, 0, 9]);
    b.seek(1).unwrap();
    b.write(&[7, 7]).unwrap();
    assert_eq!(b.as_slice(), &[1, 7, 7, 0, 0, 0, 9]);
}

#[test]
fn dynbuf_limit_is_enforced() {
    let mut b = DynBuf::new();
    b.set_limit(8);
    assert!(b.write(&[0u8; 8]).is_ok());
    assert!(matches!(
        b.write(&[0u8; 1]),
        Err(Error::LimitExceeded { .. })
    ));
    assert_eq!(b.len(), 8);
}

#[test]
fn byte_order_writers_round_trip_through_readers() {
    let out = SharedDynBuf::new();
    let mut w = writer(out.clone(), 16);
    w.w8(0x01).unwrap();
    w.wb16(0x0203).unwrap();
    w.wl16(0x0405).unwrap();
    w.wb24(0x0006_0708).unwrap();
    w.wl24(0x0009_0A0B).unwrap();
    w.wb32(0x0C0D_0E0F).unwrap();
    w.wl32(0x1011_1213).unwrap();
    w.wb64(0x1415_1617_1819_1A1B).unwrap();
    w.wl64(0x1C1D_1E1F_2021_2223).unwrap();
    w.write_cstr("meta").unwrap();
    w.flush().unwrap();
    drop(w);

    let mut io = IoContext::new(
        Box::new(MemorySource::new(out.take())),
        &IoOptions::default().with_block_size(8),
    )
    .unwrap();
    assert_eq!(io.r8().unwrap(), 0x01);
    assert_eq!(io.rb16().unwrap(), 0x0203);
    assert_eq!(io.rl16().unwrap(), 0x0405);
    assert_eq!(io.rb24().unwrap(), 0x0006_0708);
    assert_eq!(io.rl24().unwrap(), 0x0009_0A0B);
    assert_eq!(io.rb32().unwrap(), 0x0C0D_0E0F);
    assert_eq!(io.rl32().unwrap(), 0x1011_1213);
    assert_eq!(io.rb64().unwrap(), 0x1415_1617_1819_1A1B);
    assert_eq!(io.rl64().unwrap(), 0x1C1D_1E1F_2021_2223);
    assert_eq!(io.get_str(16).unwrap(), "meta");
    // `at_eof` is only true once a read has actually hit the end, which is the
    // same contract `avio_feof` has.
    assert!(!io.at_eof());
    assert_eq!(io.read_partial(&mut [0u8; 1]).unwrap(), 0);
    assert!(io.at_eof());
}

#[test]
fn direct_mode_bypasses_the_buffer() {
    let out = SharedDynBuf::new();
    let mut w = IoWriter::new(
        Box::new(out.clone()),
        &IoOptions::default().with_block_size(64).with_direct(true),
    )
    .unwrap();
    w.write(&[1, 2, 3]).unwrap();
    // Nothing buffered: `direct` flushes through on every write.
    assert_eq!(out.len(), 3);
    drop(w);
}

proptest! {
    /// Writing in arbitrary chunks produces the same bytes as one big write.
    #[test]
    fn write_buffering_is_transparent(
        data in prop::collection::vec(any::<u8>(), 0..3000),
        block in 64usize..512,
        chunk in 1usize..200,
    ) {
        let out = SharedDynBuf::new();
        {
            let mut w = writer(out.clone(), block);
            for piece in data.chunks(chunk.max(1)) {
                w.write_marked(piece, DataMarker::Unknown).unwrap();
            }
            prop_assert_eq!(w.pos(), data.len() as u64);
            w.flush().unwrap();
        }
        prop_assert_eq!(out.take(), data);
    }
}
