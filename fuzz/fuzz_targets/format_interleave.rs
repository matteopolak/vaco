//! The muxer-side ordering machinery under an arbitrary call sequence.
//!
//! The untrusted input here is the *call sequence*, not a byte stream — the
//! same shape as `codec_send_receive`. Every muxer in the project runs through
//! `MuxTimestamps` and `InterleaveQueue`, so a lost packet or a hang in either
//! is a lost packet or a hang in all of them.
//!
//! Three properties, all of which a real remux depends on:
//!
//! * **conservation** — every packet that goes in comes out exactly once;
//! * **per-stream order** — the queue orders *between* streams and never
//!   reorders within one;
//! * **termination** — draining always finishes.
//! fuzz-crate: vaco-format-core

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Rational, Timestamp};
use vaco_format_core::interleave::{InterleaveQueue, MuxTimestamps};
use vaco_format_core::{FormatFlags, FormatOptions};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

#[derive(Debug, arbitrary::Arbitrary)]
enum Op {
    Push { stream: u8, dts: i32, len: u8 },
    Next { flush: bool },
    EndStream { stream: u8 },
}

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    streams: u8,
    max_interleave_delta: i64,
    chunk_size: i32,
    chunk_duration: i32,
    audio_preload: i32,
    avoid_negative_ts: i32,
    ts_negative: bool,
    ts_nonstrict: bool,
    ops: Vec<Op>,
}

fuzz_target!(|input: Input| {
    let streams = usize::from(input.streams % 5) + 1;
    if input.ops.len() > 4096 {
        return;
    }

    let mut opts = FormatOptions::default();
    opts.max_interleave_delta = input.max_interleave_delta.max(0);
    opts.chunk_size = input.chunk_size.max(0);
    opts.chunk_duration = input.chunk_duration.max(0);
    opts.audio_preload = input.audio_preload.max(0);
    opts.avoid_negative_ts = input.avoid_negative_ts;

    let mut flags = FormatFlags::empty();
    if input.ts_negative {
        flags |= FormatFlags::TS_NEGATIVE;
    }
    if input.ts_nonstrict {
        flags |= FormatFlags::TS_NONSTRICT;
    }

    let mut queue = InterleaveQueue::new(streams, &opts);
    let mut chain = MuxTimestamps::new(streams, flags, &opts);
    let tb = Rational::new(1, 1000);
    for i in 0..streams {
        queue.set_time_base(i as u32, tb);
        queue.set_preloaded(i as u32, i % 2 == 1);
    }

    let mut budget = Budget::new(Limits::strict());
    // Per stream: what went in, and what came out, in order.
    let mut pushed: Vec<Vec<i64>> = vec![Vec::new(); streams];
    let mut popped: Vec<Vec<i64>> = vec![Vec::new(); streams];

    for op in &input.ops {
        match *op {
            Op::Push { stream, dts, len } => {
                let s = usize::from(stream) % streams;
                let Ok(mut pkt) = Packet::alloc(&mut budget, usize::from(len)) else {
                    // A refused allocation under a tight budget is correct
                    // behaviour, not a finding.
                    break;
                };
                pkt.stream_index = s as u32;
                pkt.dts = Timestamp::new(i64::from(dts));
                pkt.pts = pkt.dts;
                // The chain rejects a non-monotonic DTS; that is an error the
                // caller handles, so the packet simply never enters the queue.
                if chain.apply(&mut pkt, tb, tb).is_err() {
                    continue;
                }
                let key = pkt.dts.ticks().unwrap_or(i64::MIN);
                if queue.push(pkt).is_ok() {
                    pushed[s].push(key);
                }
            }
            Op::Next { flush } => {
                if let Some(p) = queue.next(flush) {
                    let s = p.stream_index as usize;
                    assert!(s < streams, "the queue produced an unknown stream");
                    popped[s].push(p.dts.ticks().unwrap_or(i64::MIN));
                }
            }
            Op::EndStream { stream } => {
                queue.end_stream((usize::from(stream) % streams) as u32);
            }
        }
    }

    // Draining terminates.
    let mut guard = 0u32;
    for p in queue.drain() {
        guard += 1;
        assert!(guard < 100_000, "drain did not terminate");
        let s = p.stream_index as usize;
        popped[s].push(p.dts.ticks().unwrap_or(i64::MIN));
    }
    assert!(queue.is_empty(), "drain left packets behind");

    for s in 0..streams {
        assert_eq!(
            pushed[s], popped[s],
            "stream {s}: packets were lost, duplicated or reordered within the stream"
        );
    }
});
