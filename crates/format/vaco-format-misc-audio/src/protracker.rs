//! ProTracker (`.mod`) structural demuxing.
//!
//! The reader accepts the four-channel `M.K.` revision of the published
//! ProTracker layout. It walks the fixed header, pattern table, and pattern
//! bytes, then exposes each sample's signed 8-bit payload as one packet on a
//! mono structural stream. It does not interpret tracker events, periods,
//! effects, or sample playback rates; this is container framing only.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecParameters;
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

const MAGIC: &[u8; 4] = b"M.K.";
const TITLE_BYTES: usize = 20;
const SAMPLE_COUNT: usize = 31;
const SAMPLE_HEADER_BYTES: usize = 30;
const ORDER_BYTES: usize = 128;
const PATTERN_BYTES: u64 = 64 * 4 * 4;
const HEADER_BEFORE_PATTERNS: u64 = 1084;
const MAX_PATTERN_COUNT: u8 = 64;
const MAX_SAMPLE_DATA: u64 = 1 << 30;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.tag(1080) == Some(*MAGIC) {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

/// Descriptor for the bounded four-channel ProTracker structural demuxer.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mod",
    long_name: "ProTracker Module",
    extensions: &["mod"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(ProtrackerDemuxer::open(src)?))
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    stream_index: u32,
    data_offset: u64,
    data_len: usize,
}

#[derive(Debug)]
pub struct ProtrackerDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    samples: Vec<Sample>,
    next_sample: usize,
    budget: Budget,
}

impl ProtrackerDemuxer {
    /// Opens a bounded `M.K.` ProTracker module.
    ///
    /// # Errors
    /// Returns [`Error::InvalidData`] for malformed or truncated fixed
    /// structures and [`Error::NotSeekable`] because sample packets are read
    /// after the complete module layout has been scanned.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }

        let mut title = [0; TITLE_BYTES];
        io.read_exact(&mut title)?;
        let mut headers = [[0u8; SAMPLE_HEADER_BYTES]; SAMPLE_COUNT];
        for header in &mut headers {
            io.read_exact(header)?;
        }

        let song_length = usize::from(io.r8()?);
        let _restart_position = io.r8()?;
        if song_length == 0 || song_length > ORDER_BYTES {
            return Err(Error::InvalidData("mod: song length is outside 1..128"));
        }
        let mut orders = [0u8; ORDER_BYTES];
        io.read_exact(&mut orders)?;
        let magic = io.tag()?;
        if magic != *MAGIC {
            return Err(Error::InvalidData(
                "mod: only the M.K. four-channel tag is supported",
            ));
        }
        let max_pattern = orders
            .get(..song_length)
            .and_then(|orders| orders.iter().copied().max())
            .unwrap_or(0);
        if max_pattern >= MAX_PATTERN_COUNT {
            return Err(Error::InvalidData(
                "mod: pattern order exceeds 64-pattern scope",
            ));
        }
        let pattern_count = u64::from(max_pattern) + 1;
        let pattern_data = pattern_count.saturating_mul(PATTERN_BYTES);
        if pattern_data > MAX_SAMPLE_DATA {
            return Err(Error::InvalidData("mod: pattern data exceeds limit"));
        }
        ensure_end(&io, HEADER_BEFORE_PATTERNS.saturating_add(pattern_data))?;
        io.skip(pattern_data)?;

        let mut streams = Vec::new();
        let mut samples = Vec::new();
        let mut total_sample_data = 0u64;
        for header in &headers {
            let sample_words = u64::from(u16::from_be_bytes([header[22], header[23]]));
            let data_len_u64 = sample_words.saturating_mul(2);
            total_sample_data = total_sample_data.saturating_add(data_len_u64);
            if data_len_u64 > MAX_SAMPLE_DATA || total_sample_data > MAX_SAMPLE_DATA {
                return Err(Error::InvalidData("mod: sample data exceeds limit"));
            }
            let repeat_offset = u64::from(u16::from_be_bytes([header[26], header[27]]));
            let repeat_length = u64::from(u16::from_be_bytes([header[28], header[29]]));
            if repeat_offset.saturating_add(repeat_length) > sample_words {
                return Err(Error::InvalidData(
                    "mod: sample loop exceeds sample payload",
                ));
            }
            if header[25] > 64 {
                return Err(Error::InvalidData("mod: sample volume exceeds 64"));
            }
            let data_offset = io.pos();
            ensure_end(&io, data_offset.saturating_add(data_len_u64))?;
            let stream_index = u32::try_from(streams.len())
                .map_err(|_| Error::InvalidData("mod: too many streams"))?;
            let mut stream = Stream::new(stream_index, MediaType::Audio, Rational::UNDEFINED);
            stream.params = CodecParameters::audio();
            if let Some(audio) = stream.params.audio.as_mut() {
                audio.layout = ChannelLayout::default_for(1);
            }
            stream.duration_ts = i64::try_from(data_len_u64).ok();
            stream.frame_count = Some(data_len_u64);
            let name = sample_name(&header[..22]);
            if !name.is_empty() {
                stream.metadata.push(("title".to_string(), name));
            }
            streams.push(stream);
            if data_len_u64 > 0 {
                samples.push(Sample {
                    stream_index,
                    data_offset,
                    data_len: usize::try_from(data_len_u64)
                        .map_err(|_| Error::InvalidData("mod: sample payload is too large"))?,
                });
            }
            io.skip(data_len_u64)?;
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

impl Demuxer for ProtrackerDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let Some(sample) = self.samples.get(self.next_sample).copied() else {
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
        Err(Error::Unsupported(
            "mod: indexed sample seek is not implemented",
        ))
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

fn ensure_end(io: &IoContext, end: u64) -> Result<()> {
    if io.size().is_some_and(|size| end > size) {
        return Err(Error::InvalidData("mod: declared block extends past file"));
    }
    Ok(())
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
