//! Raw Bluetooth SBC (the A2DP subband codec), self-delimited by a per-frame
//! header.
//!
//! Frame layout: `sync:u8(0x9C)  freq:2b blocks:2b mode:2b alloc:1b sub:1b
//! bitpool:u8  crc:u8  [scale factors...]  [subband samples...]`.
//! `blocks` decodes to `4*(n+1)`; `sub` decodes to `4` or `8` subbands.
//!
//! Frame length in bytes (mono/dual-channel case; joint/plain stereo are
//! implemented per the same published formula but not independently
//! measured):
//!
//! ```text
//! frame_length = 4 + (4*subbands*channels)/8 + ceil(blocks*channels*bitpool/8)          [mono, dual]
//! frame_length = 4 + (4*subbands*channels)/8 + ceil(blocks*bitpool/8)                   [stereo]
//! frame_length = 4 + (4*subbands*channels)/8 + ceil((subbands + blocks*bitpool)/8)      [joint stereo]
//! ```
//!
//! Measured: a 0.3 s mono fixture reports `blocks=16 mode=mono alloc=loudness
//! subbands=8 bitpool=60`, and the mono formula above gives exactly `128`
//! bytes — the actual file is `4736` bytes, `128 * 37` exactly.
//!
//! No file-level magic exists; probing is by extension only, same as the raw
//! ITU-T codecs in [`crate::rawcodec`].

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

const SYNC: u8 = 0x9C;
const EXTENSIONS: &[&str] = &["sbc"];
const FREQ_TABLE: [u32; 4] = [16000, 32000, 44100, 48000];

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.extension_matches(EXTENSIONS) {
        ProbeScore::EXTENSION
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "sbc",
    long_name: "raw SBC (low-complexity subband codec)",
    extensions: EXTENSIONS,
    mime_types: &["audio/sbc"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(SbcDemuxer::open(src)?))
}

#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    sample_rate: u32,
    channels: u16,
    frame_len: usize,
    samples_per_frame: u32,
}

fn parse_frame_header(hdr: [u8; 4]) -> Option<FrameHeader> {
    if hdr[0] != SYNC {
        return None;
    }
    let freq_idx = usize::from((hdr[1] >> 6) & 0x3);
    let blocks = 4 * (u32::from((hdr[1] >> 4) & 0x3) + 1);
    let mode = (hdr[1] >> 2) & 0x3;
    let subbands = if hdr[1] & 0x1 == 1 { 8 } else { 4 };
    let bitpool = u32::from(hdr[2]);
    let channels: u32 = if mode == 0 { 1 } else { 2 };
    let sample_rate = *FREQ_TABLE.get(freq_idx)?;

    let header_and_scale = 4 + (4 * subbands * channels).div_ceil(8);
    let body_bits = match mode {
        0 | 1 => blocks * channels * bitpool,
        2 => blocks * bitpool,
        _ => subbands + blocks * bitpool,
    };
    let frame_len = header_and_scale + body_bits.div_ceil(8);

    Some(FrameHeader {
        sample_rate,
        channels: channels.try_into().ok()?,
        frame_len: frame_len as usize,
        samples_per_frame: blocks * subbands,
    })
}

#[derive(Debug)]
pub struct SbcDemuxer {
    io: IoContext,
    stream: Stream,
    frames_emitted: u64,
    budget: Budget,
    eof: bool,
}

impl SbcDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the first frame's header does not parse.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let raw = io.peek(4)?;
        let hdr4: [u8; 4] = raw
            .get(..4)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::InvalidData("sbc: file shorter than one frame header"))?;
        let first = parse_frame_header(hdr4).ok_or(Error::InvalidData("sbc: invalid frame sync"))?;

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, first.sample_rate.cast_signed()),
        );
        let mut params = CodecParameters::audio();
        params.codec_id = Some(CodecId::Sbc);
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = first.sample_rate;
            audio.layout = ChannelLayout::default_for(u32::from(first.channels));
        }
        stream.params = params;

        Ok(Self {
            io,
            stream,
            frames_emitted: 0,
            budget: Budget::new(vaco_limits::Limits::permissive()),
            eof: false,
        })
    }
}

impl Demuxer for SbcDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let Ok(raw) = self.io.peek(4) else {
            self.eof = true;
            return Err(Error::Eof);
        };
        let Some(hdr4) = raw.get(..4).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            self.eof = true;
            return Err(Error::Eof);
        };
        let Some(hdr) = parse_frame_header(hdr4) else {
            self.eof = true;
            return Err(Error::InvalidData("sbc: invalid frame sync mid-stream"));
        };
        let mut pkt = Packet::alloc(&mut self.budget, hdr.frame_len)?;
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(i64::try_from(self.frames_emitted).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        let rate = self.stream.params.audio.as_ref().map_or(1, |a| a.sample_rate.max(1));
        let micros = u64::from(hdr.samples_per_frame)
            .saturating_mul(1_000_000)
            .checked_div(u64::from(rate))
            .unwrap_or(0);
        pkt.duration = vaco_core::Duration::from_micros(i64::try_from(micros).unwrap_or(i64::MAX));
        self.frames_emitted = self
            .frames_emitted
            .saturating_add(u64::from(hdr.samples_per_frame));
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    /// The exact configuration measured in the module docs: mono, 16 kHz,
    /// 16-block, 8-subband, bitpool 60 -> 128-byte frames.
    fn frame(payload_len: usize) -> Vec<u8> {
        let mut v = vec![SYNC, 0b0011_0001, 60, 0];
        v.resize(4 + payload_len, 0xAA);
        v
    }

    #[test]
    fn frame_length_matches_the_measured_formula() {
        let hdr: [u8; 4] = [SYNC, 0b0011_0001, 60, 0];
        let parsed = parse_frame_header(hdr).unwrap();
        assert_eq!(parsed.sample_rate, 16_000);
        assert_eq!(parsed.channels, 1);
        assert_eq!(parsed.frame_len, 128);
    }

    #[test]
    fn reads_consecutive_self_delimited_frames() {
        let mut data = frame(124);
        data.extend(frame(124));
        let mut d = SbcDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.read_packet().unwrap().len, 128);
        assert_eq!(d.read_packet().unwrap().len, 128);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn probe_is_extension_only() {
        assert_eq!(probe(&ProbeData::new(b"\x9c\x31\x3c\x71")), ProbeScore::NONE);
        assert_eq!(
            probe(&ProbeData::new(b"whatever").with_filename("x.sbc")),
            ProbeScore::EXTENSION
        );
    }
}
