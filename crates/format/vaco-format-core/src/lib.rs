//! The container framework: what a demuxer and muxer are, plus the probing,
//! timestamp, seeking and interleaving models they share.
//!
//! Depends on `vaco-codec-core` for [`CodecParameters`] (D14.1), but never on a
//! concrete codec: bitstream parsers arrive through the injected
//! [`ParserProvider`], so no format crate depends on a codec crate.

use vaco_codec_core::{CodecId, CodecParameters, Parser};
use vaco_core::{Duration, MediaType, Rational, Result, Timestamp};
use vaco_io::{MediaSink, MediaSource};
use vaco_packet::Packet;

pub mod probe;
pub mod seek;

pub use probe::{ProbeData, ProbeScore};
pub use seek::{SeekFlags, SeekTarget};

/// One elementary stream in a container.
#[derive(Debug, Clone)]
pub struct Stream {
    pub index: u32,
    /// The container's own stream identifier — an MPEG-TS PID, a Matroska track
    /// number. Distinct from `index`, and addressable from the CLI as `#id`.
    pub id: Option<i64>,
    pub params: CodecParameters,
    /// The unit every timestamp on this stream is counted in.
    pub time_base: Rational,
    pub start_time: Timestamp,
    pub duration: Option<Duration>,
    pub frame_count: Option<u64>,
    pub disposition: Disposition,
    pub metadata: Vec<(String, String)>,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Disposition: u32 {
        const DEFAULT          = 1 << 0;
        const DUB              = 1 << 1;
        const ORIGINAL         = 1 << 2;
        const COMMENT          = 1 << 3;
        const LYRICS           = 1 << 4;
        const KARAOKE          = 1 << 5;
        const FORCED           = 1 << 6;
        const HEARING_IMPAIRED = 1 << 7;
        const VISUAL_IMPAIRED  = 1 << 8;
        const ATTACHED_PIC     = 1 << 9;
        const CAPTIONS         = 1 << 10;
        const DESCRIPTIONS     = 1 << 11;
        const METADATA         = 1 << 12;
        const DEPENDENT        = 1 << 13;
        const STILL_IMAGE      = 1 << 14;
    }
}

/// A named group of streams, as MPEG-TS programs and similar express.
#[derive(Debug, Clone)]
pub struct Program {
    pub id: i64,
    pub stream_indices: Vec<u32>,
    pub metadata: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub id: i64,
    pub time_base: Rational,
    pub start: Timestamp,
    pub end: Timestamp,
    pub metadata: Vec<(String, String)>,
}

/// Supplies bitstream parsers to a demuxer without the demuxer naming a codec
/// crate.
///
/// This is the seam that keeps the layering acyclic (D14.1): demuxers genuinely
/// need to parse elementary-stream headers to fill in [`CodecParameters`], but a
/// dependency edge from every container crate to every codec crate would make the
/// graph unmanageable. The registry implements this.
pub trait ParserProvider: Send + Sync {
    fn parser_for(&self, codec: CodecId) -> Option<Box<dyn Parser>>;
}

/// Read packets out of a container.
pub trait Demuxer: Send {
    fn streams(&self) -> &[Stream];

    fn programs(&self) -> &[Program] {
        &[]
    }

    fn chapters(&self) -> &[Chapter] {
        &[]
    }

    fn metadata(&self) -> &[(String, String)] {
        &[]
    }

    /// Read the next packet in storage order.
    ///
    /// # Errors
    /// [`vaco_core::Error::Eof`] at end of input;
    /// [`vaco_core::Error::InvalidData`] for a recoverable corruption.
    fn read_packet(&mut self) -> Result<Packet>;

    /// # Errors
    /// [`vaco_core::Error::NotSeekable`] when the source or format cannot seek.
    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()>;

    /// Duration of the longest stream, if the container states or implies one.
    fn duration(&self) -> Option<Duration> {
        None
    }
}

/// Write packets into a container.
pub trait Muxer: Send {
    /// Declare a stream. All streams must be added before [`Muxer::write_header`].
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] when this container cannot carry the codec.
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32>;

    /// # Errors
    /// Propagates I/O failure.
    fn write_header(&mut self) -> Result<()>;

    /// Write one packet. Packets must arrive in interleaved order; the caller
    /// (or `vaco-sched`) is responsible for that ordering.
    ///
    /// # Errors
    /// Propagates I/O failure.
    fn write_packet(&mut self, packet: &Packet) -> Result<()>;

    /// Finalise: indexes, trailing boxes, header rewrites.
    ///
    /// # Errors
    /// Propagates I/O failure.
    fn write_trailer(&mut self) -> Result<()>;
}

/// Static description of a container implementation.
#[derive(Debug, Clone, Copy)]
pub struct DemuxerDesc {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub mime_types: &'static [&'static str],
    /// Cheap content sniff, run before the source is fully opened.
    pub probe: fn(&ProbeData<'_>) -> ProbeScore,
    pub open: fn(Box<dyn MediaSource>, &dyn ParserProvider) -> Result<Box<dyn Demuxer>>,
}

#[derive(Debug, Clone, Copy)]
pub struct MuxerDesc {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub default_video: Option<CodecId>,
    pub default_audio: Option<CodecId>,
    pub open: fn(Box<dyn MediaSink>) -> Result<Box<dyn Muxer>>,
}

/// Media type of a stream, convenience re-export for callers matching on it.
pub use vaco_core::MediaType as StreamType;
const _: () = {
    let _ = MediaType::Video;
};
