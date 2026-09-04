//! Nintendo BRSTM (`.brstm`) container headers.
//!
//! BRSTM stores a fixed `RSTM` file header followed by `HEAD`, `ADPC`, and
//! `DATA` chunks. The measured subset here is stereo DSP-ADPCM with 32, 64,
//! 96, or 256 byte channel blocks; final blocks are either full or half sized
//! and physically padded to a full block. `CodecId` has no `adpcm_thp` variant,
//! so stream metadata deliberately leaves the codec identity absent even though
//! the reference reports that name.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags, PacketSideData};

const MAGIC: [u8; 4] = *b"RSTM";
const HEAD_MAGIC: [u8; 4] = *b"HEAD";
const DATA_MAGIC: [u8; 4] = *b"DATA";
const HEADER_SIZE: u64 = 0x40;
const DATA_HEADER_SIZE: u64 = 0x20;
const DSP_ADPCM_CODEC: u8 = 2;
const CHANNELS: u32 = 2;
const COEFFICIENT_BYTES_PER_CHANNEL: usize = 32;
const ADPC_ENTRY_BYTES: usize = 8;
const SYNTHESIZED_PREFIX_BYTES: usize = 80;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(&MAGIC) && data.rb16(4) == Some(0xfeff) {
        ProbeScore::MAGIC_CHECKED
    } else if data.starts_with(&MAGIC) {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "brstm",
    long_name: "Nintendo BRSTM (stereo DSP-ADPCM)",
    extensions: &["brstm"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(BrstmDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct BrstmDemuxer {
    io: IoContext,
    stream: Stream,
    data_start: u64,
    adpc_data_start: u64,
    coefficients: [u8; COEFFICIENT_BYTES_PER_CHANNEL * 2],
    block_count: u64,
    block_size: u32,
    samples_per_block: u32,
    final_block_size: u32,
    final_block_samples: u32,
    blocks_emitted: u64,
    budget: Budget,
}

impl BrstmDemuxer {
    /// Opens a big-endian DSP-ADPCM BRSTM after checking the source-defined
    /// chunk and audio-data bounds.
    ///
    /// # Errors
    /// Returns [`Error::InvalidData`] for a malformed RSTM/HEAD/DATA layout,
    /// and [`Error::Unsupported`] for a BRSTM codec other than DSP-ADPCM.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.tag()? != MAGIC {
            return Err(Error::InvalidData("brstm: missing RSTM signature"));
        }
        if io.rb16()? != 0xfeff {
            return Err(Error::InvalidData(
                "brstm: only big-endian files are supported",
            ));
        }
        let _major = io.r8()?;
        let _minor = io.r8()?;
        let declared_file_size = u64::from(io.rb32()?);
        let header_size = u64::from(io.rb16()?);
        let chunk_count = io.rb16()?;
        if header_size != HEADER_SIZE || chunk_count < 3 {
            return Err(Error::InvalidData("brstm: malformed file header"));
        }
        let head_offset = u64::from(io.rb32()?);
        let head_size = u64::from(io.rb32()?);
        let adpc_offset = u64::from(io.rb32()?);
        let adpc_size = u64::from(io.rb32()?);
        let data_offset = u64::from(io.rb32()?);
        let data_size = u64::from(io.rb32()?);
        validate_range(head_offset, head_size, declared_file_size, "HEAD")?;
        validate_range(adpc_offset, adpc_size, declared_file_size, "ADPC")?;
        validate_range(data_offset, data_size, declared_file_size, "DATA")?;
        if let Some(actual_size) = io.size()
            && declared_file_size > actual_size
        {
            return Err(Error::InvalidData(
                "brstm: declared file size exceeds source",
            ));
        }

        io.seek(head_offset)?;
        if io.tag()? != HEAD_MAGIC || u64::from(io.rb32()?) != head_size {
            return Err(Error::InvalidData("brstm: malformed HEAD chunk"));
        }
        let _part1_marker = io.rb32()?;
        let part1_relative = u64::from(io.rb32()?);
        let part1_offset = head_offset
            .checked_add(8)
            .and_then(|base| base.checked_add(part1_relative))
            .ok_or(Error::InvalidData("brstm: HEAD part 1 offset overflows"))?;
        let head_end = head_offset
            .checked_add(head_size)
            .ok_or(Error::InvalidData("brstm: HEAD range overflows"))?;
        validate_range(part1_offset, 0x34, head_end, "HEAD part 1")?;

        io.seek(head_offset + 0x1c)?;
        let part3_relative = u64::from(io.rb32()?);
        let part3_offset = head_offset
            .checked_add(8)
            .and_then(|base| base.checked_add(part3_relative))
            .ok_or(Error::InvalidData("brstm: HEAD part 3 offset overflows"))?;

        io.seek(part1_offset)?;
        let codec = io.r8()?;
        let _loop_flag = io.r8()?;
        let channels = u32::from(io.r8()?);
        let _padding = io.r8()?;
        let sample_rate = u32::from(io.rb16()?);
        let _padding = io.rb16()?;
        let _loop_start = io.rb32()?;
        let total_samples = u64::from(io.rb32()?);
        let data_start = u64::from(io.rb32()?);
        let block_count = u64::from(io.rb32()?);
        let block_size = io.rb32()?;
        let samples_per_block = io.rb32()?;
        let final_block_size = io.rb32()?;
        let final_block_samples = io.rb32()?;
        let final_block_padded_size = io.rb32()?;
        let adpc_samples_per_entry = io.rb32()?;
        let adpc_bytes_per_entry = io.rb32()?;

        if codec != DSP_ADPCM_CODEC {
            return Err(Error::Unsupported("brstm: only DSP-ADPCM is supported"));
        }
        if sample_rate == 0 || channels == 0 {
            return Err(Error::InvalidData("brstm: invalid audio geometry"));
        }
        if channels != CHANNELS {
            return Err(Error::Unsupported(
                "brstm: only reference-accepted stereo DSP-ADPCM is supported",
            ));
        }
        if !matches!(block_size, 32 | 64 | 96 | 256)
            || samples_per_block != block_size.div_euclid(8) * 14
            || final_block_size != block_size && final_block_size != block_size.div_euclid(2)
            || final_block_samples != final_block_size.div_euclid(8) * 14
            || final_block_padded_size != block_size
            || adpc_samples_per_entry != samples_per_block
            || adpc_bytes_per_entry != ADPC_ENTRY_BYTES as u32
            || block_count == 0
        {
            return Err(Error::Unsupported(
                "brstm: block geometry is outside the measured stereo subset",
            ));
        }
        let expected_data_start = data_offset
            .checked_add(DATA_HEADER_SIZE)
            .ok_or(Error::InvalidData("brstm: DATA header offset overflows"))?;
        if data_start != expected_data_start {
            return Err(Error::InvalidData("brstm: unexpected ADPCM data offset"));
        }
        let data_end = data_offset
            .checked_add(data_size)
            .ok_or(Error::InvalidData("brstm: DATA range overflows"))?;
        validate_range(data_start, 0, data_end, "ADPCM data")?;
        io.seek(adpc_offset)?;
        if io.tag()? != *b"ADPC" {
            return Err(Error::InvalidData("brstm: malformed ADPC chunk"));
        }
        let adpc_length = u64::from(io.rb32()?);
        let adpc_entry_bytes = u64::try_from(ADPC_ENTRY_BYTES).unwrap_or(0);
        let adpc_required = block_count
            .checked_mul(adpc_entry_bytes)
            .and_then(|entries| entries.checked_add(8))
            .ok_or(Error::InvalidData("brstm: ADPC table length overflows"))?;
        if adpc_length > adpc_size || adpc_length < adpc_required {
            return Err(Error::InvalidData("brstm: truncated ADPC table"));
        }

        validate_range(part3_offset, 4 + 2 * 8, head_end, "HEAD part 3")?;
        io.seek(part3_offset)?;
        if u32::from(io.r8()?) != CHANNELS {
            return Err(Error::InvalidData("brstm: HEAD part 3 channel mismatch"));
        }
        io.seek(part3_offset + 4)?;
        let mut coefficients = [0; COEFFICIENT_BYTES_PER_CHANNEL * 2];
        for channel in 0..usize::try_from(CHANNELS).unwrap_or(0) {
            let _marker = io.rb32()?;
            let channel_info_relative = u64::from(io.rb32()?);
            let channel_info = head_offset
                .checked_add(8)
                .and_then(|base| base.checked_add(channel_info_relative))
                .ok_or(Error::InvalidData("brstm: channel info offset overflows"))?;
            validate_range(
                channel_info,
                8 + COEFFICIENT_BYTES_PER_CHANNEL as u64,
                head_end,
                "channel info",
            )?;
            let return_pos = io.pos();
            io.seek(channel_info + 8)?;
            let coefficients_start = channel * COEFFICIENT_BYTES_PER_CHANNEL;
            let coefficients_end = coefficients_start
                .checked_add(COEFFICIENT_BYTES_PER_CHANNEL)
                .ok_or(Error::InvalidData("brstm: coefficient range overflows"))?;
            let coefficients = coefficients
                .get_mut(coefficients_start..coefficients_end)
                .ok_or(Error::InvalidData("brstm: coefficient range is invalid"))?;
            io.read_exact(coefficients)?;
            io.seek(return_pos)?;
        }

        io.seek(data_offset)?;
        if io.tag()? != DATA_MAGIC || u64::from(io.rb32()?) != data_size {
            return Err(Error::InvalidData("brstm: malformed DATA chunk"));
        }

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
        stream.duration_ts = i64::try_from(total_samples).ok();
        stream.frame_count = Some(total_samples);
        Ok(Self {
            io,
            stream,
            data_start,
            adpc_data_start: adpc_offset + 8,
            coefficients,
            block_count,
            block_size,
            samples_per_block,
            final_block_size,
            final_block_samples,
            blocks_emitted: 0,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for BrstmDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.blocks_emitted >= self.block_count {
            return Err(Error::Eof);
        }
        let is_final = self.blocks_emitted + 1 == self.block_count;
        let block_bytes = if is_final {
            self.final_block_size
        } else {
            self.block_size
        };
        let samples = if is_final {
            self.final_block_samples
        } else {
            self.samples_per_block
        };
        let raw_bytes = usize::try_from(u64::from(block_bytes) * u64::from(CHANNELS))
            .map_err(|_| Error::InvalidData("brstm: packet size overflows"))?;
        let total_bytes = SYNTHESIZED_PREFIX_BYTES
            .checked_add(raw_bytes)
            .ok_or(Error::InvalidData("brstm: packet allocation overflows"))?;
        let mut packet = Packet::alloc(&mut self.budget, total_bytes)?;
        packet.len = total_bytes;
        let payload = packet.payload_mut();
        let raw_bytes = u32::try_from(raw_bytes)
            .map_err(|_| Error::InvalidData("brstm: packet payload size overflows"))?;
        payload
            .get_mut(..4)
            .ok_or(Error::InvalidData("brstm: packet prefix is truncated"))?
            .copy_from_slice(&raw_bytes.to_be_bytes());
        payload
            .get_mut(4..8)
            .ok_or(Error::InvalidData("brstm: packet prefix is truncated"))?
            .copy_from_slice(&samples.to_be_bytes());
        payload
            .get_mut(8..72)
            .ok_or(Error::InvalidData(
                "brstm: packet coefficient prefix is truncated",
            ))?
            .copy_from_slice(&self.coefficients);

        let adpc_pos = self
            .adpc_data_start
            .checked_add(self.blocks_emitted.saturating_mul(ADPC_ENTRY_BYTES as u64))
            .ok_or(Error::InvalidData("brstm: ADPC entry offset overflows"))?;
        self.io.seek(adpc_pos)?;
        let adpc = payload
            .get_mut(72..SYNTHESIZED_PREFIX_BYTES)
            .ok_or(Error::InvalidData("brstm: packet ADPC prefix is truncated"))?;
        self.io.read_exact(adpc)?;

        let frame_start = self
            .data_start
            .checked_add(
                self.blocks_emitted
                    .saturating_mul(u64::from(CHANNELS))
                    .saturating_mul(u64::from(self.block_size)),
            )
            .ok_or(Error::InvalidData("brstm: packet data offset overflows"))?;
        let per_channel = usize::try_from(block_bytes)
            .map_err(|_| Error::InvalidData("brstm: channel block size overflows"))?;
        for channel in 0..usize::try_from(CHANNELS).unwrap_or(0) {
            let offset = frame_start
                .checked_add(u64::try_from(channel).unwrap_or(0) * u64::from(self.block_size))
                .ok_or(Error::InvalidData("brstm: channel data offset overflows"))?;
            self.io.seek(offset)?;
            let start = SYNTHESIZED_PREFIX_BYTES
                .checked_add(channel.saturating_mul(per_channel))
                .ok_or(Error::InvalidData("brstm: packet channel offset overflows"))?;
            let end = start
                .checked_add(per_channel)
                .ok_or(Error::InvalidData("brstm: packet channel range overflows"))?;
            let channel_data = payload
                .get_mut(start..end)
                .ok_or(Error::InvalidData("brstm: packet channel range is invalid"))?;
            self.io.read_exact(channel_data)?;
        }
        let pts = self
            .blocks_emitted
            .saturating_mul(u64::from(self.samples_per_block));
        packet.stream_index = 0;
        packet.pts = vaco_core::Timestamp::new(i64::try_from(pts).unwrap_or(i64::MAX));
        packet.dts = packet.pts;
        packet.flags = PacketFlags::KEY;
        packet
            .side_data
            .push(PacketSideData::DurationTicks(i64::from(samples)));
        let sample_rate = i64::from(
            self.stream
                .params
                .audio
                .as_ref()
                .map_or(1, |audio| audio.sample_rate.max(1)),
        );
        packet.duration = vaco_core::Duration::from_micros(
            i64::from(samples)
                .saturating_mul(1_000_000)
                .div_euclid(sample_rate),
        );
        self.blocks_emitted = self.blocks_emitted.saturating_add(1);
        Ok(packet)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        Err(Error::Unsupported("brstm: seeking is not implemented"))
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        self.stream.duration()
    }
}

fn validate_range(start: u64, len: u64, end: u64, name: &'static str) -> Result<()> {
    let Some(range_end) = start.checked_add(len) else {
        return Err(Error::InvalidData("brstm: chunk range overflows"));
    };
    if range_end > end {
        return Err(Error::InvalidData(match name {
            "HEAD" => "brstm: HEAD range exceeds file",
            "DATA" => "brstm: DATA range exceeds file",
            "HEAD part 1" => "brstm: HEAD part 1 range exceeds HEAD",
            "ADPCM data" => "brstm: ADPCM data exceeds DATA",
            _ => "brstm: chunk range exceeds container",
        }));
    }
    Ok(())
}
