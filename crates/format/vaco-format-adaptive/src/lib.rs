//! What HLS and DASH genuinely share, and nothing they do not.
//!
//! # What it is
//!
//! `vaco-demux-hls`, `vaco-mux-hls`, `vaco-demux-dash` and `vaco-mux-dash` each
//! parse a completely different text format (an M3U8 line-oriented playlist
//! versus an XML manifest). None of that syntax lives here. What both
//! protocols actually share, once the syntax is stripped away, is:
//!
//! 1. **A segment timeline** — an ordered run of `(start, duration)` pairs,
//!    possibly with mid-stream discontinuities. DASH states this directly with
//!    `SegmentTimeline`'s `S@t/@d/@r` run-length encoding; HLS states it
//!    implicitly as a list of `EXTINF` durations. [`timeline`] expands both into
//!    the same [`timeline::SegmentTiming`] sequence.
//! 2. **Variant/representation selection** — HLS's `EXT-X-STREAM-INF` and
//!    DASH's `Representation` are the same idea (a bitrate/resolution/codec
//!    tuple, one media presentation among several); [`variant`] holds the
//!    common model and the bandwidth-capped selection rule.
//! 3. **A byte-range segment reader** — HLS's `EXT-X-BYTERANGE` and DASH's
//!    `indexRange`/`SegmentBase` both address a sub-range of one file rather
//!    than a whole file per segment. [`byterange`] is the one implementation.
//! 4. **A reference to a nested container demuxer/muxer.** A segment is MPEG-TS
//!    or fragmented MP4; the actual box/packet parsing is not this layer's job
//!    (D14.1 forbids a `format` crate from reaching into another concrete
//!    format crate, and the pair genuinely already exist as demuxers/muxers of
//!    their own). [`provider`] is the seam, structured exactly like
//!    [`vaco_format_core::ParserProvider`]: a trait defined at this layer,
//!    implemented with the concrete crates by whoever assembles the registry.
//! 5. **Relative URL resolution and wall-clock timestamp parsing.** Every
//!    segment and sub-manifest URL in both formats is commonly relative to the
//!    manifest that named it ([`url::resolve`]); every "what time is this" field
//!    (`EXT-X-PROGRAM-DATE-TIME`, `availabilityStartTime`, `publishTime`) is an
//!    ISO 8601 timestamp ([`walltime`]).
//!
//! What is deliberately **not** here: any `EXT-X-` tag spelling and any MPD
//! element name. Those have nothing in common syntactically and belong in
//! `vaco-demux-hls`/`vaco-mux-hls` and `vaco-demux-dash`/`vaco-mux-dash`
//! respectively. An earlier draft of this crate's brief guessed this crate
//! might hold the playlist/manifest grammar too; it does not, on purpose.
//!
//! # How to change it
//!
//! This crate has no registry entry and registers nothing — it is a library,
//! not a component. Add to it only when a THIRD adaptive-streaming format
//! shows up and needs the same seam (low-latency HLS parts and CMAF chunks are
//! the obvious future case), or when HLS and DASH need to agree on more than
//! they do today.
//!
//! # Configuration
//!
//! None directly; [`timeline::expand`] takes a [`vaco_limits::Budget`] because
//! a hostile `SegmentTimeline`'s `@r` (repeat count) can request an unbounded
//! number of segments in a few bytes of XML, and the same is true of an
//! absurdly small HLS `EXTINF` repeated by a very long playlist.
//!
//! # Dependencies
//!
//! `vaco-core`, `vaco-io`, `vaco-limits`, `vaco-time` (wall clock, never
//! `std::time`), `vaco-format-core` (`Demuxer`/`Muxer`), `vaco-codec-core`
//! (`CodecId`/`CodecParameters`, for [`variant::Variant`]).
//!
//! `vaco-protocol-core` (rule W2: never a concrete protocol crate) is a
//! dependency too, for [`access::RemoteAccess`]/[`write_access::WriteAccess`]
//! — moved here from `vaco-demux-hls`/`vaco-mux-hls` once `vaco-demux-dash`
//! needed the identical "keep a whitelist-gated capability to open more URLs
//! alive for the demuxer's whole lifetime" shape (see [`url::resolve`] for
//! the one thing in this file that is *not* a protocol open: pure string
//! manipulation on a manifest's own address, needing no `ProtocolEnv` at
//! all).

#![forbid(unsafe_code)]

pub mod access;
pub mod byterange;
pub mod provider;
pub mod readall;
pub mod timeline;
pub mod url;
pub mod variant;
pub mod walltime;
pub mod write_access;

pub use access::RemoteAccess;
pub use byterange::{BoundedSource, ByteRange};
pub use provider::{
    NoSegmentDemuxers, NoSegmentMuxers, SegmentContainerHint, SegmentDemuxerProvider,
    SegmentMuxerProvider,
};
pub use readall::read_all_bounded;
pub use timeline::{SegmentTiming, TimelineEntry, expand};
pub use url::resolve;
pub use variant::{Rendition, RenditionKind, Variant, select_variant};
pub use walltime::WallClock;
pub use write_access::WriteAccess;
