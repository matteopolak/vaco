//! Konami PS2 SVAG (`.svag`), an interleaved PS-ADPCM container.
//!
//! A community format note reports the `VAGm` magic, sample rate at offset
//! 0x08, 16-byte interleave unit, and a 32-byte header in its observed files
//! (`Vaco-Spec-Ref svag-format-note`). Black-box sweeps against `ffprobe`
//! 9.0.1 establish the reference demuxer's narrower contract: it consumes a
//! 20-byte header and treats bytes from offset 20 onward as audio. The fields
//! are `data_size`, `sample_rate`, `channels`, and `interleave`; all are
//! little-endian `u32` values.
//!
//! `data_size` controls only stream duration. Packet reads continue to the
//! source's physical EOF in `channels * interleave` byte groups, even when the
//! declared size is smaller or larger. A short final group is emitted as a
//! corrupt packet without timestamps, matching the reference exactly
//! (`Vaco-Spec-Ref vaco-format-misc-audio-svag-fixtures-probe`).

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags, PacketSideData};

const MAGIC: &[u8; 4] = b"VAGm";
const HEADER_LEN: u64 = 20;
const BLOCK_BYTES_PER_CHANNEL: u32 = 16;
const SAMPLES_PER_BLOCK: u32 = 28;
const MAX_PACKET_BYTES: u32 = 1 << 24;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(MAGIC) {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "svag",
    long_name: "Konami PS2 SVAG",
    extensions: &["svag"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(SvagDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct SvagDemuxer {
    io: IoContext,
    stream: Stream,
    packet_bytes: u32,
    packet_samples: u64,
    declared_frames: u64,
    frames_emitted: u64,
    eof: bool,
    budget: Budget,
}

impl SvagDemuxer {
    /// Opens an SVAG stream after validating its fixed header and block geometry.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidData`] for a missing signature, zero stream
    /// geometry, a non-block-aligned interleave, or an oversized packet.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut magic = [0u8; 4];
        io.read_exact(&mut magic)?;
        if magic != *MAGIC {
            return Err(Error::InvalidData("svag: missing VAGm signature"));
        }

        let data_size = io.rl32()?;
        let sample_rate = io.rl32()?;
        let channels = io.rl32()?;
        let interleave = io.rl32()?;
        if sample_rate == 0 || i32::try_from(sample_rate).is_err() {
            return Err(Error::InvalidData("svag: invalid sample rate"));
        }
        if channels == 0 {
            return Err(Error::InvalidData("svag: zero channels"));
        }
        if interleave == 0 || !interleave.is_multiple_of(BLOCK_BYTES_PER_CHANNEL) {
            return Err(Error::InvalidData("svag: invalid interleave"));
        }

        let packet_bytes = channels
            .checked_mul(interleave)
            .filter(|&size| size <= MAX_PACKET_BYTES)
            .ok_or(Error::InvalidData("svag: packet size is too large"))?;
        #[allow(
            clippy::integer_division,
            reason = "a valid interleave is an exact number of PS-ADPCM blocks"
        )]
        let blocks_per_packet = interleave / BLOCK_BYTES_PER_CHANNEL;
        let packet_samples = u64::from(blocks_per_packet)
            .checked_mul(u64::from(SAMPLES_PER_BLOCK))
            .ok_or(Error::InvalidData("svag: packet duration overflows"))?;
        let frame_bytes = channels
            .checked_mul(BLOCK_BYTES_PER_CHANNEL)
            .ok_or(Error::InvalidData("svag: channel block size overflows"))?;
        #[allow(
            clippy::integer_division,
            reason = "the reference truncates an incomplete declared channel block"
        )]
        let declared_frames = u64::from(data_size / frame_bytes)
            .checked_mul(u64::from(SAMPLES_PER_BLOCK))
            .ok_or(Error::InvalidData("svag: duration overflows"))?;

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, sample_rate.cast_signed()),
        );
        let mut params = CodecParameters::audio();
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            audio.layout = Some(
                ChannelLayout::default_for(channels)
                    .unwrap_or_else(|| ChannelLayout::unspecified(channels)),
            );
        }
        stream.params = params;
        stream.duration_ts = i64::try_from(declared_frames).ok();
        stream.frame_count = Some(declared_frames);

        Ok(Self {
            io,
            stream,
            packet_bytes,
            packet_samples,
            declared_frames,
            frames_emitted: 0,
            eof: false,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for SvagDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }

        let pos = self.io.pos();
        let mut packet = Packet::alloc(&mut self.budget, self.packet_bytes as usize)?;
        let read = self.io.read_partial(packet.payload_mut())?;
        if read == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        packet.len = read;
        packet.stream_index = 0;
        packet.pos = Some(pos);
        packet.flags = PacketFlags::KEY;

        if read < self.packet_bytes as usize {
            packet.flags |= PacketFlags::CORRUPT;
            self.eof = true;
            return Ok(packet);
        }

        let pts = i64::try_from(self.frames_emitted).unwrap_or(i64::MAX);
        packet.pts = vaco_core::Timestamp::new(pts);
        packet.dts = packet.pts;
        let ticks = i64::try_from(self.packet_samples).unwrap_or(i64::MAX);
        packet.side_data.push(PacketSideData::DurationTicks(ticks));
        packet.duration = vaco_core::Duration::from_ticks(ticks, self.stream.time_base)
            .unwrap_or(vaco_core::Duration::ZERO);
        self.frames_emitted = self.frames_emitted.saturating_add(self.packet_samples);
        Ok(packet)
    }

    #[allow(
        clippy::integer_division,
        reason = "seeking rounds down to the containing interleaved packet"
    )]
    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let packet_index = match target {
            SeekTarget::Byte(byte) => {
                byte.saturating_sub(HEADER_LEN) / u64::from(self.packet_bytes)
            }
            SeekTarget::Frame { frame, .. } => frame / self.packet_samples,
            SeekTarget::Timestamp { ts, .. } => {
                u64::try_from(ts.ticks().unwrap_or(0).max(0)).unwrap_or(0) / self.packet_samples
            }
        };
        let byte_pos =
            HEADER_LEN.saturating_add(packet_index.saturating_mul(u64::from(self.packet_bytes)));
        self.io.seek(byte_pos)?;
        self.frames_emitted = packet_index.saturating_mul(self.packet_samples);
        self.eof = false;
        Ok(())
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        vaco_core::Duration::from_ticks(
            i64::try_from(self.declared_frames).ok()?,
            self.stream.time_base,
        )
    }
}
