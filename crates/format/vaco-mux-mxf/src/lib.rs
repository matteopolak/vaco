//! MXF (Material eXchange Format) muxers: three variants matching three
//! distinct registered `ffmpeg` muxer names (`ffmpeg -muxers | grep mxf`) —
//! `mxf` (`OP1a`, frame-wrapped essence, [`MUXER`]), `mxf_d10` (SMPTE 386M,
//! video-only in this crate today, [`MUXER_D10`]), and `mxf_opatom` (SMPTE
//! 390, one clip-wrapped essence track per file, [`MUXER_OPATOM`]) — see
//! `ul::MxfVariant`.
//!
//! Layer 4. Writes the four things `vaco-demux-mxf` reads: the KLV/BER
//! wrapper, the Partition Pack, the structural-metadata graph (`Preface` ->
//! `ContentStorage` -> `Package` -> `Track` -> `Sequence` ->
//! `StructuralComponent` -> `Descriptor`, keyed by the Primer Pack), and the
//! Generic Container essence element plus its Index Table Segment.
//!
//! Written clean-room (D7/D15): the byte layout below is this crate's own
//! encoding of the same SMPTE ST 377-1/379-1/336/RP 210 structures
//! `vaco-demux-mxf` already measured against real `ffmpeg 8.1` output — this
//! crate does not read `ffmpeg` source, and reuses only its own sibling
//! crate's already-published, clean-room-measured Universal Labels and
//! property tags (`provenance/sources.toml`'s `ffmpeg-mxf-probe` family).
//! Round-trip verified against both `vaco-demux-mxf` and the reference
//! `ffprobe`/`ffmpeg` — see `docs/format/vaco-mux-mxf.md`.
//!
//! ## Partition layout, decided once (see `docs/format/vaco-mux-mxf.md`)
//!
//! A "closed, complete" header partition (full structural metadata, no
//! Duration/Index yet — genuinely not known until every packet has been
//! seen) directly followed by essence (no separate body partition pack,
//! the same single-partition-carries-essence shape `vaco-demux-mxf` had to
//! learn to read for real D-10 files), then a "closed, complete" footer
//! partition restating the same graph with the real Duration and a real
//! Index Table Segment, then a Random Index Pack. This needs no backpatch
//! for the essence bytes themselves — only `write_trailer`'s own partition
//! pack (this file's `docs/format/vaco-mux-mxf.md` "How it works" explains
//! why this works even on a non-seekable sink, and what degrades when it
//! is not seekable).

#![forbid(unsafe_code)]

mod ber;
mod essence;
mod index;
mod klv;
mod localset;
mod metadata;
mod mux;
mod partition;
mod uid;
mod ul;

pub use mux::{MUXER, MUXER_D10, MUXER_OPATOM, MxfMuxer, MxfOptions};
