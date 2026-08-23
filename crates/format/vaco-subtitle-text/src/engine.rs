//! The one generic demuxer and the one generic muxer every format in this
//! crate is built from.
//!
//! Sixteen-plus formats share exactly one demux-side shape: read the whole
//! file (these are never more than a few hundred kilobytes), sniff its
//! encoding, parse it into [`Cue`]s, and hand them out as packets in file
//! order. [`CueDemuxer`] is that shape, written once. Every format's `open`
//! function differs only in *how bytes become cues*, which is
//! [`open_generic`]'s `parse` parameter.
//!
//! The mux side is more heterogeneous — different formats need per-cue
//! numbering, a running "previous end" for gap-filling, or none of that — so
//! it is a small trait, [`CueMux`], rather than one closed function. Still one
//! driver, [`GenericTextMuxer`], for every implementation of it.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{Demuxer, Muxer, Stream};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use vaco_format_subtitle::Cue;

/// A demuxer never reads more than this many bytes for one file before giving
/// up. Subtitle files are text and never legitimately this large; the cap
/// exists so a hostile or endless pipe cannot grow this crate's one
/// whole-file buffer without bound.
pub const MAX_SUBTITLE_BYTES: usize = 256 * 1024 * 1024;

/// Flags shared by every demuxer in this crate: no byte-seek support and
/// neither generic strategy, because [`CueDemuxer::seek`] resolves every
/// timestamp target itself against the in-memory cue list rather than through
/// the core's index or bisection paths. Deliberately **not**
/// [`FormatFlags::empty`] — `vaco-probe`'s registration test rejects that.
pub const DEMUX_FLAGS: FormatFlags = FormatFlags::NOBINSEARCH
    .union(FormatFlags::NOGENSEARCH)
    .union(FormatFlags::NO_BYTE_SEEK);

/// Flags shared by every muxer in this crate. `TS_NONSTRICT` because two cues
/// legitimately share a start time (overlapping dialogue is routine in ASS,
/// and two SAMI `SYNC` blocks can share a `Start`), so DTS need only be
/// non-decreasing, not strictly increasing.
pub const MUX_FLAGS: FormatFlags = FormatFlags::TS_NONSTRICT;

/// Read the rest of `io` into memory, bounded by [`MAX_SUBTITLE_BYTES`].
///
/// # Errors
/// [`Error::LimitExceeded`] past the cap; otherwise whatever the transport
/// reports.
pub fn read_all(io: &mut IoContext) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let n = io.read_partial(&mut chunk)?;
        if n == 0 {
            return Ok(buf);
        }
        if buf.len().saturating_add(n) > MAX_SUBTITLE_BYTES {
            return Err(Error::LimitExceeded {
                limit: "subtitle_file_bytes",
                requested: (buf.len().saturating_add(n)) as u64,
                cap: MAX_SUBTITLE_BYTES as u64,
            });
        }
        buf.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
    }
}

/// Open a [`CueDemuxer`] by reading all of `src`, sniffing its encoding, and
/// handing the UTF-8 bytes to `parse`.
///
/// `parse` is expected to be lenient — see
/// `planning/AGENT-CONSTRAINTS.md`'s "Detection and demuxing ask different
/// questions" — and to return whatever cues it could recover rather than
/// failing outright on a damaged file. It is not, itself, given a chance to
/// fail: a format whose grammar cannot be damaged incrementally (there isn't
/// one here) would return `Result` instead.
///
/// # Errors
/// Whatever [`read_all`] reports.
pub fn open_generic(
    src: Box<dyn MediaSource>,
    codec_id: Option<CodecId>,
    parse: impl FnOnce(&[u8]) -> Vec<Cue>,
) -> Result<Box<dyn Demuxer>> {
    let mut io = IoContext::new(src, &IoOptions::default())?;
    let raw = read_all(&mut io)?;
    let (utf8, _encoding) = vaco_format_subtitle::decode_to_utf8_bytes(&raw);
    let cues = parse(&utf8);
    Ok(Box::new(CueDemuxer::new(codec_id, cues)))
}

/// The one demuxer type every format in this crate builds.
#[derive(Debug)]
pub struct CueDemuxer {
    streams: [Stream; 1],
    cues: Vec<Cue>,
    pos: usize,
    budget: Budget,
}

impl CueDemuxer {
    /// A demuxer over already-parsed `cues`, on a single subtitle stream
    /// tagged `codec_id` (`None` when this format has no [`CodecId`] yet —
    /// see the crate's module docs).
    #[must_use]
    pub fn new(codec_id: Option<CodecId>, cues: Vec<Cue>) -> Self {
        let mut params = CodecParameters::new(MediaType::Subtitle);
        if let Some(id) = codec_id {
            params = params.with_codec(id);
        }
        let mut stream = Stream::new(0, MediaType::Subtitle, TIME_BASE_Q);
        stream.params = params;
        stream.start_time = cues
            .first()
            .map_or(Timestamp::NONE, |c| Timestamp::new(c.start.as_micros()));
        if let Some(last) = cues.last() {
            stream.set_duration_ts(last.end.as_micros().max(0));
        }
        Self {
            streams: [stream],
            cues,
            pos: 0,
            budget: Budget::new(Limits::permissive()),
        }
    }

    /// The cues this demuxer was built from, for tests.
    #[must_use]
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }
}

impl Demuxer for CueDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let cue = self.cues.get(self.pos).ok_or(Error::Eof)?;
        self.pos = self.pos.saturating_add(1);
        let mut pkt = Packet::from_slice(&mut self.budget, &cue.text)?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(cue.start.as_micros());
        pkt.dts = pkt.pts;
        pkt.duration = cue.duration();
        pkt.flags = PacketFlags::KEY;
        Ok(pkt)
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let target = target.resolve_frames(vaco_core::Rational::ZERO, TIME_BASE_Q)?;
        let SeekTarget::Timestamp { ts, .. } = target else {
            return Err(Error::NotSeekable);
        };
        let Some(target_us) = ts.ticks() else {
            return Err(Error::InvalidData("seek target has no timestamp"));
        };
        self.pos = if flags.contains(SeekFlags::BACKWARD) {
            self.cues
                .iter()
                .rposition(|c| c.start.as_micros() <= target_us)
                .unwrap_or(0)
        } else {
            self.cues
                .iter()
                .position(|c| c.start.as_micros() >= target_us)
                .unwrap_or(self.cues.len())
        };
        Ok(())
    }

    fn duration(&self) -> Option<Duration> {
        self.cues.last().map(|c| c.end)
    }
}

/// One format's mux-side serialisation, driven by [`GenericTextMuxer`].
pub trait CueMux: Send {
    /// Whether this container can carry a stream declaring `codec_id`.
    ///
    /// `None` is accepted by every format that has no [`CodecId`] of its own
    /// yet (see the crate's module docs) — it is the only value a caller can
    /// currently construct for one of them.
    fn accepts(&self, codec_id: Option<CodecId>) -> bool;

    /// Called once, before any cue, to write a file-level header.
    ///
    /// # Errors
    /// Propagates I/O failure.
    fn write_header(&mut self, out: &mut IoWriter) -> Result<()> {
        let _ = out;
        Ok(())
    }

    /// Serialise one cue. `index` is the 1-based count of cues written so far,
    /// including this one — the numbering several formats (`SubRip`, `JACOsub`
    /// via its cue markers) print inline.
    ///
    /// # Errors
    /// Propagates I/O failure.
    fn write_cue(&mut self, out: &mut IoWriter, index: usize, cue: &Cue) -> Result<()>;

    /// Called once, after the last cue, to write a file-level trailer.
    ///
    /// # Errors
    /// Propagates I/O failure.
    fn write_trailer(&mut self, out: &mut IoWriter) -> Result<()> {
        let _ = out;
        Ok(())
    }
}

/// The one muxer type every format in this crate builds, parameterised by its
/// [`CueMux`] implementation.
#[derive(Debug)]
pub struct GenericTextMuxer<F> {
    out: IoWriter,
    fmt: F,
    flags: FormatFlags,
    header_written: bool,
    trailer_written: bool,
    stream_added: bool,
    count: usize,
}

impl<F: CueMux> GenericTextMuxer<F> {
    /// A muxer over `sink`, backed by format logic `fmt`.
    ///
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, fmt: F, flags: FormatFlags) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            fmt,
            flags,
            header_written: false,
            trailer_written: false,
            stream_added: false,
            count: 0,
        })
    }
}

impl<F: CueMux + std::fmt::Debug> Muxer for GenericTextMuxer<F> {
    fn flags(&self) -> FormatFlags {
        self.flags
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream_added {
            return Err(Error::Unsupported(
                "this container carries exactly one subtitle stream",
            ));
        }
        if !self.fmt.accepts(params.codec_id) {
            return Err(Error::Unsupported(
                "codec is not one this container's subtitle track can carry",
            ));
        }
        self.stream_added = true;
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        if !self.stream_added {
            return Err(Error::InvalidData("header written before add_stream"));
        }
        if self.header_written {
            return Err(Error::InvalidData("header written twice"));
        }
        self.fmt.write_header(&mut self.out)?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("packet written before the header"));
        }
        self.count = self.count.saturating_add(1);
        let start_us = packet.pts.ticks().unwrap_or(0);
        let dur_us = packet.duration.as_micros().max(0);
        let cue = Cue::new(
            Duration::from_micros(start_us),
            Duration::from_micros(start_us.saturating_add(dur_us)),
            packet.payload().to_vec(),
        );
        self.fmt.write_cue(&mut self.out, self.count, &cue)
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("trailer written before the header"));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("trailer written twice"));
        }
        self.trailer_written = true;
        self.fmt.write_trailer(&mut self.out)?;
        self.out.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn cue(start_ms: i64, end_ms: i64, text: &str) -> Cue {
        Cue::new(
            Duration::from_micros(start_ms * 1000),
            Duration::from_micros(end_ms * 1000),
            text.as_bytes().to_vec(),
        )
    }

    #[test]
    fn read_packet_yields_cues_in_order_then_eof() {
        let mut d = CueDemuxer::new(None, vec![cue(0, 1000, "a"), cue(1000, 2000, "b")]);
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.payload(), b"a");
        assert_eq!(p1.pts, Timestamp::new(0));
        let p2 = d.read_packet().unwrap();
        assert_eq!(p2.payload(), b"b");
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn duration_is_the_last_cues_end() {
        let d = CueDemuxer::new(None, vec![cue(0, 1000, "a"), cue(1000, 5000, "b")]);
        assert_eq!(d.duration(), Some(Duration::from_micros(5_000_000)));
    }

    #[test]
    fn seek_forward_lands_on_first_cue_at_or_after_target() {
        let mut d = CueDemuxer::new(
            None,
            vec![
                cue(0, 1000, "a"),
                cue(2000, 3000, "b"),
                cue(4000, 5000, "c"),
            ],
        );
        d.seek(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts: Timestamp::new(2_500_000),
            },
            SeekFlags::empty(),
        )
        .unwrap();
        assert_eq!(d.read_packet().unwrap().payload(), b"c");
    }

    #[test]
    fn seek_backward_lands_on_last_cue_at_or_before_target() {
        let mut d = CueDemuxer::new(
            None,
            vec![
                cue(0, 1000, "a"),
                cue(2000, 3000, "b"),
                cue(4000, 5000, "c"),
            ],
        );
        d.seek(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts: Timestamp::new(3_500_000),
            },
            SeekFlags::BACKWARD,
        )
        .unwrap();
        assert_eq!(d.read_packet().unwrap().payload(), b"b");
    }

    #[derive(Debug, Default)]
    struct PlainMux {
        header_calls: usize,
    }

    impl CueMux for PlainMux {
        fn accepts(&self, codec_id: Option<CodecId>) -> bool {
            codec_id.is_none()
        }
        fn write_header(&mut self, out: &mut IoWriter) -> Result<()> {
            self.header_calls += 1;
            out.write(b"HEADER\n")
        }
        fn write_cue(&mut self, out: &mut IoWriter, index: usize, cue: &Cue) -> Result<()> {
            out.write(format!("{index}:").as_bytes())?;
            out.write(&cue.text)?;
            out.write(b"\n")
        }
        fn write_trailer(&mut self, out: &mut IoWriter) -> Result<()> {
            out.write(b"TRAILER\n")
        }
    }

    #[test]
    fn generic_muxer_drives_header_cues_and_trailer_in_order() {
        use vaco_format_core::vacoraw::MemorySink;
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut mux =
            GenericTextMuxer::new(Box::new(sink), PlainMux::default(), MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle))
            .unwrap();
        mux.write_header().unwrap();
        let mut pkt = Packet::from_slice(&mut Budget::new(Limits::permissive()), b"hi").unwrap();
        pkt.pts = Timestamp::new(1_000_000);
        pkt.duration = Duration::from_micros(2_000_000);
        mux.write_packet(&pkt).unwrap();
        mux.write_trailer().unwrap();
        assert_eq!(shared.snapshot(), b"HEADER\n1:hi\nTRAILER\n");
    }

    #[test]
    fn second_stream_is_rejected() {
        let sink = vaco_format_core::vacoraw::MemorySink::new();
        let mut mux =
            GenericTextMuxer::new(Box::new(sink), PlainMux::default(), MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle))
            .unwrap();
        assert!(
            mux.add_stream(&CodecParameters::new(MediaType::Subtitle))
                .is_err()
        );
    }
}
