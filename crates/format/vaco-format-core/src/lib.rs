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
//! | [`mux`] | [`MuxBuilder`]/[`MuxWriter`] — the muxer state machine |
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
pub mod mux;
pub mod options;
pub mod probe;
pub mod seek;
pub mod sidedata;
pub mod time;
pub mod vacoraw;

#[cfg(test)]
mod test_support;

pub use discovery::{Discovery, DiscoveryReport, NoParsers, StopReason};
pub use flags::FormatFlags;
pub use interleave::{
    ChunkPolicy, InterleaveQueue, MuxTimestamps, interleave_none, interleave_per_dts,
};
pub use mux::{
    BitstreamAction, BsfChain, BsfProvider, CodecSupport, MuxBuilder, MuxReport, MuxWriter, NoBsfs,
};
pub use options::{AvoidNegativeTs, FFlags, FormatOptions};
pub use probe::{Detected, Probe, ProbeData, ProbeScore};
pub use seek::{IndexEntry, IndexFlags, PacketIndex, SeekFlags, SeekStrategy, SeekTarget};
pub use sidedata::{StreamSideData, display_rotation, is_identity_matrix};
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
    /// Duration **in `time_base` ticks**, exactly as the container states it.
    ///
    /// Deliberately not a [`Duration`]. A `Duration` counts microseconds and
    /// cannot round-trip a media timescale: 25 500 ticks at 1/12800 is
    /// 1 992 187.5 µs, and `ffprobe` prints `duration_ts=25500`. Every demuxer
    /// in the workspace had to keep the tick count in a private side table to
    /// work around that, and none of them could hand it back through
    /// `dyn Demuxer`. Use [`Stream::duration`] for the microsecond view.
    pub duration_ts: Option<i64>,
    pub frame_count: Option<u64>,
    /// The lowest frame rate that represents every timestamp in the stream
    /// exactly — `ffprobe`'s `r_frame_rate`.
    ///
    /// A *pair* with [`Stream::avg_frame_rate`], because the two genuinely
    /// differ: a track whose `stts` holds mostly 60-tick deltas with a few
    /// 20-tick ones reports `r_frame_rate=10/1` and `avg_frame_rate=300/29` on
    /// the same file. They used to share `params.video.frame_rate`, which made
    /// that file unrepresentable.
    ///
    /// [`Rational::UNDEFINED`] (`0/1`… printed `0/0`) means "not stated"; the
    /// reference prints `0/0` rather than `N/A`, including for every audio
    /// stream, so there is no third state to model and this is not an
    /// `Option`.
    pub r_frame_rate: Rational,
    /// Frames over duration — `ffprobe`'s `avg_frame_rate`. See
    /// [`Stream::r_frame_rate`] for why they are two fields.
    pub avg_frame_rate: Rational,
    pub disposition: Disposition,
    pub metadata: Vec<(String, String)>,
    /// Side data describing the stream rather than any packet — today, the
    /// display matrix. See [`sidedata`] for why this is a list and not a
    /// field per kind.
    pub side_data: Vec<StreamSideData>,
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
            duration_ts: None,
            frame_count: None,
            r_frame_rate: Rational::UNDEFINED,
            avg_frame_rate: Rational::UNDEFINED,
            disposition: Disposition::empty(),
            metadata: Vec::new(),
            side_data: Vec::new(),
        }
    }

    /// [`Stream::duration_ts`] as an absolute duration, for cross-stream
    /// comparison and for the `duration` field `vaco-probe` prints in seconds.
    ///
    /// Lossy by construction — that is the whole reason `duration_ts` is
    /// stored and this is derived, rather than the other way round.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        Timestamp::new(self.duration_ts?).to_duration(self.time_base)
    }

    /// Record a duration stated in `time_base` ticks.
    ///
    /// A negative tick count is refused rather than clamped: no container
    /// states a negative length, so one means the arithmetic that produced it
    /// was wrong, and storing `None` keeps that visible as `N/A`.
    pub const fn set_duration_ts(&mut self, ticks: i64) {
        self.duration_ts = if ticks >= 0 { Some(ticks) } else { None };
    }

    /// The first display matrix on the stream, if it carries one.
    #[must_use]
    pub fn display_matrix(&self) -> Option<[i32; 9]> {
        self.side_data
            .iter()
            .map(|d| match *d {
                StreamSideData::DisplayMatrix(m) => m,
            })
            .next()
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

/// Re-exported from `vaco-core`, where it now lives.
///
/// This crate and `vaco-cli-core` each defined one, with the same nineteen
/// flags at the same bits. This crate's `from_cli_name` matched
/// case-*insensitively* and cli-core's `by_name` did not — so one duplication
/// was quietly two behaviours. Measured: the reference is case-sensitive, and
/// says so (`Undefined constant or missing '(' in 'DEFAULT'`), because it
/// resolves these through its expression evaluator's named-constant table.
///
/// `DISPOSITION_NAMES` is now [`Disposition::ALL`] and `from_cli_name` is
/// [`Disposition::by_name`]. See [`vaco_core::disposition`].
pub use vaco_core::Disposition;

/// A named group of streams, as MPEG-TS programs and similar express.
///
/// The four `Option` fields below are MPEG-TS specifics that plan 18 §1.1
/// specifies and that `vaco-probe -show_programs` prints. Before they existed
/// `vaco-demux-mpegts` put them in [`Program::metadata`], where they printed as
/// `TAG:pmt_pid=…` — the right values in the wrong section.
#[derive(Debug, Clone)]
pub struct Program {
    pub id: i64,
    /// MPEG-TS `program_number`. Distinct from [`Program::id`] in the model
    /// even though every container that sets both sets them equal, because the
    /// reference prints them as two fields and a caller cannot tell from `id`
    /// alone whether a container stated a program number at all.
    pub program_num: Option<i64>,
    /// PID of the PMT section that describes this program.
    pub pmt_pid: Option<u16>,
    /// PID carrying this program's PCR.
    pub pcr_pid: Option<u16>,
    /// `version_number` of the PMT last applied.
    ///
    /// **Not printed by `ffprobe 8.1`** — the brief that asked for it, and plan
    /// 18 §1.1, both say `-show_programs` prints it, and measurement says
    /// otherwise: `-of flat -show_optional_fields always -show_programs` emits
    /// `program_id`, `program_num`, `nb_streams`, `pmt_pid`, `pcr_pid` and the
    /// tags, and nothing else. It is kept because it is a genuine container
    /// statement a demuxer needs in order to notice a PMT change, not because
    /// anything prints it.
    pub pmt_version: Option<u8>,
    pub stream_indices: Vec<u32>,
    pub metadata: Vec<(String, String)>,
}

impl Program {
    /// An empty program with `id` and nothing else stated.
    #[must_use]
    pub const fn new(id: i64) -> Self {
        Self {
            id,
            program_num: None,
            pmt_pid: None,
            pcr_pid: None,
            pmt_version: None,
            stream_indices: Vec::new(),
            metadata: Vec::new(),
        }
    }
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

/// So a boxed demuxer is itself a [`Demuxer`].
///
/// `DemuxerDesc::open` returns a trait object and [`Discovery::new`] takes
/// `D: Demuxer` by value, so without this there is no way to compose the two —
/// which is exactly what `vaco-probe` hit, and worked around with a
/// hand-written seven-method newtype.
///
/// `vaco-codec-core` has had the equivalent `impl Parser for Box<P>` since the
/// wave before, and `Discovery::refine` depends on it. The absence here was an
/// inconsistency between sibling trait layers rather than a deliberate
/// restriction.
impl<D: Demuxer + ?Sized> Demuxer for Box<D> {
    fn streams(&self) -> &[Stream] {
        (**self).streams()
    }
    fn programs(&self) -> &[Program] {
        (**self).programs()
    }
    fn chapters(&self) -> &[Chapter] {
        (**self).chapters()
    }
    fn metadata(&self) -> &[(String, String)] {
        (**self).metadata()
    }
    fn read_packet(&mut self) -> Result<Packet> {
        (**self).read_packet()
    }
    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        (**self).seek(target, flags)
    }
    fn duration(&self) -> Option<Duration> {
        (**self).duration()
    }
}

/// Write packets into a container.
pub trait Muxer: Send {
    /// This container's own flags.
    ///
    /// Not a caller preference: they decide whether `avoid_negative_ts`
    /// resolves to shifting, whether DTS must strictly increase, and whether
    /// the container stores timestamps at all. Only the muxer knows.
    ///
    /// **The default is the strictest reading**, and deliberately so — an
    /// absent [`FormatFlags::TS_NONSTRICT`] *means* strictly increasing DTS, so
    /// a muxer that forgets to answer gets the conservative behaviour rather
    /// than a permissive one. Override it; the default is a safety net, not a
    /// sensible value.
    fn flags(&self) -> FormatFlags {
        FormatFlags::empty()
    }

    /// Declare a stream. All streams must be added before [`Muxer::write_header`].
    ///
    /// Prefer driving this through [`mux::MuxBuilder`], which makes the
    /// ordering a property of the type rather than of the caller's discipline.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] when this container cannot carry the codec.
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32>;

    /// Called once after every stream is declared and before the header.
    ///
    /// The place to settle anything that depends on the whole stream set: a
    /// timescale derived from the frame rates present, a track order, a
    /// container profile. [`Muxer::stream_time_base`] is read *after* this, so
    /// a muxer may rewrite what it will accept here (M12).
    ///
    /// # Errors
    /// Whatever the container's own consistency checks find.
    fn init(&mut self) -> Result<()> {
        Ok(())
    }

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

    /// This container's interleaving policy (§1.9 N7).
    ///
    /// The default is per-DTS. MOV in fragmented mode interleaves within a
    /// fragment; MPEG-TS does not interleave in the queue sense at all and
    /// wants [`interleave::interleave_none`], because it multiplexes at the
    /// 188-byte level against a PCR clock and anything the queue reordered
    /// first would only be reordered again.
    ///
    /// # Errors
    /// As [`interleave::InterleaveQueue::push`].
    fn interleave(
        &mut self,
        queue: &mut InterleaveQueue,
        packet: Option<Packet>,
        flush: bool,
    ) -> Result<Option<Packet>> {
        interleave::interleave_per_dts(queue, packet, flush)
    }

    /// Whether this stream's bitstream form needs converting first (§1.10).
    ///
    /// Asked on the stream's **first** packet, then again on each inserted
    /// filter's notional output until it answers
    /// [`mux::BitstreamAction::Keep`], to a depth of
    /// [`mux::MAX_BSF_DEPTH`]. The answer is cached for the rest of the file
    /// (B3): a stream that switches form mid-file is deliberately not
    /// re-examined, which is what `avc3`/`hev1` sample entries exist for.
    ///
    /// [`mux::global_header_action`] is the answer a
    /// [`FormatFlags::GLOBALHEADER`] container wants when it has no more
    /// specific opinion.
    ///
    /// # Errors
    /// When the packet is in a form this container cannot carry at all.
    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        packet: &Packet,
    ) -> Result<mux::BitstreamAction> {
        let _ = (params, packet);
        Ok(mux::BitstreamAction::Keep)
    }

    /// Whether this container carries `codec`, at compliance level `strict`.
    ///
    /// Consulted by [`mux::MuxBuilder::add_stream`] *before* the muxer is asked
    /// to do anything, so an impossible combination fails at the point the user
    /// can still act on it rather than three seconds into a transcode.
    ///
    /// Deliberately `&self` and object-safe, unlike plan 18 §1.3's
    /// `fn query_codec(codec, strict) -> CodecSupport where Self: Sized`: a
    /// `where Self: Sized` method cannot be called through `dyn Muxer`, and
    /// every caller in this workspace holds one.
    fn query_codec(&self, codec: CodecId, strict: i32) -> mux::CodecSupport {
        let _ = (codec, strict);
        mux::CodecSupport::Supported
    }

    /// Flush whatever is buffered internally, without ending the file (M20).
    ///
    /// Only called on a muxer declaring [`FormatFlags::ALLOW_FLUSH`]. Plan 18
    /// §1.3 spells this as `write_packet(None)`; a separate method says the
    /// same thing without changing `write_packet`'s signature, which matters
    /// because implementations of it are being written in parallel with this.
    ///
    /// # Errors
    /// Propagates I/O failure.
    fn write_flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// So a boxed muxer is itself a [`Muxer`].
///
/// The mirror of the [`Demuxer`] impl above, and needed for the same reason:
/// [`mux::MuxBuilder`] owns a `Box<dyn Muxer>`, and a caller wanting to wrap
/// one (a `tee`, a segmenter, a counting shim) otherwise cannot.
impl<M: Muxer + ?Sized> Muxer for Box<M> {
    fn flags(&self) -> FormatFlags {
        (**self).flags()
    }
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        (**self).add_stream(params)
    }
    fn init(&mut self) -> Result<()> {
        (**self).init()
    }
    fn write_header(&mut self) -> Result<()> {
        (**self).write_header()
    }
    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        (**self).write_packet(packet)
    }
    fn write_trailer(&mut self) -> Result<()> {
        (**self).write_trailer()
    }
    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        (**self).stream_time_base(stream_index)
    }
    fn interleave(
        &mut self,
        queue: &mut InterleaveQueue,
        packet: Option<Packet>,
        flush: bool,
    ) -> Result<Option<Packet>> {
        (**self).interleave(queue, packet, flush)
    }
    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        packet: &Packet,
    ) -> Result<mux::BitstreamAction> {
        (**self).check_bitstream(params, packet)
    }
    fn query_codec(&self, codec: CodecId, strict: i32) -> mux::CodecSupport {
        (**self).query_codec(codec, strict)
    }
    fn write_flush(&mut self) -> Result<()> {
        (**self).write_flush()
    }
}

/// Static description of a container implementation.
#[derive(Debug, Clone, Copy)]
pub struct DemuxerDesc {
    pub name: &'static str,
    pub long_name: &'static str,
    pub extensions: &'static [&'static str],
    pub mime_types: &'static [&'static str],
    /// Behavioural flags for this container, so a caller composing
    /// [`Discovery`] can reach them **through the registry**.
    ///
    /// Without this a composer has to name each demuxer crate's public `FLAGS`
    /// const, which puts a dependency edge on every container and defeats the
    /// point of a registry. `vaco-probe` hit exactly that and had to keep a
    /// name-keyed transcription guarded by a test.
    ///
    /// Getting a flag wrong is not free: `TS_DISCONT` *suppresses* the
    /// monotonic-DTS repair, so guessing it absent would silently rewrite a
    /// genuine discontinuity. That is why this is a field and not a default.
    pub flags: FormatFlags,
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
        for &(flag, name) in Disposition::ALL {
            assert_eq!(Disposition::by_name(name), Some(flag));
        }
        assert_eq!(Disposition::by_name("nonesuch"), None);
    }

    #[test]
    fn disposition_fields_are_in_output_order() {
        let d = Disposition::DEFAULT | Disposition::FORCED;
        let set: Vec<&str> = d.fields().filter(|&(_, on)| on).map(|(n, _)| n).collect();
        assert_eq!(set, vec!["default", "forced"]);
        assert_eq!(d.fields().count(), Disposition::ALL.len());
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
