//! `peek` must not consume, and must work where seeking cannot.
//!
//! Every demuxer's probe path depends on both halves of that sentence, so both
//! are tested against a source that reports `Seekability::None` *and* refuses
//! `seek`, and against a real OS pipe.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use std::io::Write;

use proptest::prelude::*;
use vaco_core::Error;
use vaco_io::{
    IoContext, IoOptions, MediaSource, MemorySource, PeekSource, ReaderSource, Seekability,
};

#[test]
fn peek_works_on_a_real_pipe() {
    // The load-bearing case: no seeking is physically possible here.
    let (reader, mut writer) = std::io::pipe().unwrap();
    let payload: Vec<u8> = (0..4096u32).map(|i| i as u8).collect();
    let handle = std::thread::spawn(move || {
        writer.write_all(&payload).unwrap();
        drop(writer);
    });

    let src = PeekSource::new(ReaderSource::new(reader));
    assert_eq!(src.seekability(), Seekability::None);
    let mut io = IoContext::new(Box::new(src), &IoOptions::default().with_block_size(64)).unwrap();

    // Probe: look at a prefix bigger than the buffer, then rewind to nothing.
    let head = io.peek(300).unwrap().to_vec();
    assert_eq!(head.len(), 300);
    assert_eq!(io.pos(), 0, "peek must not consume");

    // Peeking again is idempotent.
    assert_eq!(io.peek(300).unwrap(), &head[..]);
    assert_eq!(io.pos(), 0);

    // And the bytes the probe saw are the bytes the demuxer then reads.
    let mut got = vec![0u8; 300];
    io.read_exact(&mut got).unwrap();
    assert_eq!(got, head);
    assert_eq!(io.pos(), 300);

    let mut rest = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = io.read_partial(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        rest.extend_from_slice(&buf[..n]);
    }
    assert_eq!(rest.len(), 4096 - 300);
    handle.join().unwrap();
}

#[test]
fn media_source_peek_on_forward_only_memory() {
    let mut src = MemorySource::forward_only((0..64u32).map(|i| i as u8).collect());
    assert!(matches!(src.seek(1), Err(Error::NotSeekable)));
    assert_eq!(src.peek(8).unwrap(), &[0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(src.position(), 0);
    let mut got = [0u8; 8];
    src.read_exact(&mut got).unwrap();
    assert_eq!(got, [0, 1, 2, 3, 4, 5, 6, 7]);
}

#[test]
fn peek_source_adapter_peeks_over_a_forward_only_transport() {
    let payload: Vec<u8> = (0..500u32).map(|i| i as u8).collect();
    let cursor = std::io::Cursor::new(payload.clone());
    let mut src = PeekSource::new(ReaderSource::new(cursor));

    assert_eq!(src.peek(10).unwrap(), &payload[..10]);
    assert_eq!(src.position(), 0);
    // A forward seek inside the peek window is free; outside it is impossible.
    assert_eq!(src.seek(5).unwrap(), 5);
    assert!(matches!(src.seek(400), Err(Error::NotSeekable)));
    assert_eq!(src.peek(4).unwrap(), &payload[5..9]);
}

#[test]
fn peek_at_eof_returns_what_exists() {
    let mut io = IoContext::new(
        Box::new(MemorySource::forward_only(vec![1, 2, 3])),
        &IoOptions::default().with_block_size(64),
    )
    .unwrap();
    assert_eq!(io.peek(64).unwrap(), &[1, 2, 3]);
    assert_eq!(io.pos(), 0);
    assert_eq!(io.peek(2).unwrap(), &[1, 2]);
}

#[test]
fn peek_beyond_probe_budget_is_refused_not_allocated() {
    let limits = vaco_limits::Limits::strict().with_alloc_total(1 << 20);
    let opts = IoOptions::default().with_limits(limits);
    let mut io = IoContext::new(Box::new(MemorySource::new(vec![0u8; 16])), &opts).unwrap();
    let huge = usize::try_from(opts.limits.max_probe_bytes).unwrap() + 1;
    assert!(matches!(io.peek(huge), Err(Error::LimitExceeded { .. })));
}

proptest! {
    /// `peek` never consumes, whatever the read pattern around it.
    #[test]
    fn peek_never_consumes(
        data in prop::collection::vec(any::<u8>(), 1..1500),
        block in 64usize..300,
        ops in prop::collection::vec((0usize..3, 1usize..400), 1..25),
    ) {
        let mut io = IoContext::new(
            Box::new(MemorySource::forward_only(data.clone())),
            &IoOptions::default().with_block_size(block),
        ).unwrap();
        let mut cursor = 0usize;
        for (kind, n) in ops {
            if kind == 0 {
                let before = io.pos();
                let want = n.min(4096);
                let seen = io.peek(want).unwrap().to_vec();
                prop_assert_eq!(io.pos(), before);
                let end = (cursor + want).min(data.len());
                prop_assert_eq!(&seen[..], &data[cursor..end]);
            } else {
                let mut buf = vec![0u8; n];
                let got = io.read_partial(&mut buf).unwrap();
                prop_assert_eq!(&buf[..got], &data[cursor..cursor + got]);
                cursor += got;
            }
        }
    }

    /// A peek followed by a read of the same length returns identical bytes.
    #[test]
    fn peek_then_read_agree(
        data in prop::collection::vec(any::<u8>(), 1..1000),
        block in 64usize..256,
        want in 1usize..600,
    ) {
        let mut io = IoContext::new(
            Box::new(MemorySource::forward_only(data)),
            &IoOptions::default().with_block_size(block),
        ).unwrap();
        let peeked = io.peek(want).unwrap().to_vec();
        let mut buf = vec![0u8; want];
        let n = io.read_partial(&mut buf).unwrap();
        prop_assert_eq!(&peeked[..n.min(peeked.len())], &buf[..n.min(peeked.len())]);
    }
}
