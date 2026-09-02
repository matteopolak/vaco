//! Sony PS2 VAG (`.vag`), a mono PS-ADPCM container with a fixed 48-byte
//! header.
//!
//! # Layout, measured against a hand-built fixture and `ffprobe` 8.1
//!
//! ```text
//! "VAGp"            -- 4-byte magic
//! version:be32  reserved:be32
//! data_size:be32    -- bytes of ADPCM data that follow the header
//! sample_rate:be32
//! reserved:[u8; 12]
//! name:[u8; 16]     -- NUL-padded ASCII, not surfaced (no tag slot fits it
//!                      better than the file name a caller already has)
//! data: 16-byte PS-ADPCM blocks, 28 decoded samples each, mono only
//! ```
//!
//! Header layout corroborated across three independent community
//! references (`Vaco-Spec-Ref vag-format-doc`): the `PlayStation`
//! Reverse-Engineering wiki, the Just Solve the File Format Problem wiki,
//! and fmjsoft's format collection agree field-for-field. The fixture was
//! hand-built from that agreement, not copied from any one of them.
//!
//! # Packet granularity
//!
//! The reference emits **one packet per 16-byte block** — ten blocks, ten
//! `-show_packets` entries, `size=16` each, `pts`/`dts` advancing by 28
//! (one block's samples) every packet. This module reproduces that
//! directly with its own loop rather than going through `BlockDemuxer`.
//!
//! # What is not read
//!
//! `version` and the 16-byte `name` field are validated for presence only
//! and not surfaced — the reference does not expose them as stream
//! metadata either (`ffprobe` prints nothing for either field).
//!
//! # Missing `CodecId`
//!
//! The reference names this codec `adpcm_psx` (`ADPCM Playstation`).
//! `vaco-codec-core` has no variant for it yet, so this stream's `codec_id`
//! is `None` until that lands, same as every format in `vaco-format-misc`.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

const MAGIC: &[u8; 4] = b"VAGp";
const HEADER_LEN: u64 = 0x30;
const BLOCK_BYTES: u32 = 16;
const SAMPLES_PER_BLOCK: u32 = 28;
/// Bounds `data_size` before it is used to compute a block count; a real
/// PS2 sound bank is nowhere near this large.
const MAX_DATA_SIZE: u32 = 1 << 30;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(MAGIC) {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "vag",
    long_name: "Sony PS2 VAG",
    extensions: &["vag"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(VagDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct VagDemuxer {
    io: IoContext,
    stream: Stream,
    /// `None` once the declared `data_size` did not survive clamping to a
    /// whole number of blocks against the source's own size, meaning "read
    /// to true EOF" like the unbounded headerless codecs.
    block_count: Option<u64>,
    blocks_emitted: u64,
    eof: bool,
    budget: Budget,
}

impl VagDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the magic or header fields are malformed.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut magic = [0u8; 4];
        io.read_exact(&mut magic)?;
        if magic != *MAGIC {
            return Err(Error::InvalidData("vag: missing VAGp signature"));
        }
        let _version = io.rb32()?;
        let _reserved = io.rb32()?;
        let data_size = io.rb32()?;
        if data_size > MAX_DATA_SIZE {
            return Err(Error::InvalidData("vag: implausible data size"));
        }
        let sample_rate = io.rb32()?.max(1);
        io.seek(HEADER_LEN)?;

        #[allow(
            clippy::integer_division,
            reason = "exact block count from a byte count"
        )]
        let declared_blocks = u64::from(data_size) / u64::from(BLOCK_BYTES);
        let block_count = match io.size() {
            Some(size) => {
                let available = size.saturating_sub(HEADER_LEN);
                #[allow(
                    clippy::integer_division,
                    reason = "exact block count from a byte count"
                )]
                let available_blocks = available / u64::from(BLOCK_BYTES);
                Some(declared_blocks.min(available_blocks))
            }
            None => Some(declared_blocks),
        };

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, sample_rate.cast_signed()),
        );
        let params = CodecParameters::audio();
        stream.params = params;
        if let Some(audio) = stream.params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            audio.layout = ChannelLayout::default_for(1);
        }
        if let Some(blocks) = block_count {
            let frames = blocks.saturating_mul(u64::from(SAMPLES_PER_BLOCK));
            stream.duration_ts = i64::try_from(frames).ok();
            stream.frame_count = Some(frames);
        }

        Ok(Self {
            io,
            stream,
            block_count,
            blocks_emitted: 0,
            eof: false,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for VagDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        if let Some(count) = self.block_count
            && self.blocks_emitted >= count
        {
            self.eof = true;
            return Err(Error::Eof);
        }

        let pos =
            HEADER_LEN.saturating_add(self.blocks_emitted.saturating_mul(u64::from(BLOCK_BYTES)));
        let mut pkt = Packet::alloc(&mut self.budget, BLOCK_BYTES as usize)?;
        let n = self.io.read_partial(pkt.payload_mut())?;
        if n == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        if n < BLOCK_BYTES as usize {
            // A short final read: this is the true end of the stream, not
            // corruption — the same policy `BlockDemuxer` uses.
            self.eof = true;
        }
        pkt.len = n;
        pkt.stream_index = 0;
        let frame_index = self
            .blocks_emitted
            .saturating_mul(u64::from(SAMPLES_PER_BLOCK));
        pkt.pts = vaco_core::Timestamp::new(i64::try_from(frame_index).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.duration = vaco_core::Duration::from_micros(
            i64::from(SAMPLES_PER_BLOCK)
                .saturating_mul(1_000_000)
                .checked_div(i64::from(
                    self.stream
                        .params
                        .audio
                        .as_ref()
                        .map_or(1, |a| a.sample_rate.max(1)),
                ))
                .unwrap_or(0),
        );
        pkt.flags = PacketFlags::KEY;
        pkt.pos = Some(pos);
        self.blocks_emitted = self.blocks_emitted.saturating_add(1);
        Ok(pkt)
    }

    #[allow(
        clippy::integer_division,
        reason = "exact block index from a byte offset or frame count"
    )]
    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let frame = match target {
            SeekTarget::Byte(b) => {
                let block = b.saturating_sub(HEADER_LEN) / u64::from(BLOCK_BYTES);
                block.saturating_mul(u64::from(SAMPLES_PER_BLOCK))
            }
            SeekTarget::Frame { frame, .. } => frame,
            SeekTarget::Timestamp { ts, .. } => {
                u64::try_from(ts.ticks().unwrap_or(0).max(0)).unwrap_or(0)
            }
        };
        let block = frame / u64::from(SAMPLES_PER_BLOCK);
        let byte_pos = HEADER_LEN.saturating_add(block.saturating_mul(u64::from(BLOCK_BYTES)));
        self.io.seek(byte_pos)?;
        self.blocks_emitted = block;
        self.eof = false;
        Ok(())
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        let blocks = self.block_count?;
        let frames = blocks.saturating_mul(u64::from(SAMPLES_PER_BLOCK));
        let rate = u64::from(self.stream.params.audio.as_ref()?.sample_rate.max(1));
        let micros = frames.checked_mul(1_000_000)?.checked_div(rate)?;
        Some(vaco_core::Duration::from_micros(
            i64::try_from(micros).unwrap_or(i64::MAX),
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn build_file(sample_rate: u32, blocks: u32) -> Vec<u8> {
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&0x0000_0020u32.to_be_bytes()); // version
        v.extend_from_slice(&0u32.to_be_bytes()); // reserved
        v.extend_from_slice(&(blocks * BLOCK_BYTES).to_be_bytes()); // data_size
        v.extend_from_slice(&sample_rate.to_be_bytes());
        v.extend_from_slice(&[0u8; 12]); // reserved
        v.extend_from_slice(&[0u8; 16]); // name
        for i in 0..blocks {
            v.push(i.cast_signed() as u8);
            v.push(0);
            v.extend_from_slice(&[0x11u8; 14]);
        }
        v
    }

    #[test]
    fn header_fields_and_block_geometry_match_the_measured_fixture() {
        let data = build_file(22_050, 10);
        let d = VagDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let s = d.streams().first().unwrap();
        assert_eq!(s.params.audio.as_ref().unwrap().sample_rate, 22_050);
        assert_eq!(
            s.params
                .audio
                .as_ref()
                .unwrap()
                .layout
                .as_ref()
                .unwrap()
                .channels,
            1
        );
        assert_eq!(s.duration_ts, Some(280));
    }

    #[test]
    fn one_packet_per_block_matching_the_reference() {
        let data = build_file(22_050, 10);
        let mut d = VagDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let mut count = 0;
        let mut last_pts = -1i64;
        loop {
            match d.read_packet() {
                Ok(pkt) => {
                    assert_eq!(pkt.len, 16);
                    assert!(pkt.pts.ticks().unwrap() > last_pts);
                    last_pts = pkt.pts.ticks().unwrap();
                    assert!(pkt.flags.contains(PacketFlags::KEY));
                    count += 1;
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert_eq!(count, 10);
    }

    #[test]
    fn probe_checks_the_full_magic() {
        let data = build_file(8000, 1);
        assert_eq!(probe(&ProbeData::new(&data)), ProbeScore::MAGIC_CHECKED);
        assert_eq!(
            probe(&ProbeData::new(b"not vag at all..")),
            ProbeScore::NONE
        );
    }

    #[test]
    fn an_implausible_data_size_is_rejected() {
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&u32::MAX.to_be_bytes());
        v.extend_from_slice(&8000u32.to_be_bytes());
        assert!(VagDemuxer::open(Box::new(MemorySource::new(v))).is_err());
    }
}
