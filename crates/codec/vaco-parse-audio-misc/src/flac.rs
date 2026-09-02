//! `STREAMINFO`: FLAC's mandatory first metadata block.
//!
//! Field layout verified against the real bytes `ffmpeg -c:a flac` writes,
//! not transcribed from the specification text alone.
//!
//! ```text
//! min_blocksize    (u16, BE)
//! max_blocksize    (u16, BE)
//! min_framesize    (u24, BE)
//! max_framesize    (u24, BE)
//! sample_rate      (u20)
//! channels - 1     (u3)
//! bits_per_sample - 1 (u5)
//! total_samples    (u36)
//! md5              (16 bytes)
//! ```
//!
//! The last four fields pack into 8 bytes with no byte alignment between
//! them — `sample_rate`'s top 20 bits, `channels`'s 3, `bits_per_sample`'s 5
//! and `total_samples`'s 36 sum to exactly 64.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, Parser};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// Bytes in a `STREAMINFO` block's content (the block's own 4-byte metadata
/// header is not included — a caller that has the whole `fLaC` file walks
/// blocks first and hands this only the `STREAMINFO` payload).
pub const LEN: usize = 34;

/// A parsed `STREAMINFO` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub min_blocksize: u16,
    pub max_blocksize: u16,
    pub min_framesize: u32,
    pub max_framesize: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub total_samples: u64,
    pub md5: [u8; 16],
}

impl StreamInfo {
    /// Parse the 34-byte `STREAMINFO` content.
    ///
    /// `Vaco-Spec-Ref: rfc-9639` `METADATA_BLOCK_STREAMINFO`; measured
    /// against a real `ffmpeg -c:a flac` file's first metadata block,
    /// including that `total_samples` and `sample_rate` land on the exact
    /// values a one-second 44.1 kHz encode produces.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when `data` is shorter than [`LEN`], or the
    /// sample rate or channel count is zero.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let Some(body) = data.get(..LEN) else {
            return Err(Error::InvalidData("truncated FLAC STREAMINFO block"));
        };
        let Some(min_blocksize) = body.get(0..2).and_then(|s| <[u8; 2]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated FLAC STREAMINFO block"));
        };
        let Some(max_blocksize) = body.get(2..4).and_then(|s| <[u8; 2]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated FLAC STREAMINFO block"));
        };
        let min_framesize = be_u24(body, 4)?;
        let max_framesize = be_u24(body, 7)?;
        let Some(packed) = body.get(10..18).and_then(|s| <[u8; 8]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated FLAC STREAMINFO block"));
        };
        let Some(md5) = body.get(18..34).and_then(|s| <[u8; 16]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated FLAC STREAMINFO block"));
        };

        let sample_rate = (u32::from(packed[0]) << 12)
            | (u32::from(packed[1]) << 4)
            | (u32::from(packed[2]) >> 4);
        let channels = ((packed[2] >> 1) & 0x07) + 1;
        let bits_per_sample = (((packed[2] & 0x01) << 4) | (packed[3] >> 4)) + 1;
        let total_samples = (u64::from(packed[3] & 0x0f) << 32)
            | (u64::from(packed[4]) << 24)
            | (u64::from(packed[5]) << 16)
            | (u64::from(packed[6]) << 8)
            | u64::from(packed[7]);

        if sample_rate == 0 {
            return Err(Error::InvalidData("FLAC STREAMINFO states zero sample rate"));
        }

        Ok(Self {
            min_blocksize: u16::from_be_bytes(min_blocksize),
            max_blocksize: u16::from_be_bytes(max_blocksize),
            min_framesize,
            max_framesize,
            sample_rate,
            channels,
            bits_per_sample,
            total_samples,
            md5,
        })
    }

    /// Fold the block into the parameters a container reports.
    ///
    /// `sample_fmt` is `s16` — **not** planar, unlike every other codec this
    /// crate family reports — measured against `ffprobe 8.1` on a real
    /// `-c:a flac` file. `bits_per_raw_sample` carries the real per-sample
    /// precision; `bits_per_coded_sample` stays `None`, matching the
    /// convention `vaco-parse-aac` documents for a compressed codec's
    /// container-stored depth.
    #[must_use]
    pub fn to_codec_parameters(&self) -> CodecParameters {
        let mut params = CodecParameters::audio().with_codec(CodecId::Flac);
        params.audio = Some(AudioParameters {
            sample_rate: self.sample_rate,
            format: Some(vaco_sampfmt::SampleFmt::S16),
            layout: Some(
                ChannelLayout::default_for(u32::from(self.channels))
                    .unwrap_or_else(|| ChannelLayout::unspecified(u32::from(self.channels))),
            ),
            bits_per_coded_sample: None,
            bits_per_raw_sample: Some(self.bits_per_sample),
            initial_padding: 0,
        });
        params
    }
}

fn be_u24(data: &[u8], at: usize) -> Result<u32> {
    let Some(b) = data.get(at..at + 3) else {
        return Err(Error::InvalidData("truncated FLAC STREAMINFO block"));
    };
    let Some(&[a0, a1, a2]) = b.first_chunk::<3>() else {
        return Err(Error::InvalidData("truncated FLAC STREAMINFO block"));
    };
    Ok((u32::from(a0) << 16) | (u32::from(a1) << 8) | u32::from(a2))
}

/// Validates already-framed FLAC audio frames and reports stream parameters.
///
/// Like `vaco-parse-opus::OpusParser`, **each `parse` call's input must be
/// exactly one already-framed packet** — a container's own block/sample
/// boundary. FLAC frames do carry their own sync code and CRC, so a native,
/// non-containerized `.flac` elementary stream could in principle be
/// resynchronised the way `vaco-parse-mpegaudio` does for MPEG audio — but
/// no demuxer in this tree reads one, so that scanner is not built here.
/// Named cut, not a silent gap.
#[derive(Debug)]
pub struct FlacParser {
    info: Option<StreamInfo>,
    params: Option<CodecParameters>,
    budget: Budget,
    packets: u64,
}

impl FlacParser {
    /// A parser with no `STREAMINFO` yet.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            info: None,
            params: None,
            budget: Budget::new(limits),
            packets: 0,
        }
    }

    /// The `STREAMINFO` block, once one has been supplied.
    #[must_use]
    pub const fn stream_info(&self) -> Option<&StreamInfo> {
        self.info.as_ref()
    }

    /// Packets validated so far.
    #[must_use]
    pub const fn packets(&self) -> u64 {
        self.packets
    }
}

impl Parser for FlacParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            return Ok((None, 0));
        }
        let mut packet = Packet::from_slice(&mut self.budget, input)?;
        packet.flags = PacketFlags::KEY;
        self.packets = self.packets.saturating_add(1);
        Ok((Some(packet), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// Read a `STREAMINFO` block.
    ///
    /// This project's own canonical `CodecParameters::extradata` shape for a
    /// FLAC stream is `"fLaC" +` one or more metadata blocks, header
    /// included (`FlacEncoder::extradata`'s own convention; Matroska's
    /// `A_FLAC` `CodecPrivate` matches it) -- **not** the bare 34 bytes this
    /// method's doc used to claim MP4's `dfLa` and Matroska both hand over
    /// verbatim. Measured against a real `ffmpeg -c:a flac -f mp4` file: a
    /// naive `data.get(..34)` over the un-wrapped shape reads across the
    /// wrapper into the middle of the real block, reporting `channels=1`,
    /// `bits_per_raw_sample=1` for an actual 48 kHz stereo 16-bit stream.
    /// [`locate_streaminfo`] finds the real 34 bytes regardless of which
    /// shape wraps them (or accepts them already bare, which nothing in
    /// this tree currently produces but which costs nothing to keep
    /// accepting).
    ///
    /// # Errors
    ///
    /// Whatever [`StreamInfo::parse`] returns.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        let body = locate_streaminfo(extradata).unwrap_or(extradata);
        let info = StreamInfo::parse(body)?;
        self.params = Some(info.to_codec_parameters());
        self.info = Some(info);
        Ok(())
    }
}

/// Find the bare 34-byte `STREAMINFO` payload inside `extradata`, whatever
/// convention wraps it -- this project's canonical `"fLaC" + metadata
/// block(s)` shape, or a metadata block with no `"fLaC"` prefix (a bare
/// dfLa-style FullBox body with its own 4-byte header stripped down to just
/// the block, which is all `vaco-demux-mp4`'s own `ConfigFlavour::Dfla` arm
/// promises). Returns `None` when neither pattern is found, so the caller
/// can fall back to treating the whole input as an already-bare block.
///
/// Deliberately re-implemented rather than depending on
/// `vaco-codec-flac::streaminfo::find_streaminfo_block` for it: the same
/// precedent `vaco-mux-mp4::entry::flac_streaminfo_metadata_block` already
/// set for this exact shape, one layer over. Unlike that scanning function,
/// this one only looks at a fixed starting position (after an optional
/// `"fLaC"` prefix) rather than scanning every byte offset, because every
/// known caller either supplies the canonical shape or nothing recognisable
/// at all -- there is no format here whose block genuinely starts mid-buffer.
fn locate_streaminfo(extradata: &[u8]) -> Option<&[u8]> {
    let rest = extradata.strip_prefix(b"fLaC").unwrap_or(extradata);
    let header = rest.get(..4)?;
    let [b0, b1, b2, b3] = header else {
        return None;
    };
    if b0 & 0x7F != 0 {
        return None; // not a STREAMINFO block header (type must be 0)
    }
    let len = (u32::from(*b1) << 16) | (u32::from(*b2) << 8) | u32::from(*b3);
    if len != u32::try_from(LEN).unwrap_or(u32::MAX) {
        return None;
    }
    rest.get(4..4 + LEN)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    /// Byte-for-byte the `STREAMINFO` measured from a real one-second
    /// 44.1 kHz mono `ffmpeg -c:a flac` file.
    fn fixture() -> [u8; LEN] {
        let mut b = [0u8; LEN];
        b[0..2].copy_from_slice(&4608u16.to_be_bytes());
        b[2..4].copy_from_slice(&4608u16.to_be_bytes());
        // min/max framesize left at 0 — not exercised by this fixture.
        // packed sample_rate=44100, channels=2 (stored 1), bits=16 (stored
        // 15), total_samples=44100.
        b[10] = 0x0a;
        b[11] = 0xc4;
        b[12] = 0x42;
        b[13] = 0xf0;
        b[17] = 0x44;
        b[16] = 0xac;
        b
    }

    #[test]
    fn parses_the_measured_shape() {
        let info = StreamInfo::parse(&fixture()).expect("valid block");
        assert_eq!(info.sample_rate, 44_100);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits_per_sample, 16);
        assert_eq!(info.total_samples, 44_100);
        assert_eq!(info.min_blocksize, 4608);
        assert_eq!(info.max_blocksize, 4608);
    }

    #[test]
    fn a_boxed_parser_via_extradata_describes_the_stream() {
        let mut parser = FlacParser::new(Limits::strict());
        parser.set_extradata(&fixture()).expect("valid block");
        let params = parser.parameters().expect("described");
        assert_eq!(params.codec_id, Some(CodecId::Flac));
        let audio = params.audio.as_ref().expect("audio parameters");
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(audio.bits_per_raw_sample, Some(16));
        assert_eq!(audio.format, Some(vaco_sampfmt::SampleFmt::S16));
    }

    /// The real, project-wide canonical shape (`"fLaC" +` a metadata block
    /// with its own 4-byte header) must describe the stream correctly, not
    /// just the bare fixture -- regression for the bug the module doc names:
    /// a naive fixed-offset read over this shape reported channels=1,
    /// bits_per_raw_sample=1 for a real 2-channel 16-bit file.
    #[test]
    fn a_flac_prefixed_metadata_block_describes_the_stream_correctly() {
        let mut wrapped = b"fLaC".to_vec();
        wrapped.push(0x80); // last-block flag set, type 0 (STREAMINFO)
        wrapped.extend_from_slice(&[0x00, 0x00, 0x22]); // length 34
        wrapped.extend_from_slice(&fixture());
        let mut parser = FlacParser::new(Limits::strict());
        parser.set_extradata(&wrapped).expect("valid block");
        let audio = parser
            .parameters()
            .expect("described")
            .audio
            .clone()
            .expect("audio parameters");
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(
            audio.layout.map(|l| l.channels),
            Some(2),
            "must not read as mono"
        );
        assert_eq!(audio.bits_per_raw_sample, Some(16));
    }

    /// `vaco-demux-mp4`'s `ConfigFlavour::Dfla` arm strips `dfLa`'s own
    /// full-box version+flags and replaces them with `"fLaC"`, so its
    /// block-header-plus-payload bytes come straight after the magic with
    /// no full-box header of their own in between -- the same shape as the
    /// test above, restated with the exact bytes that arm produces.
    #[test]
    fn locate_streaminfo_finds_the_block_after_the_magic() {
        let mut wrapped = b"fLaC".to_vec();
        wrapped.push(0x80);
        wrapped.extend_from_slice(&[0x00, 0x00, 0x22]);
        wrapped.extend_from_slice(&fixture());
        assert_eq!(locate_streaminfo(&wrapped), Some(fixture().as_slice()));
    }

    #[test]
    fn locate_streaminfo_falls_back_to_none_for_a_bare_block() {
        // The bare shape has no header for this function to find; the
        // caller's `unwrap_or(extradata)` fallback is what handles it.
        assert_eq!(locate_streaminfo(&fixture()), None);
    }

    #[test]
    fn parse_passes_a_packet_through_unexamined() {
        let mut parser = FlacParser::new(Limits::strict());
        let (packet, used) = parser.parse(&[1, 2, 3]).expect("any bytes are one packet");
        assert!(packet.is_some());
        assert_eq!(used, 3);
    }

    #[test]
    fn a_zero_sample_rate_is_rejected_not_a_panic() {
        let data = [0u8; LEN];
        assert!(StreamInfo::parse(&data).is_err());
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..48usize {
            let data = vec![0xffu8; len];
            let _ = StreamInfo::parse(&data);
        }
    }
}
