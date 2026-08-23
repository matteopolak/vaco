//! `dvbtxt` — raw DVB (enhanced) teletext (ETSI EN 300 706 / EN 300 472).
//!
//! # The demuxer/decoder line, measured
//!
//! `ffmpeg -h demuxer=dvbtxt` names it `"dvbtxt"` with the same generic
//! `-raw_packet_size` raw-demuxer option `dvbsub` has — the reference treats
//! this exactly like any other headerless elementary stream. [`DEMUXER`]
//! matches that measured behaviour: fixed [`RAW_PACKET_SIZE`]-byte chunks,
//! [`FormatFlags::NOTIMESTAMPS`]. See [`crate::dvbsub`]'s module docs for the
//! same reasoning in more detail; it applies here unchanged.
//!
//! [`teletext`] is EN 300 472's real, fixed-width 46-byte data-unit
//! structure — used by [`probe`] for a far more specific content sniff than
//! "matches the `.sub`/extension it happens to share with `dvbsub` and
//! `MicroDVD`", but, per the measured reference behaviour above, not by
//! [`DEMUXER`]'s packetisation. Unpacking a data unit's Hamming-coded
//! magazine/row address and page content is decoder work (EN 300 706 §8),
//! not this crate's.
//!
//! No muxer: the reference has none.

pub mod teletext;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

/// The reference's own default (`ffmpeg -h demuxer=dvbtxt`'s
/// `-raw_packet_size`, default `1024`).
pub const RAW_PACKET_SIZE: usize = 1024;

/// Content probe: a run of well-formed 46-byte teletext data units.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let mut buf = Vec::new();
    let mut i = 0usize;
    while let Some(b) = data.get(i) {
        buf.push(b);
        i = i.saturating_add(1);
        if i > 1 << 16 {
            break;
        }
    }
    let count = teletext::count_valid_records(&buf);
    if count > 0 {
        ProbeScore::repeating(count)
    } else {
        ProbeScore::NONE
    }
}

/// Fixed-size raw chunk reader, matching the measured reference demuxer.
#[derive(Debug)]
pub struct DvbTxtDemuxer {
    io: IoContext,
    streams: [Stream; 1],
    budget: Budget,
    eof: bool,
}

impl DvbTxtDemuxer {
    /// # Errors
    /// Propagates I/O failure from opening `src`.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default())?;
        let mut stream = Stream::new(0, MediaType::Subtitle, TIME_BASE_Q);
        stream.params = CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::DvbTeletext);
        Ok(Self {
            io,
            streams: [stream],
            budget: Budget::new(vaco_limits::Limits::permissive()),
            eof: false,
        })
    }
}

impl Demuxer for DvbTxtDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let mut buf = self.budget.alloc::<u8>(RAW_PACKET_SIZE)?;
        let mut got = 0usize;
        while got < buf.len() {
            let Some(rest) = buf.get_mut(got..) else {
                break;
            };
            let n = self.io.read_partial(rest)?;
            if n == 0 {
                break;
            }
            got = got.saturating_add(n);
        }
        if got == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        if got < buf.len() {
            self.eof = true;
        }
        let mut pkt = Packet::from_slice(&mut self.budget, buf.get(..got).unwrap_or(&[]))?;
        pkt.stream_index = 0;
        pkt.flags = PacketFlags::KEY;
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(DvbTxtDemuxer::open(src)?))
}

pub const DEMUX_FLAGS: FormatFlags = FormatFlags::NOTIMESTAMPS
    .union(FormatFlags::NOBINSEARCH)
    .union(FormatFlags::NOGENSEARCH)
    .union(FormatFlags::NO_BYTE_SEEK);

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "dvbtxt",
    long_name: "dvbtxt",
    extensions: &[],
    mime_types: &[],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for id in [0x02u8, 0x03, 0xFF, 0x02] {
            let mut r = vec![id, teletext::DATA_UNIT_LENGTH];
            r.extend(std::iter::repeat_n(0u8, teletext::RECORD_LEN - 2));
            v.extend(r);
        }
        v
    }

    #[test]
    fn probe_accepts_a_real_data_unit_run() {
        let bytes = sample();
        let data = ProbeData::new(&bytes);
        assert!(probe(&data).value() > ProbeScore::RETRY.value());
    }

    #[test]
    fn probe_rejects_plain_prose() {
        let data = ProbeData::new(b"The quick brown fox jumps over the lazy dog, repeatedly.\n");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn probe_rejects_a_dvbsub_sample() {
        let mut dvbsub_like = vec![crate::dvbsub::segments::SYNC_BYTE, 0x10, 0, 1, 0, 2, 9, 9];
        dvbsub_like.extend(std::iter::repeat_n(0u8, 40));
        let data = ProbeData::new(&dvbsub_like);
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn demux_yields_fixed_size_chunks() {
        let data = vec![0xABu8; RAW_PACKET_SIZE + 5];
        let mut d = DvbTxtDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.payload().len(), RAW_PACKET_SIZE);
        let p2 = d.read_packet().unwrap();
        assert_eq!(p2.payload().len(), 5);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }
}
