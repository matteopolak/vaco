//! Whole-file demux over arbitrary bytes, across every registered demuxer in
//! `vaco-subtitle-bitmap`, plus the two untrusted-input paths that sit
//! underneath the registry (`vobsub`'s `.idx` text grammar, and DVB's
//! length-prefixed binary segment structure).
//!
//! This crate parses two different kinds of untrusted input
//! (`planning/AGENT-CONSTRAINTS.md`, "Fuzzing"): length-prefixed binary
//! (`sup`'s segment headers, `dvbsub`'s EN 300 743 segments) and plain text
//! (`vobsub`'s `.idx`). Both are exercised here, in one target, the same
//! shape `vaco-subtitle-text`'s `subtitle_text_demux` target uses for its own
//! family.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Reading terminates.** [`MAX_PACKETS`] turns a demuxer that never
//!   reaches `Eof` into a local assertion rather than a fuzzer timeout.
//! * **`Eof` is stable**: once reported, it keeps being reported.
//! * **Every registered `probe` is total** over the same bytes.
//! * **`vobsub::idx::parse` never panics and never allocates unboundedly**
//!   over arbitrary text — it has no `Result`, so "does not panic" is the
//!   whole contract, checked directly rather than through a `Demuxer`.
//! * **`VobSubDemuxer::open_pair` over two independently-arbitrary byte
//!   strings** (an `.idx` and a `.sub`) never panics, exercising the
//!   sub-id correlation against whatever `vaco-demux-mpegps` makes of
//!   arbitrary `.sub` bytes.
//! * **DVB's structural segment parsers** (`dvbsub::segments::parse_clut`,
//!   `parse_region_composition`) never panic over arbitrary payloads — the
//!   `Rect`/`Palette` bounds-checking this crate exists to exercise.
//!
//! fuzz-crate: vaco-subtitle-bitmap

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_core::Error;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::probe::ProbeData;
use vaco_format_core::{Demuxer, DemuxerDesc};
use vaco_io::MemorySource;

/// Packets read per drain before the run is treated as non-terminating.
const MAX_PACKETS: u32 = 20_000;

fn descriptors() -> [&'static DemuxerDesc; 4] {
    [
        &vaco_subtitle_bitmap::dvbsub::DEMUXER,
        &vaco_subtitle_bitmap::dvbtxt::DEMUXER,
        &vaco_subtitle_bitmap::sup::DEMUXER,
        &vaco_subtitle_bitmap::vobsub::DEMUXER,
    ]
}

#[derive(Debug, Arbitrary)]
enum Input {
    /// Run one registered demuxer's `probe`+`open` over `bytes`.
    Registered { which: u8, bytes: Vec<u8> },
    /// `vobsub::idx::parse` over arbitrary (possibly non-UTF-8-lossy) text.
    IdxText { text: String },
    /// `VobSubDemuxer::open_pair` over two independently-arbitrary files.
    VobsubPair { idx: String, sub: Vec<u8> },
    /// DVB's structural, non-demuxing segment parsers, over an arbitrary
    /// payload — this is exactly the surface `Rect`/`Palette` bounds
    /// checking exists for.
    DvbsubSegmentPayload { payload: Vec<u8> },
}

/// Read until `Eof`, checking along the way.
fn drain(d: &mut dyn Demuxer) {
    let mut n = 0u32;
    loop {
        if n >= MAX_PACKETS {
            return;
        }
        match d.read_packet() {
            Ok(_) => n = n.saturating_add(1),
            Err(Error::Eof) => {
                assert!(matches!(d.read_packet(), Err(Error::Eof)));
                return;
            }
            Err(_) => return,
        }
    }
}

fuzz_target!(|input: Input| {
    match input {
        Input::Registered { which, bytes } => {
            let descs = descriptors();
            let data = ProbeData::new(&bytes);
            for desc in &descs {
                let _ = (desc.probe)(&data);
            }
            let Some(desc) = descs.get(usize::from(which) % descs.len()) else {
                return;
            };
            let src = Box::new(MemorySource::new(bytes));
            if let Ok(mut d) = (desc.open)(src, &NoParsers) {
                drain(d.as_mut());
            }
        }
        Input::IdxText { text } => {
            let file = vaco_subtitle_bitmap::vobsub::idx::parse(&text);
            let _ = vaco_subtitle_bitmap::vobsub::VobSubDemuxer::from_idx_only(&file);
        }
        Input::VobsubPair { idx, sub } => {
            let idx_src = Box::new(MemorySource::new(idx.into_bytes()));
            let sub_src = Box::new(MemorySource::new(sub));
            if let Ok(mut d) = vaco_subtitle_bitmap::vobsub::VobSubDemuxer::open_pair(idx_src, sub_src) {
                drain(&mut d);
            }
        }
        Input::DvbsubSegmentPayload { payload } => {
            let _ = vaco_subtitle_bitmap::dvbsub::segments::parse_region_composition(
                &payload,
                &vaco_limits::Limits::permissive(),
            );
            let _ = vaco_subtitle_bitmap::dvbsub::segments::parse_clut(&payload);
        }
    }
});
