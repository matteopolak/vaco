//! Maxis XA (`.xa`): the EA-ADPCM container Maxis games from `SimCity 3000`
//! onwards use for music and speech/SFX, distinct from Philips/Sony's
//! CD-ROM XA and from `vaco-format-misc`'s PS1 `xa`-family formats.
//!
//! # Layout, measured against `ffmpeg`/`ffprobe` 8.1 (no reference encoder
//! exists, so hand-built fixtures probed with the real `xa` demuxer)
//!
//! ```text
//! szID:[u8; 4]        -- "XAI\0" (speech/sfx) or "XAJ\0" (music); only the
//!                         first two bytes ("XA") are checked, matching the
//!                         reference's own probe
//! dwOutSize:le32       -- decompressed PCM byte count; NOT used for framing,
//!                          see below
//! wTag:le16 wChannels:le16 dwSampleRate:le32 dwAvgByteRate:le32
//! wAlign:le16 wBits:le16
//! data: EA-ADPCM blocks, 15 bytes per channel, 28 decoded samples each
//! ```
//!
//! `wTag` onwards is a Win32 `WAVEFORMATEX` (`Vaco-Spec-Ref
//! microsoft-riff-xaudio2` for the struct layout), all little-endian — this
//! project's own EA-ADPCM decoder algorithm reference
//! (`Vaco-Spec-Ref maxis-xa-format-doc`) documents field meanings but not
//! byte order, so LE was confirmed the only reading under which a hand-built
//! fixture's `sample_rate`/`channels` come back sane through the real `xa`
//! demuxer (`Vaco-Spec-Ref
//! vaco-format-misc-audio-xa-fixtures-probe`) — a BE fixture with the same
//! field values produces an implausible multi-GHz "sample rate".
//!
//! # `dwOutSize` gates packets, not duration — a real, two-part surprise
//!
//! An initial reading — that the field is decompressed PCM byte count, so
//! natural framing would floor-divide it by a PCM frame size — turned out
//! wrong on both ends, found only by sweeping the field across many values
//! against a fixture with plenty of real blocks on disk:
//!
//! - **Packet count is `ceil(dwOutSize / block_bytes)`, clamped to the
//!   blocks actually present.** `block_bytes` is the *compressed* block
//!   size (`30` stereo, `15` mono), not a PCM frame size — sweeping
//!   `dwOutSize` from `0` to `559` against a 20-block stereo fixture (block
//!   size `30`) gave exactly `ceil(n/30)` packets at every value tried
//!   (`1`→1, `29`→1, `30`→1, `31`→2, `100`→4, `125`→5, `150`→5, …, `559`→19),
//!   every packet a full, never-partial `30`-byte block. `dwOutSize = 0`
//!   produces **zero** packets, not "read to EOF" — the case that broke an
//!   earlier, wrong reading of this field as advisory-only.
//! - **`duration`/`duration_ts` ignore `dwOutSize` entirely and instead
//!   reflect the file's own full block count.** The same 20-block fixture
//!   with `dwOutSize = 100` (4 packets) still reports `duration_ts = 560`
//!   (`20 * 28`, the *whole file's* sample count) — confirmed at both the
//!   stream and format level. The reference's packet emission and its
//!   duration estimate are computed from two different quantities and
//!   genuinely disagree; this module reproduces that disagreement rather
//!   than "fixing" it into a self-consistent number the reference itself
//!   does not produce (`Vaco-Spec-Ref
//!   vaco-format-misc-audio-xa-fixtures-probe`).
//!
//! # Block size and packet granularity
//!
//! Real Maxis XA ships only mono or stereo (`SimCity 3000`'s own speech and
//! music tracks); this module rejects any other channel count rather than
//! guess at an untested block-size formula (`channels * 15` held for both
//! tested cases, but the reference's own EA-ADPCM decoder refuses more than
//! two channels outright, so a hand-built &gt;2-channel fixture cannot be
//! probed for packet-level ground truth the way `channels = 1` and `2` were
//! — `-show_packets` measurement relies on the decoder accepting the
//! stream, not just the demuxer). One packet per block, `channels * 15`
//! bytes each (`15` mono, `30` stereo), `pts`/`dts` advancing by 28 samples
//! per packet regardless of channel count.
//!
//! # Missing `CodecId`
//!
//! The reference names this codec `adpcm_ea_maxis_xa` (`ADPCM Electronic
//! Arts Maxis CDROM XA`, confirmed via `ffmpeg -codecs`). `vaco-codec-core`
//! has no variant for it; see `planning/INTERFACE-GAPS.md` gap 21's `xa`
//! entry. This stream's `codec_id` is `None` until that lands, same policy
//! as every format in this crate without one.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::Packet;

use crate::block::BlockDemuxer;

const WAVE_FORMAT_PCM: u16 = 1;
const BLOCK_BYTES_PER_CHANNEL: u32 = 15;
const SAMPLES_PER_BLOCK: u32 = 28;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if !data.starts_with(b"XA") {
        return ProbeScore::NONE;
    }
    // A self-consistency check beyond the two-byte magic the reference
    // itself keys on: real files always carry the PCM format tag for the
    // WAVEFORMATEX-shaped tail (this container never compresses anything
    // other than to EA-ADPCM, described out of band).
    match data.rl16(8) {
        Some(WAVE_FORMAT_PCM) => ProbeScore::MAGIC_CHECKED,
        Some(_) => ProbeScore::NONE,
        None => ProbeScore::MAGIC,
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "xa",
    long_name: "Maxis XA",
    extensions: &["xa"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(XaDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct XaDemuxer {
    inner: BlockDemuxer,
    budget: Budget,
    /// `ceil(dwOutSize / block_bytes)` — the reference's own packet-count
    /// bound, independent of `inner`'s own (file-length-derived) idea of
    /// how much data exists. See the module doc.
    max_blocks: u64,
    blocks_emitted: u64,
}

impl XaDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the magic, `wTag`, sample rate or channel
    /// count are malformed or outside the mono/stereo range this container
    /// is documented and measured to use.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut magic = [0u8; 2];
        io.read_exact(&mut magic)?;
        if &magic != b"XA" {
            return Err(Error::InvalidData("xa: missing XA signature"));
        }
        io.seek(4)?;
        let out_size = io.rl32()?;
        let tag = io.rl16()?;
        if tag != WAVE_FORMAT_PCM {
            return Err(Error::InvalidData("xa: unexpected WAVEFORMATEX tag"));
        }
        let channels = io.rl16()?;
        if channels != 1 && channels != 2 {
            return Err(Error::InvalidData("xa: only mono or stereo is supported"));
        }
        let sample_rate = io.rl32()?.max(1);
        let _avg_byte_rate = io.rl32()?;
        let _align = io.rl16()?;
        let _bits = io.rl16()?;
        let data_start = io.pos();

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, sample_rate.cast_signed()),
        );
        let mut params = CodecParameters::audio();
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            audio.layout = ChannelLayout::default_for(u32::from(channels));
        }
        stream.params = params;

        let bytes_per_block = BLOCK_BYTES_PER_CHANNEL.saturating_mul(u32::from(channels));
        // `duration`/`frame_count` reflect the source's own true length,
        // not `dwOutSize` — measured to disagree with the packet count the
        // reference actually emits; see the module doc. A source of
        // unknown size falls back to `None` (stream to EOF, no upfront
        // duration), same as every other consumer of this engine.
        let declared_len = io.size();
        let max_blocks = u64::from(out_size).div_ceil(u64::from(bytes_per_block));
        let inner = BlockDemuxer::new(
            io,
            stream,
            data_start,
            declared_len,
            bytes_per_block,
            SAMPLES_PER_BLOCK,
            bytes_per_block,
        );
        Ok(Self {
            inner,
            budget: Budget::new(vaco_limits::Limits::permissive()),
            max_blocks,
            blocks_emitted: 0,
        })
    }
}

impl Demuxer for XaDemuxer {
    fn streams(&self) -> &[Stream] {
        self.inner.streams()
    }
    fn read_packet(&mut self) -> Result<Packet> {
        if self.blocks_emitted >= self.max_blocks {
            return Err(Error::Eof);
        }
        let pkt = self.inner.read_packet(&mut self.budget)?;
        self.blocks_emitted = self.blocks_emitted.saturating_add(1);
        Ok(pkt)
    }
    fn seek(
        &mut self,
        target: vaco_format_core::SeekTarget,
        flags: vaco_format_core::SeekFlags,
    ) -> Result<()> {
        self.inner.seek(target, flags)?;
        // Re-derive `blocks_emitted` from the position `inner` just landed
        // on so `max_blocks` still gates correctly after a seek.
        self.blocks_emitted = self.inner.block_index();
        Ok(())
    }
    fn duration(&self) -> Option<vaco_core::Duration> {
        self.inner.duration()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn build_file(
        tag: [u8; 4],
        channels: u16,
        sample_rate: u32,
        blocks_present: u32,
        out_size: u32,
    ) -> Vec<u8> {
        let bytes_per_sample: u16 = 2;
        let align = bytes_per_sample * channels;
        let avg_byte_rate = sample_rate.saturating_mul(u32::from(align));
        let mut v = tag.to_vec();
        v.extend_from_slice(&out_size.to_le_bytes());
        v.extend_from_slice(&WAVE_FORMAT_PCM.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&sample_rate.to_le_bytes());
        v.extend_from_slice(&avg_byte_rate.to_le_bytes());
        v.extend_from_slice(&align.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        let block_size = usize::try_from(BLOCK_BYTES_PER_CHANNEL).unwrap() * usize::from(channels);
        for _ in 0..blocks_present {
            v.extend(vec![0x11u8; block_size]);
        }
        v
    }

    /// `dwOutSize` set to exactly the decompressed byte count of every
    /// block present — the "well-formed, nothing clamped" case.
    fn out_size_for(channels: u16, blocks: u32) -> u32 {
        let bytes_per_sample = 2u32;
        let align = bytes_per_sample * u32::from(channels);
        blocks
            .saturating_mul(SAMPLES_PER_BLOCK)
            .saturating_mul(align)
    }

    #[test]
    fn header_fields_and_block_geometry_match_the_measured_fixture() {
        let data = build_file(*b"XAI\0", 2, 22_050, 5, out_size_for(2, 5));
        let d = XaDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
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
            2
        );
        // duration reflects the file's own full block count, independent of
        // dwOutSize -- see the module doc.
        assert_eq!(s.duration_ts, Some(140));
    }

    #[test]
    fn one_packet_per_block_matching_the_reference() {
        let data = build_file(*b"XAJ\0", 1, 8000, 5, out_size_for(1, 5));
        let mut d = XaDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let mut count = 0;
        let mut last_pts = -1i64;
        loop {
            match d.read_packet() {
                Ok(pkt) => {
                    assert_eq!(pkt.len, 15);
                    assert!(pkt.pts.ticks().unwrap() > last_pts);
                    last_pts = pkt.pts.ticks().unwrap();
                    count += 1;
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert_eq!(count, 5);
    }

    #[test]
    fn zero_out_size_produces_zero_packets() {
        // Measured against the reference: dwOutSize = 0 is not "advisory",
        // it means "zero packets" outright, even with real blocks on disk.
        let data = build_file(*b"XAI\0", 1, 8000, 7, 0);
        let mut d = XaDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn out_size_ceil_divides_and_clamps_to_the_blocks_actually_present() {
        // 100 bytes / 30-byte stereo block = ceil(100/30) = 4 packets, even
        // though 20 blocks are physically present -- matches the reference
        // exactly (see the module doc's sweep table).
        let data = build_file(*b"XAI\0", 2, 22_050, 20, 100);
        let mut d = XaDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let mut count = 0;
        while d.read_packet().is_ok() {
            count += 1;
        }
        assert_eq!(count, 4);

        // An out_size demanding more blocks than actually exist clamps down
        // to what is present, discarding no whole block early.
        let data = build_file(*b"XAI\0", 2, 22_050, 5, out_size_for(2, 50));
        let mut d = XaDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let mut count = 0;
        while d.read_packet().is_ok() {
            count += 1;
        }
        assert_eq!(count, 5);
    }

    #[test]
    fn duration_reflects_the_file_length_not_out_size() {
        // Same fixture as the ceil/clamp test above: 4 packets emitted, but
        // duration_ts still reports the whole 20-block file's sample count
        // -- a real, measured disagreement in the reference itself.
        let data = build_file(*b"XAI\0", 2, 22_050, 20, 100);
        let d = XaDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams().first().unwrap().duration_ts, Some(20 * 28));
    }

    #[test]
    fn probe_checks_the_two_byte_magic_and_the_pcm_tag() {
        let data = build_file(*b"XAI\0", 1, 8000, 1, out_size_for(1, 1));
        assert_eq!(probe(&ProbeData::new(&data)), ProbeScore::MAGIC_CHECKED);
        assert_eq!(
            probe(&ProbeData::new(b"not xa at all...")),
            ProbeScore::NONE
        );
    }

    #[test]
    fn a_multichannel_file_is_rejected() {
        let data = build_file(*b"XAI\0", 4, 22_050, 1, out_size_for(4, 1));
        assert!(XaDemuxer::open(Box::new(MemorySource::new(data))).is_err());
    }
}
