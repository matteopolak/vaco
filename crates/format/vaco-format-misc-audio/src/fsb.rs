//! Bounded FMOD Sample Bank (`FSB4`/`FSB5`) structural demuxing.
//!
//! `FSB4` is accepted only for the reference-identified Nintendo THP mode.
//! `FSB5` uses the published compact sample-header layout and accepts PCM8,
//! PCM16, PCM32, MPEG, and Vorbis banks. The demuxer reports each bank entry as
//! a stream and returns its stored payload without decoding it.
//!
//! The FSB5 header and metadata bit fields follow the MIT-licensed
//! `python-fsb5` reader and its accompanying binary-template documentation.
//! FSB4's 80-byte directory entry follows the published FMOD/Xentax layout;
//! its THP mode and 16-byte packet geometry were measured against `ffprobe`
//! 9.0.1. No FFmpeg source is used here.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

const FSB4: &[u8; 4] = b"FSB4";
const FSB5: &[u8; 4] = b"FSB5";
const FSB4_HEADER_BYTES: u64 = 0x30;
const FSB5_HEADER_BYTES: u64 = 0x3c;
const SAMPLE_HEADER_BYTES: u64 = 0x50;
const THP_MODE: u32 = 0x4000_0802;
const THP_PACKET_BYTES: u32 = 16;
const THP_SAMPLES_PER_PACKET: u64 = 14;
const MAX_SAMPLES: u32 = 4096;
const MAX_SECTION_BYTES: u32 = 1 << 30;
const MAX_PACKET_BYTES: u64 = 1 << 24;

/// Which FSB revision was parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsbVersion {
    Four,
    Five,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    stream_index: u32,
    offset: u64,
    len: u64,
    count: u64,
    packet_bytes: Option<u32>,
}

#[derive(Debug)]
pub struct FsbDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    samples: Vec<Sample>,
    next_sample: usize,
    packet_offset: u64,
    version: FsbVersion,
    budget: Budget,
}

/// Recognizes either published FSB signature.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(FSB4) || data.starts_with(FSB5) {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

/// Descriptor for the bounded FSB4/FSB5 structural reader.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "fsb",
    long_name: "FMOD Sample Bank",
    extensions: &["fsb"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(FsbDemuxer::open(src)?))
}

impl FsbDemuxer {
    /// Opens a bounded FSB4 or FSB5 bank and indexes its sample payloads.
    ///
    /// # Errors
    /// Returns [`Error::InvalidData`] for truncated or inconsistent ranges,
    /// and [`Error::Unsupported`] for an FSB mode outside this reader's
    /// measured subset.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut magic = [0; 4];
        io.read_exact(&mut magic)?;
        let (version, streams, samples) = if magic == *FSB4 {
            parse_fsb4(&mut io)?
        } else if magic == *FSB5 {
            parse_fsb5(&mut io)?
        } else {
            return Err(Error::InvalidData("fsb: missing FSB4/FSB5 signature"));
        };
        Ok(Self {
            io,
            streams,
            samples,
            next_sample: 0,
            packet_offset: 0,
            version,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }

    /// Returns the FSB revision selected by the signature.
    #[must_use]
    pub const fn version(&self) -> FsbVersion {
        self.version
    }
}

impl Demuxer for FsbDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            let Some(sample) = self.samples.get(self.next_sample).copied() else {
                return Err(Error::Eof);
            };
            if self.packet_offset >= sample.len {
                self.next_sample = self.next_sample.saturating_add(1);
                self.packet_offset = 0;
                continue;
            }
            let requested = sample
                .packet_bytes
                .map_or(sample.len, u64::from)
                .min(sample.len.saturating_sub(self.packet_offset));
            if requested == 0 || requested > MAX_PACKET_BYTES {
                return Err(Error::InvalidData("fsb: sample packet exceeds bound"));
            }
            let packet_len = usize::try_from(requested)
                .map_err(|_| Error::InvalidData("fsb: packet length overflows usize"))?;
            let data_pos = sample.offset.saturating_add(self.packet_offset);
            self.io.seek(data_pos)?;
            let mut packet = Packet::alloc(&mut self.budget, packet_len)?;
            self.io.read_exact(packet.payload_mut())?;
            packet.len = packet_len;
            packet.stream_index = sample.stream_index;
            let (pts, duration) = if let Some(packet_bytes) = sample.packet_bytes {
                #[allow(
                    clippy::integer_division,
                    reason = "packet index is an exact integral block count"
                )]
                let packet_index = self.packet_offset / u64::from(packet_bytes);
                let ticks = packet_index.saturating_mul(THP_SAMPLES_PER_PACKET);
                (
                    ticks,
                    Duration::from_ticks(
                        i64::try_from(THP_SAMPLES_PER_PACKET).unwrap_or(i64::MAX),
                        self.streams
                            .get(sample.stream_index as usize)
                            .map_or(Rational::new(1, 1), |stream| stream.time_base),
                    )
                    .unwrap_or(Duration::ZERO),
                )
            } else {
                (
                    0,
                    Duration::from_ticks(
                        i64::try_from(sample.count).unwrap_or(i64::MAX),
                        self.streams
                            .get(sample.stream_index as usize)
                            .map_or(Rational::new(1, 1), |stream| stream.time_base),
                    )
                    .unwrap_or(Duration::ZERO),
                )
            };
            packet.pts = Timestamp::new(i64::try_from(pts).unwrap_or(i64::MAX));
            packet.dts = packet.pts;
            packet.duration = duration;
            packet.flags = PacketFlags::KEY;
            packet.pos = Some(data_pos);
            self.packet_offset = self.packet_offset.saturating_add(requested);
            return Ok(packet);
        }
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        Err(Error::Unsupported(
            "fsb: indexed sample seek is unsupported",
        ))
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

fn parse_fsb4(io: &mut IoContext) -> Result<(FsbVersion, Vec<Stream>, Vec<Sample>)> {
    let count = io.rl32()?;
    let headers_size = io.rl32()?;
    let data_size = io.rl32()?;
    let version = io.rl32()?;
    let flags = io.rl32()?;
    let mut ignored = [0; 24];
    io.read_exact(&mut ignored)?;
    if !(1..=MAX_SAMPLES).contains(&count) {
        return Err(Error::InvalidData("fsb4: sample count exceeds bound"));
    }
    if headers_size > MAX_SECTION_BYTES || data_size > MAX_SECTION_BYTES {
        return Err(Error::InvalidData("fsb4: section exceeds bound"));
    }
    if version != 0x0004_0000 {
        return Err(Error::Unsupported("fsb4: only version 4.0 is supported"));
    }
    if flags & 0x04 != 0 {
        return Err(Error::Unsupported(
            "fsb4: encrypted sample data is unsupported",
        ));
    }
    let header_end = FSB4_HEADER_BYTES
        .checked_add(u64::from(headers_size))
        .ok_or(Error::InvalidData("fsb4: header range overflows"))?;
    ensure_file_range(io, header_end, u64::from(data_size))?;
    let data_start = header_end;
    let mut streams = Vec::new();
    let mut samples = Vec::new();
    let mut data_offset = data_start;
    let headers_start = io.pos();
    for stream_index in 0..count {
        let entry_start = io.pos();
        let entry_len = u64::from(io.rl16()?);
        if entry_len < SAMPLE_HEADER_BYTES
            || entry_len
                > u64::from(headers_size).saturating_sub(entry_start.saturating_sub(headers_start))
        {
            return Err(Error::InvalidData("fsb4: invalid sample header length"));
        }
        let mut name = [0; 30];
        io.read_exact(&mut name)?;
        let sample_count = u64::from(io.rl32()?);
        let compressed_len = u64::from(io.rl32()?);
        let _loop_start = io.rl32()?;
        let _loop_end = io.rl32()?;
        // The FSB4 THP fixture accepted by ffprobe stores this scalar in the
        // byte order shown below; the surrounding directory fields remain LE.
        let mode = io.rb32()?;
        let sample_rate = io.rl32()?;
        let _volume = io.rl16()?;
        let _pan = io.rl16()?;
        let _priority = io.rl16()?;
        let channels = u32::from(io.rl16()?);
        if mode != THP_MODE {
            return Err(Error::Unsupported(
                "fsb4: only Nintendo THP audio is supported",
            ));
        }
        if sample_rate == 0 || sample_rate > 192_000 || channels == 0 || channels > 32 {
            return Err(Error::InvalidData("fsb4: invalid sample geometry"));
        }
        let entry_end = entry_start
            .checked_add(entry_len)
            .ok_or(Error::InvalidData("fsb4: sample header range overflows"))?;
        io.seek(entry_end)?;
        streams.push(audio_stream(
            stream_index,
            sample_rate,
            channels,
            None,
            sample_count,
        ));
        samples.push(Sample {
            stream_index,
            offset: data_offset,
            len: compressed_len,
            count: sample_count,
            packet_bytes: Some(THP_PACKET_BYTES),
        });
        data_offset = data_offset
            .checked_add(compressed_len)
            .ok_or(Error::InvalidData("fsb4: sample data range overflows"))?;
    }
    if io.pos() != header_end || data_offset != data_start.saturating_add(u64::from(data_size)) {
        return Err(Error::InvalidData(
            "fsb4: directory/data sizes do not agree",
        ));
    }
    ensure_file_range(io, data_start, u64::from(data_size))?;
    Ok((FsbVersion::Four, streams, samples))
}

fn parse_fsb5(io: &mut IoContext) -> Result<(FsbVersion, Vec<Stream>, Vec<Sample>)> {
    let version = io.rl32()?;
    let count = io.rl32()?;
    let headers_size = io.rl32()?;
    let names_size = io.rl32()?;
    let data_size = io.rl32()?;
    let mode = io.rl32()?;
    let mut ignored = [0; 32];
    io.read_exact(&mut ignored)?;
    let base_header = if version == 0 {
        io.skip(4)?;
        FSB5_HEADER_BYTES.saturating_add(4)
    } else {
        FSB5_HEADER_BYTES
    };
    if !(1..=MAX_SAMPLES).contains(&count) {
        return Err(Error::InvalidData("fsb5: sample count exceeds bound"));
    }
    if headers_size > MAX_SECTION_BYTES
        || names_size > MAX_SECTION_BYTES
        || data_size > MAX_SECTION_BYTES
    {
        return Err(Error::InvalidData("fsb5: section exceeds bound"));
    }
    let codec = codec_for_fsb5_mode(mode)?;
    let header_end = base_header
        .checked_add(u64::from(headers_size))
        .ok_or(Error::InvalidData("fsb5: sample-header range overflows"))?;
    ensure_file_range(io, header_end, u64::from(names_size))?;
    let mut streams = Vec::new();
    let mut samples_meta = Vec::new();
    for stream_index in 0..count {
        let raw = io.rl64()?;
        let has_metadata = raw & 1 != 0;
        let frequency_code = u32::try_from((raw >> 1) & 0x0f).unwrap_or(0);
        let channels_bit = u32::try_from((raw >> 5) & 1).unwrap_or(0);
        let data_offset_units = (raw >> 6) & 0x0fff_ffff;
        let sample_count = (raw >> 34) & 0x3fff_ffff;
        let mut frequency = frequency_from_code(frequency_code);
        let mut channels = channels_bit.saturating_add(1);
        if has_metadata {
            let (metadata_frequency, metadata_channels) = read_metadata(io, header_end)?;
            if let Some(value) = metadata_frequency {
                frequency = Some(value);
            }
            if let Some(value) = metadata_channels {
                channels = value;
            }
        }
        let sample_rate = frequency.ok_or(Error::Unsupported(
            "fsb5: sample frequency code is unsupported",
        ))?;
        if sample_rate == 0 || sample_rate > 192_000 {
            return Err(Error::InvalidData("fsb5: invalid sample frequency"));
        }
        if channels == 0 || channels > 32 {
            return Err(Error::Unsupported("fsb5: channel metadata is unsupported"));
        }
        streams.push(audio_stream(
            stream_index,
            sample_rate,
            channels,
            codec,
            sample_count,
        ));
        samples_meta.push((stream_index, data_offset_units, sample_count));
    }
    if io.pos() != header_end {
        return Err(Error::InvalidData(
            "fsb5: sample-header size does not match metadata",
        ));
    }
    let names_start = io.pos();
    let names_end = names_start
        .checked_add(u64::from(names_size))
        .ok_or(Error::InvalidData("fsb5: name-table range overflows"))?;
    let offset_count = u64::from(count).saturating_mul(4);
    if names_size != 0 && u64::from(names_size) < offset_count {
        return Err(Error::InvalidData("fsb5: name table is missing offsets"));
    }
    if names_size != 0 {
        let string_bytes = u64::from(names_size).saturating_sub(offset_count);
        for _ in 0..count {
            let offset = u64::from(io.rl32()?);
            if offset > string_bytes {
                return Err(Error::InvalidData("fsb5: name offset is outside the table"));
            }
        }
    }
    io.seek(names_end)?;
    let data_start = names_end;
    ensure_file_range(io, data_start, u64::from(data_size))?;
    let data_end = data_start.saturating_add(u64::from(data_size));
    let mut samples = Vec::new();
    for (index, (stream_index, units, sample_count)) in samples_meta.iter().copied().enumerate() {
        let offset = units
            .checked_mul(16)
            .and_then(|value| data_start.checked_add(value))
            .ok_or(Error::InvalidData("fsb5: sample data offset overflows"))?;
        if offset < data_start || offset > data_end {
            return Err(Error::InvalidData(
                "fsb5: sample data offset is outside data",
            ));
        }
        let end_relative = if let Some((_, next_units, _)) = samples_meta.get(index + 1) {
            next_units
                .checked_mul(16)
                .ok_or(Error::InvalidData("fsb5: sample data offset overflows"))?
        } else {
            u64::from(data_size)
        };
        let end = data_start
            .checked_add(end_relative)
            .ok_or(Error::InvalidData("fsb5: sample data range overflows"))?;
        if end < offset || end > data_end {
            return Err(Error::InvalidData("fsb5: sample data ranges overlap"));
        }
        samples.push(Sample {
            stream_index,
            offset,
            len: end.saturating_sub(offset),
            count: sample_count,
            packet_bytes: None,
        });
    }
    Ok((FsbVersion::Five, streams, samples))
}

fn read_metadata(io: &mut IoContext, header_end: u64) -> Result<(Option<u32>, Option<u32>)> {
    let mut frequency = None;
    let mut channels = None;
    let mut next = true;
    while next {
        let raw = io.rl32()?;
        next = raw & 1 != 0;
        let size = (raw >> 1) & 0x00ff_ffff;
        let kind = raw >> 25;
        let payload_start = io.pos();
        let payload_end = payload_start
            .checked_add(u64::from(size))
            .ok_or(Error::InvalidData("fsb5: metadata chunk overflows"))?;
        if payload_end > header_end {
            return Err(Error::InvalidData("fsb5: metadata chunk exceeds header"));
        }
        match kind {
            1 if size == 1 => channels = Some(u32::from(io.r8()?)),
            2 if size == 4 => frequency = Some(io.rl32()?),
            3 if size == 8 => {
                let _loop_start = io.rl32()?;
                let _loop_end = io.rl32()?;
            }
            1..=3 => return Err(Error::InvalidData("fsb5: metadata chunk has wrong size")),
            _ => {}
        }
        io.seek(payload_end)?;
    }
    Ok((frequency, channels))
}

fn codec_for_fsb5_mode(mode: u32) -> Result<Option<CodecId>> {
    match mode {
        1 => Ok(Some(CodecId::PcmU8)),
        2 => Ok(Some(CodecId::PcmS16le)),
        4 => Ok(Some(CodecId::PcmS32le)),
        11 => Ok(Some(CodecId::Mp3)),
        15 => Ok(Some(CodecId::Vorbis)),
        0 => Err(Error::Unsupported("fsb5: no sound format is unsupported")),
        3 => Err(Error::Unsupported(
            "fsb5: PCM24 sound format 3 is unsupported",
        )),
        5 => Err(Error::Unsupported(
            "fsb5: float PCM sound format 5 is unsupported",
        )),
        6 => Err(Error::Unsupported(
            "fsb5: GCADPCM sound format 6 is unsupported",
        )),
        7 => Err(Error::Unsupported(
            "fsb5: IMA ADPCM sound format 7 is unsupported",
        )),
        8 => Err(Error::Unsupported(
            "fsb5: VAG sound format 8 is unsupported",
        )),
        9 => Err(Error::Unsupported(
            "fsb5: HEVAG sound format 9 is unsupported",
        )),
        10 => Err(Error::Unsupported(
            "fsb5: XMA sound format 10 is unsupported",
        )),
        12 => Err(Error::Unsupported(
            "fsb5: CELT sound format 12 is unsupported",
        )),
        13 => Err(Error::Unsupported(
            "fsb5: AT9 sound format 13 is unsupported",
        )),
        14 => Err(Error::Unsupported(
            "fsb5: XWMA sound format 14 is unsupported",
        )),
        _ => Err(Error::Unsupported(
            "fsb5: unknown sound format is unsupported",
        )),
    }
}

fn frequency_from_code(code: u32) -> Option<u32> {
    match code {
        1 => Some(8000),
        2 => Some(11_000),
        3 => Some(11_025),
        4 => Some(16_000),
        5 => Some(22_050),
        6 => Some(24_000),
        7 => Some(32_000),
        8 => Some(44_100),
        9 => Some(48_000),
        _ => None,
    }
}

fn audio_stream(
    index: u32,
    sample_rate: u32,
    channels: u32,
    codec: Option<CodecId>,
    sample_count: u64,
) -> Stream {
    let mut stream = Stream::new(
        index,
        MediaType::Audio,
        Rational::new(1, sample_rate.cast_signed()),
    );
    let mut params = CodecParameters::audio();
    params.codec_id = codec;
    if let Some(id) = codec
        && let Some(audio) = params.audio.as_mut()
    {
        audio.format = match id {
            CodecId::PcmU8 => Some(SampleFmt::U8),
            CodecId::PcmS16le => Some(SampleFmt::S16),
            CodecId::PcmS32le => Some(SampleFmt::S32),
            _ => None,
        };
    }
    if let Some(audio) = params.audio.as_mut() {
        audio.sample_rate = sample_rate;
        audio.layout = Some(
            ChannelLayout::default_for(channels)
                .unwrap_or_else(|| ChannelLayout::unspecified(channels)),
        );
    }
    stream.params = params;
    stream.duration_ts = i64::try_from(sample_count).ok();
    stream.frame_count = Some(sample_count);
    stream
}

fn ensure_file_range(io: &IoContext, start: u64, len: u64) -> Result<()> {
    let end = start
        .checked_add(len)
        .ok_or(Error::InvalidData("fsb: file range overflows"))?;
    if io.size().is_some_and(|size| end > size) {
        return Err(Error::InvalidData("fsb: declared range exceeds source"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn fsb4_fixture() -> Vec<u8> {
        let payload = [0x40_u8; 16];
        let mut out = Vec::new();
        out.extend_from_slice(FSB4);
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&80_u32.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x0004_0000_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        out.extend_from_slice(&SAMPLE_HEADER_BYTES.to_le_bytes()[..2]);
        out.extend_from_slice(&[0; 30]);
        out.extend_from_slice(&14_u32.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&THP_MODE.to_be_bytes());
        out.extend_from_slice(&44_100_u32.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&2_u16.to_le_bytes());
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&payload);
        out
    }

    #[test]
    fn fsb4_thp_reports_reference_packet_geometry() {
        let mut demuxer = FsbDemuxer::open(Box::new(MemorySource::new(fsb4_fixture()))).unwrap();
        assert_eq!(demuxer.version(), FsbVersion::Four);
        assert_eq!(demuxer.streams().first().unwrap().duration_ts, Some(14));
        let packet = demuxer.read_packet().unwrap();
        assert_eq!(packet.pos, Some(128));
        assert_eq!(packet.len, 16);
        assert_eq!(
            packet.duration.to_ticks(demuxer.streams()[0].time_base),
            Some(14)
        );
        assert!(matches!(demuxer.read_packet(), Err(Error::Eof)));
    }
}
