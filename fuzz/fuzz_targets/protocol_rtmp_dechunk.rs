//! `vaco_protocol_rtmp::message::Dechunker::feed` on an arbitrary byte
//! stream — exactly what a malicious or broken RTMP peer could send, since
//! there is no live server reachable to fuzz against instead. Splits the
//! input into two pushes at an arbitrary point so both the "whole chunk
//! arrives at once" and "a chunk header is split across two reads" paths
//! get exercised.
//!
//! A streaming reassembler must be invariant to how its input is chopped
//! across `feed` calls: RTMP chunk boundaries are a protocol-level concept,
//! not an I/O-call-boundary one. So beyond panic-freedom, this target
//! checks that value directly: feeding the same bytes whole and feeding
//! them split at every reachable point must reassemble to the same
//! messages whenever both succeed.
//! fuzz-crate: vaco-protocol-rtmp
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::Limits;
use vaco_protocol_rtmp::message::{Dechunker, RtmpMessage};

/// Feed `chunks` to a fresh `Dechunker` in order, collecting every message
/// produced. `None` if any `feed` call errors (both feeding styles are then
/// allowed to disagree, since which side of a malformed byte a split lands
/// on can legitimately change where the error is raised).
fn feed_all(chunks: &[&[u8]]) -> Option<Vec<RtmpMessage>> {
    let mut d = Dechunker::new(Limits::strict());
    let mut out = Vec::new();
    for chunk in chunks {
        out.extend(d.feed(chunk).ok()?);
    }
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    let split = data.first().map_or(0, |b| usize::from(*b) % (data.len() + 1));
    let split = split.min(data.len());
    let (first, second) = data.split_at(split);

    let whole = feed_all(&[data]);
    let split_fed = feed_all(&[first, second]);

    if let (Some(whole), Some(split_fed)) = (whole, split_fed) {
        assert_eq!(
            whole, split_fed,
            "splitting the feed into two pushes changed the reassembled messages"
        );
    }
});
