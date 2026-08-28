//! Raw ITU-T G.723.1, self-delimited by the low two bits of each frame's
//! first byte (the codec's own frame-type field): `0` = 6.3 kbit/s (24
//! bytes), `1` = 5.3 kbit/s (20 bytes), `2` = SID/comfort-noise (4 bytes),
//! `3` = untransmitted/erasure, stored as a single byte so the stream stays
//! self-delimiting even when nothing was sent.
//!
//! Every frame spans 30 ms (240 samples at the fixed 8 kHz rate) regardless
//! of its size. Measured: a 0.3 s fixture encoded at the default (6.3 kbit/s)
//! rate is exactly ten 24-byte frames — `240` bytes total, `24 * 10`.

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

const SAMPLE_RATE: u32 = 8000;
const SAMPLES_PER_FRAME: u32 = 240;
const EXTENSIONS: &[&str] = &["g723_1"];

/// Bytes stored for each of the four frame-type indices (low 2 bits of the
/// first byte).
const FRAME_BYTES: [usize; 4] = [24, 20, 4, 1];

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.extension_matches(EXTENSIONS) {
        ProbeScore::EXTENSION
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "g723_1",
    long_name: "raw G.723.1",
    extensions: EXTENSIONS,
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(G723Demuxer::open(src)?))
}

#[derive(Debug)]
pub struct G723Demuxer {
    io: IoContext,
    stream: Stream,
    frames_emitted: u64,
    budget: Budget,
    eof: bool,
}

impl G723Demuxer {
    /// # Errors
    /// Propagates transport failure from `src`.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default())?;
        let mut stream = Stream::new(0, MediaType::Audio, Rational::new(1, SAMPLE_RATE.cast_signed()));
        let mut params = CodecParameters::audio();
        params.codec_id = Some(CodecId::G7231);
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = SAMPLE_RATE;
            audio.layout = ChannelLayout::default_for(1);
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

impl Demuxer for G723Demuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    #[allow(
        clippy::integer_division,
        reason = "SAMPLE_RATE and SAMPLES_PER_FRAME are fixed constants; the division is exact"
    )]
    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let Ok(first) = self.io.peek(1) else {
            self.eof = true;
            return Err(Error::Eof);
        };
        let Some(&type_byte) = first.first() else {
            self.eof = true;
            return Err(Error::Eof);
        };
        let size = FRAME_BYTES
            .get(usize::from(type_byte & 0x3))
            .copied()
            .unwrap_or(1);
        let mut pkt = Packet::alloc(&mut self.budget, size)?;
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(
            i64::try_from(self.frames_emitted.saturating_mul(u64::from(SAMPLES_PER_FRAME)))
                .unwrap_or(i64::MAX),
        );
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        pkt.duration = vaco_core::Duration::from_micros(
            i64::from(SAMPLES_PER_FRAME) * 1_000_000 / i64::from(SAMPLE_RATE),
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

    #[test]
    fn each_frame_type_reads_its_own_declared_size() {
        let mut data = Vec::new();
        data.push(0u8); // type 0: 24 bytes
        data.extend(vec![0u8; 23]);
        data.push(1u8); // type 1: 20 bytes
        data.extend(vec![0u8; 19]);
        data.push(3u8); // type 3: 1 byte total (this byte itself)

        let mut d = G723Demuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.read_packet().unwrap().len, 24);
        assert_eq!(d.read_packet().unwrap().len, 20);
        assert_eq!(d.read_packet().unwrap().len, 1);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn pts_advances_by_one_frame_regardless_of_byte_size() {
        let mut data = vec![0u8; 24];
        data.extend(vec![3u8; 1]);
        let mut d = G723Demuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.read_packet().unwrap().pts.ticks(), Some(0));
        assert_eq!(d.read_packet().unwrap().pts.ticks(), Some(240));
    }
}
