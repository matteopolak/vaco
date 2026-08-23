//! The seam to a nested segment container, without a `format` crate depending
//! on another `format` crate.
//!
//! A segment is MPEG-TS or fragmented MP4, and both demuxers/muxers already
//! exist in this workspace — reimplementing either here would be exactly the
//! kind of duplication D14.1 exists to prevent. But `crates/format/` may not
//! depend on another `crates/format/` crate (the layering rule is same-layer
//! *or* downward, and cargo would also reject the cycle this specific pair
//! creates: `vaco-registry` optionally depends on every registered demuxer,
//! including this crate's siblings, so if `vaco-demux-hls` also depended on
//! `vaco-registry` the graph would have a cycle the moment both are
//! registered).
//!
//! So this is structured exactly like
//! [`vaco_format_core::ParserProvider`]: a trait defined at this layer,
//! implemented with the concrete demuxer/muxer crates by whoever assembles
//! the registry (today, that is `vaco-registry`, which already depends on
//! every concrete format crate and is therefore the right place for a
//! `SegmentDemuxers`/`SegmentMuxers` implementation — see this crate's
//! top-level report for why that wiring is not included here).
//! [`NoSegmentDemuxers`]/[`NoSegmentMuxers`] are the safe defaults every unit
//! test and fuzz target uses, mirroring [`vaco_format_core::discovery::NoParsers`].

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, Result};
use vaco_format_core::{Demuxer, Muxer, ParserProvider};
use vaco_io::{MediaSink, MediaSource};

/// Which container a segment is wrapped in.
///
/// Determined by `#EXT-X-MAP`'s presence (fMP4) versus its absence (MPEG-TS)
/// for HLS, and by a `Representation`'s `mimeType`/`codecs` for DASH — the
/// concrete crates decide which, this enum just names the two the spec
/// allows. HLS's Packed Audio segment types (`.aac`, `.ac3`) are legitimate
/// per RFC 8216 §3.4 but are raw elementary streams reached through
/// `vaco-demux-raw`'s family, not through this provider at all: they need no
/// container demuxer, only a `ParserProvider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SegmentContainerHint {
    MpegTs,
    Fmp4,
}

/// How a demux-side segment reader gets a nested container demuxer.
pub trait SegmentDemuxerProvider: Send + Sync {
    /// Open `source` (already a single segment's bytes, or an
    /// [`crate::byterange::BoundedSource`] slice of one) as `hint`'s
    /// container.
    ///
    /// `init` carries an fMP4 `EXT-X-MAP`/DASH `Initialization` segment's
    /// bytes when the container needs one read first (CMAF/fMP4 media
    /// segments have no `moov` of their own); `None` for MPEG-TS, which is
    /// self-describing per segment.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] when this build has no implementation
    /// for `hint`; whatever the underlying demuxer's `open` reports otherwise.
    fn open_segment(
        &self,
        hint: SegmentContainerHint,
        init: Option<&[u8]>,
        source: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
    ) -> Result<Box<dyn Demuxer>>;
}

/// The provider that answers `Unsupported` to everything.
///
/// The default for every unit test that exercises playlist/MPD parsing,
/// variant selection or the byte-range reader without needing an actual
/// decodable segment — which is most of them, since none of those seams
/// touches TS or fMP4 framing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSegmentDemuxers;

impl SegmentDemuxerProvider for NoSegmentDemuxers {
    fn open_segment(
        &self,
        _hint: SegmentContainerHint,
        _init: Option<&[u8]>,
        _source: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
    ) -> Result<Box<dyn Demuxer>> {
        Err(Error::Unsupported(
            "no segment container demuxer registered (NoSegmentDemuxers)",
        ))
    }
}

/// How a mux-side segmenter gets a nested container muxer.
///
/// Mirrors [`SegmentDemuxerProvider`] for exactly the same reason: `hls`
/// and `dash` write MPEG-TS or fMP4 segments by driving the existing
/// `vaco-mux-mpegts`/`vaco-mux-mp4` muxers with segmentation-specific options
/// (fragment-per-segment, `empty_moov`/separate init), not by re-implementing
/// container writing.
pub trait SegmentMuxerProvider: Send + Sync {
    /// Construct a fresh muxer of `hint`'s container over `sink`, with
    /// `add_stream` already called once per entry of `streams`, in order —
    /// so the returned value's stream indices line up with `streams`'
    /// indices without the caller having to call `add_stream` itself.
    ///
    /// The contract stops there deliberately: the caller still drives
    /// `init`/`write_header`/`write_packet`/`write_trailer` through the
    /// ordinary [`vaco_format_core::Muxer`] trait, the same way it would for
    /// any other muxer. That is what lets `init_only` mean simply "call
    /// `write_header` then `write_trailer` with zero packets" for
    /// [`SegmentContainerHint::Fmp4`] — the caller does that itself and gets
    /// an initialization segment (`moov`, no samples) back for free, without
    /// this trait needing a second, `init`-only code path of its own.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] when this build has no implementation
    /// for `hint`.
    fn open_segment(
        &self,
        hint: SegmentContainerHint,
        sink: Box<dyn MediaSink>,
        streams: &[CodecParameters],
        init_only: bool,
    ) -> Result<Box<dyn Muxer>>;
}

/// The provider that answers `Unsupported` to everything. See
/// [`NoSegmentDemuxers`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSegmentMuxers;

impl SegmentMuxerProvider for NoSegmentMuxers {
    fn open_segment(
        &self,
        _hint: SegmentContainerHint,
        _sink: Box<dyn MediaSink>,
        _streams: &[CodecParameters],
        _init_only: bool,
    ) -> Result<Box<dyn Muxer>> {
        Err(Error::Unsupported(
            "no segment container muxer registered (NoSegmentMuxers)",
        ))
    }
}
