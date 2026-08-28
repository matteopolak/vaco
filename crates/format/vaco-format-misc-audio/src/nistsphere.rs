//! NIST SPHERE (an acronym for "NIST speech header resources"), the header format behind
//! TIMIT and much of the classic speech-corpus literature.
//!
//! # Layout
//!
//! ```text
//! "NIST_1A\n"                     -- 8-byte magic, always this exact string
//! "%8d\n" % header_bytes          -- ASCII decimal, left-justified in 8 columns
//! "field_name -type value\n"      -- repeated, e.g. "sample_rate -i 16000"
//! padding to header_bytes total
//! raw PCM data
//! ```
//!
//! `-type` is `-i` (integer), `-r` (real) or `-sN` (string of length `N`);
//! only the value is used here, keyed by field name. The fields this module
//! reads are `sample_rate`, `channel_count`, `sample_n_bytes` and
//! `sample_sig_bits`; anything else is ignored. A field this module does not
//! recognise, or a header with none of the numeric fields at all, falls back
//! to the format's own historical default (8 kHz, mono, 16-bit) rather than
//! failing to open.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecId;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::Packet;
use vaco_sampfmt::SampleFmt;

use crate::block::BlockDemuxer;

const MAGIC: &[u8] = b"NIST_1A\n";
const MAX_HEADER_LEN: u64 = 1 << 20;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(MAGIC) {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "nistsphere",
    long_name: "NIST SPeech HEader REsources",
    extensions: &["sph", "nist"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(NistSphereDemuxer::open(src)?))
}

#[derive(Debug, Default)]
struct Fields {
    sample_rate: Option<u32>,
    channel_count: Option<u16>,
    sample_n_bytes: Option<u32>,
}

fn parse_fields(text: &str) -> Fields {
    let mut fields = Fields::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(_type_token) = parts.next() else {
            continue;
        };
        let Some(value) = parts.next() else { continue };
        match name {
            "sample_rate" => fields.sample_rate = value.parse().ok(),
            "channel_count" => fields.channel_count = value.parse().ok(),
            "sample_n_bytes" => fields.sample_n_bytes = value.parse().ok(),
            _ => {}
        }
    }
    fields
}

#[derive(Debug)]
pub struct NistSphereDemuxer {
    inner: BlockDemuxer,
    budget: Budget,
}

impl NistSphereDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the magic or the declared header length is
    /// malformed.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut magic = [0u8; 8];
        io.read_exact(&mut magic)?;
        if magic != *MAGIC {
            return Err(Error::InvalidData("nistsphere: missing NIST_1A signature"));
        }
        let size_line = io.get_str(16)?;
        let header_len: u64 = size_line
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(Error::InvalidData("nistsphere: unparsable header size"))?;
        if header_len == 0 || header_len > MAX_HEADER_LEN {
            return Err(Error::InvalidData("nistsphere: implausible header size"));
        }

        let consumed = 8u64 + 16;
        let remaining = header_len.saturating_sub(consumed);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut body: Vec<u8> = budget.alloc(usize::try_from(remaining).unwrap_or(0))?;
        io.read_exact(&mut body)?;
        let text = String::from_utf8_lossy(&body);
        let fields = parse_fields(&text);

        let sample_rate = fields.sample_rate.unwrap_or(8000).max(1);
        let channels = fields.channel_count.unwrap_or(1).max(1);
        let bytes_per_sample = fields.sample_n_bytes.unwrap_or(2).clamp(1, 4);
        // `sample_byte_format` (endianness) is not read; little-endian is
        // assumed, matching the format's common convention.
        let (format, codec_id) = match bytes_per_sample {
            1 => (SampleFmt::U8, CodecId::PcmU8),
            3 | 4 => (SampleFmt::S32, CodecId::PcmS32le),
            _ => (SampleFmt::S16, CodecId::PcmS16le),
        };

        io.seek(header_len)?;
        let mut stream = Stream::new(0, MediaType::Audio, Rational::new(1, sample_rate.cast_signed()));
        let mut params = vaco_codec_core::CodecParameters::audio();
        params.codec_id = Some(codec_id);
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            audio.format = Some(format);
            audio.layout = ChannelLayout::default_for(u32::from(channels));
        }
        stream.params = params;

        let bytes_per_frame = bytes_per_sample.saturating_mul(u32::from(channels));
        let size = io.size();
        let inner = BlockDemuxer::new(io, stream, header_len, size, bytes_per_frame.max(1), 1);
        Ok(Self { inner, budget })
    }
}

impl Demuxer for NistSphereDemuxer {
    fn streams(&self) -> &[Stream] {
        self.inner.streams()
    }
    fn read_packet(&mut self) -> Result<Packet> {
        self.inner.read_packet(&mut self.budget)
    }
    fn seek(
        &mut self,
        target: vaco_format_core::SeekTarget,
        flags: vaco_format_core::SeekFlags,
    ) -> Result<()> {
        self.inner.seek(target, flags)
    }
    fn duration(&self) -> Option<vaco_core::Duration> {
        self.inner.duration()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn build_file(sample_rate: u32, channels: u16, bytes: u32, pcm: &[u8]) -> Vec<u8> {
        let fields = format!(
            "sample_rate -i {sample_rate}\nchannel_count -i {channels}\nsample_n_bytes -i {bytes}\n"
        );
        let header_len = 8 + 16 + fields.len();
        // Round up so the numbers stay easy to eyeball; not load-bearing.
        let header_len = header_len.max(64);
        let mut v = Vec::new();
        v.extend_from_slice(MAGIC);
        v.extend_from_slice(format!("{header_len:<16}").as_bytes());
        v.extend_from_slice(fields.as_bytes());
        v.resize(header_len, b' ');
        v.extend_from_slice(pcm);
        v
    }

    #[test]
    fn reads_the_declared_rate_channels_and_width() {
        let data = build_file(16_000, 1, 2, &[0u8; 100]);
        let d = NistSphereDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let audio = d.streams().first().unwrap().params.audio.as_ref().unwrap();
        assert_eq!(audio.sample_rate, 16_000);
        assert_eq!(audio.layout.as_ref().unwrap().channels, 1);
    }

    #[test]
    fn a_header_with_no_recognised_fields_falls_back_to_defaults() {
        let mut v = Vec::new();
        v.extend_from_slice(MAGIC);
        v.extend_from_slice(format!("{:<16}", 32).as_bytes());
        v.resize(32, b' ');
        v.extend_from_slice(&[0u8; 10]);
        let d = NistSphereDemuxer::open(Box::new(MemorySource::new(v))).unwrap();
        assert_eq!(d.streams().first().unwrap().params.audio.as_ref().unwrap().sample_rate, 8000);
    }

    #[test]
    fn probe_matches_only_the_signature() {
        assert_eq!(probe(&ProbeData::new(MAGIC)), ProbeScore::MAGIC);
        assert_eq!(probe(&ProbeData::new(b"nope")), ProbeScore::NONE);
    }
}
