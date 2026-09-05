//! True Audio (`.tta`), TTA1 stream format.
//!
//! `true-audio.com`'s own format page has been domain-squatted for some time
//! and could not be used as a source. The layout below was instead
//! reconstructed directly from an `ffmpeg -c:a tta -f tta` fixture (a
//! blackbox measurement, per D17) and cross-checked field by field: every
//! value below reproduces exactly, including the seek-table entry equalling
//! the measured byte length of the one frame it describes.
//!
//! # Layout
//!
//! ```text
//! header (22 bytes):
//!   magic:FourCC("TTA1")  audio_format:le16  channels:le16  bits_per_sample:le16
//!   sample_rate:le32      total_samples:le32 header_crc32:le32
//!
//! seek table: ceil(total_samples / frame_len) entries, each a le32 byte
//! length of the corresponding frame, followed by a le32 CRC32 of the table.
//!
//! frame_len (samples) = sample_rate * 256 / 245
//! ```
//!
//! Frames are simply concatenated after the seek table; there is no per-frame
//! header to skip. A trailing `APEv2` tag (measured: `ffmpeg` always writes
//! one) sits after the last frame and is not read.
//!
//! # What is not read
//!
//! The header and per-frame CRC32s are skipped, not verified — a corrupt
//! frame is still handed to the caller rather than rejected here.

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
use vaco_sampfmt::SampleFmt;

const MAGIC: [u8; 4] = *b"TTA1";
/// A file this large would need a seek table longer than any real recording;
/// past it, a corrupt `total_samples` cannot run the table-size computation
/// away to something unbounded.
const MAX_PLAUSIBLE_FRAMES: u64 = 16 * 1024 * 1024;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(&MAGIC) {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "tta",
    long_name: "TTA (True Audio)",
    extensions: &["tta"],
    mime_types: &["audio/x-tta"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(TtaDemuxer::open(src)?))
}

fn sample_fmt_for(bits: u16) -> Option<SampleFmt> {
    match bits {
        8 => Some(SampleFmt::U8),
        16 => Some(SampleFmt::S16),
        24 | 32 => Some(SampleFmt::S32),
        _ => None,
    }
}

#[must_use]
#[allow(
    clippy::integer_division,
    reason = "the frame length is exactly sample_rate*256/245 by the format's own definition"
)]
fn samples_per_frame(sample_rate: u32) -> u64 {
    u64::from(sample_rate).saturating_mul(256) / 245
}

#[derive(Debug)]
pub struct TtaDemuxer {
    io: IoContext,
    stream: Stream,
    /// Byte length of each frame, from the seek table.
    frame_sizes: Vec<u32>,
    frame_len_samples: u64,
    total_samples: u64,
    next_frame: usize,
    frames_emitted_samples: u64,
    budget: Budget,
}

impl TtaDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the magic or header fields are malformed.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.tag()? != MAGIC {
            return Err(Error::InvalidData("tta: missing TTA1 signature"));
        }
        let audio_format = io.rl16()?;
        let channels = io.rl16()?;
        let bits_per_sample = io.rl16()?;
        let sample_rate = io.rl32()?.max(1);
        let total_samples = u64::from(io.rl32()?);
        let _header_crc = io.rl32()?;

        let frame_len = samples_per_frame(sample_rate).max(1);
        let num_frames = total_samples.div_ceil(frame_len).max(1);
        if num_frames > MAX_PLAUSIBLE_FRAMES {
            return Err(Error::InvalidData("tta: implausible seek-table length"));
        }
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut frame_sizes: Vec<u32> = budget.alloc(usize::try_from(num_frames).unwrap_or(0))?;
        for entry in &mut frame_sizes {
            *entry = io.rl32()?;
        }
        let _seek_table_crc = io.rl32()?;

        if audio_format != 1 {
            return Err(Error::Unsupported(
                "tta: only PCM (audio_format=1) is supported",
            ));
        }
        let format = sample_fmt_for(bits_per_sample);

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, sample_rate.cast_signed()),
        );
        let mut params = CodecParameters::audio();
        params.codec_id = Some(CodecId::Tta);
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            audio.format = format;
            audio.layout = ChannelLayout::default_for(u32::from(channels.max(1)));
            audio.bits_per_coded_sample = Some(bits_per_sample.min(u16::from(u8::MAX)) as u8);
        }
        stream.params = params;
        stream.duration_ts = i64::try_from(total_samples).ok();
        stream.frame_count = Some(num_frames);

        Ok(Self {
            io,
            stream,
            frame_sizes,
            frame_len_samples: frame_len,
            total_samples,
            next_frame: 0,
            frames_emitted_samples: 0,
            budget,
        })
    }
}

impl Demuxer for TtaDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let Some(&size) = self.frame_sizes.get(self.next_frame) else {
            return Err(Error::Eof);
        };
        let mut pkt = Packet::alloc(&mut self.budget, size as usize)?;
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(i64::try_from(self.frames_emitted_samples).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;

        let this_frame_samples = self.frame_len_samples.min(
            self.total_samples
                .saturating_sub(self.frames_emitted_samples),
        );
        pkt.duration = i64::try_from(this_frame_samples)
            .ok()
            .and_then(|ticks| vaco_core::Duration::from_ticks(ticks, self.stream.time_base))
            .unwrap_or(vaco_core::Duration::ZERO);

        self.frames_emitted_samples = self
            .frames_emitted_samples
            .saturating_add(this_frame_samples);
        self.next_frame = self.next_frame.saturating_add(1);
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        // The seek table gives frame byte lengths, not offsets, so honouring
        // an arbitrary target means summing sizes from the start; not
        // implemented in this pass.
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        vaco_core::Duration::from_ticks(
            i64::try_from(self.total_samples).ok()?,
            self.stream.time_base,
        )
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

    /// Hand-built per the module docs: 22-byte header, a one-entry seek
    /// table (since the payload is far shorter than one TTA frame) plus its
    /// CRC, then that many bytes of arbitrary "compressed" payload.
    fn build_file(sample_rate: u32, channels: u16, total_samples: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&MAGIC);
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(&sample_rate.to_le_bytes());
        v.extend_from_slice(&total_samples.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn opens_and_reads_the_single_frame_it_declares() {
        let payload = vec![0xABu8; 100];
        let data = build_file(44_100, 2, 13_230, &payload);
        let src = Box::new(MemorySource::new(data));
        let mut d = TtaDemuxer::open(src).unwrap();
        assert_eq!(
            d.streams()
                .first()
                .unwrap()
                .params
                .audio
                .as_ref()
                .unwrap()
                .sample_rate,
            44_100
        );
        let pkt = d.read_packet().unwrap();
        assert_eq!(pkt.payload(), payload.as_slice());
        assert_eq!(pkt.pts.ticks(), Some(0));
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn aggregate_duration_keeps_the_native_sample_clock_exact() {
        let data = build_file(44_100, 2, 46_080, &[0xABu8; 100]);
        let d = TtaDemuxer::open(Box::new(MemorySource::new(data))).unwrap();

        assert_eq!(
            d.duration_exact().map(vaco_core::ExactDuration::as_ratio),
            Some((256, 245))
        );
    }

    #[test]
    fn probe_matches_only_the_signature() {
        let ok = ProbeData::new(b"TTA1garbage");
        assert_eq!(probe(&ok), ProbeScore::MAGIC);
        let bad = ProbeData::new(b"not tta at all");
        assert_eq!(probe(&bad), ProbeScore::NONE);
    }

    #[test]
    fn an_implausible_sample_count_is_rejected_before_allocating() {
        let mut v = Vec::new();
        v.extend_from_slice(&MAGIC);
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes()); // sample_rate = 1: frame_len tiny
        v.extend_from_slice(&u32::MAX.to_le_bytes()); // total_samples huge
        v.extend_from_slice(&0u32.to_le_bytes());
        let src = Box::new(MemorySource::new(v));
        assert!(TtaDemuxer::open(src).is_err());
    }
}
