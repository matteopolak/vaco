//! The container framework: what a demuxer and muxer are, plus the probing,
//! timestamp, seeking and interleaving models they share.
//!
//! Depends on `vaco-codec-core` for [`CodecParameters`] (D14.1), but never on a
//! concrete codec: bitstream parsers arrive through the injected
//! [`ParserProvider`], so no format crate depends on a codec crate.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`probe`] | [`ProbeData`], [`ProbeScore`] and the score-based detection engine |
//! | [`options`] | [`FormatOptions`] — the generic format-level option table |
//! | [`flags`] | [`FormatFlags`] — what a container declares it can do |
//! | [`time`] | wraparound, timestamp generation and repair, duration estimation |
//! | [`seek`] | [`SeekTarget`], [`PacketIndex`] and the two generic strategies |
//! | [`discovery`] | [`Discovery`] — the bounded, replayable stream-discovery pass |
//! | [`interleave`] | [`InterleaveQueue`] and the muxer-side timestamp chain |
//! | [`vacoraw`] | a worked-example container that drives every one of the above |
//!
//! # The one idea worth reading first
//!
//! **The core does not own the demuxer.** [`Demuxer`] is a self-contained
//! object: it holds its own I/O, reads its own packets, performs its own seeks.
//! Everything generic in this crate is therefore a *library the demuxer calls*
//! or a *wrapper the caller composes*, never a driver that reaches into it.
//!
//! That is the opposite of the arrangement `planning/18-formats.md` §1.2
//! sketched, where a `DemuxCtx` owns the I/O and the demuxer is a set of
//! callbacks. It follows from the frozen trait, and it turns out to be the
//! better shape: [`Discovery`] can be applied or not applied, tested against a
//! mock, and stacked, without any demuxer knowing it exists.
//!
//! ```
//! use vaco_codec_core::{CodecId, CodecParameters};
//! use vaco_core::Timestamp;
//! use vaco_format_core::discovery::NoParsers;
//! use vaco_format_core::vacoraw::{MemorySink, VacoRawDemuxer, VacoRawMuxer};
//! use vaco_format_core::{Demuxer, FormatOptions, Muxer};
//! use vaco_io::MemorySource;
//! use vaco_limits::{Budget, Limits};
//! use vaco_packet::{Packet, PacketFlags};
//!
//! // Mux one keyframe into an in-memory file...
//! let opts = FormatOptions::default();
//! let sink = MemorySink::new();
//! let written = sink.shared();
//! let mut mux = VacoRawMuxer::new(Box::new(sink), &opts)?;
//! let idx = mux.add_stream(&CodecParameters::video().with_codec(CodecId::H264))?;
//! mux.write_header()?;
//!
//! let mut budget = Budget::new(Limits::strict());
//! let mut pkt = Packet::from_slice(&mut budget, b"payload")?;
//! pkt.stream_index = idx;
//! pkt.pts = Timestamp::ZERO;
//! pkt.dts = Timestamp::ZERO;
//! pkt.flags = PacketFlags::KEY;
//! mux.write_packet(&pkt)?;
//! mux.write_trailer()?;
//!
//! // ...and read it back out.
//! let src = Box::new(MemorySource::new(written.snapshot()));
//! let mut demux = VacoRawDemuxer::open(src, &NoParsers, &opts)?;
//! assert_eq!(demux.streams().len(), 1);
//! assert_eq!(demux.read_packet()?.payload(), b"payload");
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

use vaco_codec_core::{CodecId, CodecParameters, Parser};
use vaco_core::{Duration, MediaType, Rational, Result, Timestamp};
use vaco_io::{MediaSink, MediaSource};
use vaco_packet::Packet;

pub mod discovery;
pub mod flags;
pub mod interleave;
pub mod options;
pub mod probe;
pub mod seek;
pub mod time;
pub mod vacoraw;

#[cfg(test)]
mod test_support;

pub use discovery::{Discovery, DiscoveryReport, NoParsers, StopReason};
pub use flags::FormatFlags;
pub use interleave::{ChunkPolicy, InterleaveQueue, MuxTimestamps, interleave_per_dts};
pub use options::{AvoidNegativeTs, FFlags, FormatOptions};
pub use probe::{Detected, Probe, ProbeData, ProbeScore};
pub use seek::{IndexEntry, IndexFlags, PacketIndex, SeekFlags, SeekStrategy, SeekTarget};
pub use time::{DurationEstimate, DurationSource, TimestampFixer, WrapState};

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

impl Stream {
    /// An empty stream of `media_type` with `time_base`.
    #[must_use]
    pub fn new(index: u32, media_type: MediaType, time_base: Rational) -> Self {
        Self {
            index,
            id: None,
            params: CodecParameters::new(media_type),
            time_base,
            start_time: Timestamp::NONE,
            duration: None,
            frame_count: None,
            disposition: Disposition::empty(),
            metadata: Vec::new(),
        }
    }

    /// The stream's media type, falling back to what its codec implies.
    #[must_use]
    pub fn media_type(&self) -> Option<MediaType> {
        self.params.effective_media_type()
    }

    /// `start_time` as an absolute duration, for cross-stream comparison.
    #[must_use]
    pub fn start_time_absolute(&self) -> Option<Duration> {
        self.start_time.to_duration(self.time_base)
    }

    /// The first metadata value under `key`, matched case-insensitively.
    ///
    /// A `Vec` rather than a map because iteration order *is* output order and
    /// D6 requires it to be deterministic; container metadata is also
    /// duplicate-preserving, which a map cannot express.
    #[must_use]
    pub fn metadata_get(&self, key: &str) -> Option<&str> {
        self.metadata
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// Set `key`, replacing the first existing entry in place so that
    /// insertion order survives.
    pub fn metadata_set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(slot) = self
            .metadata
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
        {
            slot.1 = value;
        } else {
            self.metadata.push((key.to_owned(), value));
        }
    }

    /// Whether this stream is a cover image rather than a timeline.
    ///
    /// Excluded from `start_time` derivation and from seek reference-stream
    /// selection, because it has no position on any timeline.
    #[must_use]
    pub const fn is_attached_pic(&self) -> bool {
        self.disposition.contains(Disposition::ATTACHED_PIC)
    }
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
        const CLEAN_EFFECTS    = 1 << 9;
        const ATTACHED_PIC     = 1 << 10;
        const TIMED_THUMBNAILS = 1 << 11;
        const NON_DIEGETIC     = 1 << 12;
        const CAPTIONS         = 1 << 13;
        const DESCRIPTIONS     = 1 << 14;
        const METADATA         = 1 << 15;
        const DEPENDENT        = 1 << 16;
        const STILL_IMAGE      = 1 << 17;
        const MULTILAYER       = 1 << 18;
    }
}

/// The name each disposition flag prints under.
///
/// `vaco-probe`'s DISPOSITION section prints one field per flag, so these names
/// are interface facts (D9) and the order is output order. All nineteen were
/// read straight out of `ffprobe -show_streams` on a real file, in the order it
/// prints them; four were missing here and the numbering diverged from bit 9,
/// which `vaco-probe`'s author found while making the section byte-identical.
///
/// The bit positions match `vaco_cli_core::Disposition`'s deliberately. **There
/// are two `Disposition` types in this workspace and there should be one** —
/// `vaco-cli-core` needs it for `-disposition:s:0` and does not depend on this
/// crate. They are aligned numerically so nothing is wrong today; deduplicating
/// them is a layering decision, not a rename, because the shared home would have
/// to sit below both.
pub const DISPOSITION_NAMES: &[(Disposition, &str)] = &[
    (Disposition::DEFAULT, "default"),
    (Disposition::DUB, "dub"),
    (Disposition::ORIGINAL, "original"),
    (Disposition::COMMENT, "comment"),
    (Disposition::LYRICS, "lyrics"),
    (Disposition::KARAOKE, "karaoke"),
    (Disposition::FORCED, "forced"),
    (Disposition::HEARING_IMPAIRED, "hearing_impaired"),
    (Disposition::VISUAL_IMPAIRED, "visual_impaired"),
    (Disposition::CLEAN_EFFECTS, "clean_effects"),
    (Disposition::ATTACHED_PIC, "attached_pic"),
    (Disposition::TIMED_THUMBNAILS, "timed_thumbnails"),
    (Disposition::NON_DIEGETIC, "non_diegetic"),
    (Disposition::CAPTIONS, "captions"),
    (Disposition::DESCRIPTIONS, "descriptions"),
    (Disposition::METADATA, "metadata"),
    (Disposition::DEPENDENT, "dependent"),
    (Disposition::STILL_IMAGE, "still_image"),
    (Disposition::MULTILAYER, "multilayer"),
];

impl Disposition {
    /// Resolve one flag by the name `vaco-probe` prints.
    ///
    /// Named `from_cli_name` rather than `from_name` because `bitflags`
    /// generates a `from_name` of its own that matches the *constant* spelling
    /// (`"ATTACHED_PIC"`); this one matches what the tool prints
    /// (`"attached_pic"`). `vaco_codec_core::Caps` resolves the same collision
    /// the same way.
    #[must_use]
    pub fn from_cli_name(name: &str) -> Option<Self> {
        DISPOSITION_NAMES
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|&(d, _)| d)
    }

    /// Every flag paired with whether it is set, in output order.
    pub fn fields(self) -> impl Iterator<Item = (&'static str, bool)> {
        DISPOSITION_NAMES
            .iter()
            .map(move |&(d, n)| (n, self.contains(d)))
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

impl Chapter {
    /// The chapter's length, when both ends are known.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        let (start, end) = (
            self.start.to_duration(self.time_base)?,
            self.end.to_duration(self.time_base)?,
        );
        Some(Duration::from_micros(
            end.as_micros().saturating_sub(start.as_micros()),
        ))
    }
}

/// Supplies bitstream parsers to a demuxer without the demuxer naming a codec
/// crate.
///
/// This is the seam that keeps the layering acyclic (D14.1): demuxers genuinely
/// need to parse elementary-stream headers to fill in [`CodecParameters`], but a
/// dependency edge from every container crate to every codec crate would make the
/// graph unmanageable. The registry implements this.
///
/// [`NoParsers`] is the default implementation and the one every demuxer unit
/// test and fuzz target uses, which keeps demuxer fuzzing fast and independent
/// of codec code.
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

    /// The time base this muxer chose for `stream_index`, or `None` if it does
    /// not have that stream.
    ///
    /// **Added after the interface freeze, with the orchestrator's approval.**
    /// [`Muxer::add_stream`] takes only [`CodecParameters`], and the muxer —
    /// not the caller — decides what the container can express: MP4 wants the
    /// media timescale, MPEG-TS is fixed at 1/90000, Matroska derives one from
    /// `TimestampScale`. But step M1 of the muxer-side chain
    /// ([`crate::interleave::MuxTimestamps::apply`]) rescales every packet
    /// *into* that base, so a caller holding a `dyn Muxer` that cannot ask what
    /// it is cannot use the interface correctly at all. That is not churn, it
    /// is a signature that does not work; see `docs/format/vaco-format-core.md`.
    ///
    /// The default returns `None`, meaning "ask me later or assume
    /// [`crate::time::TIME_BASE_Q`]" — so no existing implementation breaks,
    /// and a muxer that genuinely has no opinion need not pretend to.
    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        let _ = stream_index;
        None
    }
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

impl DemuxerDesc {
    /// Whether this descriptor answers to `name`.
    ///
    /// A descriptor's `name` may be a comma-separated family — the reference
    /// reports `"mov,mp4,m4a,3gp,3g2,mj2"` as one format name — and `-f mp4`
    /// has to select it, so every element is a valid spelling.
    #[must_use]
    pub fn matches_name(&self, name: &str) -> bool {
        self.name == name || self.name.split(',').any(|n| n == name)
    }

    /// Whether `filename`'s extension is one this container claims.
    #[must_use]
    pub fn matches_extension(&self, filename: &str) -> bool {
        ProbeData::new(&[])
            .with_filename(filename)
            .extension_matches(self.extensions)
    }
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

impl MuxerDesc {
    /// The codec this container writes by default for `media`, if it has one.
    #[must_use]
    pub const fn default_codec(&self, media: MediaType) -> Option<CodecId> {
        match media {
            MediaType::Video => self.default_video,
            MediaType::Audio => self.default_audio,
            _ => None,
        }
    }

    /// Whether this descriptor answers to `name`.
    #[must_use]
    pub fn matches_name(&self, name: &str) -> bool {
        self.name == name || self.name.split(',').any(|n| n == name)
    }
}

/// Media type of a stream, convenience re-export for callers matching on it.
pub use vaco_core::MediaType as StreamType;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::field_reassign_with_default,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::test_support::{DESC_A, DESC_B};

    #[test]
    fn descriptor_names_accept_family_members() {
        const FAMILY: DemuxerDesc = DemuxerDesc {
            name: "mov,mp4,m4a,3gp,3g2,mj2",
            ..DESC_A
        };
        assert!(FAMILY.matches_name("mov,mp4,m4a,3gp,3g2,mj2"));
        assert!(FAMILY.matches_name("mp4"));
        assert!(FAMILY.matches_name("mj2"));
        assert!(!FAMILY.matches_name("mp3"));
        assert!(DESC_B.matches_name("fmt-b"));
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(DESC_A.matches_extension("/tmp/x.FA"));
        assert!(!DESC_A.matches_extension("/tmp/x.fb"));
        assert!(!DESC_A.matches_extension("/tmp/x"));
    }

    #[test]
    fn disposition_names_round_trip() {
        for &(flag, name) in DISPOSITION_NAMES {
            assert_eq!(Disposition::from_cli_name(name), Some(flag));
        }
        assert_eq!(Disposition::from_cli_name("nonesuch"), None);
        let named = DISPOSITION_NAMES
            .iter()
            .fold(Disposition::empty(), |a, &(f, _)| a.union(f));
        assert_eq!(named, Disposition::all());
    }

    #[test]
    fn disposition_fields_are_in_output_order() {
        let d = Disposition::DEFAULT | Disposition::FORCED;
        let set: Vec<&str> = d.fields().filter(|&(_, on)| on).map(|(n, _)| n).collect();
        assert_eq!(set, vec!["default", "forced"]);
        assert_eq!(d.fields().count(), DISPOSITION_NAMES.len());
    }

    #[test]
    fn stream_metadata_preserves_insertion_order() {
        let mut s = Stream::new(0, MediaType::Video, Rational::new(1, 1000));
        s.metadata_set("title", "a");
        s.metadata_set("language", "eng");
        s.metadata_set("TITLE", "b");
        assert_eq!(s.metadata_get("Title"), Some("b"));
        let keys: Vec<&str> = s.metadata.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["title", "language"]);
    }

    #[test]
    fn chapter_duration_needs_both_ends() {
        let tb = Rational::new(1, 1000);
        let c = Chapter {
            id: 1,
            time_base: tb,
            start: Timestamp::new(1000),
            end: Timestamp::new(3000),
            metadata: Vec::new(),
        };
        assert_eq!(c.duration().unwrap().as_micros(), 2_000_000);
        let open = Chapter {
            end: Timestamp::NONE,
            ..c
        };
        assert_eq!(open.duration(), None);
    }

    #[test]
    fn muxer_default_codec_is_per_media_type() {
        let d = vacoraw::MUXER;
        assert_eq!(d.default_codec(MediaType::Video), Some(CodecId::H264));
        assert_eq!(d.default_codec(MediaType::Audio), Some(CodecId::Opus));
        assert_eq!(d.default_codec(MediaType::Subtitle), None);
    }
}
