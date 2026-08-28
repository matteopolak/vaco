//! Stream discovery: the bounded, replayable pass that fills in what a
//! container's header did not say.
//!
//! Almost every container under-describes its own streams. MPEG-TS says a PID
//! carries "AVC" and nothing else; Matroska stores a codec id and leaves the
//! profile in the bitstream; even MP4 leaves the frame rate to be derived. So
//! before anything downstream can be told what a file contains, some packets
//! have to be read — and then handed back, because the caller is entitled to
//! every packet in the file.
//!
//! [`Discovery`] is that pass. It wraps any [`Demuxer`], reads a bounded prefix,
//! refines what it can, and then behaves exactly like the demuxer it wraps,
//! replaying the packets it consumed before delegating.
//!
//! # Why a wrapper rather than a core-driven loop
//!
//! `planning/18-formats.md` §1.6 assumes a `DemuxContext` that owns the demuxer
//! and can therefore run this loop over it. The frozen [`Demuxer`] trait has no
//! such context — a demuxer owns its own I/O — so the composition has to go the
//! other way. That turns out to be the better shape: `Discovery<D>` is itself a
//! `Demuxer`, so it can be applied or not applied, tested against a mock, and
//! stacked under whatever else a caller wants, without any demuxer knowing it
//! exists.
//!
//! # Determinism
//!
//! Four rules, all because D6 requires identical output across runs, machines
//! and thread counts:
//!
//! * **No wall clock.** Analysed duration is media time, computed from packet
//!   timestamps. Nothing in the loop reads a clock.
//! * **No unordered iteration.** Per-stream state is a `Vec` indexed by stream
//!   index, never a `HashMap`.
//! * **No float accumulation.** Everything is `i64`, `i128` or [`Rational`].
//! * **No threading.** Discovery is single-threaded by construction. Parallel
//!   discovery would be a legitimate optimisation and it is forbidden, because
//!   packet-arrival order feeds the estimates.

use std::collections::VecDeque;

use vaco_codec_core::{CodecId, Parser, ParserDriver};
use vaco_core::{Duration, Error, MediaType, Rational, Result, TimeBase, Timestamp};
use vaco_limits::{Limits, ProgressGuard};
use vaco_packet::Packet;

use crate::flags::FormatFlags;
use crate::options::FormatOptions;
use crate::time::{DurationInputs, TimestampFixer, container_start_time};
use crate::{Chapter, Demuxer, ParserProvider, Program, Stream};

/// Why the discovery loop stopped.
///
/// Exposed at `-loglevel debug` and the single most useful diagnostic when a
/// reported field comes out wrong: "the loop stopped at `ProbeSize`" explains a
/// missing profile far better than the missing profile does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopReason {
    /// Every stream had everything it needed.
    Complete,
    /// `probesize` bytes were consumed.
    ProbeSize,
    /// `analyzeduration` microseconds of media time were covered.
    AnalyzeDuration,
    /// `max_probe_packets × streams` packets were read.
    PacketCap,
    /// No streams appeared within `max_ts_probe` packets.
    NoStreams,
    /// The input ended.
    Eof,
    /// The demuxer stopped making progress.
    #[default]
    NoProgress,
    /// The demuxer reported an unrecoverable error.
    Error,
}

impl StopReason {
    /// The name the debug log prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::ProbeSize => "probesize",
            Self::AnalyzeDuration => "analyzeduration",
            Self::PacketCap => "max_probe_packets",
            Self::NoStreams => "max_ts_probe",
            Self::Eof => "eof",
            Self::NoProgress => "no_progress",
            Self::Error => "error",
        }
    }
}

/// What one pass learned.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryReport {
    pub stop_reason: StopReason,
    /// Packets read and buffered for replay.
    pub packets_read: u64,
    /// Payload bytes read. Stands in for the I/O layer's `bytes_read`, which
    /// the frozen [`Demuxer`] trait does not expose — see the docs file.
    pub bytes_read: u64,
    /// Media time covered, microseconds.
    pub analyzed_us: i64,
    /// Container `start_time`, the minimum over qualifying streams.
    pub start_time: Option<Duration>,
    /// Inputs for [`crate::time::estimate_duration`], as far as this pass can
    /// fill them in.
    pub duration_inputs: DurationInputs,
}

#[derive(Debug, Clone, Default)]
struct StreamState {
    first_pts: Timestamp,
    last_end: Timestamp,
    packets: u64,
    /// Distinct DTS deltas seen, for the frame-rate estimate.
    delta_sum: i128,
    delta_count: u64,
    last_dts: Timestamp,
    parser_packets: u32,
    /// Whether the provider has already been asked for a parser for this
    /// stream. Asking once and remembering the answer is what makes "no parser
    /// for this codec" cost one lookup rather than one per packet.
    parser_asked: bool,
    complete: bool,
}

/// One stream's parser, once the provider has been asked for it.
///
/// A `Vec` beside [`StreamState`] rather than a field inside it, because
/// `StreamState` is `Clone` (it is built with `vec![…; n]`) and a boxed parser
/// is not. Keeping them apart is also what lets `StreamState` stay `Debug`.
type ParserSlot = Option<ParserDriver<Box<dyn Parser>>>;

/// A [`Demuxer`] that has read ahead, learned what it could, and will replay
/// every packet it consumed.
pub struct Discovery<D> {
    inner: D,
    streams: Vec<Stream>,
    state: Vec<StreamState>,
    /// One parser per stream, built lazily on the first packet that needs one
    /// and **kept for the whole pass**.
    ///
    /// Kept, not rebuilt per packet, for two reasons that are not about speed.
    /// An H.264 elementary stream's NAL unit ends where the *next* start code
    /// begins, so a parser that is thrown away at the end of each payload never
    /// sees the end of its last unit; and an MPEG-TS stream's parameter sets
    /// arrive in one packet while the fields they describe are wanted for all
    /// of them. Holding the parser is also the safer shape under D6's threat
    /// model: one [`vaco_limits::Budget`] accumulates across the whole pass
    /// instead of each packet getting a fresh full allowance.
    parsers: Vec<ParserSlot>,
    queue: VecDeque<Packet>,
    fixer: TimestampFixer,
    opts: FormatOptions,
    flags: FormatFlags,
    limits: Limits,
    report: DiscoveryReport,
    ran: bool,
}

/// Hand-written because a `Box<dyn Parser>` is not `Debug`, and
/// [`Discovery::parsers`] holds one per stream. Everything a reader of a debug
/// dump actually wants — the report, the stop reason, the queue depth — is
/// here; the parsers are summarised by how many were built.
impl<D: core::fmt::Debug> core::fmt::Debug for Discovery<D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Discovery")
            .field("inner", &self.inner)
            .field("streams", &self.streams.len())
            .field("parsers_built", &self.parsers.iter().flatten().count())
            .field("queued", &self.queue.len())
            .field("opts", &self.opts)
            .field("flags", &self.flags)
            .field("report", &self.report)
            .field("ran", &self.ran)
            .finish_non_exhaustive()
    }
}

impl<D: Demuxer> Discovery<D> {
    /// Wrap `inner`. Nothing is read until [`Discovery::run`] is called, so
    /// constructing one is free and a caller that does not want the pass simply
    /// does not run it.
    #[must_use]
    pub fn new(inner: D, flags: FormatFlags, opts: &FormatOptions) -> Self {
        let streams = inner.streams().to_vec();
        let n = streams.len();
        let fixer = TimestampFixer::for_streams(&streams, flags, opts);
        Self {
            inner,
            state: vec![StreamState::default(); n],
            parsers: (0..n).map(|_| None).collect(),
            streams,
            queue: VecDeque::new(),
            fixer,
            opts: opts.clone(),
            flags,
            limits: Limits::strict(),
            report: DiscoveryReport::default(),
            ran: false,
        }
    }

    /// Cap what the injected parsers may allocate while refining parameters.
    ///
    /// The frozen [`DemuxerDesc::open`](crate::DemuxerDesc) takes no
    /// [`Limits`], so this wrapper is where a caller gets to supply them —
    /// which is the right place anyway, since discovery is the only part of the
    /// pipeline that hands untrusted payloads to code it did not choose.
    /// Defaults to [`Limits::strict`], the conservative choice an embedder
    /// should get without asking.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// What the last [`Discovery::run`] learned.
    #[must_use]
    pub const fn report(&self) -> &DiscoveryReport {
        &self.report
    }

    /// The wrapped demuxer.
    pub const fn inner(&self) -> &D {
        &self.inner
    }

    /// The container flags this pass was configured with.
    ///
    /// Worth reading back: they decide whether the monotonic-DTS repair ran,
    /// which is the difference between "this file has a discontinuity" and
    /// "this file had one and we hid it".
    #[must_use]
    pub const fn flags(&self) -> FormatFlags {
        self.flags
    }

    /// Read the bounded prefix and refine what it allows.
    ///
    /// Every packet read is buffered and replayed, so running this is
    /// transparent to whoever reads from the [`Discovery`] afterwards. Running
    /// it twice is a no-op.
    ///
    /// # Errors
    ///
    /// Only a genuinely unrecoverable transport failure. A malformed packet
    /// stops the loop and is recorded as [`StopReason::Error`]; the streams
    /// discovered so far are kept, because reporting six of seven streams beats
    /// reporting none.
    pub fn run(&mut self, parsers: &dyn ParserProvider) -> Result<&DiscoveryReport> {
        if self.ran {
            return Ok(&self.report);
        }
        self.ran = true;
        // Gap 4 (`planning/INTERFACE-GAPS.md`): `DemuxerDesc::open` had no
        // seam for `Limits` or `FormatOptions`, so every demuxer invented its
        // own defaults. `Demuxer::reconfigure` is the seam that reaches an
        // already-constructed demuxer instead; calling it here means wrapping
        // a demuxer in `Discovery` is enough to hand over the real budget and
        // the real option set before anything is read through this wrapper. A
        // demuxer that predates the method ignores the call, exactly as it
        // ignored `with_limits`/the caller's `FormatOptions` before this
        // existed.
        self.inner.reconfigure(&self.limits, &self.opts)?;
        let mut guard = ProgressGuard::new();
        let packet_cap = u64::try_from(self.opts.max_probe_packets)
            .unwrap_or(u64::MAX)
            .saturating_mul(self.streams.len().max(1) as u64);
        let byte_cap = u64::try_from(self.opts.probesize).unwrap_or(u64::MAX);
        let ts_probe = u64::try_from(self.opts.max_ts_probe).unwrap_or(u64::MAX);
        let analyze_cap = if self.opts.analyzeduration > 0 {
            self.opts.analyzeduration
        } else {
            i64::MAX
        };

        loop {
            if !self.streams.is_empty() && self.state.iter().all(|s| s.complete) {
                self.report.stop_reason = StopReason::Complete;
                break;
            }
            if self.report.bytes_read >= byte_cap {
                self.report.stop_reason = StopReason::ProbeSize;
                break;
            }
            if self.report.analyzed_us >= analyze_cap {
                self.report.stop_reason = StopReason::AnalyzeDuration;
                break;
            }
            if self.report.packets_read >= packet_cap {
                self.report.stop_reason = StopReason::PacketCap;
                break;
            }
            if self.streams.is_empty() && self.report.packets_read >= ts_probe {
                self.report.stop_reason = StopReason::NoStreams;
                break;
            }
            match self.inner.read_packet() {
                Ok(mut pkt) => {
                    // A demuxer that returns packets without advancing is a
                    // scheduler infinite loop; this turns it into a localised,
                    // reproducible error instead of a fuzzer timeout.
                    guard.tick(pkt.len > 0 || pkt.pts.is_some() || pkt.dts.is_some())?;
                    self.absorb(&mut pkt, parsers);
                    self.queue.push_back(pkt);
                }
                Err(Error::Eof) => {
                    self.report.stop_reason = StopReason::Eof;
                    break;
                }
                Err(e) if e.is_recoverable() => {
                    guard.tick(false)?;
                }
                Err(_) => {
                    self.report.stop_reason = StopReason::Error;
                    break;
                }
            }
        }
        self.finish();
        Ok(&self.report)
    }

    /// Fold one packet into the per-stream state, and offer it to a parser.
    fn absorb(&mut self, pkt: &mut Packet, parsers: &dyn ParserProvider) {
        let limits = self.limits.clone();
        // A packet counts as read whatever stream it names. Counting only the
        // ones that land on a known stream is how a file with no streams at
        // all reads to EOF instead of stopping at `max_ts_probe`.
        self.report.packets_read = self.report.packets_read.saturating_add(1);
        self.report.bytes_read = self.report.bytes_read.saturating_add(pkt.len as u64);
        let Ok(i) = usize::try_from(pkt.stream_index) else {
            return;
        };
        let (Some(stream), Some(st), Some(slot)) = (
            self.streams.get_mut(i),
            self.state.get_mut(i),
            self.parsers.get_mut(i),
        ) else {
            return;
        };
        let time_base = stream.time_base;
        // Read before the parser runs, so R21's input is exactly what it was
        // before R21b existed. `refine` can fill `params.video.frame_rate` in,
        // and letting that reach the *same* packet's duration fill-in would be
        // a behaviour change nothing here asked for.
        let rate = picture_rate(stream);

        // Refine parameters from the payload, without decoding it.
        //
        // Ahead of `fix` so that R21b — the codec's own packet duration, filled
        // in just below — is in place before the timestamp rules read
        // `pkt.duration`. R21 skips a packet that already has one, R22's
        // `last_duration` becomes the real step instead of 1, and
        // `analyzed_us`/`last_end` stop under-counting an audio stream whose
        // container states no duration at all.
        let cap = u32::try_from(self.opts.max_probe_packets).unwrap_or(u32::MAX);
        if !self.opts.fflags.contains(crate::options::FFlags::NOPARSE)
            && st.parser_packets < cap
            && let Some(id) = stream.params.codec_id
            && self.opts.codec_allowed(id.name())
        {
            st.parser_packets = st.parser_packets.saturating_add(1);
            if !st.parser_asked {
                st.parser_asked = true;
                *slot = build_parser(stream, id, parsers, limits);
            }
            if let Some(driver) = slot.as_mut() {
                refine(stream, driver, pkt.payload());
            }
            // Independent of whether a `Parser` is registered for this
            // codec at all (a `--no-default-features` build, say): the
            // extradata-synthesis rule needs nothing but the payload bytes
            // and the codec id, see [`synthesize_extradata`].
            synthesize_extradata(stream, pkt.payload());
        }
        // `avg_frame_rate` for a stream whose only rate ever comes from the
        // codec's own in-band parameters (a raw elementary stream has no
        // `stts`/DTS spacing to estimate from below, since R21b never gives
        // it a timestamp). Reuses [`picture_rate`] rather than
        // `stream.params.video.frame_rate` directly, so H.264/MPEG-1's tick
        // rate is halved here exactly as it is for R21's packet-duration
        // fill just below — otherwise this falls back, at display time, to
        // the same undivided tick rate `r_frame_rate` correctly carries,
        // and doubles every `avg_frame_rate` a raw `.h264`/`.hevc` file
        // reports. Guarded so a rate this stream already has (from MP4's
        // `stts` or the DTS-delta estimate in `finish`) is never overwritten.
        if stream.media_type() == Some(MediaType::Video)
            && (!stream.avg_frame_rate.is_defined() || stream.avg_frame_rate.is_zero())
        {
            let picture = picture_rate(stream);
            if !picture.is_zero() {
                stream.avg_frame_rate = picture;
            }
        }
        fill_codec_duration(slot, pkt, time_base);

        self.fixer.fix(pkt, time_base, rate);

        st.packets = st.packets.saturating_add(1);

        if pkt.pts.is_some()
            && (st.first_pts.is_none()
                || pkt
                    .pts
                    .compare(time_base, st.first_pts, time_base)
                    .is_some_and(core::cmp::Ordering::is_lt))
        {
            st.first_pts = pkt.pts;
        }
        if let Some(p) = pkt.pts.ticks() {
            let end_us = Timestamp::new(p)
                .to_duration(time_base)
                .map_or(0, Duration::as_micros)
                .saturating_add(pkt.duration.as_micros());
            self.report.analyzed_us = self.report.analyzed_us.max(end_us);
            let end_ticks = p.saturating_add(pkt.duration.to_ticks(time_base).unwrap_or(0));
            if st.last_end.ticks().is_none_or(|e| end_ticks > e) {
                st.last_end = Timestamp::new(end_ticks);
            }
        }
        if let (Some(prev), Some(cur)) = (st.last_dts.ticks(), pkt.dts.ticks())
            && cur > prev
        {
            st.delta_sum = st.delta_sum.saturating_add(i128::from(cur - prev));
            st.delta_count = st.delta_count.saturating_add(1);
        }
        if pkt.dts.is_some() {
            st.last_dts = pkt.dts;
        }

        // A stream is complete once it has parameters, a first timestamp and
        // enough packets to estimate a rate.
        let has_params = has_essential_params(stream);
        st.complete = has_params && st.first_pts.is_some() && st.delta_count >= 2;
    }

    /// Derive everything that needed the whole prefix.
    fn finish(&mut self) {
        let fps_probe = u64::try_from(self.opts.fpsprobesize).unwrap_or(0);
        for (stream, st) in self.streams.iter_mut().zip(self.state.iter()) {
            if stream.start_time.is_none() {
                // `first_pts + initial_padding`, NOT the first pts. An encoder
                // delay makes the first packet's timestamp negative by exactly
                // the priming it declares, and the reference reports the sum —
                // which is 0 for a normally-encoded stream.
                //
                // Measured: a libopus track in Matroska declares
                // `initial_padding = 312` samples at 48 kHz and its first
                // packet has `pts = -7` ms, yet `ffprobe` reports
                // `start_pts=0`. Taking the first pts alone gives `-0.007`,
                // which is wrong on every delay-coded stream there is.
                //
                // Found by `vaco-demux-matroska`, which raised it instead of
                // compensating locally — the right call, since otherwise every
                // container carrying Opus or AAC needs the same workaround.
                let pad_ticks = stream
                    .params
                    .audio
                    .as_ref()
                    .filter(|a| a.initial_padding > 0 && a.sample_rate > 0)
                    .and_then(|a| {
                        // The priming is a sample count; the timestamp is in
                        // the stream's time base. Convert exactly rather than
                        // assuming they share units — for Matroska they do not
                        // (samples at 48 kHz against a 1 ms base).
                        vaco_core::rescale_rnd(
                            i64::from(a.initial_padding),
                            stream.time_base.den.into(),
                            i64::from(a.sample_rate) * i64::from(stream.time_base.num),
                            vaco_core::Rounding::NearestAwayFromZero,
                        )
                    })
                    .unwrap_or(0);
                stream.start_time = st.first_pts.offset(pad_ticks);
            }
            if stream.frame_count.is_none() && st.packets > 0 {
                // Only meaningful once the whole file has been read; the loop
                // records what it saw and leaves the caller to decide.
                stream.frame_count = None;
            }
            // Frame rate from the mean DTS delta, for a stream that arrived
            // without one.
            //
            // The estimate is an *average*, so it is `avg_frame_rate`'s answer
            // by construction. It is also written to `r_frame_rate`, and that
            // needs justifying rather than assuming: a mean cannot distinguish
            // the two, and the container that *can* — MP4, which has every
            // `stts` delta — fills both itself, so this branch never runs for
            // it. Leaving `r_frame_rate` at `0/0` here would print `0/0` for
            // every MPEG-TS video stream, where the reference prints a rate.
            // A genuine `r_frame_rate` wants the delta *histogram* (plan 18
            // §1.6.3 says gcd; MP4 measured most-common), which this pass does
            // not keep — recorded as the honest gap it is.
            // Video only. The reference prints `0/0` for both rates on every
            // audio and subtitle stream, however regular its packet spacing —
            // and an estimator handed a 1024-sample AAC stream will happily
            // produce `11025/256`, which is a real rate and the wrong answer.
            // This used to be enforced accidentally, by the estimate landing
            // on `params.video`, which only a video stream has.
            let enough = fps_probe == 0 || st.delta_count >= fps_probe;
            if enough && st.delta_count > 0 && stream.media_type() == Some(MediaType::Video) {
                #[allow(
                    clippy::integer_division,
                    reason = "delta_count is non-zero on this branch; the mean is exact enough \
                              for a rate estimate and is then reduced as a rational"
                )]
                let mean = st.delta_sum / i128::from(st.delta_count);
                if mean > 0 {
                    let (rate, _) = Rational::reduce(
                        i64::from(stream.time_base.den),
                        i64::try_from(mean)
                            .unwrap_or(i64::MAX)
                            .saturating_mul(i64::from(stream.time_base.num)),
                        i64::from(i32::MAX),
                    );
                    if let Some(v) = stream.params.video.as_mut()
                        && (!v.frame_rate.is_defined() || v.frame_rate.is_zero())
                    {
                        v.frame_rate = rate;
                    }
                    if !stream.avg_frame_rate.is_defined() || stream.avg_frame_rate.is_zero() {
                        stream.avg_frame_rate = rate;
                    }
                    if !stream.r_frame_rate.is_defined() || stream.r_frame_rate.is_zero() {
                        stream.r_frame_rate = rate;
                    }
                }
            }
        }
        self.report.start_time = container_start_time(
            self.streams
                .iter()
                .filter(|s| !s.disposition.contains(crate::Disposition::ATTACHED_PIC))
                .filter(|s| s.params.codec_id.is_some())
                .map(|s| (s.start_time, s.time_base)),
        );
        self.report.duration_inputs.start_time = self.report.start_time;
        self.report.duration_inputs.container = self.inner.duration();
        self.report.duration_inputs.longest_stream =
            self.streams.iter().filter_map(Stream::duration).max();
        // `from_pts` is DELIBERATELY left unset. It is documented as
        // "`max(pts + duration)` over streams, **from a tail scan**", and
        // `estimate_duration` prefers it over the container's own field — but
        // discovery reads a *prefix*, so filling it from `last_end` reports the
        // length of the probe window instead of the file.
        //
        // Measured, five containers each tracking its own window:
        //
        //   av.mp4      ref 2.000000   from the probe window 0.200000
        //   op_st.webm  ref 2.008000                         0.034000
        //   ts.ts       ref 1.000000                         0.900000
        //   a.m4a       ref 1.000000                         0.046440
        //   fhd.mp4     ref 1.000000                         0.120000
        //
        // Found independently by `vaco-probe` (which broke every container's
        // duration the moment discovery was actually composed) and by
        // `vaco-demux-mpegts` (which reported that a 10-second file with a
        // 5-second analyzeduration reports ~5 s). Two reports, one cause.
        //
        // A tail-scanning caller may set it; this pass must not.
        self.report.duration_inputs.from_pts = None;
        self.adopt_container_timings();
    }

    /// A stream the pass never saw a timestamp for takes the *container's*
    /// start time and duration, each rescaled into its own time base.
    ///
    /// **Measured on Matroska**, which is where the rule shows itself, because
    /// there a track states neither. The discriminating experiments:
    ///
    /// | file | subtitle `start_pts` | subtitle `duration_ts` |
    /// |---|---|---|
    /// | `sub.mkv` — subtitle only, container start `N/A`, duration 2.000 | `N/A` | **2000** |
    /// | `as.mkv` — opus + subtitle, container start 0, duration 2.008 | 0 | **2008** |
    /// | `as2.mkv` — as above but the subtitle ends at 1.0 s | 0 | **2008** |
    /// | `live_as.mkv` — same, muxed to a pipe so no `Duration` element | 0 | `N/A` |
    ///
    /// `as2.mkv` rules out "the stream's own extent" — the value ignores where
    /// the subtitle actually stops — and `live_as.mkv` rules out a packet
    /// scan: remove the container's statement and the field goes with it. It
    /// is the container's duration, handed to a stream that has nothing of its
    /// own, and the per-track `DURATION` *tag* is not the source either
    /// (`as2.mkv`'s says 1.0 s where the field says 2.008).
    ///
    /// Both halves are guarded on `start_time.is_none()` together, not
    /// separately: `sub.mkv` has an unknown container start and a known
    /// container duration and reports `start_pts=N/A` with `duration_ts=2000`,
    /// so each half is applied only if the container states it, but the
    /// *decision* to apply either is one test on `start_time`.
    ///
    /// This is deliberately here and not in a demuxer. It needs the container
    /// duration and the whole stream list, neither of which one track knows,
    /// and a demuxer that filled it locally would disable the shared rule for
    /// every caller that does run discovery — the hazard plan 18's composition
    /// amendment records.
    fn adopt_container_timings(&mut self) {
        let container_duration =
            crate::time::estimate_duration(&self.report.duration_inputs, &self.opts).duration;
        let container_start = self.report.start_time;
        for stream in &mut self.streams {
            if stream.start_time.is_some() {
                continue;
            }
            let tb = stream.time_base;
            if let Some(start) = container_start {
                stream.start_time = Timestamp::new(start.as_micros())
                    .checked_rescale(
                        vaco_core::TimeBase::MICROSECONDS,
                        tb,
                        vaco_core::Rounding::NearestAwayFromZero,
                    )
                    .unwrap_or(Timestamp::NONE);
            }
            if stream.duration_ts.is_none()
                && let Some(d) = container_duration
                && let Some(ts) = Timestamp::new(d.as_micros()).checked_rescale(
                    vaco_core::TimeBase::MICROSECONDS,
                    tb,
                    vaco_core::Rounding::NearestAwayFromZero,
                )
                && let Some(ticks) = ts.ticks()
            {
                stream.set_duration_ts(ticks);
            }
        }
    }
}

/// The picture rate R21 should divide by, derived from a stream's
/// codec-reported `frame_rate`.
///
/// `params.video.frame_rate` is not always a picture rate: for H.264 and
/// MPEG-1 video it is measured (see [`CodecId::ticks_per_frame`]) to be a
/// *tick* rate, exactly double the rate pictures are actually shown at. R21
/// fills in a packet's duration as `1 / rate`, so feeding it the tick rate
/// directly halves every duration it fills in for those two codecs. Dividing
/// here, once, keeps that correction in the one place both `absorb` and
/// `read_packet` pull the rate from, rather than duplicating it at each call
/// site.
fn picture_rate(stream: &Stream) -> Rational {
    let Some(video) = stream.params.video.as_ref() else {
        return Rational::ZERO;
    };
    let divisor = stream
        .params
        .codec_id
        .map_or(1, CodecId::ticks_per_frame)
        .max(1);
    video.frame_rate / Rational::new(i32::try_from(divisor).unwrap_or(1), 1)
}

/// Whether a stream has the fields every consumer needs.
fn has_essential_params(stream: &Stream) -> bool {
    if stream.params.codec_id.is_none() {
        return false;
    }
    match (&stream.params.video, &stream.params.audio) {
        (Some(v), _) => v.width > 0 && v.height > 0,
        (_, Some(a)) => a.sample_rate > 0,
        _ => true,
    }
}

/// Ask the injected provider for a parser, and seed it from the container's
/// own configuration record.
///
/// **The seeding is the half that makes any of this work in MP4 and Matroska.**
/// In an MPEG-TS or raw elementary stream every parameter set is in-band, so
/// feeding payloads is enough. In MP4 the H.264 sequence parameter set is in
/// `avcC`, the AAC configuration is in `esds`, and the Opus identification
/// header is in `dOps` — none of them appears in any packet, so a parser given
/// only payloads reports nothing however many it is given. Measured on
/// `av.mp4`: eight of the eight bitstream-derived stream fields arrive from
/// the record and none from a packet.
///
/// A record that fails to parse is **not** fatal. Discovery is offering the
/// parser what the container happened to carry; a malformed record means "this
/// told me nothing", and the container's own fields still stand.
fn build_parser(
    stream: &mut Stream,
    id: CodecId,
    parsers: &dyn ParserProvider,
    limits: Limits,
) -> ParserSlot {
    let mut parser = parsers.parser_for(id)?;
    if let Some(extra) = stream.params.extradata.clone()
        && !extra.is_empty()
    {
        let _ = parser.set_extradata(&extra);
    }
    let driver = ParserDriver::new(parser, limits);
    // Fold in whatever the record alone established, before any packet has
    // arrived. A stream whose description is wholly out of band — Opus is
    // exactly that — is complete at this point.
    if let Some(found) = driver.parameters() {
        stream.params.fill_from(found);
    }
    Some(driver)
}

/// R21b — fill in a packet duration the container did not state, from the
/// codec's own bitstream.
///
/// The container wins whenever it said anything: this only ever writes over
/// `Duration::ZERO`, which is the model's spelling of "absent". A `BlockDuration`,
/// an `stts` delta or a `DefaultDuration` therefore stands untouched, and so does
/// a duration an earlier call already filled in.
///
/// # Why here and not in the demuxer
///
/// D14.1 forbids `vaco-demux-matroska` from naming `vaco-parse-opus`, and the
/// number is not in the Matroska file: an Opus track carries no
/// `DefaultDuration` element at all, so there is nothing for the demuxer to
/// misread. `Discovery` already holds one parser per stream, already seeded from
/// the container's configuration record by [`build_parser`], and is already
/// wrapped around every demuxer `vaco-probe` opens — so putting the rule here
/// covers every container at once and costs the callers nothing. It is the same
/// argument that put `start_time` here.
///
/// # Cost
///
/// One `&self` call per packet, on the read path, over attacker-controlled
/// bytes. [`vaco_codec_core::Parser::packet_duration`] is specified to allocate
/// nothing, advance nothing and never fail — the parser is *not* driven, so the
/// packet is not copied into a `Packet` and no [`vaco_limits::Budget`] moves.
/// A parser that cannot measure the packet says so and the field stays absent,
/// which is what the container said.
fn fill_codec_duration(slot: &ParserSlot, pkt: &mut Packet, time_base: TimeBase) {
    if pkt.duration != Duration::ZERO {
        return;
    }
    let Some(driver) = slot.as_ref() else { return };
    let Some(seconds) = driver.parser().packet_duration(pkt.payload()) else {
        return;
    };
    if let Some(d) = crate::time::quantise_duration(seconds, time_base) {
        pkt.duration = d;
    }
}

/// Feed one payload to a stream's parser and fold back what it learned.
///
/// The direction is load-bearing: the container's own metadata wins and the
/// parser only supplies what the container left blank
/// ([`vaco_codec_core::CodecParameters::fill_from`]). Inverting it is how a
/// stream whose bitstream header disagrees with its container ends up reported
/// wrongly.
///
/// Driven through [`ParserDriver`], which owns reassembly across payload
/// boundaries, the empty-slice end-of-stream convention, the consumed-bytes
/// check and the progress guard that turns a parser which never advances into
/// a localised error rather than a hang. Doing that by hand here — as this did
/// before `vaco-codec-core` grew `impl<P: Parser + ?Sized> Parser for Box<P>`
/// — meant a second implementation of the same convention, which is precisely
/// how the two drift apart.
fn refine(stream: &mut Stream, driver: &mut ParserDriver<Box<dyn Parser>>, payload: &[u8]) {
    if payload.is_empty() {
        return;
    }
    // A refused push means the payload is larger than the reassembly cap, which
    // is a legitimate answer for a hostile stream and not a reason to give up
    // on the parameters the parser may already have.
    if driver.push(payload).is_ok() {
        // Drain whatever units this payload completed. The driver bounds the
        // loop itself — `NeedMoreInput` ends it, and a parser that neither
        // consumes nor produces trips its `ProgressGuard` — so the only thing
        // left to do here is stop on the first non-unit answer.
        while driver.next_unit().is_ok() {}
    }
    if let Some(found) = driver.parameters() {
        stream.params.fill_from(found);
    }
}

/// CONFORMANCE-FINDINGS 26's read half: fill in `extradata` for a stream
/// whose container carries no out-of-band configuration record — AVI,
/// MPEG-TS, raw Annex B — by pulling H.264/HEVC parameter sets back out of
/// the packets discovery is already reading.
///
/// Mirrors the reference's own mechanism: `avformat_find_stream_info` runs
/// its `extract_extradata` bitstream filter over the probe window and stores
/// whatever it collects. The assembly rule — which units count as parameter
/// sets, and how their bytes are laid out — lives in
/// [`vaco_format_nalu::extradata`], the one place D19 allows it; this
/// function only decides *when* to call it, which is the read side's own
/// question and not part of the shared rule.
///
/// # Why here, and why on the raw payload rather than through the parser
///
/// `Discovery` already holds exactly the bytes a container-supplied parser
/// never gets to see whole in MP4/Matroska (there the SPS lives in `avcC`,
/// not in a packet) but sees in full here, because AVI/MPEG-TS/raw Annex B
/// carry every parameter set in-band. Reaching into `vaco-parse-h264`'s or
/// `vaco-parse-hevc`'s private state to ask what SPS/PPS it already parsed
/// would be a second way to get the same bytes this function already has in
/// hand, and would need `Parser` widened to expose them (a rejected
/// alternative — see `vaco_format_nalu::extradata`'s module docs). Layering
/// also rules out reaching for `vaco-bsf-generic`'s filter directly: D14.1
/// keeps a `vaco-format-*` crate off `vaco-parse-*`, and going through it
/// would mean either a new `BsfProvider` seam (an interface change, recorded
/// in `planning/INTERFACE-GAPS.md` if ever taken) or depending on a crate two
/// hops removed from what this needs — a `Vec<&[u8]>` in, a `Vec<u8>` out.
///
/// Called from [`Discovery::absorb`] rather than from [`refine`], and
/// unconditionally on whether a `Parser` was actually built for the stream:
/// a `--no-default-features` build with no H.264/HEVC parser compiled in
/// still has every byte this needs, and still gets the same `-show_streams`
/// answer, because the rule only touches `vaco-format-nalu` and never asks a
/// codec crate for anything.
///
/// # Why only once
///
/// The container's own record always wins once it has supplied a non-empty
/// `extradata` — checked first, so an MP4/Matroska stream is never touched.
/// Absent that, this fires only while `extradata` is still empty: the first
/// packet in the probe window carrying a parameter set sets it, and later
/// packets are left alone. A file whose SPS/PPS change mid-stream (rare, and
/// not exercised by any of finding 26's measured containers) keeps whatever
/// the first keyframe stated, which is what `-show_streams` reports for
/// every container this was checked against.
fn synthesize_extradata(stream: &mut Stream, payload: &[u8]) {
    if stream
        .params
        .extradata
        .as_ref()
        .is_some_and(|e| !e.is_empty())
    {
        return;
    }
    let Some(id) = stream.params.codec_id else {
        return;
    };
    let header_kind = match id {
        CodecId::H264 => vaco_format_nalu::HeaderKind::H264,
        CodecId::Hevc => vaco_format_nalu::HeaderKind::H265,
        _ => return,
    };
    // Mirrors `vaco-bsf-generic`'s own framing choice: `nal_length_size`
    // absent or `Some(0)` (no configuration record) means Annex B, anything
    // else names the length-prefix width.
    let framing = stream
        .params
        .video
        .as_ref()
        .and_then(|v| v.nal_length_size)
        .and_then(vaco_format_nalu::LengthSize::new)
        .map_or(
            vaco_format_nalu::Framing::AnnexB,
            vaco_format_nalu::Framing::LengthPrefixed,
        );
    let sets = vaco_format_nalu::parameter_sets(payload, framing, header_kind);
    if sets.is_empty() {
        return;
    }
    stream.params.extradata = Some(vaco_format_nalu::assemble_extradata(sets));
}

impl<D: Demuxer> Demuxer for Discovery<D> {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn programs(&self) -> &[Program] {
        self.inner.programs()
    }

    fn chapters(&self) -> &[Chapter] {
        self.inner.chapters()
    }

    fn metadata(&self) -> &[(String, String)] {
        self.inner.metadata()
    }

    /// Replay what discovery consumed, then delegate — applying the timestamp
    /// model to anything read from here on, so a packet's treatment does not
    /// depend on whether it happened to fall inside the probe window.
    fn read_packet(&mut self) -> Result<Packet> {
        if let Some(p) = self.queue.pop_front() {
            return Ok(p);
        }
        let mut pkt = self.inner.read_packet()?;
        let (tb, rate) = usize::try_from(pkt.stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .map_or((crate::time::TIME_BASE_Q, Rational::ZERO), |s| {
                (s.time_base, picture_rate(s))
            });
        if let Ok(i) = usize::try_from(pkt.stream_index)
            && let Some(slot) = self.parsers.get(i)
        {
            fill_codec_duration(slot, &mut pkt, tb);
        }
        self.fixer.fix(&mut pkt, tb, rate);
        Ok(pkt)
    }

    /// Seek, and reset every piece of derived state (S3).
    ///
    /// The replay queue has to go: those packets are from before the seek and
    /// handing them over afterwards is the bug this method exists to prevent.
    fn seek(&mut self, target: crate::SeekTarget, flags: crate::SeekFlags) -> Result<()> {
        self.inner.seek(target, flags)?;
        self.queue.clear();
        self.fixer.flush();
        Ok(())
    }

    fn duration(&self) -> Option<Duration> {
        crate::time::estimate_duration(&self.report.duration_inputs, &self.opts).duration
    }
}

/// A [`ParserProvider`] that has no parsers.
///
/// The default, and what a demuxer's own unit tests and fuzz targets use: it
/// keeps demuxer fuzzing fast and independent of codec code, and it is what
/// makes "discovery degrades gracefully" a tested property rather than a hope.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoParsers;

impl ParserProvider for NoParsers {
    fn parser_for(&self, _codec: CodecId) -> Option<Box<dyn vaco_codec_core::Parser>> {
        None
    }
}

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
    use crate::test_support::MockDemuxer;
    use crate::{SeekFlags, SeekTarget};
    use std::sync::{Arc, Mutex};
    use vaco_core::MediaType;
    use vaco_limits::Budget;
    use vaco_packet::PacketFlags;

    fn opts() -> FormatOptions {
        FormatOptions::default()
    }

    // --------------------------------------------------- gap 4: reconfigure

    /// A demuxer that records the [`Limits`]/[`FormatOptions`] it was handed,
    /// to prove [`Discovery::run`] actually calls [`Demuxer::reconfigure`]
    /// rather than merely compiling against it.
    #[derive(Debug)]
    struct RecordingDemuxer {
        inner: MockDemuxer,
        seen: Arc<Mutex<Option<(u64, i64)>>>,
    }

    impl Demuxer for RecordingDemuxer {
        fn streams(&self) -> &[Stream] {
            self.inner.streams()
        }
        fn read_packet(&mut self) -> Result<Packet> {
            self.inner.read_packet()
        }
        fn seek(&mut self, target: crate::SeekTarget, flags: crate::SeekFlags) -> Result<()> {
            self.inner.seek(target, flags)
        }
        fn reconfigure(&mut self, limits: &Limits, opts: &FormatOptions) -> Result<()> {
            if let Ok(mut g) = self.seen.lock() {
                *g = Some((limits.max_alloc_total, opts.probesize));
            }
            Ok(())
        }
    }

    /// The default does the harmless thing: [`MockDemuxer`] does not override
    /// [`Demuxer::reconfigure`], and running discovery through it — with a
    /// caller-supplied [`Limits`] that is nothing like the demuxer's own
    /// hardcoded default — still succeeds, exactly as it did before this
    /// method existed.
    #[test]
    fn reconfigure_default_is_a_harmless_no_op() {
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(5);
        let mut d =
            Discovery::new(inner, FormatFlags::empty(), &opts()).with_limits(Limits::tiny());
        assert!(d.run(&NoParsers).is_ok());
    }

    /// An override receives exactly the [`Limits`] [`Discovery::with_limits`]
    /// was given and the exact [`FormatOptions`] [`Discovery::new`] was
    /// constructed with — not a demuxer-invented default (the D19 failure mode
    /// gap 4 names).
    #[test]
    fn reconfigure_override_receives_the_configured_limits_and_options() {
        let seen = Arc::new(Mutex::new(None));
        let inner = RecordingDemuxer {
            inner: MockDemuxer::new(1, MediaType::Video).with_packets(5),
            seen: Arc::clone(&seen),
        };
        let mut o = opts();
        o.probesize = 12_345;
        let limits = Limits::tiny();
        let mut d = Discovery::new(inner, FormatFlags::empty(), &o).with_limits(limits.clone());
        d.run(&NoParsers).unwrap();
        let got = seen.lock().unwrap().unwrap_or_default();
        assert_eq!(got.0, limits.max_alloc_total);
        assert_eq!(got.1, 12_345);
    }

    #[test]
    fn every_packet_is_replayed_in_order() {
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(20);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        assert!(d.report().packets_read > 0);
        let mut seen = Vec::new();
        loop {
            match d.read_packet() {
                Ok(p) => seen.push(p.dts.ticks()),
                Err(Error::Eof) => break,
                Err(e) => unreachable!("{e}"),
            }
        }
        assert_eq!(seen.len(), 20);
        let expect: Vec<Option<i64>> = (0..20).map(|i| Some(i * 100)).collect();
        assert_eq!(seen, expect);
    }

    /// A stream the pass never saw a packet for takes the container's start
    /// time and duration. See [`Discovery::adopt_container_timings`] for the
    /// four Matroska files this rule was measured on.
    #[test]
    fn a_stream_with_no_packets_inherits_the_container_timings() {
        let inner = MockDemuxer::new(2, MediaType::Video)
            .with_packets(10)
            .with_duration(2_000_000);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        // Stream 1 gets no packets from the mock at all.
        let s = &d.streams()[1];
        assert_eq!(s.start_time.ticks(), Some(0));
        assert_eq!(s.duration_ts, Some(2000), "1/1000 ticks");
        // Stream 0 has its own first pts and keeps it, and does *not* get the
        // container duration handed to it — that would overwrite a real
        // measurement with an approximation.
        assert_eq!(d.streams()[0].start_time.ticks(), Some(0));
        assert_eq!(d.streams()[0].duration_ts, None);
    }

    /// A container that states no duration hands out none — the field stays
    /// absent rather than becoming zero.
    #[test]
    fn no_container_duration_means_no_inherited_duration() {
        let inner = MockDemuxer::new(2, MediaType::Video).with_packets(10);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        assert_eq!(d.streams()[1].duration_ts, None);
    }

    /// The estimate fills both printed rates, and only for video.
    #[test]
    fn the_frame_rate_pair_is_video_only() {
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(10);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        // 100 ticks of 1/1000 per packet.
        assert_eq!(d.streams()[0].r_frame_rate, Rational::new(10, 1));
        assert_eq!(d.streams()[0].avg_frame_rate, Rational::new(10, 1));

        let inner = MockDemuxer::new(1, MediaType::Audio).with_packets(10);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        assert_eq!(d.streams()[0].r_frame_rate, Rational::UNDEFINED);
        assert_eq!(d.streams()[0].avg_frame_rate, Rational::UNDEFINED);
    }

    /// A rate the container stated is not replaced by an estimate.
    #[test]
    fn a_stated_frame_rate_survives_the_estimate() {
        let mut inner = MockDemuxer::new(1, MediaType::Video).with_packets(10);
        inner.set_frame_rates(Rational::new(24, 1), Rational::new(24000, 1001));
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        assert_eq!(d.streams()[0].r_frame_rate, Rational::new(24, 1));
        assert_eq!(d.streams()[0].avg_frame_rate, Rational::new(24000, 1001));
    }

    /// R21's duration fill-in must divide the codec-reported `frame_rate` by
    /// [`CodecId::ticks_per_frame`] before inverting it, exactly as the
    /// reference's `ff_compute_frame_duration` divides by `ticks_per_frame`.
    ///
    /// The mock's default video codec is H.264, measured to report a *tick*
    /// rate double the picture rate (issue #632 part 1). 20/1 here stands in
    /// for that tick rate; the true picture rate is 10 fps, so the filled
    /// duration must be 100ms — not the 50ms a naive `1/rate` produces.
    #[test]
    fn r21_divides_the_tick_rate_by_ticks_per_frame() {
        let mut inner = MockDemuxer::new(1, MediaType::Video).with_packets(1);
        inner.set_video_frame_rate(Rational::new(20, 1));
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        let p = d.read_packet().unwrap();
        assert_eq!(p.duration.as_micros(), 100_000, "10 fps, not 20 fps");
    }

    #[test]
    fn discovery_is_idempotent() {
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(5);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        let first = d.report().packets_read;
        d.run(&NoParsers).unwrap();
        assert_eq!(d.report().packets_read, first);
    }

    #[test]
    fn probesize_bounds_the_pass() {
        let mut o = opts();
        o.probesize = 16;
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(1000);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &o);
        let r = d.run(&NoParsers).unwrap().clone();
        assert_eq!(r.stop_reason, StopReason::ProbeSize);
        assert!(r.bytes_read >= 16 && r.bytes_read < 1000 * 8);
    }

    #[test]
    fn packet_cap_bounds_the_pass() {
        let mut o = opts();
        o.max_probe_packets = 3;
        o.probesize = i64::MAX;
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(1000);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &o);
        let r = d.run(&NoParsers).unwrap().clone();
        assert_eq!(r.stop_reason, StopReason::PacketCap);
        assert_eq!(r.packets_read, 3);
    }

    #[test]
    fn analyzeduration_bounds_the_pass() {
        let mut o = opts();
        o.analyzeduration = 500;
        o.probesize = i64::MAX;
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(1000);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &o);
        let r = d.run(&NoParsers).unwrap().clone();
        assert_eq!(r.stop_reason, StopReason::AnalyzeDuration);
    }

    #[test]
    fn eof_before_the_caps_is_reported_as_eof() {
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(4);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        assert_eq!(d.run(&NoParsers).unwrap().stop_reason, StopReason::Eof);
    }

    #[test]
    fn start_time_is_derived_from_the_first_pts() {
        let inner = MockDemuxer::new(1, MediaType::Video)
            .with_packets(6)
            .with_first_pts(3753);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        assert_eq!(d.streams().first().unwrap().start_time.ticks(), Some(3753));
        assert!(d.report().start_time.is_some());
    }

    #[test]
    fn frame_rate_is_estimated_from_dts_deltas() {
        // 1/1000 s time base, one packet every 100 ticks: 10 fps.
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(30);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        let rate = d
            .streams()
            .first()
            .unwrap()
            .params
            .video
            .as_ref()
            .unwrap()
            .frame_rate;
        assert_eq!(rate, Rational::new(10, 1));
    }

    #[test]
    fn seeking_discards_the_replay_queue() {
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(50);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        d.seek(
            crate::SeekTarget::Timestamp {
                stream_index: 0,
                ts: Timestamp::new(0),
            },
            crate::SeekFlags::empty(),
        )
        .unwrap();
        // The very next packet comes from the demuxer, not from before the seek.
        let p = d.read_packet().unwrap();
        assert_eq!(p.dts.ticks(), Some(0));
    }

    #[test]
    fn a_demuxer_with_no_streams_stops_at_the_ts_probe_cap() {
        let mut o = opts();
        o.max_ts_probe = 4;
        let inner = MockDemuxer::new(0, MediaType::Video).with_packets(1000);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &o);
        let r = d.run(&NoParsers).unwrap().clone();
        assert_eq!(r.stop_reason, StopReason::NoStreams);
    }

    #[test]
    fn no_parsers_degrades_gracefully() {
        let inner = MockDemuxer::new(1, MediaType::Audio).with_packets(10);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        // No panic, no error, and the container's own parameters survive.
        d.run(&NoParsers).unwrap();
        assert_eq!(
            d.streams().first().unwrap().params.effective_media_type(),
            Some(MediaType::Audio)
        );
    }

    // ----------------------------------------------------- the parser seam

    /// A provider that records what it was asked for and answers with a parser
    /// whose whole description comes out of the container's record.
    ///
    /// Deliberately not a real codec: the point under test is the *seam* — is a
    /// parser built, is it built once, does it get the extradata, does what it
    /// learns reach the stream — and a real codec would make a failure here
    /// look like a bitstream bug.
    #[derive(Debug, Default)]
    struct CountingProvider {
        built: std::sync::atomic::AtomicUsize,
    }

    #[derive(Debug, Default)]
    struct RecordParser {
        params: Option<vaco_codec_core::CodecParameters>,
        payloads: u32,
    }

    impl vaco_codec_core::Parser for RecordParser {
        fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
            if input.is_empty() {
                return Ok((None, 0));
            }
            self.payloads = self.payloads.saturating_add(1);
            Ok((None, input.len()))
        }

        fn parameters(&self) -> Option<&vaco_codec_core::CodecParameters> {
            self.params.as_ref()
        }

        fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
            // One byte of "record" stands in for a whole `avcC`: the width the
            // container did not state.
            let width = u32::from(*extradata.first().unwrap_or(&0));
            let mut p = vaco_codec_core::CodecParameters::video();
            if let Some(v) = p.video.as_mut() {
                v.width = width;
                v.height = width;
                v.has_b_frames = 2;
            }
            self.params = Some(p);
            Ok(())
        }
    }

    impl ParserProvider for CountingProvider {
        fn parser_for(&self, _codec: CodecId) -> Option<Box<dyn vaco_codec_core::Parser>> {
            self.built
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(Box::new(RecordParser::default()))
        }
    }

    /// The half that was missing, and the reason `-show_streams` reported no
    /// profile, pixel format or channel count on any container: a parser that
    /// is never given the container's configuration record describes nothing,
    /// because in MP4 the sequence parameter set is in `avcC` and in no packet.
    #[test]
    fn the_container_record_reaches_the_parser() {
        let inner = MockDemuxer::new(1, MediaType::Video)
            .with_packets(4)
            .with_extradata(&[64]);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        let p = CountingProvider::default();
        d.run(&p).unwrap();
        let v = d.streams()[0].params.video.as_ref().unwrap();
        assert_eq!(v.width, 64, "the record never reached the parser");
        assert_eq!(v.has_b_frames, 2);
    }

    /// One parser per stream for the whole pass, not one per packet.
    ///
    /// Rebuilding per packet is not merely wasteful: an H.264 NAL unit ends
    /// where the *next* start code begins, so a parser thrown away at the end
    /// of each payload never sees the end of its last unit, and an MPEG-TS
    /// stream's parameter sets arrive in one packet while the fields they
    /// describe are wanted for all of them.
    #[test]
    fn a_stream_gets_exactly_one_parser() {
        // `MockDemuxer` puts every packet on stream 0, so 40 packets is 40
        // chances to build a second parser for the same stream.
        let inner = MockDemuxer::new(1, MediaType::Video).with_packets(40);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        let p = CountingProvider::default();
        d.run(&p).unwrap();
        assert_eq!(
            p.built.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "one parser per stream, however many packets it sees"
        );
    }

    // ------------------------------------------------------- R21b, end to end

    /// A parser that states a packet duration and nothing else.
    ///
    /// 120 samples at 48 kHz is a 2.5 ms Opus packet, which is exactly half a
    /// tick of `MockDemuxer`'s 1/1000 base — the case that separates the
    /// reference's truncation from rounding, and the reason
    /// `Parser::packet_duration` returns an exact ratio rather than a
    /// microsecond count.
    #[derive(Debug, Default)]
    struct TimedParser;

    impl vaco_codec_core::Parser for TimedParser {
        fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
            Ok((None, input.len()))
        }
        fn parameters(&self) -> Option<&vaco_codec_core::CodecParameters> {
            None
        }
        fn packet_duration(&self, packet: &[u8]) -> Option<Rational> {
            (!packet.is_empty()).then(|| Rational::new(120, 48000))
        }
    }

    #[derive(Debug, Default)]
    struct TimedProvider;

    impl ParserProvider for TimedProvider {
        fn parser_for(&self, _codec: CodecId) -> Option<Box<dyn vaco_codec_core::Parser>> {
            Some(Box::new(TimedParser))
        }
    }

    /// R21b — the codec's own duration reaches the packet, truncated into the
    /// stream's time base.
    ///
    /// This is the gap the Matroska/Opus measurement exposed: the container
    /// carries no `DefaultDuration` element and no `BlockDuration`, so nothing
    /// downstream can derive the 20 ms the reference prints. `Discovery` owns
    /// the parsers, so this is where it lands.
    #[test]
    fn a_codec_packet_duration_reaches_every_packet() {
        let inner = MockDemuxer::new(1, MediaType::Audio).with_packets(6);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&TimedProvider).unwrap();
        for n in 0..6 {
            let p = d.read_packet().unwrap();
            // 2.5 ticks truncated, not rounded: 2. Stored as 2000 µs.
            assert_eq!(p.duration.as_micros(), 2000, "packet {n}");
            assert_eq!(p.duration.to_ticks(Rational::new(1, 1000)), Some(2));
        }
    }

    /// The packets read *after* the discovery prefix get it too, from the
    /// parser the prefix built. A rule that only applied to the replay queue
    /// would fill in the first few packets of a file and no others.
    #[test]
    fn the_duration_survives_past_the_discovery_prefix() {
        let mut o = opts();
        o.max_probe_packets = 2;
        let inner = MockDemuxer::new(1, MediaType::Audio).with_packets(40);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &o);
        d.run(&TimedProvider).unwrap();
        let mut seen = 0;
        while let Ok(p) = d.read_packet() {
            assert_eq!(p.duration.as_micros(), 2000, "packet {seen}");
            seen += 1;
        }
        assert_eq!(seen, 40);
    }

    /// The container wins. `BlockDuration`, an `stts` delta and a
    /// `DefaultDuration` are all statements the file made, and R21b only ever
    /// writes over the model's spelling of "absent".
    #[test]
    fn a_stated_duration_is_never_overwritten() {
        #[derive(Debug)]
        struct Stating(MockDemuxer);
        impl Demuxer for Stating {
            fn streams(&self) -> &[Stream] {
                self.0.streams()
            }
            fn read_packet(&mut self) -> Result<Packet> {
                let mut p = self.0.read_packet()?;
                p.duration = Duration::from_micros(40_000);
                Ok(p)
            }
            fn seek(&mut self, t: crate::SeekTarget, f: crate::SeekFlags) -> Result<()> {
                self.0.seek(t, f)
            }
        }
        let inner = Stating(MockDemuxer::new(1, MediaType::Audio).with_packets(4));
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&TimedProvider).unwrap();
        for _ in 0..4 {
            assert_eq!(d.read_packet().unwrap().duration.as_micros(), 40_000);
        }
    }

    /// No parser, no duration — and no failure. A build with
    /// `--no-default-features` reports what the container stated, which is what
    /// `NoParsers` exists to keep testable.
    #[test]
    fn no_parser_leaves_the_duration_absent() {
        let inner = MockDemuxer::new(1, MediaType::Audio).with_packets(4);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        for _ in 0..4 {
            assert_eq!(d.read_packet().unwrap().duration, Duration::ZERO);
        }
    }

    /// `fflags=noparse` must still mean no parser is even asked for.
    #[test]
    fn noparse_asks_for_no_parser_at_all() {
        let mut o = opts();
        o.fflags = o.fflags.union(crate::options::FFlags::NOPARSE);
        let inner = MockDemuxer::new(1, MediaType::Video)
            .with_packets(4)
            .with_extradata(&[64]);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &o);
        let p = CountingProvider::default();
        d.run(&p).unwrap();
        assert_eq!(p.built.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(d.streams()[0].params.video.as_ref().unwrap().width, 0);
    }

    /// A parser that refuses its record, or returns nothing at all, must not
    /// stop the pass: discovery is *offering* the parser what the container
    /// carried, and reporting six streams of seven beats reporting none.
    #[test]
    fn a_parser_that_learns_nothing_is_not_a_failure() {
        #[derive(Debug)]
        struct Refusing;
        impl vaco_codec_core::Parser for Refusing {
            fn parse(&mut self, _input: &[u8]) -> Result<(Option<Packet>, usize)> {
                Err(Error::InvalidData("no"))
            }
            fn parameters(&self) -> Option<&vaco_codec_core::CodecParameters> {
                None
            }
            fn set_extradata(&mut self, _extradata: &[u8]) -> Result<()> {
                Err(Error::InvalidData("no"))
            }
        }
        struct P;
        impl ParserProvider for P {
            fn parser_for(&self, _c: CodecId) -> Option<Box<dyn vaco_codec_core::Parser>> {
                Some(Box::new(Refusing))
            }
        }
        let inner = MockDemuxer::new(1, MediaType::Video)
            .with_packets(20)
            .with_extradata(&[64]);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        let r = d.run(&P).unwrap().clone();
        assert_eq!(r.stop_reason, StopReason::Eof);
        assert_eq!(d.streams().len(), 1);
    }

    // ------------------------------------------ CONFORMANCE-FINDINGS 26, read half

    /// A demuxer that hands out a fixed payload on every packet, instead of
    /// [`MockDemuxer`]'s content-free `[0u8; 8]` — needed here because the
    /// thing under test reads the bytes, not just their length.
    #[derive(Debug)]
    struct FixedPayload {
        inner: MockDemuxer,
        payload: Vec<u8>,
    }

    impl Demuxer for FixedPayload {
        fn streams(&self) -> &[Stream] {
            self.inner.streams()
        }
        fn read_packet(&mut self) -> Result<Packet> {
            let template = self.inner.read_packet()?;
            let mut budget = Budget::new(Limits::permissive());
            let mut p = Packet::from_slice(&mut budget, &self.payload)?;
            p.stream_index = template.stream_index;
            p.pts = template.pts;
            p.dts = template.dts;
            p.flags = template.flags;
            Ok(p)
        }
        fn seek(&mut self, t: SeekTarget, f: SeekFlags) -> Result<()> {
            self.inner.seek(t, f)
        }
    }

    /// H.264 SPS/PPS measured in `planning/CONFORMANCE-FINDINGS.md` finding
    /// 26 (the `a.avi` example), Annex-B framed with a four-byte start code
    /// on both units — the framing AVI's own in-band stream actually uses,
    /// distinct from the *output* convention finding 26 documents.
    fn h264_annexb_sps_pps_slice() -> Vec<u8> {
        let sps = [
            0x67, 0x64, 0x00, 0x0a, 0xac, 0xd9, 0x44, 0x26, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00,
            0x04, 0x00, 0x00, 0x03, 0x00, 0xc8, 0x3c, 0x48, 0x96, 0x58,
        ];
        let pps = [0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];
        let mut buf = vec![0, 0, 0, 1];
        buf.extend_from_slice(&sps);
        buf.extend_from_slice(&[0, 0, 0, 1]);
        buf.extend_from_slice(&pps);
        buf.extend_from_slice(&[0, 0, 0, 1, 0x65, 0xAA, 0xBB]); // an IDR slice
        buf
    }

    /// The read half of finding 26: an AVI-shaped stream — H.264 in-band,
    /// no `avcC`, `nal_length_size` never stated — gets `extradata`
    /// synthesised from the SPS/PPS its own packets carry, exactly as
    /// `avformat_find_stream_info` does by running `extract_extradata`.
    ///
    /// Runs with [`NoParsers`], deliberately: the point under test is that
    /// this rule does not need a `vaco-parse-h264` at all, only the raw
    /// bytes discovery already has (see [`synthesize_extradata`]'s docs).
    #[test]
    fn h264_extradata_is_synthesised_from_in_band_parameter_sets() {
        let inner = FixedPayload {
            inner: MockDemuxer::new(1, MediaType::Video).with_packets(3),
            payload: h264_annexb_sps_pps_slice(),
        };
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();

        let extra = d.streams()[0].params.extradata.clone().unwrap();
        let mut expected = vec![0, 0, 1]; // first unit: three-byte start code
        expected.extend_from_slice(&h264_annexb_sps_pps_slice()[4..4 + 24]); // sps
        expected.extend_from_slice(&[0, 0, 0, 1]); // later unit: four-byte
        expected.extend_from_slice(&h264_annexb_sps_pps_slice()[32..38]); // pps
        assert_eq!(extra, expected);
    }

    /// Falsifies the naive reading: if extraction used a four-byte start
    /// code on the first unit too, this would still pass. It must not.
    #[test]
    fn falsified_a_naive_four_byte_first_start_code_would_be_wrong() {
        let inner = FixedPayload {
            inner: MockDemuxer::new(1, MediaType::Video).with_packets(1),
            payload: h264_annexb_sps_pps_slice(),
        };
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        let extra = d.streams()[0].params.extradata.clone().unwrap();
        assert_eq!(&extra[..3], &[0, 0, 1], "first unit must be three bytes");
        assert_ne!(&extra[..4], &[0, 0, 0, 1]);
    }

    /// A one-stream HEVC demuxer that emits a fixed payload on every packet.
    /// Not built on [`MockDemuxer`], which hardcodes `CodecId::H264` for
    /// video and has no public way to change it.
    #[derive(Debug)]
    struct HevcDemuxer {
        streams: [Stream; 1],
        payload: Vec<u8>,
        remaining: u64,
        budget: Budget,
    }

    impl HevcDemuxer {
        fn new(payload: Vec<u8>, packets: u64) -> Self {
            let mut params = vaco_codec_core::CodecParameters::video();
            params = params.with_codec(CodecId::Hevc);
            let mut s = Stream::new(0, MediaType::Video, Rational::new(1, 1000));
            s.params = params;
            Self {
                streams: [s],
                payload,
                remaining: packets,
                budget: Budget::new(Limits::permissive()),
            }
        }
    }

    impl Demuxer for HevcDemuxer {
        fn streams(&self) -> &[Stream] {
            &self.streams
        }
        fn read_packet(&mut self) -> Result<Packet> {
            if self.remaining == 0 {
                return Err(Error::Eof);
            }
            self.remaining -= 1;
            let mut p = Packet::from_slice(&mut self.budget, &self.payload)?;
            p.stream_index = 0;
            p.flags = PacketFlags::KEY;
            Ok(p)
        }
        fn seek(&mut self, _t: SeekTarget, _f: SeekFlags) -> Result<()> {
            Err(Error::NotSeekable)
        }
    }

    /// HEVC's identical fault, identical fix. No container/HEVC combination
    /// on this machine produces Annex-B extradata to measure against
    /// (finding 26's own note), so this is a synthetic VPS/SPS/PPS rather
    /// than a captured file — the assembly rule itself is what
    /// `vaco_format_nalu::extradata`'s tests check against measured bytes.
    #[test]
    fn hevc_extradata_is_synthesised_from_in_band_parameter_sets() {
        let vps = [0x40, 0x01, 0x0c, 0x01];
        let sps = [0x42, 0x01, 0x01, 0x02];
        let pps = [0x44, 0x01, 0xc0];
        let mut payload = Vec::new();
        for unit in [&vps[..], &sps[..], &pps[..]] {
            payload.extend_from_slice(&[0, 0, 0, 1]);
            payload.extend_from_slice(unit);
        }
        payload.extend_from_slice(&[0, 0, 0, 1, 0x02, 0x01]); // a slice, ignored

        let inner = HevcDemuxer::new(payload, 1);
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();

        let extra = d.streams()[0].params.extradata.clone().unwrap();
        let mut expected = vec![0, 0, 1];
        expected.extend_from_slice(&vps);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&sps);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&pps);
        assert_eq!(extra, expected);
    }

    /// ASF's half of finding 26: the container already supplies an Annex-B
    /// configuration record, so nothing here should touch it — synthesis is
    /// only for a stream discovery finds with no extradata of its own.
    #[test]
    fn a_container_supplied_extradata_is_never_overwritten() {
        let inner = FixedPayload {
            inner: MockDemuxer::new(1, MediaType::Video)
                .with_packets(3)
                .with_extradata(&[0, 0, 0, 1, 0x67, 0x11, 0x22]),
            payload: h264_annexb_sps_pps_slice(),
        };
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        assert_eq!(
            d.streams()[0].params.extradata.as_deref(),
            Some(&[0, 0, 0, 1, 0x67, 0x11, 0x22][..])
        );
    }

    /// A packet with no parameter sets in it — an ordinary non-keyframe —
    /// must not manufacture extradata out of nothing.
    #[test]
    fn a_payload_with_no_parameter_sets_synthesises_nothing() {
        let inner = FixedPayload {
            inner: MockDemuxer::new(1, MediaType::Video).with_packets(2),
            payload: vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB], // slice only
        };
        let mut d = Discovery::new(inner, FormatFlags::empty(), &opts());
        d.run(&NoParsers).unwrap();
        assert!(d.streams()[0].params.extradata.is_none());
    }

    /// `fflags=noparse` disables bitstream inspection outright — extradata
    /// synthesis is exactly that, so it must be disabled with everything
    /// else the flag turns off.
    #[test]
    fn noparse_also_disables_extradata_synthesis() {
        let mut o = opts();
        o.fflags = o.fflags.union(crate::options::FFlags::NOPARSE);
        let inner = FixedPayload {
            inner: MockDemuxer::new(1, MediaType::Video).with_packets(2),
            payload: h264_annexb_sps_pps_slice(),
        };
        let mut d = Discovery::new(inner, FormatFlags::empty(), &o);
        d.run(&NoParsers).unwrap();
        assert!(d.streams()[0].params.extradata.is_none());
    }
}
