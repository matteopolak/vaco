//! The `null` output: a muxer that writes nothing and counts everything.
//!
//! # Why this exists
//!
//! D5 puts **zero muxers** in v0.1, and `crates/format/` contains three
//! `vaco-demux-*` crates and no `vaco-mux-*`. So `vaco -i in.mp4 out.mkv`
//! cannot produce a file, and a binary that pretended otherwise would look
//! finished and do nothing.
//!
//! `null` is the honest half of that. It is a real format in the reference
//! (`ffmpeg -i in.mkv -c copy -f null -` exits 0 and prints
//! `video:7KiB audio:16KiB …`), it needs no container knowledge at all, and it
//! makes the whole spine — protocol → probe → demux → discovery → selection →
//! `vaco-sched` → sink — runnable and *observable*. The counts it keeps are the
//! acceptance surface: with no bytes to compare, "the same packets reached the
//! output" is the strongest statement available.
//!
//! The other half is [`crate::exec::muxer_for`], which refuses every other
//! format with a message naming the real reason instead of a generic failure.
//!
//! # Deliberately no default encoders
//!
//! The reference's `null` muxer declares `wrapped_avframe` and `pcm_s16le` as
//! its defaults, so `-f null -` transcodes. This build has no encoders and no
//! decoders, so declaring defaults it cannot honour would move the failure from
//! a clear message to a confusing one. [`NULL_MUXER`] declares none, which puts
//! an output with no `-c copy` on the reference's own
//! "Default encoder for format null (codec none) is probably disabled" path —
//! the error the reference itself emits for a build that lacks an encoder,
//! which is exactly our situation.

use std::sync::{Arc, Mutex};

use vaco_codec_core::CodecParameters;
use vaco_core::{MediaType, Rational, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::{Muxer, MuxerDesc};
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
/// # The divergence this leaves
///
/// [`REORDERED_VIDEO_DIVERGENCE`] describes what still fails.
pub const FLAGS: FormatFlags = FormatFlags::NOFILE
    .union(FormatFlags::VARIABLE_FPS)
    .union(FormatFlags::TS_NONSTRICT)
    .union(FormatFlags::TS_NEGATIVE);

/// Reordered video cannot be streamcopied by this build, and the reason is not
/// in this crate.
///
/// Matroska stores no DTS. `vaco_format_core::time`'s rule R19 correctly
/// declines to set `dts = pts` for a codec that reorders, and no rule then
/// supplies one — R20 generates PTS *from* DTS through `push_reorder`, and the
/// mirror rule does not exist. So every packet of a reordered H.264 track
/// reaches the mux side with `dts = None`, even though
/// `params.video.has_b_frames` is `Some(2)` and `set_stream_delay` has already
/// been told. The reference reconstructs it: `ffprobe -show_entries
/// packet=pts,dts` on the same file prints `dts = N/A, N/A, 0, 40, 80, …`
/// against `pts = 0, 160, 80, 40, 120, …`.
///
/// `MuxTimestamps` then fills the missing DTS from PTS, and the fabricated
/// sequence `0, 160, 80` is decreasing, so the run stops with
/// "decreasing dts: this container requires non-decreasing timestamps".
///
/// Files without frame reordering — audio, intra-only video, `-bf 0` H.264 —
/// stream-copy end to end today.
///
/// The name exists so a conformance audit finds it, per D17.1's pattern. It is
/// **not** pinned by a synthetic fixture, and that is deliberate rather than an
/// omission: a synthetic Matroska track carries no `CodecPrivate`, so nothing
/// marks its codec as reordering and `DemuxTimestamps` takes the R19 path
/// instead. The divergence was established against a reference-produced H.264
/// file; the adjacent behaviour that *is* reachable synthetically — R22's
/// silent repair — is pinned in
/// `tests::a_reordered_pts_sequence_on_a_non_reordering_codec_is_repaired_not_refused`.
pub const REORDERED_VIDEO_DIVERGENCE: &str = "streamcopy of a reordered video stream fails: the container states no DTS and \
     nothing reconstructs it from has_b_frames";

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
        // And NOTIMESTAMPS is deliberately absent — see the constant's docs.
        assert!(!FLAGS.contains(FormatFlags::NOTIMESTAMPS));
        assert!(!REORDERED_VIDEO_DIVERGENCE.is_empty());
    }

    #[test]
    fn the_descriptor_declares_no_default_encoders() {
        assert!(NULL_MUXER.default_codec(MediaType::Video).is_none());
        assert!(NULL_MUXER.default_codec(MediaType::Audio).is_none());
        assert!(NULL_MUXER.matches_name("null"));
    }
}
