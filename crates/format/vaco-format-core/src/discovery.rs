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

use vaco_codec_core::{CodecId, CodecProperties, ParserDriver};
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
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
    complete: bool,
}

/// A [`Demuxer`] that has read ahead, learned what it could, and will replay
/// every packet it consumed.
#[derive(Debug)]
pub struct Discovery<D> {
    inner: D,
    streams: Vec<Stream>,
    state: Vec<StreamState>,
    queue: VecDeque<Packet>,
    fixer: TimestampFixer,
    opts: FormatOptions,
    flags: FormatFlags,
    limits: Limits,
    report: DiscoveryReport,
    ran: bool,
}

impl<D: Demuxer> Discovery<D> {
    /// Wrap `inner`. Nothing is read until [`Discovery::run`] is called, so
    /// constructing one is free and a caller that does not want the pass simply
    /// does not run it.
    #[must_use]
    pub fn new(inner: D, flags: FormatFlags, opts: &FormatOptions) -> Self {
        let streams = inner.streams().to_vec();
        let n = streams.len();
        let mut fixer = TimestampFixer::new(n, flags, opts);
        for s in &streams {
            let delay = s.params.video.as_ref().map_or(0, |v| v.has_b_frames);
            let reorders = s
                .params
                .codec_id
                .is_some_and(|c| c.properties().contains(CodecProperties::REORDER));
            fixer.set_stream_delay(s.index, delay, reorders);
        }
        Self {
            inner,
            state: vec![StreamState::default(); n],
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
        let (Some(stream), Some(st)) = (self.streams.get_mut(i), self.state.get_mut(i)) else {
            return;
        };
        let time_base = stream.time_base;
        let rate = stream
            .params
            .video
            .as_ref()
            .map_or(Rational::ZERO, |v| v.frame_rate);
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

        // Refine parameters from the payload, without decoding it.
        let cap = u32::try_from(self.opts.max_probe_packets).unwrap_or(u32::MAX);
        if !self.opts.fflags.contains(crate::options::FFlags::NOPARSE)
            && st.parser_packets < cap
            && let Some(id) = stream.params.codec_id
            && self.opts.codec_allowed(id.name())
        {
            st.parser_packets = st.parser_packets.saturating_add(1);
            refine(stream, id, pkt.payload(), parsers, limits);
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
            // Average frame rate from the mean DTS delta. `Stream` has no
            // avg_frame_rate/r_frame_rate pair, so the estimate lands on the
            // codec parameters — see the docs file.
            let enough = fps_probe == 0 || st.delta_count >= fps_probe;
            if enough
                && st.delta_count > 0
                && let Some(v) = stream.params.video.as_mut()
                && (!v.frame_rate.is_defined() || v.frame_rate.is_zero())
            {
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
                    v.frame_rate = rate;
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
            self.streams.iter().filter_map(|s| s.duration).max();
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
    }
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

/// Ask the injected provider for a parser and let it fill in the blanks.
///
/// The direction is load-bearing: the container's own metadata wins and the
/// parser only supplies what the container left blank
/// ([`vaco_codec_core::CodecParameters::fill_from`]). Inverting it is how a
/// stream whose bitstream header disagrees with its container ends up reported
/// wrongly.
///
/// Driven through [`ParserDriver`], which owns reassembly across payload
/// boundaries, the empty-slice end-of-stream convention and the
/// consumed-bytes check. Doing that by hand here — as this did before
/// `vaco-codec-core` grew `impl<P: Parser + ?Sized> Parser for Box<P>` — meant
/// a second implementation of the same convention, which is precisely how the
/// two drift apart.
fn refine(
    stream: &mut Stream,
    id: CodecId,
    payload: &[u8],
    parsers: &dyn ParserProvider,
    limits: Limits,
) {
    if payload.is_empty() {
        return;
    }
    let Some(parser) = parsers.parser_for(id) else {
        return;
    };
    let mut driver = ParserDriver::new(parser, limits);
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
                (
                    s.time_base,
                    s.params
                        .video
                        .as_ref()
                        .map_or(Rational::ZERO, |v| v.frame_rate),
                )
            });
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
    use vaco_core::MediaType;

    fn opts() -> FormatOptions {
        FormatOptions::default()
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
}
