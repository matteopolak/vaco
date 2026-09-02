//! The meta-muxers (and one meta-demuxer): `concat`, `ffmetadata`, `segment`,
//! `stream_segment`, `tee`, `fifo`.
//!
//! # What ties these together
//!
//! Every registration here except `ffmetadata` either **owns other
//! muxers/demuxers and routes packets into or out of them** (`tee` fans one
//! input out to several muxers; `segment`/`stream_segment` hand successive
//! spans of packets to successive muxer instances; `fifo` buffers packets in
//! front of one inner muxer) or is a **virtual concatenation** of other
//! containers' output (`concat`). `ffmetadata` is the odd one out — a flat
//! key/value text format with no inner anything — and it lives here rather
//! than in `vaco-mux-utility` because it is grouped with the other
//! meta-formats, not because it shares their shape.
//!
//! # `concat` is a demuxer, not a muxer
//!
//! Measured: `ffmpeg -muxers | grep concat` prints nothing; `ffmpeg -demuxers
//! | grep concat` prints `D   concat          Virtual concatenation script`.
//! The issue that asked for this crate assumed a muxer; probing said
//! otherwise, so [`concat`] registers a [`vaco_format_core::DemuxerDesc`].
//! The crate name (`vaco-mux-stream`) is therefore slightly off for this one
//! registration — noted rather than worked around, since renaming the crate
//! is not this brief's call to make mid-wave.
//!
//! # The registry seam does not fit most of these
//!
//! [`vaco_format_core::MuxerDesc::open`] is `fn(Box<dyn MediaSink>) ->
//! Result<Box<dyn Muxer>>` — one sink, no options, no way to name an inner
//! muxer or a list of output URLs. `tee`, `segment`/`stream_segment` and
//! `fifo` all *need* exactly that (a target format name, a URL list, per-
//! output options) to do anything beyond the degenerate case. Each module
//! below therefore offers two things, mirroring the pattern
//! `vaco-demux-image2` already established for the same kind of gap (see its
//! `multi.rs` module docs):
//!
//! * a real, richly configurable constructor (`TeeMuxer::new`,
//!   `SegmentMuxer::new`, `FifoMuxer::new`) that a caller with the missing
//!   information — an embedder, `vaco-cli` once it grows one, this crate's
//!   own tests — uses directly;
//! * the `MuxerDesc`/`DemuxerDesc` registration, whose `open` either does the
//!   one honest thing the bare signature allows, or reports the gap with
//!   [`vaco_core::Error::Unsupported`] rather than guessing at a default that
//!   was never asked for.
//!
//! `concat` has the same shape of gap one level down: [`DemuxerDesc::open`]
//! gets one already-open [`vaco_io::MediaSource`] (the concat *script*) and a
//! [`vaco_format_core::ParserProvider`], with no way to open the *other*
//! files the script names. [`concat::ConcatSource`] is this crate's own
//! version of the same seam [`vaco_format_core::BsfProvider`]/`ParserProvider`
//! use one layer up: a caller that has `vaco-registry` in scope implements it
//! by probing and opening each named file; this crate's tests supply a fake.
//!
//! # Layout
//!
//! | Module | Registration(s) | Shape |
//! |---|---|---|
//! | [`ffmetadata`] | `ffmetadata` | flat key/value text, no inner anything |
//! | [`concat`] | `concat` (demuxer) | reads a script, drives N inner demuxers in sequence |
//! | [`tee`] | `tee` | one input, N inner muxers |
//! | [`segment`] | `segment`, `stream_segment` | successive spans, each its own inner muxer |
//! | [`fifo`] | `fifo` | buffers packets in front of one inner muxer |

#![forbid(unsafe_code)]

pub mod concat;
pub mod ffmetadata;
pub mod fifo;
pub mod segment;
pub mod tee;

pub use concat::DEMUXER_CONCAT;
pub use ffmetadata::MUXER_FFMETADATA;
pub use fifo::MUXER_FIFO;
pub use segment::{MUXER_SEGMENT, MUXER_STREAM_SEGMENT};
pub use tee::MUXER_TEE;

use vaco_format_core::{DemuxerDesc, MuxerDesc};

/// Every muxer this crate registers (everything except `concat`, which is a
/// demuxer — see [`all_demuxers`]).
#[must_use]
pub fn all_muxers() -> Vec<&'static MuxerDesc> {
    vec![
        &MUXER_FFMETADATA,
        &MUXER_TEE,
        &MUXER_SEGMENT,
        &MUXER_STREAM_SEGMENT,
        &MUXER_FIFO,
    ]
}

/// Every demuxer this crate registers: `concat` alone.
#[must_use]
pub fn all_demuxers() -> Vec<&'static DemuxerDesc> {
    vec![&DEMUXER_CONCAT]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_five_muxer_registrations() {
        assert_eq!(all_muxers().len(), 5);
    }

    #[test]
    fn there_is_exactly_one_demuxer_registration() {
        assert_eq!(all_demuxers().len(), 1);
    }

    #[test]
    fn every_muxer_name_is_unique() {
        let all = all_muxers();
        let mut names: Vec<&str> = all.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names, dedup, "duplicate muxer name registered");
    }

    #[test]
    fn every_muxer_descriptor_opens() {
        use vaco_format_core::vacoraw::MemorySink;
        for desc in all_muxers() {
            let sink = Box::new(MemorySink::new());
            // Some of these (`tee`, `segment`, `fifo`) are honest structural
            // failures through this bare signature — see the crate docs —
            // so this only checks that `open` returns *something* rather
            // than panicking, not that it succeeds.
            let _ = (desc.open)(sink);
        }
    }
}
