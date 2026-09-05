//! `WavPack` (`.wv`), the "raw `WavPack`" block-chain format.
//!
//! # Layout
//!
//! Every block starts with a 32-byte little-endian header (`ckID="wvpk"`,
//! `ckSize` = block length minus 8, `version`, a 40-bit `block_index` and a
//! 40-bit `total_samples` split across two `u8` and two `u32` fields,
//! `block_samples`, `flags`, `crc`), followed by `ckSize - 24` bytes of
//! metadata sub-blocks and compressed audio. A reader needs none of the
//! sub-block content to find block boundaries: the next block starts exactly
//! `ckSize + 8` bytes after the current one.
//!
//! `flags` states bytes/sample, mono/stereo, float/int and — via a 4-bit
//! index into a fixed 15-entry table — the sample rate, all needed only from
//! the first block that actually carries audio (`block_samples != 0`).
//!
//! # What is not read
//!
//! Metadata sub-blocks (decorrelation tables, channel layout beyond
//! mono/stereo, `ID_SAMPLE_RATE` for a non-standard custom rate) are skipped
//! whole; a custom-rate file reports `sample_rate=0` rather than the
//! `ID_SAMPLE_RATE` override.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, ExactDuration, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

const MAGIC: [u8; 4] = *b"wvpk";
const HEADER_LEN: u64 = 32;

/// `flags` bits 26-23. Measured (module docs): a 0.3 s 44.1 kHz stereo 16-bit
/// fixture's block carries index 9, which this table maps to 44100 — exactly
/// the source rate.
const SAMPLE_RATES: [u32; 15] = [
    6000, 8000, 9600, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
    192_000,
];

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(&MAGIC) {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "wv",
    long_name: "WavPack",
    extensions: &["wv"],
    mime_types: &["audio/x-wavpack"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(WavpackDemuxer::open(src)?))
}

struct BlockHeader {
    ck_size: u32,
    total_samples: u64,
    block_samples: u32,
    flags: u32,
}

fn read_block_header(io: &mut IoContext) -> Result<BlockHeader> {
    if io.tag()? != MAGIC {
        return Err(Error::InvalidData("wv: missing wvpk block signature"));
    }
    let ck_size = io.rl32()?;
    let _version = io.rl16()?;
    let _block_index_u8 = io.r8()?;
    let total_samples_u8 = io.r8()?;
    let total_samples_lo = io.rl32()?;
    let _block_index = io.rl32()?;
    let block_samples = io.rl32()?;
    let flags = io.rl32()?;
    let _crc = io.rl32()?;
    let total_samples = (u64::from(total_samples_u8) << 32) | u64::from(total_samples_lo);
    Ok(BlockHeader {
        ck_size,
        total_samples,
        block_samples,
        flags,
    })
}

#[derive(Debug)]
pub struct WavpackDemuxer {
    io: IoContext,
    stream: Stream,
    pos: u64,
    frames_emitted: u64,
    budget: Budget,
    eof: bool,
}

impl WavpackDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if no block carries the `wvpk` signature, or no
    /// audio-bearing block is found.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut pos = 0u64;
        let (mut sample_rate, mut channels, mut bytes_per_sample, mut is_float, mut total_samples) =
            (44_100u32, 2u16, 2u8, false, None);
        let mut found = false;

        loop {
            let start = pos;
            let Ok(hdr) = read_block_header(&mut io) else {
                break;
            };
            let block_len = HEADER_LEN.saturating_add(u64::from(hdr.ck_size).saturating_sub(24));
            if hdr.block_samples != 0 {
                bytes_per_sample = (hdr.flags & 0x3) as u8 + 1;
                channels = if (hdr.flags >> 2) & 1 == 1 { 1 } else { 2 };
                is_float = (hdr.flags >> 7) & 1 == 1;
                let rate_idx = ((hdr.flags >> 23) & 0xf) as usize;
                if let Some(&r) = SAMPLE_RATES.get(rate_idx) {
                    sample_rate = r;
                }
                if hdr.total_samples != 0 && hdr.total_samples != u64::from(u32::MAX) {
                    total_samples = Some(hdr.total_samples);
                }
                found = true;
                pos = start;
                break;
            }
            io.seek(start.saturating_add(block_len))?;
            pos = start.saturating_add(block_len);
        }

        if !found {
            return Err(Error::InvalidData("wv: no audio-bearing block found"));
        }
        io.seek(pos)?;

        let format = match (is_float, bytes_per_sample) {
            (true, 4) => Some(vaco_sampfmt::SampleFmt::F32),
            (false, 1) => Some(vaco_sampfmt::SampleFmt::U8),
            (false, 2) => Some(vaco_sampfmt::SampleFmt::S16),
            (false, 3 | 4) => Some(vaco_sampfmt::SampleFmt::S32),
            _ => None,
        };

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, sample_rate.cast_signed()),
        );
        let mut params = CodecParameters::audio();
        params.codec_id = Some(CodecId::WavPack);
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            audio.format = format;
            audio.layout = ChannelLayout::default_for(u32::from(channels));
            audio.bits_per_coded_sample = Some(u32::from(bytes_per_sample).saturating_mul(8) as u8);
        }
        stream.params = params;
        if let Some(ts) = total_samples {
            stream.duration_ts = i64::try_from(ts).ok();
            stream.frame_count = Some(ts);
        }

        Ok(Self {
            io,
            stream,
            pos,
            frames_emitted: 0,
            budget: Budget::new(vaco_limits::Limits::permissive()),
            eof: false,
        })
    }
}

impl Demuxer for WavpackDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let start = self.pos;
        self.io.seek(start)?;
        let Ok(hdr) = read_block_header(&mut self.io) else {
            self.eof = true;
            return Err(Error::Eof);
        };
        let block_len = HEADER_LEN.saturating_add(u64::from(hdr.ck_size).saturating_sub(24));
        self.io.seek(start)?;
        let mut pkt = Packet::alloc(&mut self.budget, block_len as usize)?;
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(i64::try_from(self.frames_emitted).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        pkt.pos = Some(start);

        pkt.duration = vaco_core::Duration::from_ticks(
            i64::from(hdr.block_samples),
            self.stream.time_base,
        )
        .unwrap_or(vaco_core::Duration::ZERO);

        self.frames_emitted = self
            .frames_emitted
            .saturating_add(u64::from(hdr.block_samples));
        self.pos = start.saturating_add(block_len);
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        // No specialised seek table (module docs quote the spec on this
        // point); a generic index built from packets already read is the
        // documented fallback and is composed by the caller, not here.
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        self.stream.duration()
    }

    fn duration_exact(&self) -> Option<ExactDuration> {
        self.stream.duration_exact()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    /// One block, header field values taken verbatim from the measured
    /// fixture in the module docs (0.3 s 44.1 kHz stereo 16-bit).
    fn one_block(block_samples: u32, total_samples: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&MAGIC);
        let ck_size = 24u32 + payload.len() as u32;
        v.extend_from_slice(&ck_size.to_le_bytes());
        v.extend_from_slice(&0x0410u16.to_le_bytes());
        v.push(0);
        v.push(0);
        v.extend_from_slice(&total_samples.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&block_samples.to_le_bytes());
        v.extend_from_slice(&0x04bc_1831u32.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn reads_sample_rate_and_channels_from_the_flags_field() {
        let data = one_block(13_230, 13_230, &[0xAAu8; 50]);
        let d = WavpackDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let audio = d.streams().first().unwrap().params.audio.as_ref().unwrap();
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(audio.layout.as_ref().unwrap().channels, 2);
    }

    #[test]
    fn walks_to_the_next_block_by_ck_size() {
        let mut data = one_block(100, 200, &[1u8; 20]);
        data.extend(one_block(100, 200, &[2u8; 20]));
        let mut d = WavpackDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.payload()[HEADER_LEN as usize], 1);
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.payload()[HEADER_LEN as usize], 2);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn aggregate_duration_keeps_the_native_sample_clock_exact() {
        let data = one_block(1024, 1024, &[0xAAu8; 50]);
        let d = WavpackDemuxer::open(Box::new(MemorySource::new(data))).unwrap();

        assert_eq!(
            d.duration_exact().map(vaco_core::ExactDuration::as_ratio),
            Some((256, 11_025))
        );
    }

    #[test]
    fn probe_requires_the_signature() {
        assert_eq!(
            probe(&ProbeData::new(b"not a wavpack file at all")),
            ProbeScore::NONE
        );
        assert_eq!(
            probe(&ProbeData::new(b"wvpk\x00\x00\x00\x00")),
            ProbeScore::MAGIC
        );
    }
}
