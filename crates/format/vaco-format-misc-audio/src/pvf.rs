//! Portable Voice Format (`.pvf`): a tiny text header in front of raw,
//! big-endian PCM.
//!
//! # Layout
//!
//! ```text
//! "PVF1\n"                          -- 5-byte magic
//! "<channels> <sample_rate> <bits>\n"  -- ASCII decimal, space-separated
//! raw PCM, big-endian, immediately following
//! ```

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

const MAGIC: &[u8] = b"PVF1\n";
const MAX_LINE: usize = 256;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(MAGIC) {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "pvf",
    long_name: "PVF (Portable Voice Format)",
    extensions: &["pvf"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

/// Reads bytes up to and including a `\n`, or `max` bytes, whichever comes
/// first, and returns everything before the newline. Unlike
/// [`IoContext::get_str`], this stops exactly where the text header ends, so
/// the cursor lands precisely at the start of the binary data that follows —
/// `get_str` only stops early on a NUL byte, which a PCM tail will not
/// reliably contain.
fn read_line(io: &mut IoContext, max: usize) -> Result<String> {
    let mut out: Vec<u8> = Vec::new();
    for _ in 0..max {
        match io.r8() {
            Ok(b'\n') | Err(Error::UnexpectedEof | Error::Eof) => break,
            Ok(b) => out.push(b),
            Err(e) => return Err(e),
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(PvfDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct PvfDemuxer {
    inner: BlockDemuxer,
    budget: Budget,
}

impl PvfDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the magic or the `channels rate bits` line
    /// does not parse.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut magic = [0u8; 5];
        io.read_exact(&mut magic)?;
        if magic != *MAGIC {
            return Err(Error::InvalidData("pvf: missing PVF1 signature"));
        }
        let line = read_line(&mut io, MAX_LINE)?;
        let mut parts = line.split_whitespace();
        let channels: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(Error::InvalidData("pvf: unparsable channel count"))?;
        let sample_rate: u32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(Error::InvalidData("pvf: unparsable sample rate"))?;
        let bits: u32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(Error::InvalidData("pvf: unparsable bit depth"))?;
        if channels == 0 || sample_rate == 0 || (bits != 8 && bits != 16 && bits != 32) {
            return Err(Error::InvalidData("pvf: implausible header values"));
        }

        let (format, codec_id) = match bits {
            8 => (SampleFmt::U8, CodecId::PcmU8),
            32 => (SampleFmt::S32, CodecId::PcmS32be),
            _ => (SampleFmt::S16, CodecId::PcmS16be),
        };
        #[allow(clippy::integer_division, reason = "bits is checked above to be 8/16/32; exact")]
        let bytes_per_sample = bits / 8;
        let data_start = io.pos();

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
        let inner = BlockDemuxer::new(io, stream, data_start, size, bytes_per_frame.max(1), 1);
        Ok(Self {
            inner,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for PvfDemuxer {
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

    fn build_file(channels: u16, rate: u32, bits: u32, pcm: &[u8]) -> Vec<u8> {
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(format!("{channels} {rate} {bits}\n").as_bytes());
        v.extend_from_slice(pcm);
        v
    }

    #[test]
    fn reads_the_header_line_and_frames_the_pcm_tail() {
        let data = build_file(2, 22_050, 16, &[0u8; 40]);
        let mut d = PvfDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let audio = d.streams().first().unwrap().params.audio.as_ref().unwrap();
        assert_eq!(audio.sample_rate, 22_050);
        assert_eq!(audio.layout.as_ref().unwrap().channels, 2);
        let pkt = d.read_packet().unwrap();
        assert_eq!(pkt.len, 40);
    }

    #[test]
    fn an_unsupported_bit_depth_is_rejected() {
        let data = build_file(1, 8000, 12, &[0u8; 8]);
        assert!(PvfDemuxer::open(Box::new(MemorySource::new(data))).is_err());
    }

    #[test]
    fn probe_matches_only_the_signature() {
        assert_eq!(probe(&ProbeData::new(MAGIC)), ProbeScore::MAGIC);
        assert_eq!(probe(&ProbeData::new(b"nope")), ProbeScore::NONE);
    }
}
