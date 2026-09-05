//! FastTracker 2 Extended Module (`.xm`) structural demuxing.
//!
//! This is intentionally a container reader, not a tracker player. It accepts
//! the v1.04 little-endian layout described by Kaitai's Unlicense format
//! description, walks the order/pattern/instrument/sample structures, and
//! exposes each uncompressed sample payload as one packet on its own stream.
//! Pattern events and XM delta-coded sample bytes are not interpreted;
//! decoding either would be a separate codec/player feature.
//!
//! The reader refuses older format revisions, malformed variable-size blocks,
//! and unsupported reserved sample flags. Count and byte limits keep
//! declarations from turning into unbounded allocations or seeks.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecParameters;
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

const SIGNATURE: &[u8; 17] = b"Extended Module: ";
const VERSION_1_04: u16 = 0x0104;
const PREHEADER_BYTES: u64 = 60;
const HEADER_BYTES: u32 = 276;
const PATTERN_HEADER_BYTES: u32 = 9;
const SAMPLE_HEADER_BYTES: u32 = 40;
const MAX_PATTERN_COUNT: u16 = 256;
const MAX_INSTRUMENT_COUNT: u16 = 128;
const MAX_SAMPLES_PER_INSTRUMENT: u16 = 256;
const MAX_TOTAL_SAMPLES: usize = 4096;
const MAX_VARIABLE_BLOCK: u32 = 1 << 20;
const MAX_PATTERN_DATA: u64 = 64 << 20;
const MAX_SAMPLE_DATA: u64 = 1 << 30;

/// Recognizes the XM signature without consuming the module body.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(SIGNATURE) {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

/// Descriptor for the bounded XM v1.04 structural demuxer.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "xm",
    long_name: "FastTracker 2 Extended Module",
    extensions: &["xm"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(XmDemuxer::open(src)?))
}

#[derive(Debug, Clone)]
struct Sample {
    stream_index: u32,
    data_offset: u64,
    data_len: usize,
}

#[derive(Debug)]
pub struct XmDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    samples: Vec<Sample>,
    next_sample: usize,
    budget: Budget,
}

impl XmDemuxer {
    /// Open a bounded XM v1.04 structural reader.
    ///
    /// # Errors
    /// [`Error::InvalidData`] is returned for malformed layout, and
    /// [`Error::Unsupported`] for versions or sample shapes outside this
    /// reader's declared scope. [`Error::NotSeekable`] is returned because
    /// packet emission revisits sample payload offsets after the structural
    /// scan.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let mut signature = [0; SIGNATURE.len()];
        io.read_exact(&mut signature)?;
        if signature != *SIGNATURE {
            return Err(Error::InvalidData("xm: missing Extended Module signature"));
        }
        let mut module_name = [0; 20];
        io.read_exact(&mut module_name)?;
        let mut marker = [0; 1];
        io.read_exact(&mut marker)?;
        if marker[0] != 0x1a {
            return Err(Error::InvalidData("xm: missing 0x1a signature marker"));
        }
        let mut tracker_name = [0; 20];
        io.read_exact(&mut tracker_name)?;
        let version = io.rl16()?;
        if version != VERSION_1_04 {
            return Err(Error::Unsupported("xm: only version 1.04 is supported"));
        }
        let header_size = io.rl32()?;
        if header_size != HEADER_BYTES {
            return Err(Error::Unsupported(
                "xm: non-standard v1.04 header size is unsupported",
            ));
        }
        let header_end = PREHEADER_BYTES.saturating_add(u64::from(header_size));
        ensure_end(&io, header_end)?;
        let header = read_vec(
            &mut io,
            usize::try_from(header_size - 4).unwrap_or(usize::MAX),
        )?;
        let song_length = read_u16(&header, 0)?;
        let restart_position = read_u16(&header, 2)?;
        let channels = read_u16(&header, 4)?;
        let patterns = read_u16(&header, 6)?;
        let instruments = read_u16(&header, 8)?;
        let _flags = read_u16(&header, 10)?;
        let _tempo = read_u16(&header, 12)?;
        let _bpm = read_u16(&header, 14)?;
        if !(2..=32).contains(&channels) || channels % 2 != 0 {
            return Err(Error::InvalidData("xm: channel count is not 2..32 even"));
        }
        if patterns > MAX_PATTERN_COUNT || instruments > MAX_INSTRUMENT_COUNT {
            return Err(Error::InvalidData("xm: count exceeds bounded scope"));
        }
        if song_length == 0 || song_length > 256 || restart_position >= song_length {
            return Err(Error::InvalidData("xm: invalid song order bounds"));
        }
        let order_table = header
            .get(16..272)
            .ok_or(Error::InvalidData("xm: truncated pattern order table"))?;
        if order_table
            .iter()
            .take(usize::from(song_length))
            .any(|&pattern| u16::from(pattern) >= patterns)
        {
            return Err(Error::InvalidData(
                "xm: pattern order references missing pattern",
            ));
        }

        let mut pattern_bytes = 0u64;
        for _ in 0..patterns {
            let pattern_header_size = io.rl32()?;
            if pattern_header_size != PATTERN_HEADER_BYTES {
                return Err(Error::Unsupported(
                    "xm: non-standard pattern header is unsupported",
                ));
            }
            let mut pattern_header = [0; 5];
            io.read_exact(&mut pattern_header)?;
            if pattern_header[0] != 0 {
                return Err(Error::InvalidData("xm: unsupported pattern packing type"));
            }
            let rows = u16::from_le_bytes([pattern_header[1], pattern_header[2]]);
            let packed_len = u16::from_le_bytes([pattern_header[3], pattern_header[4]]);
            if !(1..=256).contains(&rows) {
                return Err(Error::InvalidData("xm: pattern row count outside 1..256"));
            }
            pattern_bytes = pattern_bytes.saturating_add(u64::from(packed_len));
            if pattern_bytes > MAX_PATTERN_DATA {
                return Err(Error::InvalidData("xm: pattern data exceeds limit"));
            }
            io.skip(u64::from(packed_len))?;
        }

        let mut streams = Vec::new();
        let mut samples = Vec::new();
        let mut total_sample_data = 0u64;
        for _ in 0..instruments {
            let instrument_header_size = io.rl32()?;
            if !(29..=MAX_VARIABLE_BLOCK).contains(&instrument_header_size) {
                return Err(Error::InvalidData("xm: invalid instrument header size"));
            }
            let instrument_body = read_vec(
                &mut io,
                usize::try_from(instrument_header_size - 4).unwrap_or(usize::MAX),
            )?;
            let sample_count = read_u16(&instrument_body, 23)?;
            if sample_count > MAX_SAMPLES_PER_INSTRUMENT {
                return Err(Error::InvalidData(
                    "xm: samples per instrument exceed limit",
                ));
            }
            if sample_count == 0 {
                continue;
            }
            if instrument_body.len() < 29 {
                return Err(Error::InvalidData("xm: truncated instrument extension"));
            }
            let sample_header_size = read_u32(&instrument_body, 25)?;
            if sample_header_size != SAMPLE_HEADER_BYTES {
                return Err(Error::Unsupported(
                    "xm: non-standard sample header is unsupported",
                ));
            }
            let mut headers = Vec::new();
            for _ in 0..sample_count {
                let mut sample_header = [0; SAMPLE_HEADER_BYTES as usize];
                io.read_exact(&mut sample_header)?;
                let sample_len = u64::from(read_u32(&sample_header, 0)?);
                let sample_type = sample_header[14];
                if sample_type & 0xec != 0 {
                    return Err(Error::Unsupported(
                        "xm: reserved sample flags are unsupported",
                    ));
                }
                let bytes_per_point = if sample_type & 0x10 != 0 { 2 } else { 1 };
                let data_len = sample_len.saturating_mul(bytes_per_point);
                let max_usize = u64::try_from(usize::MAX).unwrap_or(u64::MAX);
                if data_len > MAX_SAMPLE_DATA || data_len > max_usize {
                    return Err(Error::InvalidData("xm: sample payload exceeds limit"));
                }
                total_sample_data = total_sample_data.saturating_add(data_len);
                if total_sample_data > MAX_SAMPLE_DATA {
                    return Err(Error::InvalidData("xm: total sample payload exceeds limit"));
                }
                headers.push((
                    sample_len,
                    usize::try_from(data_len).unwrap_or(usize::MAX),
                    sample_header,
                ));
            }
            for (sample_len, data_len, sample_header) in headers {
                let data_offset = io.pos();
                ensure_end(
                    &io,
                    data_offset.saturating_add(u64::try_from(data_len).unwrap_or(u64::MAX)),
                )?;
                if streams.len() >= MAX_TOTAL_SAMPLES {
                    return Err(Error::InvalidData("xm: total sample count exceeds limit"));
                }
                let stream_index = u32::try_from(streams.len())
                    .map_err(|_| Error::InvalidData("xm: too many streams"))?;
                let mut stream = Stream::new(stream_index, MediaType::Audio, Rational::UNDEFINED);
                stream.params = CodecParameters::audio();
                if let Some(audio) = stream.params.audio.as_mut() {
                    audio.layout = ChannelLayout::default_for(1);
                }
                stream.duration_ts = i64::try_from(sample_len).ok();
                stream.frame_count = Some(sample_len);
                let name = sample_name(&sample_header[18..40]);
                if !name.is_empty() {
                    stream.metadata.push(("title".to_string(), name));
                }
                streams.push(stream);
                if data_len > 0 {
                    samples.push(Sample {
                        stream_index,
                        data_offset,
                        data_len,
                    });
                }
                io.skip(u64::try_from(data_len).unwrap_or(u64::MAX))?;
            }
        }

        Ok(Self {
            io,
            streams,
            samples,
            next_sample: 0,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for XmDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let Some(sample) = self.samples.get(self.next_sample).cloned() else {
            return Err(Error::Eof);
        };
        self.next_sample = self.next_sample.saturating_add(1);
        self.io.seek(sample.data_offset)?;
        let mut packet = Packet::alloc(&mut self.budget, sample.data_len)?;
        self.io.read_exact(packet.payload_mut())?;
        packet.len = sample.data_len;
        packet.stream_index = sample.stream_index;
        packet.pts = Timestamp::new(0);
        packet.dts = packet.pts;
        packet.duration = Duration::ZERO;
        packet.flags = PacketFlags::KEY;
        packet.pos = Some(sample.data_offset);
        Ok(packet)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        Err(Error::Unsupported(
            "xm: indexed sample seek is not implemented",
        ))
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

fn read_vec(io: &mut IoContext, len: usize) -> Result<Vec<u8>> {
    let mut data = vec![0; len];
    io.read_exact(&mut data)?;
    Ok(data)
}

fn ensure_end(io: &IoContext, end: u64) -> Result<()> {
    if io.size().is_some_and(|size| end > size) {
        return Err(Error::InvalidData("xm: declared block extends past file"));
    }
    Ok(())
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset.saturating_add(2))
        .ok_or(Error::InvalidData("xm: truncated header field"))?;
    let bytes = bytes
        .try_into()
        .map_err(|_| Error::InvalidData("xm: truncated header field"))?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset.saturating_add(4))
        .ok_or(Error::InvalidData("xm: truncated header field"))?;
    let bytes = bytes
        .try_into()
        .map_err(|_| Error::InvalidData("xm: truncated header field"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn sample_name(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(bytes.get(..end).unwrap_or_default())
        .trim()
        .to_string()
}
