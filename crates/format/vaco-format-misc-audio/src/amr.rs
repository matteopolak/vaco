//! 3GPP AMR (`amr`, plus the header-less `amrnb`/`amrwb` elementary streams).
//!
//! # Layout
//!
//! ```text
//! amr:    magic ("#!AMR\n" narrowband, "#!AMR-WB\n" wideband)
//! amrnb, amrwb: no magic; frames start immediately
//!
//! frame:  toc:u8  payload[SIZE[toc.mode]]
//!   toc bit7    = F (more frames follow; multi-frame-per-RTP-payload only)
//!   toc bits6-3 = mode (frame type index)
//!   toc bits2-0 = quality bit + 2 padding bits
//! ```
//!
//! `SIZE` (payload bytes, excluding the TOC byte) for narrowband is the
//! 3GPP TS 26.101 / IETF RFC 4867 §"Table 1" total-speech-bit-count for each
//! mode, rounded up to whole bytes: `{95,103,118,134,148,159,204,244,39} / 8`.
//! The wideband table is the standard 3GPP TS 26.201 byte sizes,
//! reproduced from the widely-published table rather than a freshly fetched
//! copy of that document.

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

const MAGIC_NB: &[u8] = b"#!AMR\n";
const MAGIC_WB: &[u8] = b"#!AMR-WB\n";

/// Payload bytes per mode index 0-8 (8 speech modes plus SID); 9-15 carry no
/// payload (future-use, speech-lost, no-data).
const SIZE_NB: [usize; 16] = [12, 13, 15, 17, 19, 20, 26, 31, 5, 0, 0, 0, 0, 0, 0, 0];
const SIZE_WB: [usize; 16] = [17, 23, 32, 36, 40, 46, 50, 58, 60, 5, 0, 0, 0, 0, 0, 0];

#[derive(Debug, Clone, Copy)]
pub enum Band {
    Narrow,
    Wide,
}

impl Band {
    const fn sample_rate(self) -> u32 {
        match self {
            Self::Narrow => 8000,
            Self::Wide => 16000,
        }
    }

    fn frame_size(self, mode: u8) -> usize {
        let table: &[usize; 16] = match self {
            Self::Narrow => &SIZE_NB,
            Self::Wide => &SIZE_WB,
        };
        table.get(usize::from(mode)).copied().unwrap_or(0)
    }
}

#[must_use]
pub fn probe_amr(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(MAGIC_NB) || data.starts_with(MAGIC_WB) {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

#[must_use]
pub fn probe_amrnb(data: &ProbeData<'_>) -> ProbeScore {
    if data.extension_matches(&["amrnb"]) {
        ProbeScore::EXTENSION
    } else {
        ProbeScore::NONE
    }
}

#[must_use]
pub fn probe_amrwb(data: &ProbeData<'_>) -> ProbeScore {
    if data.extension_matches(&["amrwb", "awb"]) {
        ProbeScore::EXTENSION
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER_AMR: DemuxerDesc = DemuxerDesc {
    name: "amr",
    long_name: "3GPP AMR",
    extensions: &["amr"],
    mime_types: &["audio/amr", "audio/amr-wb"],
    flags: FormatFlags::GENERIC_INDEX,
    probe: probe_amr,
    open: open_amr,
};

pub const DEMUXER_AMRNB: DemuxerDesc = DemuxerDesc {
    name: "amrnb",
    long_name: "raw AMR-NB",
    extensions: &["amrnb"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe: probe_amrnb,
    open: open_amrnb,
};

pub const DEMUXER_AMRWB: DemuxerDesc = DemuxerDesc {
    name: "amrwb",
    long_name: "raw AMR-WB",
    extensions: &["amrwb", "awb"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe: probe_amrwb,
    open: open_amrwb,
};

fn open_amr(src: Box<dyn MediaSource>, _parsers: &dyn ParserProvider) -> Result<Box<dyn Demuxer>> {
    let mut io = IoContext::new(src, &IoOptions::default())?;
    let peeked = io.peek(MAGIC_WB.len())?;
    let (band, magic_len) = if peeked.get(..MAGIC_WB.len()) == Some(MAGIC_WB) {
        (Band::Wide, MAGIC_WB.len())
    } else if peeked.get(..MAGIC_NB.len()) == Some(MAGIC_NB) {
        (Band::Narrow, MAGIC_NB.len())
    } else {
        return Err(Error::InvalidData("amr: missing #!AMR signature"));
    };
    io.skip(u64::try_from(magic_len).unwrap_or(0))?;
    Ok(Box::new(AmrDemuxer::new(io, band)))
}

fn open_amrnb(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    let io = IoContext::new(src, &IoOptions::default())?;
    Ok(Box::new(AmrDemuxer::new(io, Band::Narrow)))
}

fn open_amrwb(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    let io = IoContext::new(src, &IoOptions::default())?;
    Ok(Box::new(AmrDemuxer::new(io, Band::Wide)))
}

#[derive(Debug)]
pub struct AmrDemuxer {
    io: IoContext,
    stream: Stream,
    band: Band,
    frames_emitted: u64,
    budget: Budget,
    eof: bool,
}

impl AmrDemuxer {
    #[must_use]
    pub fn new(io: IoContext, band: Band) -> Self {
        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, band.sample_rate().cast_signed()),
        );
        let mut params = CodecParameters::audio();
        params.codec_id = Some(match band {
            Band::Narrow => CodecId::AmrNb,
            Band::Wide => CodecId::AmrWb,
        });
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = band.sample_rate();
            audio.layout = ChannelLayout::default_for(1);
        }
        stream.params = params;
        Self {
            io,
            stream,
            band,
            frames_emitted: 0,
            budget: Budget::new(vaco_limits::Limits::permissive()),
            eof: false,
        }
    }

    const fn samples_per_frame(&self) -> u32 {
        match self.band {
            Band::Narrow => 160,
            Band::Wide => 320,
        }
    }
}

impl Demuxer for AmrDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    #[allow(
        clippy::integer_division,
        reason = "samples-per-frame and sample-rate are both fixed per band; the division is exact"
    )]
    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let Ok(toc) = self.io.r8() else {
            self.eof = true;
            return Err(Error::Eof);
        };
        let mode = (toc >> 3) & 0xf;
        let payload_len = self.band.frame_size(mode);
        let mut pkt = Packet::alloc(&mut self.budget, 1 + payload_len)?;
        {
            let payload = pkt.payload_mut();
            if let Some(first) = payload.first_mut() {
                *first = toc;
            }
            if let Some(rest) = payload.get_mut(1..) {
                self.io.read_exact(rest)?;
            }
        }
        pkt.stream_index = 0;
        let spf = u64::from(self.samples_per_frame());
        pkt.pts = Timestamp::new(i64::try_from(self.frames_emitted.saturating_mul(spf)).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        let rate = u64::from(self.band.sample_rate());
        pkt.duration = vaco_core::Duration::from_micros(
            i64::try_from(spf.saturating_mul(1_000_000) / rate.max(1)).unwrap_or(i64::MAX),
        );
        self.frames_emitted = self.frames_emitted.saturating_add(1);
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

    fn nb_frame(mode: u8) -> Vec<u8> {
        let mut v = vec![mode << 3];
        v.resize(1 + Band::Narrow.frame_size(mode), 0xAB);
        v
    }

    #[test]
    fn magic_selects_narrowband_or_wideband() {
        let mut data = MAGIC_NB.to_vec();
        data.extend(nb_frame(7));
        let mut d = open_amr(Box::new(MemorySource::new(data)), &vaco_format_core::discovery::NoParsers)
            .unwrap();
        assert_eq!(d.streams().first().unwrap().params.audio.as_ref().unwrap().sample_rate, 8000);
        let pkt = d.read_packet().unwrap();
        assert_eq!(pkt.len, 1 + 31);
    }

    #[test]
    fn frame_sizes_follow_the_mode_table() {
        let mut data = Vec::new();
        for mode in 0u8..=7 {
            data.extend(nb_frame(mode));
        }
        let io = IoContext::new(Box::new(MemorySource::new(data)), &IoOptions::default()).unwrap();
        let mut d = AmrDemuxer::new(io, Band::Narrow);
        for (i, &want) in SIZE_NB.iter().take(8).enumerate() {
            let pkt = d.read_packet().unwrap();
            assert_eq!(pkt.len, 1 + want, "mode {i}");
        }
    }

    #[test]
    fn a_missing_signature_is_rejected() {
        let data = b"not amr".to_vec();
        assert!(open_amr(Box::new(MemorySource::new(data)), &vaco_format_core::discovery::NoParsers).is_err());
    }
}
