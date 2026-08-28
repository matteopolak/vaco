//! CD+Graphics (`.cdg`), the karaoke-machine subchannel format: a bare
//! stream of fixed-size instruction packets with no file header at all.
//!
//! `Vaco-Spec-Ref cdg-revealed` (Jim Bumgardner's "CD+G Revealed", the
//! format's standard public reference) gives the 24-byte packet layout.
//!
//! # Layout
//!
//! ```text
//! packet, repeated, no file header
//!   0   1   command       only the low 6 bits matter; 0x09 = CD+G data
//!   1   1   instruction   low 6 bits select the operation
//!   2   2   parity Q
//!   4  16   data
//!  20   4   parity P
//! ```
//!
//! 300 packets per second (75 CD sectors/s × 4 packets/sector) is the
//! format's fixed playback rate; nothing in the stream states it.
//!
//! # Measured against the reference (`ffprobe` 8.1)
//!
//! * **Every** 24-byte-aligned chunk becomes its own packet, unconditionally
//!   — a chunk whose command byte's low 6 bits are not `0x09` still gets a
//!   packet. The `0x09` test is a *decoder* concern (which instructions to
//!   act on), not a demuxer one, confirmed by building a file with one
//!   non-`0x09` chunk among otherwise-valid ones and counting packets out.
//! * `probe_score` is exactly `min(n, 85)`, where `n` is the count of
//!   complete 24-byte chunks in the probed prefix whose command byte's low 6
//!   bits equal `0x09` — measured by holding chunk count fixed at values
//!   from 20 to 1000 and watching the score track it 1:1 up to 80, land on
//!   85 at 90, and stay at 85 through 1000. This is a genuinely weak
//!   content test (a file of all-zero bytes loses to an unrelated format's
//!   probe entirely), which fits a format with no magic at all.
//! * `width`/`height` are the fixed CD+G screen size, `300×216`, stated
//!   nowhere in the stream.
//! * `r_frame_rate` and `avg_frame_rate` both come out as `300/1`.
//! * Only packet index 0 is a keyframe — the same positional rule `flic`
//!   and (for its video stream) `roq` show, not a property of the packet's
//!   command/instruction bytes.
//!
//! # What is not implemented
//!
//! `cdgraphics` has no [`CodecId`] variant in `vaco-codec-core`; `codec_id`
//! is `None` here (see the crate-level report for the full list).

use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

const PACKET_LEN: usize = 24;
const CDG_COMMAND_MASK: u8 = 0x3f;
const CDG_COMMAND: u8 = 0x09;

/// Measured cap on `probe_score`: `AVPROBE_SCORE_MAX - 15`, reached once 90
/// or more valid-looking packets are found in the probed prefix.
const SCORE_CAP: u8 = 85;

const SAMPLE_RATE: u32 = 300;
const WIDTH: u32 = 300;
const HEIGHT: u32 = 216;

const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let mut valid: u32 = 0;
    let mut at = 0usize;
    while let Some(cmd) = data.get(at) {
        if at.saturating_add(PACKET_LEN) > data.len() {
            break;
        }
        if cmd & CDG_COMMAND_MASK == CDG_COMMAND {
            valid = valid.saturating_add(1);
        }
        at = at.saturating_add(PACKET_LEN);
    }
    ProbeScore(u8::try_from(valid).unwrap_or(u8::MAX).min(SCORE_CAP))
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "cdg",
    long_name: "CD Graphics",
    extensions: &["cdg"],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(CdgDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct CdgDemuxer {
    io: IoContext,
    stream: Stream,
    budget: Budget,
    packet_index: i64,
    eof: bool,
}

impl CdgDemuxer {
    /// # Errors
    /// Propagates transport failure from `src`.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default())?;
        let time_base = Rational::new(1, SAMPLE_RATE.cast_signed());
        let mut stream = Stream::new(0, MediaType::Video, time_base);
        stream.r_frame_rate = Rational::new(SAMPLE_RATE.cast_signed(), 1);
        stream.avg_frame_rate = stream.r_frame_rate;
        let mut params = vaco_codec_core::CodecParameters::video();
        if let Some(v) = params.video.as_mut() {
            v.width = WIDTH;
            v.height = HEIGHT;
            v.coded_width = WIDTH;
            v.coded_height = HEIGHT;
            v.frame_rate = stream.r_frame_rate;
            v.field_order = vaco_codec_core::FieldOrder::Unknown;
        }
        stream.params = params;
        if let Some(size) = io.size() {
            #[allow(
                clippy::integer_division,
                reason = "packet count from a byte count; a partial trailing packet is not counted"
            )]
            let packets = size / PACKET_LEN as u64;
            stream.duration_ts = i64::try_from(packets).ok();
            // Measured: the reference leaves `nb_frames` at `N/A` for cdg
            // even though it states `duration_ts` — the two come from
            // different estimation paths there, and only one is a frame
            // count.
        }
        Ok(Self {
            io,
            stream,
            budget: Budget::new(vaco_limits::Limits::permissive()),
            packet_index: 0,
            eof: false,
        })
    }
}

impl Demuxer for CdgDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let pos = self.io.pos();
        let mut pkt = Packet::alloc(&mut self.budget, PACKET_LEN)?;
        match self.io.read_exact(pkt.payload_mut()) {
            Ok(()) => {}
            Err(Error::UnexpectedEof) => {
                self.eof = true;
                return Err(Error::Eof);
            }
            Err(e) => return Err(e),
        }
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(self.packet_index);
        pkt.dts = pkt.pts;
        pkt.duration = Duration::ZERO;
        pkt.pos = Some(pos);
        if self.packet_index == 0 {
            pkt.flags = PacketFlags::KEY;
        }
        self.packet_index = self.packet_index.saturating_add(1);
        Ok(pkt)
    }

    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        let SeekTarget::Timestamp { stream_index, ts } = target else {
            return Err(Error::Unsupported("cdg: unsupported seek target"));
        };
        if stream_index != 0 {
            return Err(Error::InvalidData("cdg: no such stream"));
        }
        let ticks = ts.ticks().unwrap_or(0).max(0);
        let byte_pos = ticks
            .cast_unsigned()
            .saturating_mul(PACKET_LEN as u64);
        self.io.seek(byte_pos)?;
        self.packet_index = ticks;
        self.eof = false;
        Ok(())
    }

    fn duration(&self) -> Option<Duration> {
        self.stream.duration()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn packet(command: u8) -> Vec<u8> {
        let mut v = vec![command, 0, 0, 0];
        v.extend_from_slice(&[0u8; 16]);
        v.extend_from_slice(&[0u8; 4]);
        v
    }

    #[test]
    fn probe_counts_valid_packets_and_caps_at_85() {
        let mut data = Vec::new();
        for _ in 0..20 {
            data.extend_from_slice(&packet(0x09));
        }
        assert_eq!(probe(&ProbeData::new(&data)).value(), 20);

        let mut data = Vec::new();
        for _ in 0..200 {
            data.extend_from_slice(&packet(0x09));
        }
        assert_eq!(probe(&ProbeData::new(&data)).value(), 85);

        assert_eq!(probe(&ProbeData::new(&[])), ProbeScore::NONE);
    }

    #[test]
    fn every_chunk_becomes_a_packet_regardless_of_command_byte() {
        let mut data = Vec::new();
        data.extend_from_slice(&packet(0x09));
        data.extend_from_slice(&packet(0x00)); // not a CD+G command
        data.extend_from_slice(&packet(0x09));

        let mut d = CdgDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert!(d.read_packet().unwrap().is_key());
        assert!(!d.read_packet().unwrap().is_key());
        assert!(!d.read_packet().unwrap().is_key());
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn dimensions_are_fixed() {
        let d = CdgDemuxer::open(Box::new(MemorySource::new(packet(0x09)))).unwrap();
        let v = d.streams().first().unwrap().params.video.as_ref().unwrap();
        assert_eq!((v.width, v.height), (300, 216));
    }

    #[test]
    fn seek_lands_on_a_packet_boundary() {
        let mut data = Vec::new();
        for i in 0..10u8 {
            data.extend_from_slice(&packet(0x09 | (i << 6)));
        }
        let mut d = CdgDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        d.seek(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts: Timestamp::new(5),
            },
            SeekFlags::empty(),
        )
        .unwrap();
        let pkt = d.read_packet().unwrap();
        assert_eq!(pkt.pts, Timestamp::new(5));
    }
}
