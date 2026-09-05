//! Nintendo BFSTM/BCSTM (`FSTM`/`CSTM`) container headers.
//!
//! The supported subset is stereo DSP-ADPCM in either byte order, with the
//! measured block geometries documented in `docs/format/vaco-format-misc-audio.md`.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags, PacketSideData};

const FSTM_MAGIC: [u8; 4] = *b"FSTM";
const CSTM_MAGIC: [u8; 4] = *b"CSTM";
const INFO_MAGIC: [u8; 4] = *b"INFO";
const SEEK_MAGIC: [u8; 4] = *b"SEEK";
const DATA_MAGIC: [u8; 4] = *b"DATA";
const HEADER_SIZE: u64 = 0x40;
const CHANNELS: u32 = 2;
const DSP_ADPCM_CODEC: u8 = 2;
const COEFFICIENT_BYTES_PER_CHANNEL: usize = 32;
const SEEK_BYTES_PER_CHANNEL: u32 = 4;
const SYNTHESIZED_PREFIX_BYTES: usize = 80;

#[derive(Clone, Copy, Debug)]
enum ByteOrder {
    Big,
    Little,
}

impl ByteOrder {
    fn read_u16(self, io: &mut IoContext) -> Result<u16> {
        match self {
            Self::Big => io.rb16(),
            Self::Little => io.rl16(),
        }
    }

    fn read_u32(self, io: &mut IoContext) -> Result<u32> {
        match self {
            Self::Big => io.rb32(),
            Self::Little => io.rl32(),
        }
    }

    fn write_u32(self, value: u32, dst: &mut [u8]) {
        let bytes = match self {
            Self::Big => value.to_be_bytes(),
            Self::Little => value.to_le_bytes(),
        };
        dst.copy_from_slice(&bytes);
    }
}

#[derive(Clone, Copy, Debug)]
struct Section {
    offset: u64,
    size: u64,
}

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let magic = data.starts_with(&FSTM_MAGIC) || data.starts_with(&CSTM_MAGIC);
    if !magic {
        return ProbeScore::NONE;
    }
    if matches!(data.rb16(4), Some(0xfeff | 0xfffe)) {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::MAGIC
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "bfstm",
    long_name: "Nintendo BFSTM/BCSTM (stereo DSP-ADPCM)",
    extensions: &["bfstm", "bcstm"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(BfstmDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct BfstmDemuxer {
    io: IoContext,
    stream: Stream,
    order: ByteOrder,
    data_start: u64,
    seek_data_start: u64,
    coefficients: [u8; COEFFICIENT_BYTES_PER_CHANNEL * 2],
    block_count: u64,
    block_size: u32,
    samples_per_block: u32,
    final_block_size: u32,
    final_block_samples: u32,
    blocks_emitted: u64,
    budget: Budget,
}

impl BfstmDemuxer {
    /// Opens a documented `FSTM` or `CSTM` stereo DSP-ADPCM stream.
    ///
    /// # Errors
    /// Returns [`Error::InvalidData`] for malformed section/reference layouts
    /// and [`Error::Unsupported`] outside the measured subset.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let magic = io.tag()?;
        if magic != FSTM_MAGIC && magic != CSTM_MAGIC {
            return Err(Error::InvalidData("bfstm: missing FSTM/CSTM signature"));
        }
        let order = match io.rb16()? {
            0xfeff => ByteOrder::Big,
            0xfffe => ByteOrder::Little,
            _ => return Err(Error::InvalidData("bfstm: invalid byte-order marker")),
        };
        let header_size = u64::from(order.read_u16(&mut io)?);
        let _version = order.read_u32(&mut io)?;
        let declared_file_size = u64::from(order.read_u32(&mut io)?);
        let section_count = order.read_u16(&mut io)?;
        let _reserved = order.read_u16(&mut io)?;
        if header_size != HEADER_SIZE || section_count != 3 {
            return Err(Error::Unsupported(
                "bfstm: only the three-section INFO/SEEK/DATA layout is supported",
            ));
        }
        if declared_file_size < HEADER_SIZE {
            return Err(Error::InvalidData("bfstm: declared file is too small"));
        }
        if let Some(actual_size) = io.size()
            && declared_file_size > actual_size
        {
            return Err(Error::InvalidData(
                "bfstm: declared file size exceeds source",
            ));
        }

        let info = read_section(&mut io, order, 0x4000, "INFO")?;
        let seek = read_section(&mut io, order, 0x4001, "SEEK")?;
        let data = read_section(&mut io, order, 0x4002, "DATA")?;
        validate_range(info.offset, info.size, declared_file_size, "INFO")?;
        validate_range(seek.offset, seek.size, declared_file_size, "SEEK")?;
        validate_range(data.offset, data.size, declared_file_size, "DATA")?;

        let info_end = info
            .offset
            .checked_add(info.size)
            .ok_or(Error::InvalidData("bfstm: INFO range overflows"))?;
        io.seek(info.offset)?;
        if io.tag()? != INFO_MAGIC || u64::from(order.read_u32(&mut io)?) != info.size {
            return Err(Error::InvalidData("bfstm: malformed INFO section"));
        }
        let info_reference_base = info
            .offset
            .checked_add(8)
            .ok_or(Error::InvalidData("bfstm: INFO reference base overflows"))?;
        let stream_info = read_reference(&mut io, order, 0x4100, info_reference_base, "stream")?;
        let _track_kind = order.read_u16(&mut io)?;
        let _track_padding = order.read_u16(&mut io)?;
        let _track_offset = order.read_u32(&mut io)?;
        let channel_table =
            read_reference(&mut io, order, 0x0101, info_reference_base, "channel table")?;
        validate_range(stream_info, 0x38, info_end, "stream info")?;

        io.seek(stream_info)?;
        let codec = io.r8()?;
        let _loop_flag = io.r8()?;
        let channels = u32::from(io.r8()?);
        let regions = io.r8()?;
        let sample_rate = order.read_u32(&mut io)?;
        let _loop_start = order.read_u32(&mut io)?;
        let total_samples = u64::from(order.read_u32(&mut io)?);
        let block_count = u64::from(order.read_u32(&mut io)?);
        let block_size = order.read_u32(&mut io)?;
        let samples_per_block = order.read_u32(&mut io)?;
        let final_block_size = order.read_u32(&mut io)?;
        let final_block_samples = order.read_u32(&mut io)?;
        let final_block_padded_size = order.read_u32(&mut io)?;
        let seek_bytes_per_channel = order.read_u32(&mut io)?;
        let seek_interval_samples = order.read_u32(&mut io)?;
        let data_kind = order.read_u16(&mut io)?;
        let _data_padding = order.read_u16(&mut io)?;
        let data_relative = u64::from(order.read_u32(&mut io)?);

        if codec != DSP_ADPCM_CODEC {
            return Err(Error::Unsupported(
                "bfstm: only DSP-ADPCM encoding is supported",
            ));
        }
        if channels != CHANNELS {
            return Err(Error::Unsupported(
                "bfstm: only measured stereo DSP-ADPCM is supported",
            ));
        }
        if regions != 0 {
            return Err(Error::Unsupported("bfstm: region tables are not supported"));
        }
        if sample_rate == 0 {
            return Err(Error::InvalidData("bfstm: sample rate is zero"));
        }
        let expected_total_samples = block_count
            .checked_sub(1)
            .and_then(|blocks| blocks.checked_mul(u64::from(samples_per_block)))
            .and_then(|samples| samples.checked_add(u64::from(final_block_samples)));
        if !matches!(block_size, 16 | 32 | 64 | 96 | 256)
            || block_count == 0
            || samples_per_block != block_size.div_euclid(8) * 14
            || final_block_size != block_size && final_block_size != block_size.div_euclid(2)
            || final_block_samples != final_block_size.div_euclid(8) * 14
            || final_block_padded_size != block_size
            || seek_bytes_per_channel != SEEK_BYTES_PER_CHANNEL
            || seek_interval_samples != samples_per_block
            || expected_total_samples != Some(total_samples)
        {
            return Err(Error::Unsupported(
                "bfstm: block geometry is outside the measured subset",
            ));
        }
        if data_kind != 0x1f00 {
            return Err(Error::InvalidData("bfstm: malformed sample-data reference"));
        }

        validate_range(channel_table, 20, info_end, "channel table")?;
        io.seek(channel_table)?;
        if order.read_u32(&mut io)? != CHANNELS {
            return Err(Error::InvalidData("bfstm: channel table count mismatch"));
        }
        let mut coefficients = [0; COEFFICIENT_BYTES_PER_CHANNEL * 2];
        for channel in 0..usize::try_from(CHANNELS).unwrap_or(0) {
            let reference = channel_table
                .checked_add(4)
                .and_then(|base| base.checked_add(u64::try_from(channel).unwrap_or(0) * 8))
                .ok_or(Error::InvalidData("bfstm: channel reference overflows"))?;
            io.seek(reference)?;
            let channel_info =
                read_reference(&mut io, order, 0x4102, channel_table, "channel info")?;
            validate_range(channel_info, 8, info_end, "channel info")?;
            io.seek(channel_info)?;
            let dsp_info = read_reference(&mut io, order, 0x0300, channel_info, "DSP-ADPCM info")?;
            validate_range(
                dsp_info,
                COEFFICIENT_BYTES_PER_CHANNEL as u64,
                info_end,
                "DSP-ADPCM info",
            )?;
            io.seek(dsp_info)?;
            let start = channel * COEFFICIENT_BYTES_PER_CHANNEL;
            let end = start + COEFFICIENT_BYTES_PER_CHANNEL;
            let channel_coefficients = coefficients
                .get_mut(start..end)
                .ok_or(Error::InvalidData("bfstm: coefficient range is invalid"))?;
            io.read_exact(channel_coefficients)?;
        }

        let seek_required = block_count
            .checked_mul(u64::from(CHANNELS))
            .and_then(|entries| entries.checked_mul(u64::from(SEEK_BYTES_PER_CHANNEL)))
            .and_then(|bytes| bytes.checked_add(8))
            .ok_or(Error::InvalidData("bfstm: SEEK table length overflows"))?;
        if seek.size < seek_required {
            return Err(Error::InvalidData("bfstm: truncated SEEK table"));
        }
        io.seek(seek.offset)?;
        if io.tag()? != SEEK_MAGIC || u64::from(order.read_u32(&mut io)?) != seek.size {
            return Err(Error::InvalidData("bfstm: malformed SEEK section"));
        }

        let data_reference_base = data
            .offset
            .checked_add(8)
            .ok_or(Error::InvalidData("bfstm: DATA reference base overflows"))?;
        let data_start = data_reference_base
            .checked_add(data_relative)
            .ok_or(Error::InvalidData("bfstm: sample-data offset overflows"))?;
        let data_end = data
            .offset
            .checked_add(data.size)
            .ok_or(Error::InvalidData("bfstm: DATA range overflows"))?;
        let physical_data_bytes = block_count
            .checked_sub(1)
            .and_then(|blocks| blocks.checked_mul(u64::from(CHANNELS)))
            .and_then(|blocks| blocks.checked_mul(u64::from(block_size)))
            .and_then(|bytes| {
                u64::from(CHANNELS)
                    .checked_mul(u64::from(final_block_padded_size))
                    .and_then(|final_bytes| bytes.checked_add(final_bytes))
            })
            .ok_or(Error::InvalidData("bfstm: sample-data length overflows"))?;
        validate_range(data_start, physical_data_bytes, data_end, "sample data")?;
        io.seek(data.offset)?;
        if io.tag()? != DATA_MAGIC || u64::from(order.read_u32(&mut io)?) != data.size {
            return Err(Error::InvalidData("bfstm: malformed DATA section"));
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
            order,
            data_start,
            seek_data_start: seek.offset + 8,
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

impl Demuxer for BfstmDemuxer {
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
        let raw_bytes = u64::from(block_bytes)
            .checked_mul(u64::from(CHANNELS))
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(Error::InvalidData("bfstm: packet size overflows"))?;
        let total_bytes = SYNTHESIZED_PREFIX_BYTES
            .checked_add(raw_bytes)
            .ok_or(Error::InvalidData("bfstm: packet allocation overflows"))?;
        let mut packet = Packet::alloc(&mut self.budget, total_bytes)?;
        packet.len = total_bytes;
        let payload = packet.payload_mut();
        let raw_bytes_dst = payload
            .get_mut(..4)
            .ok_or(Error::InvalidData("bfstm: packet prefix is truncated"))?;
        self.order.write_u32(
            u32::try_from(raw_bytes)
                .map_err(|_| Error::InvalidData("bfstm: packet payload size overflows"))?,
            raw_bytes_dst,
        );
        let samples_dst = payload
            .get_mut(4..8)
            .ok_or(Error::InvalidData("bfstm: packet prefix is truncated"))?;
        self.order.write_u32(samples, samples_dst);
        let coefficient_dst = payload
            .get_mut(8..72)
            .ok_or(Error::InvalidData("bfstm: packet prefix is truncated"))?;
        coefficient_dst.copy_from_slice(&self.coefficients);

        let seek_entry = self
            .seek_data_start
            .checked_add(self.blocks_emitted.saturating_mul(8))
            .ok_or(Error::InvalidData("bfstm: SEEK entry offset overflows"))?;
        self.io.seek(seek_entry)?;
        let seek_dst = payload
            .get_mut(72..80)
            .ok_or(Error::InvalidData("bfstm: packet prefix is truncated"))?;
        self.io.read_exact(seek_dst)?;

        let frame_start = self
            .data_start
            .checked_add(
                self.blocks_emitted
                    .saturating_mul(u64::from(CHANNELS))
                    .saturating_mul(u64::from(self.block_size)),
            )
            .ok_or(Error::InvalidData("bfstm: packet data offset overflows"))?;
        let per_channel = usize::try_from(block_bytes)
            .map_err(|_| Error::InvalidData("bfstm: channel block size overflows"))?;
        for channel in 0..usize::try_from(CHANNELS).unwrap_or(0) {
            let channel_offset = frame_start
                .checked_add(u64::try_from(channel).unwrap_or(0) * u64::from(self.block_size))
                .ok_or(Error::InvalidData("bfstm: channel data offset overflows"))?;
            self.io.seek(channel_offset)?;
            let start = SYNTHESIZED_PREFIX_BYTES + channel * per_channel;
            let end = start + per_channel;
            let channel_dst = payload
                .get_mut(start..end)
                .ok_or(Error::InvalidData("bfstm: packet channel range is invalid"))?;
            self.io.read_exact(channel_dst)?;
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
        packet.duration = vaco_core::Duration::from_ticks(i64::from(samples), self.stream.time_base)
            .unwrap_or(vaco_core::Duration::ZERO);
        self.blocks_emitted = self.blocks_emitted.saturating_add(1);
        Ok(packet)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        Err(Error::Unsupported("bfstm: seeking is not implemented"))
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        self.stream.duration()
    }
}

fn read_section(
    io: &mut IoContext,
    order: ByteOrder,
    expected_kind: u16,
    name: &'static str,
) -> Result<Section> {
    let kind = order.read_u16(io)?;
    let _padding = order.read_u16(io)?;
    let offset = u64::from(order.read_u32(io)?);
    let size = u64::from(order.read_u32(io)?);
    if kind != expected_kind || size < 8 {
        return Err(Error::InvalidData(match name {
            "INFO" => "bfstm: malformed INFO reference",
            "SEEK" => "bfstm: malformed SEEK reference",
            _ => "bfstm: malformed DATA reference",
        }));
    }
    Ok(Section { offset, size })
}

fn read_reference(
    io: &mut IoContext,
    order: ByteOrder,
    expected_kind: u16,
    base: u64,
    name: &'static str,
) -> Result<u64> {
    let kind = order.read_u16(io)?;
    let _padding = order.read_u16(io)?;
    let relative = u64::from(order.read_u32(io)?);
    if kind != expected_kind || relative == u64::from(u32::MAX) {
        return Err(Error::InvalidData(match name {
            "stream" => "bfstm: malformed stream-info reference",
            "channel table" => "bfstm: malformed channel-table reference",
            "channel info" => "bfstm: malformed channel-info reference",
            _ => "bfstm: malformed DSP-ADPCM reference",
        }));
    }
    base.checked_add(relative)
        .ok_or(Error::InvalidData("bfstm: reference offset overflows"))
}

fn validate_range(start: u64, len: u64, end: u64, name: &'static str) -> Result<()> {
    let Some(range_end) = start.checked_add(len) else {
        return Err(Error::InvalidData("bfstm: range overflows"));
    };
    if range_end > end {
        return Err(Error::InvalidData(match name {
            "INFO" => "bfstm: INFO range exceeds file",
            "SEEK" => "bfstm: SEEK range exceeds file",
            "DATA" => "bfstm: DATA range exceeds file",
            "stream info" => "bfstm: stream info exceeds INFO",
            "channel table" => "bfstm: channel table exceeds INFO",
            "channel info" => "bfstm: channel info exceeds INFO",
            "DSP-ADPCM info" => "bfstm: DSP-ADPCM info exceeds INFO",
            _ => "bfstm: sample data exceeds DATA",
        }));
    }
    Ok(())
}
