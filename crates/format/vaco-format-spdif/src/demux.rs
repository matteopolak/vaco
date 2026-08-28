//! The `spdif` demuxer: fixed-size IEC 61937 bursts in, AC-3 packets out.
//!
//! # Why the burst size is a constant, not a scan
//!
//! Measured directly (see `iec61937.rs`'s module docs): three separate
//! `ffmpeg -f spdif` captures — 192 kb/s and 384 kb/s AC-3 at 48 kHz, and
//! 192 kb/s AC-3 at 44.1 kHz — all produced bursts of exactly
//! [`AC3_BURST_BYTES`] (6144) bytes regardless of the AC-3 frame's own byte
//! length, which is exactly what the spec's "1536 samples x 4 bytes"
//! repetition period predicts for AC-3 specifically. This demuxer reads
//! whole 6144-byte bursts rather than scanning forward for the next sync
//! word, because the fixed size is the thing that was actually measured —
//! scanning would be a plausible-sounding behaviour this crate never
//! checked.

use crate::ac3;
use crate::iec61937::{BurstHeader, DATA_TYPE_AC3, HEADER_LEN};
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// Measured: see the module docs. A `u32` because it is used in byte-count
/// arithmetic against `u64` I/O offsets elsewhere; it is a small compile-time
/// constant, not anything derived from input.
pub const AC3_BURST_BYTES: usize = 6144;

/// No index of its own; a fixed burst size for AC-3 makes it generic-index
/// friendly, but this demuxer does not build one today.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

/// The `spdif` demuxer.
pub struct SpdifDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    budget: Budget,
    frame_index: u64,
    pending: Option<Packet>,
    eof: bool,
}

impl std::fmt::Debug for SpdifDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpdifDemuxer")
            .field("frame_index", &self.frame_index)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl SpdifDemuxer {
    /// Open an IEC 61937 (S/PDIF) stream.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when the stream does not open with a valid
    /// burst header; [`Error::Unsupported`] for a data type other than
    /// AC-3 (see the crate's module docs for why only AC-3 is supported);
    /// [`Error::Eof`] on an empty input.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        Self::open_with_limits(src, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling.
    ///
    /// # Errors
    /// As [`SpdifDemuxer::open`].
    pub fn open_with_limits(src: Box<dyn MediaSource>, limits: Limits) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut demux = Self {
            io,
            streams: Vec::new(),
            budget: Budget::new(limits),
            frame_index: 0,
            pending: None,
            eof: false,
        };
        demux.read_burst()?;
        Ok(demux)
    }

    fn read_burst(&mut self) -> Result<()> {
        let peeked = self.io.peek(AC3_BURST_BYTES)?;
        if peeked.is_empty() {
            return Err(Error::Eof);
        }
        if peeked.len() < HEADER_LEN {
            return Err(Error::Eof);
        }
        let Some(header) = BurstHeader::parse(peeked, false) else {
            return Err(Error::InvalidData("spdif: expected an IEC 61937 sync burst"));
        };
        if header.data_type() != DATA_TYPE_AC3 {
            return Err(Error::Unsupported(
                "spdif: only AC-3 (data type 1) bursts are supported",
            ));
        }
        let payload_len = header.ac3_payload_len_bytes()?;
        if payload_len > AC3_BURST_BYTES.saturating_sub(HEADER_LEN) {
            return Err(Error::InvalidData(
                "spdif: burst declares a payload longer than the burst itself",
            ));
        }
        // A truncated final burst: not a whole 6144 bytes left, but at
        // least a header and its declared payload fit. Read what exists;
        // there is no padding to skip past on the very last burst.
        let available = peeked.len();
        let take = available.min(AC3_BURST_BYTES);
        if take < HEADER_LEN.saturating_add(payload_len) {
            return Err(Error::Eof);
        }
        let mut raw = self.budget.alloc::<u8>(take)?;
        self.io.read_exact(&mut raw)?;

        let swapped = raw
            .get(HEADER_LEN..HEADER_LEN.saturating_add(payload_len))
            .ok_or(Error::UnexpectedEof)?;
        let unswapped = crate::iec61937::unswap_payload(swapped, payload_len);

        if self.streams.is_empty() {
            self.streams.push(new_stream(&unswapped));
        }

        let mut pkt = Packet::from_slice(&mut self.budget, &unswapped)?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(self.frame_index.cast_signed());
        pkt.dts = pkt.pts;
        // AC-3 is always 1536 samples/frame; the time base below is
        // 1/sample_rate, so duration in that base is exactly 1536 ticks.
        pkt.duration = Duration::from_micros(32_000); // 1536/48000 s, the
        // spec-fixed frame duration this burst size assumes.
        pkt.flags |= PacketFlags::KEY; // AC-3 frames decode independently
        self.frame_index += 1;
        self.pending = Some(pkt);
        Ok(())
    }
}

fn new_stream(first_frame: &[u8]) -> Stream {
    let info = ac3::parse(first_frame);
    let sample_rate = info.map_or(48_000, |i| i.sample_rate);
    let channels = info.map_or(2, |i| i.channels);
    let audio = AudioParameters {
        sample_rate,
        layout: vaco_chlayout::ChannelLayout::default_for(u32::from(channels)),
        ..AudioParameters::default()
    };
    let mut params = CodecParameters::new(MediaType::Audio).with_codec(CodecId::Ac3);
    params.audio = Some(audio);
    let time_base = Rational {
        num: 1,
        den: sample_rate.cast_signed(),
    };
    let mut stream = Stream::new(0, MediaType::Audio, time_base);
    stream.params = params;
    stream
}

impl Demuxer for SpdifDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if let Some(pkt) = self.pending.take() {
            return Ok(pkt);
        }
        if self.eof {
            return Err(Error::Eof);
        }
        match self.read_burst() {
            Ok(()) => self.pending.take().ok_or(Error::Eof),
            Err(Error::Eof) => {
                self.eof = true;
                Err(Error::Eof)
            }
            Err(e) => Err(e),
        }
    }

    #[allow(
        clippy::integer_division,
        reason = "exact burst/sample-count arithmetic, not an approximation"
    )]
    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == vaco_io::Seekability::None {
            return Err(Error::NotSeekable);
        }
        let burst = AC3_BURST_BYTES as u64;
        let frame = match target {
            SeekTarget::Byte(pos) => pos / burst,
            SeekTarget::Timestamp { ts, .. } => {
                let ticks = ts.ticks().unwrap_or(0);
                if ticks < 0 { 0 } else { ticks.cast_unsigned() / 1536 }
            }
            SeekTarget::Frame { frame, .. } => frame,
        };
        let byte_pos = frame.saturating_mul(burst);
        self.io.seek(byte_pos)?;
        self.pending = None;
        self.eof = false;
        self.frame_index = frame;
        Ok(())
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}
