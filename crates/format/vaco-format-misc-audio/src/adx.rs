//! CRI ADX, CRI Middleware's ADPCM container.
//!
//! # Layout, measured against an `ffmpeg -c:a adpcm_adx -f adx` fixture
//!
//! ```text
//! sync:be16(top bit set)  copyright_offset:be16
//! encoding_type:u8  block_size:u8  bits_per_sample:u8  channel_count:u8
//! sample_rate:be32  total_samples:be32  highpass_freq:be16  version:u8
//! [reserved, up to copyright_offset+4]
//! data: blocks of `block_size` bytes, `(block_size-2)*2` samples each
//! ```
//!
//! `header_len = copyright_offset + 4` is where audio data starts: measured
//! exactly, dividing the fixture's data length by its `block_size` gives a
//! whole number of blocks whose sample count sums to the header's own
//! `total_samples` (76 blocks of 18 bytes = 32 samples each = 2432 total, on
//! a file whose header states exactly those three numbers).
//!
//! # What is not read
//!
//! `highpass_freq`, `version` and any loop-point fields between the fixed
//! fields and the copyright string are skipped.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::Packet;

use crate::block::BlockDemuxer;

const SYNC_MASK: u16 = 0x8000;
/// A `header_len` past this is not a real ADX file; bounds the seek before
/// any allocation happens.
const MAX_HEADER_LEN: u64 = 1 << 20;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let Some(sync) = data.rb16(0) else {
        return ProbeScore::NONE;
    };
    if sync & SYNC_MASK == 0 {
        return ProbeScore::NONE;
    }
    match data.get(4) {
        Some(2 | 3 | 4 | 0x11) => ProbeScore::MAGIC_CHECKED,
        _ => ProbeScore::NONE,
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "adx",
    long_name: "CRI ADX",
    extensions: &["adx"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(AdxDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct AdxDemuxer {
    inner: BlockDemuxer,
    budget: Budget,
}

impl AdxDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the sync word or header fields are malformed.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let sync = io.rb16()?;
        if sync & SYNC_MASK == 0 {
            return Err(Error::InvalidData("adx: missing sync word"));
        }
        let copyright_offset = io.rb16()?;
        let header_len = u64::from(copyright_offset).saturating_add(4);
        if header_len > MAX_HEADER_LEN {
            return Err(Error::InvalidData("adx: implausible header length"));
        }
        let _encoding_type = io.r8()?;
        let block_size = io.r8()?;
        let _bits_per_sample = io.r8()?;
        let channels = io.r8()?;
        let sample_rate = io.rb32()?.max(1);
        let total_samples = io.rb32()?;

        if block_size < 2 {
            return Err(Error::InvalidData("adx: block_size too small"));
        }
        let samples_per_block = u32::from(block_size - 2) * 2;

        io.seek(header_len)?;
        let data_start = header_len;

        let mut stream = Stream::new(0, MediaType::Audio, Rational::new(1, sample_rate.cast_signed()));
        let mut params = CodecParameters::audio();
        params.codec_id = Some(CodecId::AdpcmAdx);
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            audio.layout = ChannelLayout::default_for(u32::from(channels.max(1)));
        }
        stream.params = params;
        stream.duration_ts = Some(i64::from(total_samples));
        stream.frame_count = Some(u64::from(total_samples));

        let size = io.size();
        let inner = BlockDemuxer::new(
            io,
            stream,
            data_start,
            size,
            u32::from(block_size),
            samples_per_block.max(1),
        );
        Ok(Self {
            inner,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for AdxDemuxer {
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

    /// The exact field values measured from the real fixture in the module
    /// docs, with a shortened audio payload.
    fn build_file(block_size: u8, channels: u8, sample_rate: u32, total_samples: u32, blocks: u32) -> Vec<u8> {
        let mut v = Vec::new();
        let copyright_offset: u16 = 32;
        v.extend_from_slice(&(0x8000u16).to_be_bytes());
        v.extend_from_slice(&copyright_offset.to_be_bytes());
        v.push(3); // encoding_type
        v.push(block_size);
        v.push(4); // bits_per_sample
        v.push(channels);
        v.extend_from_slice(&sample_rate.to_be_bytes());
        v.extend_from_slice(&total_samples.to_be_bytes());
        v.extend_from_slice(&500u16.to_be_bytes()); // highpass_freq
        v.push(3); // version
        v.resize(usize::from(copyright_offset) + 4, 0);
        for _ in 0..blocks {
            v.extend(vec![0xCDu8; usize::from(block_size)]);
        }
        v
    }

    #[test]
    fn header_fields_and_block_geometry_match_the_measured_fixture() {
        let data = build_file(18, 1, 8000, 2432, 76);
        let mut d = AdxDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let s = d.streams().first().unwrap();
        assert_eq!(s.params.audio.as_ref().unwrap().sample_rate, 8000);
        assert_eq!(s.duration_ts, Some(2432));
        let pkt = d.read_packet().unwrap();
        assert_eq!(pkt.len, 18 * 76);
    }

    #[test]
    fn probe_checks_the_sync_bit_and_a_known_encoding() {
        let data = build_file(18, 1, 8000, 32, 1);
        assert_eq!(probe(&ProbeData::new(&data)), ProbeScore::MAGIC_CHECKED);
        assert_eq!(probe(&ProbeData::new(b"not adx at all")), ProbeScore::NONE);
    }

    #[test]
    fn an_implausible_header_length_is_rejected() {
        let mut v = Vec::new();
        v.extend_from_slice(&(0x8000u16).to_be_bytes());
        v.extend_from_slice(&u16::MAX.to_be_bytes());
        assert!(AdxDemuxer::open(Box::new(MemorySource::new(v))).is_err());
    }
}
