//! The container framework: what a demuxer and muxer are, plus the probing,
//! timestamp, seeking and interleaving models they share. Depends on
//! `vaco-codec-core` for [`CodecParameters`] (D14.1) but never a concrete
//! codec — parsers arrive through the injected [`ParserProvider`]. See
//! [`Demuxer`] for the core design idea.
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
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_io::{MediaSink, MediaSource};
use vaco_limits::Limits;
use vaco_packet::Packet;

pub mod discovery;
pub mod flags;
pub mod interleave;
pub mod metadata;
pub mod mux;
pub mod options;
pub mod probe;
pub mod seek;
pub mod sidedata;
pub mod stream_group;
pub mod time;
pub mod vacoraw;

#[cfg(test)]
mod test_support;

pub use discovery::{Discovery, DiscoveryReport, NoParsers, StopReason};
pub use flags::FormatFlags;
pub use interleave::{
    ChunkPolicy, InterleaveQueue, MuxTimestamps, interleave_none, interleave_per_dts,
};
pub use metadata::{MuxAttachment, MuxMetadata};
pub use mux::{
    BitstreamAction, BsfChain, BsfProvider, CodecSupport, MuxBuilder, MuxReport, MuxWriter, NoBsfs,
};
pub use options::{AvoidNegativeTs, FFlags, FormatOptions};
pub use probe::{Detected, Probe, ProbeData, ProbeScore};
pub use seek::{IndexEntry, IndexFlags, PacketIndex, SeekFlags, SeekStrategy, SeekTarget};
pub use sidedata::{
    DisplayTransform, StreamSideData, dihedral_transform_from_angle_and_flips,
    dihedral_transform_from_matrix, display_rotation, is_identity_matrix,
};
pub use stream_group::{StreamGroup, StreamGroupIndex, StreamGroupKind, TileGrid};
pub use time::{DurationEstimate, DurationSource, TimestampFixer, WrapState};

/// Which of the two things an absent [`Stream::start_time`] means.
///
/// `Timestamp::NONE` in that field is overloaded. It reads as *"no demuxer has
/// answered yet"*, which is what lets [`crate::Discovery`] derive a start time
/// from the first packet, and it has to also express *"this container's answer
/// is that the stream has none"* — and those want opposite behaviour from the
/// same value.
///
/// Measured against ffmpeg 9.0.1 on a single still image, which is where the
/// two come apart: `ffprobe` reports `start_pts=N/A` and `start_time=N/A` for
/// the stream while the packet it hands out carries `pts=0`, `dts=0`,
/// `duration=1`. Stream metadata and packet timestamps are independent there,
/// so a demuxer cannot express the reference's answer by dropping the packet's
/// timestamps — and before this existed, `vaco-demux-image2` did exactly that
/// and no still image could be transcoded at all.
///
/// This qualifies only the *absence*: a `start_time` that holds a value is
/// self-describing and this field says nothing about it, so the two cannot
/// disagree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StartTimeAbsence {
    /// Nobody has answered. Discovery derives a start time from the first
    /// packet, falling back to the container's own.
    #[default]
    Underived,
    /// The container's own answer, and final: this stream has no start time,
    /// however its packets are timestamped.
    Stated,
}

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
    /// Which of the two things an absent [`Stream::start_time`] means — see
    /// [`StartTimeAbsence`]. [`Stream::start_time_underived`] is the question
    /// callers actually ask; [`Stream::state_no_start_time`] is how a demuxer
    /// answers it.
    pub start_time_absence: StartTimeAbsence,
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
            start_time_absence: StartTimeAbsence::Underived,
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

    /// Whether anything may still fill in this stream's `start_time`.
    ///
    /// The single test both of [`crate::Discovery`]'s derivation steps ask, so
    /// that "absent because unanswered" and "absent because the container says
    /// so" cannot drift apart between them.
    #[must_use]
    pub fn start_time_underived(&self) -> bool {
        self.start_time.is_none() && self.start_time_absence == StartTimeAbsence::Underived
    }

    /// State that this stream genuinely has no start time, so nothing derives
    /// one from its packets or from the container.
    ///
    /// For a container whose packets are timestamped but whose *stream* the
    /// reference reports as having no start — a single still image, or any
    /// `*_pipe` image splitter. See [`StartTimeAbsence`] for the measurement.
    pub fn state_no_start_time(&mut self) {
        self.start_time = Timestamp::NONE;
        self.start_time_absence = StartTimeAbsence::Stated;
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
///
/// **The core does not own the demuxer.** A `Demuxer` is a self-contained
/// object: it holds its own I/O, reads its own packets, performs its own
/// seeks. Everything generic in this crate is therefore a *library the
/// demuxer calls* or a *wrapper the caller composes* ([`Discovery`], for
/// instance, can be applied or not, tested against a mock, and stacked),
/// never a driver that reaches into it.
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

    /// Groups of streams that together form one logical unit — a HEIF/AVIF
    /// tile grid's tiles, say. Empty for every container that has no such
    /// notion, which is nearly all of them.
    fn stream_groups(&self) -> &[StreamGroup] {
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

    /// Rebind this demuxer to a caller's [`Limits`] and [`FormatOptions`],
    /// after construction.
    ///
    /// **Why this exists instead of a parameter on [`DemuxerDesc::open`].**
    /// `open` is a bare `fn` pointer, and every one of the ~90 registered
    /// demuxers already supplies its own free function of that exact
    /// signature; a function item only coerces to a function-pointer type
    /// with a matching parameter list, so widening `open`'s signature would
    /// require editing every one of those functions, not just the descriptor
    /// literals that reference them. That is the edit this wave's brief
    /// forbids, so the seam has to be a call a caller makes *after*
    /// construction instead of a parameter *to* it.
    ///
    /// [`Discovery::run`] calls this once, with its own configured `limits`
    /// and `opts`, before reading anything — so wrapping a demuxer in
    /// [`Discovery`] is enough to reach it. A fuzz target driving
    /// `(desc.open)(..)` directly, with no [`Discovery`] in between, can and
    /// should call it too: that is precisely the "cannot bound a demuxer it
    /// cannot hand a budget to" case the gap names.
    ///
    /// **What this does not fix.** It cannot bound allocation that already
    /// happened *during* `open` itself — a header, an index, anything a
    /// container reads eagerly before any `Demuxer` exists to call this on.
    /// Every demuxer in this workspace does that parsing with a budget it
    /// invents internally (see `vacoraw::VacoRawDemuxer::open`'s
    /// `Budget::new(Limits::permissive())`, which this method cannot reach).
    /// Closing that half needs the `open`-signature change this method is
    /// explicitly the substitute for, and that is only possible in a wave that
    /// touches every implementor at once. Recorded as a known limit rather
    /// than papered over; see `docs/format/vaco-format-core.md`.
    ///
    /// The default does nothing, which is exactly today's behaviour: every
    /// demuxer that predates this method already ignores whatever budget or
    /// options a caller wants, because there was nowhere to tell it. A
    /// demuxer that opts in re-derives its internal `Budget` and any
    /// option-driven state from `limits`/`opts`.
    ///
    /// # Errors
    /// Whatever the demuxer's own validation of `opts` finds. The default
    /// never errs.
    fn reconfigure(&mut self, limits: &Limits, opts: &FormatOptions) -> Result<()> {
        let _ = (limits, opts);
        Ok(())
    }

    /// Rebind this demuxer to the URL it was opened from, for a format whose
    /// real unit of demuxing needs more filesystem access than the one
    /// [`Box<dyn MediaSource>`] `open` received — a filename *pattern* that
    /// expands to many files (`image2`'s `img_%03d.png`), or a sidecar file
    /// whose name is a convention relative to this one (`VobSub`'s `.sub` next
    /// to its `.idx`).
    ///
    /// **Why this exists instead of a parameter on [`DemuxerDesc::open`], or
    /// a second [`MediaSource`] the caller opens itself.** `open` is a bare
    /// `fn` pointer roughly 90 registered demuxers already implement at a
    /// fixed `(Box<dyn MediaSource>, &dyn ParserProvider)` signature, so
    /// widening it would touch every one of them. A caller-opened second
    /// source does not fit either: there is no `MediaSource::path()`, so
    /// nothing downstream of the protocol layer can name a sidecar or a
    /// pattern's other members without the URL string itself — which the
    /// caller already holds, since it is what resolved to this demuxer in
    /// the first place.
    ///
    /// A caller may call this once, immediately after `open` returns and
    /// before reading anything, so a demuxer that needs more than the source
    /// it was constructed with can re-derive its real state from the URL —
    /// typically by replacing itself outright
    /// (`*self = Self::open_pattern(url, ..)?`). That is exactly what a
    /// demuxer whose primary `open` call could never have succeeded needs:
    /// the caller passes a throwaway placeholder source to `open` (a pattern
    /// like `img_%03d.png` is not itself an openable file) and this method
    /// does the real work once the real URL is known.
    ///
    /// The default returns [`Error::Unsupported`], matching every demuxer's
    /// actual behaviour before this method existed: none of them could ever
    /// see their own URL, so refusing is not a behaviour change, only an
    /// explicit answer instead of a capability with nowhere to express
    /// itself.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when this demuxer needs nothing beyond the
    /// source `open` already received (the default). Otherwise whatever
    /// resolving `url` finds — no file matches a pattern, a sidecar file is
    /// missing.
    fn bind_url(&mut self, url: &str) -> Result<()> {
        let _ = url;
        Err(Error::Unsupported(
            "this demuxer reads from the source it was opened with; it has no separate URL to bind",
        ))
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
    fn stream_groups(&self) -> &[StreamGroup] {
        (**self).stream_groups()
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
    fn reconfigure(&mut self, limits: &Limits, opts: &FormatOptions) -> Result<()> {
        (**self).reconfigure(limits, opts)
    }
    // Forwarded explicitly, not inherited from the default — same trap as
    // `impl Muxer for Box<M>`'s `add_stream_with`: the default would call
    // nothing on the boxed value and always answer `Unsupported`, silently
    // hiding whatever the concrete type underneath overrides.
    fn bind_url(&mut self, url: &str) -> Result<()> {
        (**self).bind_url(url)
    }
}

/// What [`Muxer::add_stream_with`] knows about a stream beyond
/// [`CodecParameters`].
///
/// Deliberately minimal: `time_base` and `display_matrix` are populated
/// today, because each has a caller and a measured need (`framecrc`'s `#tb`,
/// see [`Muxer::add_stream_with`]'s docs; `display_matrix`, `vaco-mux-mp4`'s
/// `tkhd`, interface gap 22c's muxer half). Disposition flags and program
/// membership are the other two facts `-disposition`/`-program` parse but
/// have nowhere to land — the same gap, not yet closed — and are named here
/// rather than invented speculatively (D19): a field with no reader is a
/// guess about a shape nobody has measured yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamSpec {
    /// The time base packets for this stream arrive in, when the caller
    /// knows a better answer than [`CodecParameters`] alone implies —
    /// typically the input stream's own base, for a stream-copy output.
    pub time_base: Option<Rational>,
    /// The display transformation matrix this stream's *container output*
    /// should carry, row-major, in the same fixed-point encoding
    /// [`StreamSideData::DisplayMatrix`] uses. `None` means "the muxer's own
    /// default" (identity, for every muxer in this workspace today).
    ///
    /// This is deliberately about the *output* container, not a copy of
    /// whatever the input said: a caller that already baked the rotation
    /// into the pixels (decoded, rotated, re-encoded) must pass `None`
    /// here, or a player applies the same rotation twice. A caller that did
    /// *not* touch the pixels — a `-c copy` remux, or a transcode with
    /// rotation deliberately suppressed — passes the source's own matrix
    /// through unchanged, so the file still plays correctly. Getting this
    /// backwards is silent both ways: a dropped matrix looks like a
    /// forgotten rotation, and a doubled one looks like a correct file that
    /// happens to be upside down.
    pub display_matrix: Option<[i32; 9]>,
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

    /// [`Muxer::add_stream`], plus whatever [`StreamSpec`] carries beyond
    /// [`CodecParameters`].
    ///
    /// # Why this exists
    ///
    /// `CodecParameters` states codec facts — dimensions, sample rate, a
    /// frame rate — never the time base packets for this stream will actually
    /// arrive in. That is invisible for a muxer that fixes its own scale
    /// (MPEG-TS's 1/90000, MP4's media timescale) or that never has to print
    /// one, but `vaco-mux-hash`'s `framecrc`/`framemd5`/`framehash` print a
    /// `#tb` line and rescale every packet's duration back into it for
    /// display — for those, "derive `1/frame_rate` from `CodecParameters`"
    /// (the only thing `add_stream` alone allows) is a guess that happens to
    /// match the reference only for freshly encoded raw/PCM media, and is
    /// simply wrong for stream copy, where the reference keeps the *input's*
    /// base (measured: `1/12800` for one MP4, `1/90000` for one MPEG-TS,
    /// against a naive `1/frame_rate` of `1/25` and `1/50`).
    ///
    /// # Why a new method rather than widening `add_stream`
    ///
    /// `add_stream` is implemented by roughly 60 containers today. Changing
    /// its signature would touch every one of them for a fact only a handful
    /// need. The default below forwards to `add_stream`, so an implementor
    /// that has no opinion needs no change at all; only a muxer that actually
    /// wants `spec` overrides this method instead.
    ///
    /// # The `Box<dyn Muxer>` trap
    ///
    /// [`impl Muxer for Box<M>`](#impl-Muxer-for-Box<M>) must forward this
    /// method explicitly rather than inherit the default — the default would
    /// call `add_stream` on the box, silently discarding `spec` for every
    /// boxed muxer regardless of what the concrete type underneath actually
    /// overrides. A wrapping `Muxer` (a tee, a tally, a segmenter) has the
    /// same obligation for the same reason.
    ///
    /// # Errors
    /// As [`Muxer::add_stream`].
    fn add_stream_with(&mut self, params: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
        let _ = spec;
        self.add_stream(params)
    }

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

    /// Accept file- and stream-level metadata: tags, chapters, attachments.
    ///
    /// Called once by [`mux::MuxBuilder::open`], after [`Muxer::init`] and
    /// after stream time bases are read, but before [`Muxer::write_header`]
    /// (M30) — the same point M12 settles anything else that depends on the
    /// whole stream set, and the point every container that has a place to
    /// put a title or a chapter table needs to know it by.
    ///
    /// The default does nothing, which is exactly today's behaviour: before
    /// this method existed there was no channel for [`crate::metadata::MuxMetadata`]
    /// at all, so every muxer already drops it, and the default drops it the
    /// same way — no existing muxer's write changes.
    ///
    /// # Errors
    /// Whatever the container's own validation of tags, chapters or
    /// attachments finds. The default never errs.
    fn set_metadata(&mut self, metadata: &metadata::MuxMetadata) -> Result<()> {
        let _ = metadata;
        Ok(())
    }

    /// Whether the top-level `-bitexact` was requested for this output.
    ///
    /// # Why this exists
    ///
    /// The reference suppresses anything that encodes a library build or a
    /// wall clock under `-bitexact` — `vaco-mux-hash`'s `#software` line is
    /// one of them (measured, `ffmpeg 8.1`: `#software: Lavf62.12.100`
    /// appears only *without* `-bitexact`). Nothing reached a `Muxer` to say
    /// so before this: [`FormatOptions`] is known to [`mux::MuxBuilder`] but
    /// never handed to the muxer itself, the same gap closed for
    /// [`metadata::MuxMetadata`] via [`Muxer::set_metadata`].
    ///
    /// Called once by [`mux::MuxBuilder::open`], at the same point as
    /// [`Muxer::set_metadata`] — after stream time bases are read, before
    /// [`Muxer::write_header`] — from
    /// `FormatOptions::fflags.contains(FFlags::BITEXACT)`, which is already
    /// true today for `-fflags +bitexact` given directly on the output (the
    /// mechanism `vaco-mux-matroska` already reads for its own random-UID
    /// suppression) and, since this method's introduction, for the top-level
    /// `-bitexact` shorthand `vaco-cli` now folds onto it as well.
    ///
    /// The default does nothing: no existing muxer's write changes, because
    /// no muxer could have been reading this fact before now.
    fn set_bitexact(&mut self, bitexact: bool) {
        let _ = bitexact;
    }

    /// Set one muxer-private option by name — the seam for a per-container
    /// knob like `-movflags` that has no home in the generic
    /// [`FormatOptions`] table.
    ///
    /// Mirrors [`vaco_opts::OptionsExt::set_str`]'s name/value-string
    /// contract on purpose: a caller that already knows how to drive an
    /// `#[derive(Options)]` struct from a CLI-parsed pair needs no second
    /// convention to reach a muxer through the registry. `vaco-mux-mp4`'s
    /// `MovMuxer::with_options` is exactly the constructor this exists to make
    /// reachable — `vaco-mux-mp4` still owns parsing its own `movflags`
    /// spelling, this method is only the door.
    ///
    /// **Why not a parameter on [`MuxerDesc::open`].** Same reason as
    /// [`Demuxer::reconfigure`]: `open` is a bare `fn` pointer that ~90
    /// registered free functions already implement at a fixed signature, and
    /// widening it would require editing every one of them. [`mux::MuxBuilder`]
    /// calls this once per option a caller explicitly supplies via
    /// [`mux::MuxBuilder::with_private_options`], before [`Muxer::init`] (M29)
    /// — early enough that a fragmentation flag can still change what `init`
    /// decides.
    ///
    /// The default answers "no such option" for every name, which is correct
    /// for the ~90 existing muxers that declare no private options: nothing
    /// calls this today (no caller could reach it before now), so the default
    /// is never exercised by current behaviour. A muxer that grows options
    /// overrides it to parse its own.
    ///
    /// # Errors
    /// [`Error::Option`] naming `name`, when this muxer has no such option or
    /// `value` does not parse for it. The default always errs.
    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        let _ = value;
        Err(Error::Option {
            name: name.to_owned(),
            detail: "this muxer has no such option".to_owned(),
        })
    }

    /// Rebind this muxer to the URL it is writing to, for a container whose
    /// real output is a sequence of files rather than one continuous stream
    /// — one file per frame (`image2`), one file per segment
    /// (`segment`/`stream_segment`, the HLS/DASH family).
    ///
    /// Exists instead of a parameter on [`MuxerDesc::open`] because `open` is
    /// a bare `fn` pointer roughly 90 registered muxers already implement at
    /// a fixed `Box<dyn MediaSink>` signature; widening it would touch every
    /// one for the handful that need this.
    ///
    /// Call once, immediately after `open` and before
    /// [`Muxer::add_stream`]/[`Muxer::write_header`], so a muxer whose real
    /// unit of output is a sequence of files can re-derive its state from the
    /// URL — typically by replacing itself outright
    /// (`*self = Self::for_pattern(url, ..)?`). Mirrors [`Demuxer::bind_url`]
    /// on the read side: the caller passes a throwaway placeholder sink to
    /// `open` (a pattern like `out_%03d.png` is not itself an openable
    /// destination) and this method does the real work once the real URL is
    /// known. The default returns [`Error::Unsupported`], matching every
    /// muxer's behaviour before this method existed.
    ///
    /// The same seam also serves a *negotiating* muxer such as WHIP, where
    /// the destination is not a byte sink at all until an SDP/ICE/DTLS
    /// handshake completes: it declares [`FormatFlags::NOFILE`], has `open`
    /// ignore its sink, uses `bind_url` to store the destination with no I/O
    /// yet, and performs the handshake in [`Muxer::init`] once every stream
    /// is known — leaving a live transport in place before the header.
    /// `vaco-cli`'s `open_output` tries `bind_url` for `NOFILE` the same way
    /// it already did for [`FormatFlags::NEEDNUMBER`], treating the default
    /// [`Error::Unsupported`] as "this muxer has no use for its URL".
    ///
    /// # Errors
    /// [`Error::Unsupported`] when this muxer writes to the sink it was
    /// opened with and has no separate URL to bind (the default). Otherwise
    /// whatever resolving `url` finds — a pattern with no `%d` placeholder
    /// when one is required, or (for the negotiating case above) nothing at
    /// all: storing a URL string cannot fail.
    fn bind_url(&mut self, url: &str) -> Result<()> {
        let _ = url;
        Err(Error::Unsupported(
            "this muxer writes to the sink it was opened with; it has no separate URL to bind",
        ))
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
    // Forwarded explicitly, not inherited from the default: the default
    // would call `add_stream` on the box and silently drop `spec` for every
    // boxed muxer, regardless of what the concrete type underneath overrides
    // — see `Muxer::add_stream_with`'s doc comment.
    fn add_stream_with(&mut self, params: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
        (**self).add_stream_with(params, spec)
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
    fn set_bitexact(&mut self, bitexact: bool) {
        (**self).set_bitexact(bitexact);
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
    fn set_metadata(&mut self, metadata: &metadata::MuxMetadata) -> Result<()> {
        (**self).set_metadata(metadata)
    }
    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        (**self).set_option(name, value)
    }
    // Forwarded explicitly, not inherited from the default — same trap as
    // `add_stream_with` above: the default would always answer
    // `Unsupported` on the box, hiding whatever the concrete type
    // underneath overrides.
    fn bind_url(&mut self, url: &str) -> Result<()> {
        (**self).bind_url(url)
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

    /// This muxer's [`FormatFlags`], read without keeping the instance.
    ///
    /// **Why this is a method and not a `flags` field to match
    /// [`DemuxerDesc::flags`].** A field is the right shape and was the
    /// brief's own proposal, but it cannot be added the way this wave adds
    /// everything else: every one of the ~90 registered `MuxerDesc` constants
    /// already lists every current field with no `..base` update syntax (a
    /// literal search confirms it), so Rust requires any new field —
    /// regardless of its type or whether it has a sensible default — to be
    /// named at every one of those call sites. Default field values
    /// (`x: T = default`, RFC 3681) would remove that requirement and were
    /// checked directly against this workspace's pinned `rustc 1.97.1`: they
    /// remain behind `#![feature(default_field_values)]`
    /// (`error[E0658]`), which is unavailable on the stable toolchain this
    /// project pins and would not be reached for regardless. So the field is
    /// not additive today, and this method is the closest substitute: it
    /// reproduces exactly what `vaco-cli`'s `exec::open_output` already did by
    /// hand — construct once against a throwaway sink, read `.flags()`, keep
    /// the answer — except written once, here, instead of once per caller.
    ///
    /// It does not remove the double construction `exec::open_output`
    /// documents (a real, non-`NOFILE` output is still opened separately,
    /// against its own sink); it removes the *duplication* of the probing
    /// logic itself. Landing the field for real needs a wave that touches
    /// every `MuxerDesc` literal at once, the same wave `DemuxerDesc.flags`
    /// itself must have needed when the field was first authored — before any
    /// implementor existed to edit.
    ///
    /// A muxer whose `open` fails against an empty, writable
    /// [`vacoraw::MemorySink`] answers the safe default, [`FormatFlags::empty`]
    /// — none of the muxers in this workspace do, but a descriptor that could
    /// only panic on a hypothetical one would be worse than one that answers
    /// "nothing declared".
    #[must_use]
    pub fn probe_flags(&self) -> FormatFlags {
        (self.open)(Box::new(vacoraw::MemorySink::new()))
            .map(|m| m.flags())
            .unwrap_or_default()
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

    // ------------------------------------------------- gap 6: probe_flags

    /// `VacoRawMuxer` does not override [`Muxer::flags`], so this exercises
    /// the trait's own default alongside [`MuxerDesc::probe_flags`]'s: the
    /// harmless answer when nobody declared anything.
    #[test]
    fn probe_flags_reads_the_trait_default_when_the_muxer_declares_none() {
        assert_eq!(vacoraw::MUXER.probe_flags(), FormatFlags::empty());
    }

    /// A muxer that overrides [`Muxer::flags`], to prove `probe_flags` reads
    /// a real answer rather than always reporting the default — the useful
    /// half of the gap 6 substitute.
    #[derive(Debug, Default)]
    struct FlaggedMuxer;

    impl Muxer for FlaggedMuxer {
        fn flags(&self) -> FormatFlags {
            FormatFlags::NOFILE
        }
        fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
            Ok(0)
        }
        fn write_header(&mut self) -> Result<()> {
            Ok(())
        }
        fn write_packet(&mut self, _packet: &Packet) -> Result<()> {
            Ok(())
        }
        fn write_trailer(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn open_flagged(_sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
        Ok(Box::new(FlaggedMuxer))
    }

    #[test]
    fn probe_flags_reads_a_real_flag() {
        const DESC: MuxerDesc = MuxerDesc {
            name: "test-flagged",
            long_name: "test",
            extensions: &[],
            default_video: None,
            default_audio: None,
            open: open_flagged,
        };
        assert_eq!(DESC.probe_flags(), FormatFlags::NOFILE);
    }

    // ------------------------------------------ gaps 2 and 7: bind_url

    #[test]
    fn muxer_bind_url_default_is_unsupported() {
        // `FlaggedMuxer` (gap 6, above) overrides nothing else about `Muxer`,
        // so this is the trait's own default.
        let mut m = FlaggedMuxer;
        assert!(matches!(
            m.bind_url("out_%03d.png"),
            Err(Error::Unsupported(_))
        ));
    }

    /// A minimal [`Demuxer`] that overrides nothing, to exercise the trait's
    /// own default for methods this test module needs a value for.
    #[derive(Debug, Default)]
    struct NoopDemuxer;

    impl Demuxer for NoopDemuxer {
        fn streams(&self) -> &[Stream] {
            &[]
        }
        fn programs(&self) -> &[Program] {
            &[]
        }
        fn chapters(&self) -> &[Chapter] {
            &[]
        }
        fn metadata(&self) -> &[(String, String)] {
            &[]
        }
        fn read_packet(&mut self) -> Result<Packet> {
            Err(Error::Eof)
        }
        fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
            Err(Error::NotSeekable)
        }
    }

    #[test]
    fn demuxer_bind_url_default_is_unsupported() {
        let mut d = NoopDemuxer;
        assert!(matches!(
            d.bind_url("img_%03d.png"),
            Err(Error::Unsupported(_))
        ));
    }

    /// A muxer that records its [`Muxer::bind_url`] call in shared state —
    /// the shape a real `image2`/segment implementation uses (typically
    /// replacing itself outright), minimised here to prove the mechanism
    /// rather than a real pattern grammar. Shared state, not a field read
    /// back off the value, because the point of this test is that the call
    /// reaches the concrete type *through* a `Box<dyn Muxer>`.
    #[derive(Debug)]
    struct RebindingMuxer(std::sync::Arc<std::sync::Mutex<Option<String>>>);

    impl Muxer for RebindingMuxer {
        fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
            Ok(0)
        }
        fn write_header(&mut self) -> Result<()> {
            Ok(())
        }
        fn write_packet(&mut self, _packet: &Packet) -> Result<()> {
            Ok(())
        }
        fn write_trailer(&mut self) -> Result<()> {
            Ok(())
        }
        fn bind_url(&mut self, url: &str) -> Result<()> {
            *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(url.to_owned());
            Ok(())
        }
    }

    #[test]
    fn muxer_bind_url_override_is_reachable_through_a_box() {
        // Forwarding must be explicit on `impl Muxer for Box<M>`, or a
        // boxed muxer silently takes the default and `bind_url` never
        // reaches the concrete type underneath.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut boxed: Box<dyn Muxer> = Box::new(RebindingMuxer(seen.clone()));
        boxed.bind_url("out_%03d.png").unwrap();
        assert_eq!(
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_deref(),
            Some("out_%03d.png")
        );
    }

    /// The [`Demuxer`] mirror of the box-forwarding test above.
    #[derive(Debug)]
    struct RebindingDemuxer(std::sync::Arc<std::sync::Mutex<Option<String>>>);

    impl Demuxer for RebindingDemuxer {
        fn streams(&self) -> &[Stream] {
            &[]
        }
        fn programs(&self) -> &[Program] {
            &[]
        }
        fn chapters(&self) -> &[Chapter] {
            &[]
        }
        fn metadata(&self) -> &[(String, String)] {
            &[]
        }
        fn read_packet(&mut self) -> Result<Packet> {
            Err(Error::Eof)
        }
        fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
            Err(Error::NotSeekable)
        }
        fn bind_url(&mut self, url: &str) -> Result<()> {
            *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(url.to_owned());
            Ok(())
        }
    }

    #[test]
    fn demuxer_bind_url_override_is_reachable_through_a_box() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut boxed: Box<dyn Demuxer> = Box::new(RebindingDemuxer(seen.clone()));
        boxed.bind_url("img_%03d.png").unwrap();
        assert_eq!(
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_deref(),
            Some("img_%03d.png")
        );
    }
}
