//! Packet and byte accounting for an output, plus (now redundant — see below)
//! this crate's original standalone `null` muxer.
//!
//! # This module predates the container wave
//!
//! It was written when D5 still put **zero muxers** in v0.1: `crates/format/`
//! held three `vaco-demux-*` crates and no `vaco-mux-*`, so [`NullMuxer`] was
//! the only way to make the spine — protocol → probe → demux → discovery →
//! selection → `vaco-sched` → sink — runnable and observable at all, and
//! [`NULL_MUXER`] was never registered anywhere because there was no registry
//! entry to put it in.
//!
//! Both of those are false now. `vaco-mux-utility::MUXER_NULL` is a real,
//! registered `-f null` descriptor (`vaco-component.toml` in that crate), and
//! [`crate::exec::muxer_for`] resolves `null` through `vaco_registry` like
//! every other format rather than special-casing it. **[`NullMuxer`] and
//! [`NULL_MUXER`] are therefore redundant** — kept rather than deleted, per
//! this crate's standing instruction not to remove a module another agent's
//! work might still be reaching for, and because they cost nothing to leave:
//! nothing in `exec.rs` constructs them any more.
//!
//! What is *not* redundant is the counting itself. [`Sink`] and
//! [`OutputTally`] are the source of the end-of-run summary line for every
//! output now, real or `null` — see [`TallyingMuxer`], which wraps whatever
//! [`vaco_registry::muxer_by_name`] returned and counts what actually reaches
//! its `write_packet`, rather than a separate counter that might drift from
//! it.
//!
//! # Deliberately no default encoders on the local copy
//!
//! The reference's `null` muxer declares `wrapped_avframe` and `pcm_s16le` as
//! its defaults, so `-f null -` transcodes. This build had no encoders and no
//! decoders when [`NULL_MUXER`] was written, so declaring defaults it could not
//! honour would have moved the failure from a clear message to a confusing
//! one — it declares none, which puts an output with no `-c copy` on the
//! reference's own "Default encoder for format null (codec none) is probably
//! disabled" path. `vaco-mux-utility::MUXER_NULL` made the more accurate call
//! once it existed: `default_audio` is `Some(PcmS16le)`, because that codec is
//! representable in this workspace and a container's own defaults are a fact
//! about the container, not about which encoders happen to be built yet.

use std::sync::{Arc, Mutex};

use vaco_codec_core::CodecParameters;
use vaco_core::{MediaType, Rational, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::{Muxer, MuxerDesc, StreamSpec};
use vaco_io::MediaSink;
use vaco_packet::Packet;

/// Per-stream totals, in the order streams were added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamTally {
    pub media: Option<MediaType>,
    pub packets: u64,
    pub bytes: u64,
}

/// What a run pushed through one output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputTally {
    pub streams: Vec<StreamTally>,
    pub header_written: bool,
    pub trailer_written: bool,
}

impl OutputTally {
    /// Total payload bytes for one media type, as the summary line reports it.
    #[must_use]
    pub fn bytes_of(&self, media: MediaType) -> u64 {
        self.streams
            .iter()
            .filter(|s| s.media == Some(media))
            .map(|s| s.bytes)
            .sum()
    }

    #[must_use]
    pub fn packets(&self) -> u64 {
        self.streams.iter().map(|s| s.packets).sum()
    }
}

/// A shared handle to a [`NullMuxer`]'s counters.
///
/// `vaco-sched` takes the muxer by value into the pipeline and never gives it
/// back, so the counts have to be reachable from outside it. `Arc<Mutex<…>>`
/// rather than an atomic because the whole `OutputTally` is read at once and a
/// half-updated one would be a lie; the lock is taken once per packet, on a
/// path that already does an allocation-free `Vec` index.
#[derive(Debug, Clone, Default)]
pub struct Sink(Arc<Mutex<OutputTally>>);

impl Sink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot. Returns the default tally if the lock was poisoned, which
    /// can only happen if a node panicked — and a panic is already a defect we
    /// report rather than a state we depend on.
    #[must_use]
    pub fn tally(&self) -> OutputTally {
        self.0.lock().map(|t| t.clone()).unwrap_or_default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut OutputTally) -> R) -> Option<R> {
        self.0.lock().ok().map(|mut t| f(&mut t))
    }
}

/// A [`Muxer`] that discards every packet and counts what it discarded.
#[derive(Debug)]
pub struct NullMuxer {
    sink: Sink,
    next_index: u32,
}

impl NullMuxer {
    #[must_use]
    pub fn new(sink: Sink) -> Self {
        Self {
            sink,
            next_index: 0,
        }
    }
}

impl Muxer for NullMuxer {
    fn flags(&self) -> FormatFlags {
        FLAGS
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        let index = self.next_index;
        self.next_index += 1;
        self.sink.with(|t| {
            t.streams.push(StreamTally {
                media: params.media_type,
                packets: 0,
                bytes: 0,
            });
        });
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        self.sink.with(|t| t.header_written = true);
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        self.sink.with(|t| {
            if let Some(s) = t.streams.get_mut(packet.stream_index as usize) {
                s.packets += 1;
                s.bytes += packet.len as u64;
            }
        });
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.sink.with(|t| t.trailer_written = true);
        Ok(())
    }

    fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
        // No opinion: the null format has no timescale of its own, so packets
        // stay in whatever base they arrived in and `vaco-sched` falls back to
        // `TIME_BASE_Q`. Reporting a base we do not use would silently rescale
        // every timestamp for no reason.
        None
    }
}

/// The container flags `-f null` is declared with.
///
/// # What this sink actually wants, and why it cannot have it
///
/// A sink that writes nothing has no ordering requirement, so the right flag is
/// `NOTIMESTAMPS` — which is what the reference's own `null` muxer declares.
/// **It cannot be used here.** In `vaco_format_core::interleave`,
/// `MuxTimestamps::apply` under `NOTIMESTAMPS` sets `pts` and `dts` to
/// `Timestamp::NONE` and returns; `InterleaveQueue::push` then rejects exactly
/// that packet with "packet has no dts; interleaving cannot order it". Two
/// functions in one module, each correct alone and contradictory together.
/// Reported; see `docs/app/vaco-cli.md`.
///
/// So the set below is the most permissive one that works with
/// `vaco-format-core` as it stands. `TS_NONSTRICT` turns
/// `requires_strict_dts()` off, which matters because an **empty** flag set
/// means *strict* — the default is the strictest container, not the loosest
/// one.
///
pub const FLAGS: FormatFlags = FormatFlags::NOFILE
    .union(FormatFlags::VARIABLE_FPS)
    .union(FormatFlags::TS_NONSTRICT)
    .union(FormatFlags::TS_NEGATIVE)
    .union(FormatFlags::NOTIMESTAMPS);

/// The descriptor `-f null` resolves to.
///
/// `default_video`/`default_audio` are deliberately `None`; see the module
/// docs. The `open` function ignores its sink, because there is nothing to
/// write — `-f null out.bin` leaves `out.bin` empty in the reference too.
pub static NULL_MUXER: MuxerDesc = MuxerDesc {
    name: "null",
    long_name: "raw null video",
    extensions: &[],
    default_video: None,
    default_audio: None,
    open: open_null,
};

#[expect(
    clippy::unnecessary_wraps,
    reason = "the signature is MuxerDesc::open's, which every container shares"
)]
fn open_null(_sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(NullMuxer::new(Sink::new())))
}

/// Wraps any [`Muxer`] the registry produced and counts what actually reaches
/// [`Muxer::write_packet`], into the same [`Sink`]/[`OutputTally`] shape
/// [`NullMuxer`] used to keep for itself.
///
/// # Why counting here rather than trusting the container's own byte count
///
/// `vaco-sched`'s mux node does now drive a
/// [`vaco_format_core::mux::MuxWriter`] (gap 8, `planning/INTERFACE-GAPS.md`,
/// closed), so `vaco_format_core::mux::MuxReport` exists for every run — but
/// it is returned from `MuxWriter::finish`, deep inside the pipeline, and
/// `vaco-sched` does not thread a per-output report back out to its caller.
/// Adding that channel is a `vaco-sched` API change this crate did not need:
/// this type already counts *real* writes without it, and does so at a finer
/// grain than `MuxReport` does (per media type, not just per stream) — every
/// packet handed to `write_packet` here is the same packet the inner muxer
/// receives and, `write_packet` having returned successfully, has already
/// turned into bytes on the wire or on disk.
pub struct TallyingMuxer {
    inner: Box<dyn Muxer>,
    sink: Sink,
}

impl core::fmt::Debug for TallyingMuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TallyingMuxer").finish_non_exhaustive()
    }
}

impl TallyingMuxer {
    #[must_use]
    pub fn new(inner: Box<dyn Muxer>, sink: Sink) -> Self {
        Self { inner, sink }
    }
}

impl Muxer for TallyingMuxer {
    fn flags(&self) -> FormatFlags {
        self.inner.flags()
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        let index = self.inner.add_stream(params)?;
        self.sink.with(|t| {
            t.streams.push(StreamTally {
                media: params.media_type,
                packets: 0,
                bytes: 0,
            });
        });
        Ok(index)
    }

    // Forwarded explicitly, not inherited from the default — same trap as
    // `impl Muxer for Box<M>` (`vaco-format-core`'s doc comment on
    // `Muxer::add_stream_with`). Every real muxer this crate opens is wrapped
    // in a `TallyingMuxer` before `vaco-sched` ever sees it (`run_pipeline` in
    // `exec.rs`), so inheriting the default here would silently discard the
    // input time base `MuxBuilder::add_stream` now passes down (gap 9) for
    // every output this build writes, regardless of what the wrapped muxer
    // overrides.
    fn add_stream_with(&mut self, params: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
        let index = self.inner.add_stream_with(params, spec)?;
        self.sink.with(|t| {
            t.streams.push(StreamTally {
                media: params.media_type,
                packets: 0,
                bytes: 0,
            });
        });
        Ok(index)
    }

    fn init(&mut self) -> Result<()> {
        self.inner.init()
    }

    fn write_header(&mut self) -> Result<()> {
        self.inner.write_header()?;
        self.sink.with(|t| t.header_written = true);
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        self.inner.write_packet(packet)?;
        self.sink.with(|t| {
            if let Some(s) = t.streams.get_mut(packet.stream_index as usize) {
                s.packets += 1;
                s.bytes += packet.len as u64;
            }
        });
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.inner.write_trailer()?;
        self.sink.with(|t| t.trailer_written = true);
        Ok(())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        self.inner.stream_time_base(stream_index)
    }

    fn interleave(
        &mut self,
        queue: &mut vaco_format_core::interleave::InterleaveQueue,
        packet: Option<Packet>,
        flush: bool,
    ) -> Result<Option<Packet>> {
        self.inner.interleave(queue, packet, flush)
    }

    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        packet: &Packet,
    ) -> Result<vaco_format_core::mux::BitstreamAction> {
        self.inner.check_bitstream(params, packet)
    }

    fn query_codec(
        &self,
        codec: vaco_codec_core::CodecId,
        strict: i32,
    ) -> vaco_format_core::mux::CodecSupport {
        self.inner.query_codec(codec, strict)
    }

    fn write_flush(&mut self) -> Result<()> {
        self.inner.write_flush()
    }

    fn set_metadata(&mut self, metadata: &vaco_format_core::metadata::MuxMetadata) -> Result<()> {
        // Not overriding this would silently fall through to `Muxer::set_metadata`'s
        // own no-op default *on this wrapper*, dropping every `-metadata` on
        // the floor without an error — `TallyingMuxer` wraps the container
        // that actually needs to see it (CL-16, gap 1).
        self.inner.set_metadata(metadata)
    }

    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        self.inner.set_option(name, value)
    }

    fn set_bitexact(&mut self, bitexact: bool) {
        // Same reasoning as `set_metadata` above: the default is a silent
        // no-op, and `TallyingMuxer` sits between every real muxer this
        // binary opens and `MuxBuilder::open`'s call to this method.
        self.inner.set_bitexact(bitexact);
    }

    // Forwarded explicitly, not inherited from the default — same trap as
    // `add_stream_with` above: the default always answers `Unsupported`,
    // which would make every `NEEDNUMBER` muxer `open_output` opens
    // (image2, and any future segmenting muxer) look unsupported regardless
    // of what the wrapped muxer actually implements.
    fn bind_url(&mut self, url: &str) -> Result<()> {
        self.inner.bind_url(url)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    fn params(media: MediaType) -> CodecParameters {
        CodecParameters::new(media)
    }

    fn packet(len: usize) -> Packet {
        let mut budget = Budget::new(Limits::permissive());
        Packet::from_slice(&mut budget, &vec![0u8; len]).unwrap_or_else(|_| Packet::empty())
    }

    #[test]
    fn counts_per_stream_and_per_media_type() {
        let sink = Sink::new();
        let mut m = NullMuxer::new(sink.clone());
        assert_eq!(m.add_stream(&params(MediaType::Video)).unwrap(), 0);
        assert_eq!(m.add_stream(&params(MediaType::Audio)).unwrap(), 1);
        m.write_header().unwrap();

        let mut p = packet(100);
        p.stream_index = 0;
        m.write_packet(&p).unwrap();
        m.write_packet(&p).unwrap();
        let mut q = packet(40);
        q.stream_index = 1;
        m.write_packet(&q).unwrap();
        m.write_trailer().unwrap();

        let t = sink.tally();
        assert!(t.header_written && t.trailer_written);
        assert_eq!(t.packets(), 3);
        assert_eq!(t.bytes_of(MediaType::Video), 200);
        assert_eq!(t.bytes_of(MediaType::Audio), 40);
        assert_eq!(t.streams.len(), 2);
    }

    #[test]
    fn a_packet_naming_a_stream_that_does_not_exist_is_dropped_not_a_panic() {
        // The muxer sees whatever the pipeline hands it; an out-of-range index
        // must not index out of bounds. `indexing_slicing` is denied
        // workspace-wide precisely so this cannot be written the wrong way.
        let sink = Sink::new();
        let mut m = NullMuxer::new(sink.clone());
        let mut p = packet(8);
        p.stream_index = 7;
        m.write_packet(&p).unwrap();
        assert_eq!(sink.tally().packets(), 0);
    }

    #[test]
    fn the_flags_relax_the_strict_dts_requirement() {
        // Not decoration. An *empty* flag set means `requires_strict_dts()`,
        // which is the strictest container, and a null sink is the loosest.
        assert!(FormatFlags::empty().requires_strict_dts());
        assert!(!FLAGS.requires_strict_dts());
        // NOTIMESTAMPS is present now that `vaco-format-core` accepts what it
        // implies: `MuxTimestamps::apply` clears the fields and
        // `InterleaveQueue::without_timestamps` takes the result. It was
        // omitted before because the two disagreed and this was the only
        // workaround available from inside this crate.
        assert!(FLAGS.contains(FormatFlags::NOTIMESTAMPS));
    }

    #[test]
    fn the_descriptor_declares_no_default_encoders() {
        assert!(NULL_MUXER.default_codec(MediaType::Video).is_none());
        assert!(NULL_MUXER.default_codec(MediaType::Audio).is_none());
        assert!(NULL_MUXER.matches_name("null"));
    }

    #[test]
    fn tallying_muxer_counts_what_the_inner_muxer_actually_accepted() {
        // The inner muxer here is the same `NullMuxer` these tests already
        // exercise, standing in for "any real container" — `TallyingMuxer`
        // does not know or care what is underneath it, which is the point.
        let inner_sink = Sink::new();
        let inner = Box::new(NullMuxer::new(inner_sink));
        let outer_sink = Sink::new();
        let mut m = TallyingMuxer::new(inner, outer_sink.clone());

        assert_eq!(m.add_stream(&params(MediaType::Video)).unwrap(), 0);
        m.write_header().unwrap();
        let mut p = packet(321);
        p.stream_index = 0;
        m.write_packet(&p).unwrap();
        m.write_trailer().unwrap();

        let t = outer_sink.tally();
        assert!(t.header_written && t.trailer_written);
        assert_eq!(t.packets(), 1);
        assert_eq!(t.bytes_of(MediaType::Video), 321);
    }

    /// A double that records what it was actually handed, through shared
    /// state readable after `m` (which owns the `Box<dyn Muxer>`) has moved
    /// it away — to prove `TallyingMuxer` forwards the two gap-9-shaped
    /// methods rather than falling through to their no-op defaults *on the
    /// wrapper itself*. That is the same trap
    /// `vaco_format_core::Muxer::add_stream_with`'s doc comment records for
    /// `impl Muxer for Box<M>`, and `TallyingMuxer` sits in exactly the same
    /// position in every real pipeline this crate builds.
    #[derive(Clone, Default)]
    struct Received(Arc<Mutex<(Option<vaco_core::Rational>, Option<bool>)>>);

    struct RecordingMuxer(Received);

    impl Muxer for RecordingMuxer {
        fn add_stream(&mut self, _: &CodecParameters) -> Result<u32> {
            Ok(0)
        }
        fn add_stream_with(
            &mut self,
            _params: &CodecParameters,
            spec: &StreamSpec,
        ) -> Result<u32> {
            self.0.0.lock().unwrap_or_else(|e| e.into_inner()).0 = spec.time_base;
            Ok(0)
        }
        fn set_bitexact(&mut self, bitexact: bool) {
            self.0.0.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(bitexact);
        }
        fn write_header(&mut self) -> Result<()> {
            Ok(())
        }
        fn write_packet(&mut self, _: &Packet) -> Result<()> {
            Ok(())
        }
        fn write_trailer(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn tallying_muxer_forwards_add_stream_with_and_set_bitexact() {
        let received = Received::default();
        let inner = Box::new(RecordingMuxer(received.clone()));
        let mut m = TallyingMuxer::new(inner, Sink::new());

        let spec = StreamSpec {
            time_base: Some(vaco_core::Rational::new(1, 12_800)),
        };
        m.add_stream_with(&params(MediaType::Video), &spec)
            .unwrap();
        m.set_bitexact(true);

        let got = received.0.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(got.0, Some(vaco_core::Rational::new(1, 12_800)));
        assert_eq!(got.1, Some(true));
    }

    #[test]
    fn tallying_muxer_forwards_a_failure_without_tallying_it() {
        struct AlwaysFails;
        impl Muxer for AlwaysFails {
            fn add_stream(&mut self, _: &CodecParameters) -> Result<u32> {
                Ok(0)
            }
            fn write_header(&mut self) -> Result<()> {
                Ok(())
            }
            fn write_packet(&mut self, _: &Packet) -> Result<()> {
                Err(vaco_core::Error::InvalidData("refused"))
            }
            fn write_trailer(&mut self) -> Result<()> {
                Ok(())
            }
        }
        let sink = Sink::new();
        let mut m = TallyingMuxer::new(Box::new(AlwaysFails), sink.clone());
        m.add_stream(&params(MediaType::Audio)).unwrap();
        m.write_header().unwrap();
        let mut p = packet(10);
        p.stream_index = 0;
        assert!(m.write_packet(&p).is_err());
        // A write that failed is not a write that happened.
        assert_eq!(sink.tally().packets(), 0);
    }
}
