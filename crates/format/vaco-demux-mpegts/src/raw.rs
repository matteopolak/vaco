//! `mpegtsraw`: the PID-level view that hands up whole transport packets
//! instead of reassembled PES.
//!
//! # Measured against `ffprobe 8.1`
//!
//! `ffmpeg -demuxers` lists `mpegtsraw` as a demuxer distinct from `mpegts`,
//! confirming the brief's suspicion that "the roadmap can name more than
//! actually exist" cuts both ways — here the second one is real. Four facts
//! were measured directly (`ffprobe -f mpegtsraw -show_streams -show_packets`
//! against a muxed fixture, `ffmpeg -h demuxer=mpegtsraw` for the options):
//!
//! 1. **Never auto-detected.** Opening the same file with no `-f` reports
//!    `format_name=mpegts`; `mpegtsraw` is reached only by naming it, exactly
//!    the shape `vaco-demux-asf`'s `asf_o` already uses in this workspace —
//!    [`RAW_DEMUXER`]'s probe is [`ProbeScore::NONE`], unconditionally.
//! 2. **Exactly one stream**, `codec_type=data`, `time_base=1/27000000` — the
//!    27 MHz PCR clock, not the 90 kHz PES clock `mpegts` uses.
//! 3. **One output packet per input transport packet**, `size=188` even over
//!    an M2TS-strided file (the 4-byte `TP_extra_header` is stripped), and
//!    every packet carries `flags=K__`.
//! 4. **No timestamps by default.** `-show_packets` on a five-stream muxed
//!    fixture reports `pts=N/A` on all 105 packets; the reference only
//!    produces a `pts` when the demuxer option `-compute_pcr 1` is passed
//!    (default `false`, per `ffmpeg -h demuxer=mpegtsraw`), and even then the
//!    values are a byte-position-interpolated PCR, not a real per-packet
//!    clock — they are not monotonic across PIDs. `compute_pcr` is not
//!    implemented here; see *Deliberately deferred* in the docs file.
//!
//! `pos` is the offset **after** the packet, i.e. cumulative bytes consumed
//! including any stride prefix — measured on both a plain and an M2TS-strided
//! fixture (`192, 384, 576, …` for M2TS, not `188, 376, …`).

use vaco_core::{Error, MediaType, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::options::FormatOptions;
use vaco_format_core::probe::ProbeScore;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Program, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use vaco_format_mpegts_tables::packet::{PCR_HZ, PacketStride};

/// The bound the reference calls `resync_size` and defaults to 65536 bytes
/// (`ffmpeg -h demuxer=mpegtsraw`). Not exposed as an option — `FormatOptions`
/// has no per-demuxer option slot for it — but the bound itself is honoured so
/// a hostile file cannot force an unbounded scan.
pub const RESYNC_SIZE: u64 = 65_536;

/// `time_base` for the raw view: the 27 MHz PCR clock, measured against
/// `ffprobe 8.1`'s `time_base=1/27000000` — **not** [`vaco_format_mpegts_tables::TIME_BASE`],
/// which is the 90 kHz PES clock the reassembled-PES `mpegts` demuxer uses.
#[must_use]
pub const fn raw_time_base() -> vaco_core::Rational {
    vaco_core::Rational::new(1, PCR_HZ as i32)
}

/// Never chosen by content probing (see the module docs); reached only by
/// naming `mpegtsraw` explicitly, exactly like `vaco-demux-asf`'s `asf_o`.
#[must_use]
pub fn probe_never(_data: &vaco_format_core::probe::ProbeData<'_>) -> ProbeScore {
    ProbeScore::NONE
}

/// Declared capabilities. [`FormatFlags::NOTIMESTAMPS`] is the honest
/// declaration for the default (`compute_pcr` unimplemented) behaviour: every
/// packet's `pts`/`dts` is [`vaco_core::Timestamp::NONE`]. Without a
/// timestamp, neither the generic index nor bisection has anything to search
/// on, hence [`FormatFlags::NOGENSEARCH`] and [`FormatFlags::NOBINSEARCH`]; a
/// byte seek remains meaningful (round down to the nearest packet boundary),
/// so [`FormatFlags::NO_BYTE_SEEK`] is *not* set.
pub const RAW_FLAGS: FormatFlags = FormatFlags::NOTIMESTAMPS
    .union(FormatFlags::NOGENSEARCH)
    .union(FormatFlags::NOBINSEARCH);

/// The `mpegtsraw` registry descriptor.
pub const RAW_DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mpegtsraw",
    long_name: "raw MPEG-TS (MPEG-2 Transport Stream)",
    extensions: &[],
    mime_types: &[],
    flags: RAW_FLAGS,
    probe: probe_never,
    open: open_raw_demuxer,
};

fn open_raw_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(MpegTsRawDemuxer::open(
        src,
        &FormatOptions::default(),
    )?))
}

/// The `mpegtsraw` demuxer: one transport packet in, one [`Packet`] out.
#[derive(Debug)]
pub struct MpegTsRawDemuxer {
    io: IoContext,
    stride: PacketStride,
    first_packet: u64,
    stream: Stream,
    budget: Budget,
    eof: bool,
}

impl MpegTsRawDemuxer {
    /// Open a transport stream for the raw, packet-per-packet view.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when no MPEG-TS packet rhythm can be found.
    pub fn open(src: Box<dyn MediaSource>, opts: &FormatOptions) -> Result<Self> {
        Self::open_with_limits(src, opts, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling, mirroring
    /// [`crate::MpegTsDemuxer::open_with_limits`].
    ///
    /// # Errors
    ///
    /// As [`MpegTsRawDemuxer::open`].
    pub fn open_with_limits(
        src: Box<dyn MediaSource>,
        _opts: &FormatOptions,
        limits: Limits,
    ) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let window = io.peek(1 << 16).map(<[u8]>::to_vec).unwrap_or_default();
        let data = vaco_format_core::probe::ProbeData::new(&window);
        let Some((stride, at, _)) = crate::probe::best_run(&data) else {
            return Err(Error::InvalidData("no MPEG-TS packet rhythm found"));
        };
        let first_packet = io.pos().saturating_add(at as u64);
        io.seek(first_packet)?;

        let mut stream = Stream::new(0, MediaType::Data, raw_time_base());
        stream.id = Some(0);
        // No `CodecId` describes "one raw transport packet"; `codec_name`
        // therefore prints `unknown` through `vaco-probe`'s codec_id-only
        // lookup, where the reference prints `mpegts`. Reported, not worked
        // around — see the docs file's gap list, the same shape as
        // `TsCodec`'s existing `codec_id() -> None` cases.
        stream.metadata_set("ts_codec", "mpegtsraw");

        Ok(Self {
            io,
            stride,
            first_packet,
            stream,
            budget: Budget::new(limits),
            eof: false,
        })
    }

    /// Read one stride, resynchronising within [`RESYNC_SIZE`] if the sync
    /// byte is missing. Returns the body (always 188 bytes, stride prefix
    /// stripped) and the offset *after* the stride — matching the reference's
    /// `pos`, measured on both a plain and an M2TS-strided fixture.
    fn next_packet(&mut self) -> Result<(Vec<u8>, u64)> {
        let mut buf = [0u8; PacketStride::MAX_STRIDE];
        let n = self.stride.stride();
        let Some(dst) = buf.get_mut(..n) else {
            return Err(Error::InvalidData("stride buffer too small"));
        };
        self.io.read_exact(dst)?;
        if dst.get(self.stride.prefix()) == Some(&vaco_format_mpegts_tables::packet::SYNC_BYTE) {
            let body = self.stride.body(dst).unwrap_or(&[]).to_vec();
            return Ok((body, self.io.pos()));
        }
        let mut skipped = 0u64;
        while skipped < RESYNC_SIZE {
            let pos = self.io.pos();
            let b = self.io.r8()?;
            skipped = skipped.saturating_add(1);
            if b != vaco_format_mpegts_tables::packet::SYNC_BYTE {
                continue;
            }
            let start = pos.saturating_sub(self.stride.prefix() as u64);
            self.io.seek(start)?;
            self.io.read_exact(dst)?;
            if dst.get(self.stride.prefix()) == Some(&vaco_format_mpegts_tables::packet::SYNC_BYTE)
            {
                let body = self.stride.body(dst).unwrap_or(&[]).to_vec();
                return Ok((body, self.io.pos()));
            }
            self.io.seek(pos.saturating_add(1))?;
        }
        Err(Error::InvalidData("lost transport packet synchronisation"))
    }
}

impl Demuxer for MpegTsRawDemuxer {
    fn streams(&self) -> &[Stream] {
        core::slice::from_ref(&self.stream)
    }

    fn programs(&self) -> &[Program] {
        &[]
    }

    fn metadata(&self) -> &[(String, String)] {
        &[]
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        match self.next_packet() {
            Ok((body, pos)) => {
                let mut pkt = Packet::from_slice(&mut self.budget, &body)?;
                pkt.stream_index = 0;
                pkt.pos = Some(pos);
                // Every packet is reported a sync point: measured `flags=K__`
                // on every packet the reference emits, which follows from
                // there being no codec-level notion of "key" at this layer.
                pkt.flags = PacketFlags::KEY;
                Ok(pkt)
            }
            Err(Error::Eof | Error::UnexpectedEof) => {
                self.eof = true;
                Err(Error::Eof)
            }
            Err(e) => Err(e),
        }
    }

    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        match target {
            SeekTarget::Byte(pos) => {
                if self.io.seekability() == Seekability::None {
                    return Err(Error::NotSeekable);
                }
                // Round down to the nearest packet boundary; the resync scan
                // in `next_packet` recovers alignment regardless, but landing
                // on a boundary already is the common case and avoids paying
                // for a scan on every seek.
                let stride = self.stride.stride() as u64;
                let from = self.first_packet.max(pos);
                let offset = from.saturating_sub(self.first_packet);
                #[allow(
                    clippy::integer_division,
                    reason = "rounding down to a whole packet boundary is the point"
                )]
                let whole_strides = offset / stride;
                let aligned = self
                    .first_packet
                    .saturating_add(whole_strides.saturating_mul(stride));
                self.io.seek(aligned)?;
                self.eof = false;
                Ok(())
            }
            SeekTarget::Timestamp { .. } | SeekTarget::Frame { .. } => Err(Error::Unsupported(
                "mpegtsraw carries no timestamps to seek on",
            )),
        }
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        // The reference derives an estimate from file size and bitrate
        // (`ffprobe` prints "Estimating duration from bitrate, this may be
        // inaccurate" for exactly this demuxer). Reproducing that needs a
        // bitrate this raw, no-PES view has no way to learn on its own — the
        // 90 kHz clock lives one layer up, in `mpegts`. Reported, not
        // guessed at.
        None
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn ts_packet(pid: u16, cc: u8) -> [u8; 188] {
        let mut p = [0xFFu8; 188];
        p[0] = 0x47;
        p[1] = (pid >> 8) as u8 & 0x1F;
        p[2] = (pid & 0xFF) as u8;
        p[3] = 0x10 | (cc & 0x0F);
        p
    }

    fn plain_file(n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..n {
            out.extend_from_slice(&ts_packet(0x100, (i % 16) as u8));
        }
        out
    }

    fn m2ts_file(n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..n {
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&ts_packet(0x100, (i % 16) as u8));
        }
        out
    }

    #[test]
    fn never_auto_detected() {
        let bytes = plain_file(20);
        let data = vaco_format_core::probe::ProbeData::new(&bytes);
        assert_eq!(probe_never(&data), ProbeScore::NONE);
    }

    #[test]
    fn one_stream_of_media_type_data_at_the_pcr_clock() {
        let src = Box::new(MemorySource::new(plain_file(20)));
        let d = MpegTsRawDemuxer::open(src, &FormatOptions::default()).unwrap();
        assert_eq!(d.streams().len(), 1);
        assert_eq!(d.streams()[0].media_type(), Some(MediaType::Data));
        assert_eq!(d.streams()[0].time_base, raw_time_base());
        assert_eq!(d.streams()[0].params.codec_id, None);
    }

    #[test]
    fn every_packet_is_the_stripped_188_byte_body_with_no_timestamp() {
        let src = Box::new(MemorySource::new(plain_file(12)));
        let mut d = MpegTsRawDemuxer::open(src, &FormatOptions::default()).unwrap();
        let mut n = 0;
        loop {
            match d.read_packet() {
                Ok(p) => {
                    assert_eq!(p.len, 188);
                    assert!(p.pts.is_none());
                    assert!(p.dts.is_none());
                    assert!(p.is_key());
                    n += 1;
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected {e:?}"),
            }
        }
        assert_eq!(n, 12);
    }

    #[test]
    fn pos_advances_by_the_full_stride_including_the_m2ts_prefix() {
        let src = Box::new(MemorySource::new(m2ts_file(5)));
        let mut d = MpegTsRawDemuxer::open(src, &FormatOptions::default()).unwrap();
        let mut positions = Vec::new();
        while let Ok(p) = d.read_packet() {
            assert_eq!(
                p.len, 188,
                "the 4-byte prefix must be stripped from the body"
            );
            positions.push(p.pos.unwrap());
        }
        // Measured against ffprobe 8.1: pos is 192, 384, 576, … — the full
        // stride, not the 188-byte body.
        assert_eq!(positions, vec![192, 384, 576, 768, 960]);
    }

    #[test]
    fn eof_is_sticky() {
        let src = Box::new(MemorySource::new(plain_file(4)));
        let mut d = MpegTsRawDemuxer::open(src, &FormatOptions::default()).unwrap();
        while d.read_packet().is_ok() {}
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn a_stream_starting_mid_packet_is_resynchronised() {
        let mut bytes = vec![0x11u8; 50];
        bytes.extend_from_slice(&plain_file(6));
        let src = Box::new(MemorySource::new(bytes));
        let mut d = MpegTsRawDemuxer::open(src, &FormatOptions::default()).unwrap();
        let mut n = 0;
        while d.read_packet().is_ok() {
            n += 1;
        }
        assert_eq!(n, 6);
    }

    #[test]
    fn byte_seek_rounds_down_to_a_packet_boundary() {
        let src = Box::new(MemorySource::new(plain_file(10)));
        let mut d = MpegTsRawDemuxer::open(src, &FormatOptions::default()).unwrap();
        d.seek(SeekTarget::Byte(377), SeekFlags::empty()).unwrap();
        let p = d.read_packet().unwrap();
        // Landed inside packet index 2 (offsets 376..564); rounding down
        // lands at the start of packet 2, whose pos-after is 564.
        assert_eq!(p.pos, Some(564));
    }

    #[test]
    fn timestamp_seek_is_refused_rather_than_guessed_at() {
        let src = Box::new(MemorySource::new(plain_file(4)));
        let mut d = MpegTsRawDemuxer::open(src, &FormatOptions::default()).unwrap();
        let target = SeekTarget::Timestamp {
            stream_index: 0,
            ts: vaco_core::Timestamp::new(0),
        };
        assert!(d.seek(target, SeekFlags::empty()).is_err());
    }

    #[test]
    fn a_file_with_no_packet_rhythm_is_refused() {
        let src = Box::new(MemorySource::new(vec![0u8; 100]));
        assert!(MpegTsRawDemuxer::open(src, &FormatOptions::default()).is_err());
    }
}
