//! Buffering must be invisible.
//!
//! The property that matters: for any input, any buffer size and any pattern of
//! reads, seeks and peeks, an `IoContext` yields exactly the bytes the source
//! holds. Everything below is a specialisation of that.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use proptest::prelude::*;
use vaco_core::Error;
use vaco_io::{IoContext, IoOptions, MediaSource, MemorySource, Seekability};

fn ctx(data: Vec<u8>, block: usize) -> IoContext {
    IoContext::new(
        Box::new(MemorySource::new(data)),
        &IoOptions::default().with_block_size(block),
    )
    .unwrap()
}

fn ctx_of(src: impl MediaSource + 'static, block: usize) -> IoContext {
    IoContext::new(Box::new(src), &IoOptions::default().with_block_size(block)).unwrap()
}

#[test]
fn read_across_buffer_boundary() {
    let data: Vec<u8> = (0..200u32).map(|i| i as u8).collect();
    let mut io = ctx(data.clone(), 64);
    let mut out = vec![0u8; 200];
    io.read_exact(&mut out).unwrap();
    assert_eq!(out, data);
    assert_eq!(io.pos(), 200);
    assert_eq!(io.read_partial(&mut [0u8; 1]).unwrap(), 0);
    assert!(io.at_eof());
}

#[test]
fn byte_order_readers() {
    let data = vec![
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, b'm', b'o', b'o', b'v',
    ];
    let mut io = ctx(data, 4);
    assert_eq!(io.rb16().unwrap(), 0x0102);
    assert_eq!(io.rl16().unwrap(), 0x0403);
    assert_eq!(io.rb24().unwrap(), 0x0005_0607);
    assert_eq!(io.r8().unwrap(), 0x08);
    assert_eq!(&io.tag().unwrap(), b"moov");
}

#[test]
fn short_seek_on_expensive_source_does_not_seek() {
    // A forward hop under the threshold must be served by reading, and a
    // forward-only source must serve it the same way.
    let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    let mut io = ctx_of(MemorySource::forward_only(data.clone()), 16);
    assert_eq!(io.seekability(), Seekability::None);
    io.seek(500).unwrap();
    assert_eq!(io.pos(), 500);
    assert_eq!(io.r8().unwrap(), data[500]);
    // Backwards, outside the buffer, is genuinely impossible.
    assert!(matches!(io.seek(0), Err(Error::NotSeekable)));
}

#[test]
fn backward_seek_inside_buffer_is_free() {
    let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    let mut io = ctx_of(MemorySource::forward_only(data.clone()), 256);
    let mut scratch = vec![0u8; 100];
    io.read_exact(&mut scratch).unwrap();
    let before = io.bytes_read();
    io.seek(10).unwrap();
    assert_eq!(io.r8().unwrap(), data[10]);
    assert_eq!(
        io.bytes_read(),
        before,
        "rewind must not re-read the source"
    );
}

#[test]
fn seek_past_end_then_read_is_eof() {
    let mut io = ctx(vec![1, 2, 3], 64);
    io.seek(3).unwrap();
    assert_eq!(io.read_partial(&mut [0u8; 4]).unwrap(), 0);
}

#[test]
fn strings() {
    let mut io = ctx(b"hello\0world\0".to_vec(), 4);
    assert_eq!(io.get_str(64).unwrap(), "hello");
    assert_eq!(io.get_str(64).unwrap(), "world");

    let utf16: Vec<u8> = "hi".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut io = ctx(utf16, 8);
    assert_eq!(io.get_str16be(4).unwrap(), "hi");
}

#[test]
fn checksum_covers_consumed_bytes_only() {
    let data: Vec<u8> = b"123456789EXTRA".to_vec();
    let mut io = ctx(data, 4);
    io.start_checksum(vaco_io::ChecksumKind::Crc32Ieee);
    let mut nine = [0u8; 9];
    io.read_exact(&mut nine).unwrap();
    assert_eq!(io.take_checksum(), 0xCBF4_3926);
    // Region closed: later reads are not accumulated.
    assert_eq!(io.take_checksum(), 0);
}

/// A source that fails after `ok_bytes`, to exercise the sticky error path.
#[derive(Debug)]
struct FlakySource {
    data: Vec<u8>,
    pos: usize,
    ok_bytes: usize,
}

impl MediaSource for FlakySource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if self.pos >= self.ok_bytes {
            return Err(Error::Io(std::io::Error::other("transport died")));
        }
        let n = buf
            .len()
            .min(self.ok_bytes - self.pos)
            .min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
    fn seek(&mut self, _pos: u64) -> Result<u64, Error> {
        Err(Error::NotSeekable)
    }
    fn position(&self) -> u64 {
        self.pos as u64
    }
    fn seekability(&self) -> Seekability {
        Seekability::None
    }
    fn peek(&mut self, _len: usize) -> Result<&[u8], Error> {
        Ok(&[])
    }
}

#[test]
fn error_is_sticky() {
    let mut io = ctx_of(
        FlakySource {
            data: vec![7u8; 100],
            pos: 0,
            ok_bytes: 8,
        },
        4,
    );
    let mut out = [0u8; 8];
    io.read_exact(&mut out).unwrap();
    assert_eq!(out, [7u8; 8]);
    let first = io.r8().unwrap_err();
    assert!(matches!(first, Error::Io(_)), "{first:?}");
    assert!(io.error().is_some());
    // Every later call replays it rather than retrying a dead transport.
    for _ in 0..3 {
        assert!(matches!(io.r8(), Err(Error::Io(_))));
    }
    io.clear_error();
    assert!(io.error().is_none());
}

// ------------------------------------------------------------- past-EOF seek

// Regression coverage for the `io_buffered_reader` fuzz crash
// (`crash-3992b5c70cae36a7e8539dde389deab9d6946703`, replayed as a seed under
// `fuzz/seeds/io_buffered_reader/`): `MemorySource::seek` used to clamp to
// `data.len()`, contradicting its own comment and disagreeing with
// `FileSource` (a bare `File::seek(SeekFrom::Start)`, which a real OS never
// clamps); and `IoContext::skip` discarded the actual position `seek`
// returned, so a caller could not tell a full skip from a short one. Every
// `MediaSource` kind this crate ships — `Cheap`, `Expensive`, `None` — is
// exercised here so they cannot disagree with each other again.

/// `MemorySource::seek` past the end must report where it actually landed,
/// not clamp — matching `lseek`/`File::seek(SeekFrom::Start)`, which never
/// clamps either. This is `MediaSource`-level, below `IoContext`.
#[test]
fn memory_source_seek_past_end_is_not_clamped() {
    let mut src = MemorySource::new(vec![1, 2, 3]);
    assert_eq!(src.seek(1_000).unwrap(), 1_000);
    assert_eq!(src.position(), 1_000);
    // The far-past-end position reads as EOF, not an error or stale data.
    assert_eq!(src.read(&mut [0u8; 4]).unwrap(), 0);

    let mut expensive = MemorySource::expensive(vec![1, 2, 3]);
    assert_eq!(expensive.seek(1_000).unwrap(), 1_000);
    assert_eq!(expensive.read(&mut [0u8; 4]).unwrap(), 0);
}

/// Every seekable `IoContext` agrees: a seek past the end lands exactly where
/// asked, and a read from there is EOF, not clamped bytes or a panic.
///
/// The target is chosen past `short_seek_max` (64 KiB default) for each
/// source, so both land via a real transport seek rather than
/// read-and-discard — the discard path's own past-EOF behaviour (fail, don't
/// clamp) is covered separately by the `short_skip_*`/`skip_past_end_*` tests.
#[test]
fn io_context_seek_past_end_lands_exactly_and_reads_eof() {
    let far = 200_000u64;
    for src in [
        MemorySource::new(vec![1, 2, 3, 4, 5]),
        MemorySource::expensive(vec![1, 2, 3, 4, 5]),
    ] {
        let mut io = ctx_of(src, 64);
        assert_eq!(io.seek(far).unwrap(), far);
        assert_eq!(io.pos(), far);
        assert_eq!(io.read_partial(&mut [0u8; 4]).unwrap(), 0);
        assert_eq!(io.pos(), far, "a zero-byte EOF read must not move pos");
    }
}

/// The exact scenario from the crash artifact: an empty `Cheap` source,
/// `skip` past the end. `skip` must either land exactly `n` past where it
/// started or fail — never silently report success while landing short.
#[test]
fn skip_past_end_on_cheap_source_is_exact_or_fails() {
    let mut io = ctx_of(MemorySource::new(Vec::new()), 8192);
    let before = io.pos();
    io.skip(225).unwrap();
    assert_eq!(io.pos(), before + 225, "skip must land exactly n forward");
    assert_eq!(io.read_partial(&mut [0u8; 1]).unwrap(), 0);
}

/// On an `Expensive` source, a short forward skip is served by
/// read-and-discard. Past real EOF there are not enough bytes to discard, so
/// `skip` must fail rather than pretend it moved the full distance.
#[test]
fn short_skip_past_end_on_expensive_source_fails_and_resyncs() {
    let data = vec![1u8, 2, 3, 4, 5];
    let mut io = ctx_of(MemorySource::expensive(data.clone()), 8192);
    // `n` well under `short_seek_max` (64 KiB default), so this is the
    // read-and-discard path, not a transport seek.
    let err = io.skip(1_000).unwrap_err();
    assert!(matches!(err, Error::UnexpectedEof), "{err:?}");
    // It consumed every real byte on the way to failing, same as read_exact.
    assert_eq!(io.pos(), data.len() as u64);
    assert_eq!(io.read_partial(&mut [0u8; 1]).unwrap(), 0);
}

/// A long forward skip on an `Expensive` source crosses the short-seek
/// threshold into a real transport seek, which (like a file) does not clamp:
/// `skip` must land exactly `n` forward, past the end.
#[test]
fn long_skip_past_end_on_expensive_source_lands_exactly() {
    let data = vec![1u8, 2, 3, 4, 5];
    let mut io = ctx_of(MemorySource::expensive(data), 8192);
    let far = 200_000; // over the 64 KiB short-seek threshold
    io.skip(far).unwrap();
    assert_eq!(io.pos(), far);
    assert_eq!(io.read_partial(&mut [0u8; 1]).unwrap(), 0);
}

/// A forward-only source always serves a forward hop as read-and-discard
/// (its short-seek threshold is `u64::MAX`), so a skip past the real end must
/// fail there too, landing at the true end rather than lying about `n`.
#[test]
fn skip_past_end_on_forward_only_source_fails_and_resyncs() {
    let data = vec![1u8, 2, 3, 4, 5];
    let mut io = ctx_of(MemorySource::forward_only(data.clone()), 8192);
    let err = io.skip(1_000).unwrap_err();
    assert!(matches!(err, Error::UnexpectedEof), "{err:?}");
    assert_eq!(io.pos(), data.len() as u64);
}

/// A forward-only source has no transport seek to fall back on — every
/// forward hop, however requested, is read-and-discard (`short_seek_max` is
/// `u64::MAX` for `Seekability::None`). So unlike `Cheap`/`Expensive`, this
/// kind cannot represent a position past its real end at all: `seek` past it
/// fails exactly like `skip` does, through the same code path.
#[test]
fn seek_past_end_on_forward_only_source_fails() {
    let data = vec![1u8, 2, 3];
    let mut io = ctx_of(MemorySource::forward_only(data), 8192);
    let err = io.seek(1_000).unwrap_err();
    assert!(matches!(err, Error::UnexpectedEof), "{err:?}");
}

// ------------------------------------------------------------------ properties

proptest! {
    /// Any read pattern yields the same bytes as one big read.
    #[test]
    fn buffering_is_transparent(
        data in prop::collection::vec(any::<u8>(), 0..2000),
        block in 64usize..512,
        chunks in prop::collection::vec(1usize..300, 0..40),
    ) {
        let mut io = ctx(data.clone(), block);
        let mut got = Vec::new();
        for c in chunks {
            let mut buf = vec![0u8; c];
            let n = io.read_partial(&mut buf).unwrap();
            got.extend_from_slice(&buf[..n]);
            if n == 0 { break; }
        }
        prop_assert_eq!(&got[..], &data[..got.len()]);
        prop_assert_eq!(io.pos(), got.len() as u64);
    }

    /// Reading to exhaustion in arbitrary chunks reproduces the whole source.
    #[test]
    fn full_read_matches_source(
        data in prop::collection::vec(any::<u8>(), 0..3000),
        block in 64usize..1024,
        chunk in 1usize..97,
    ) {
        let mut io = ctx(data.clone(), block);
        let mut got = Vec::new();
        loop {
            let mut buf = vec![0u8; chunk];
            let n = io.read_partial(&mut buf).unwrap();
            if n == 0 { break; }
            got.extend_from_slice(&buf[..n]);
        }
        prop_assert_eq!(got, data);
    }

    /// Seek-then-read equals read-from-offset.
    #[test]
    fn seek_then_read_equals_offset(
        data in prop::collection::vec(any::<u8>(), 1..2000),
        block in 64usize..512,
        offset in 0usize..2000,
        len in 1usize..200,
    ) {
        let offset = offset.min(data.len());
        let mut io = ctx(data.clone(), block);
        prop_assert_eq!(io.seek(offset as u64).unwrap(), offset as u64);
        let mut buf = vec![0u8; len];
        let n = io.read_partial(&mut buf).unwrap();
        prop_assert_eq!(&buf[..n], &data[offset..offset + n]);
    }

    /// A walk of seeks and reads never diverges from indexing the source.
    #[test]
    fn interleaved_seeks_and_reads(
        data in prop::collection::vec(any::<u8>(), 1..1500),
        block in 64usize..256,
        ops in prop::collection::vec((0usize..1500, 1usize..64), 1..30),
    ) {
        let mut io = ctx(data.clone(), block);
        for (off, len) in ops {
            let off = off.min(data.len());
            io.seek(off as u64).unwrap();
            prop_assert_eq!(io.pos(), off as u64);
            let mut buf = vec![0u8; len];
            let n = io.read_partial(&mut buf).unwrap();
            prop_assert_eq!(&buf[..n], &data[off..off + n]);
            prop_assert_eq!(io.pos(), (off + n) as u64);
        }
    }

    /// The short-seek path must land in the same place as a real seek.
    ///
    /// `Expensive` turns a forward hop under the threshold into a
    /// read-and-discard while `Cheap` issues a real seek, so this is the
    /// property that says the optimisation is invisible.
    #[test]
    fn short_seek_matches_real_seek(
        data in prop::collection::vec(any::<u8>(), 200..2000),
        hop in 0u64..300,
    ) {
        let start = 50u64;
        let target = (start + hop).min(data.len() as u64 - 16);

        let mut cheap = ctx_of(MemorySource::new(data.clone()), 128);
        cheap.seek(start).unwrap();
        cheap.seek(target).unwrap();

        let mut expensive = ctx_of(MemorySource::expensive(data.clone()), 128);
        expensive.seek(start).unwrap();
        expensive.seek(target).unwrap();

        prop_assert_eq!(cheap.pos(), expensive.pos());
        let mut a = [0u8; 16];
        let mut b = [0u8; 16];
        cheap.read_exact(&mut a).unwrap();
        expensive.read_exact(&mut b).unwrap();
        prop_assert_eq!(a, b);
        prop_assert_eq!(&a[..], &data[target as usize..target as usize + 16]);
    }
}
