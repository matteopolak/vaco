//! `vaco_protocol_rtmp::message::Dechunker::feed` on an arbitrary byte
//! stream — exactly what a malicious or broken RTMP peer could send, since
//! there is no live server reachable to fuzz against instead. Splits the
//! input into two pushes at an arbitrary point so both the "whole chunk
//! arrives at once" and "a chunk header is split across two reads" paths
//! get exercised.
//! fuzz-crate: vaco-protocol-rtmp
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::Limits;
use vaco_protocol_rtmp::message::Dechunker;

fuzz_target!(|data: &[u8]| {
    let mut d = Dechunker::new(Limits::strict());
    let split = data.first().map_or(0, |b| usize::from(*b) % (data.len() + 1));
    let (first, second) = data.split_at(split.min(data.len()));
    let _ = d.feed(first);
    let _ = d.feed(second);
});
