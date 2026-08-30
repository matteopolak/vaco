//! `ALACSpecificConfig`: the 24-byte "magic cookie" that carries an ALAC
//! stream's decode parameters.
//!
//! Field layout verified against the real bytes `ffmpeg -c:a alac` writes
//! into an MP4 `alac` sample-entry box, not transcribed from a
//! specification alone (Apple never published one; this is a real, widely
//! interoperated-with wire format, measured directly).
//!
//! ```text
//! frameLength     (u32, BE)   samples per frame, before the final frame
//! compatibleVersion (u8)
//! bitDepth        (u8)
//! pb              (u8)   rice history mult
//! mb              (u8)   rice initial history
//! kb              (u8)   rice parameter limit
//! numChannels     (u8)
//! maxRun          (u16, BE)
//! maxFrameBytes   (u32, BE)   0 means unknown/unbounded
//! avgBitRate      (u32, BE)   0 means unknown/unbounded
//! sampleRate      (u32, BE)
//! ```

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, Parser};
use vaco_core::{Error, Result, Rational};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// Bytes in the config proper, once any container wrapper is stripped.
pub const LEN: usize = 24;

/// A parsed `ALACSpecificConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlacSpecificConfig {
    pub frame_length: u32,
    pub compatible_version: u8,
    pub bit_depth: u8,
    pub pb: u8,
    pub mb: u8,
    pub kb: u8,
    pub num_channels: u8,
    pub max_run: u16,
    pub max_frame_bytes: u32,
    pub avg_bit_rate: u32,
    pub sample_rate: u32,
}

impl AlacSpecificConfig {
    /// Parse an `ALACSpecificConfig` from a container's magic-cookie bytes.
    ///
    /// Reads the **last** [`LEN`] bytes of `data`, whatever precedes them:
    /// the config proper is a fixed 24 bytes, but different containers wrap
    /// it in a different amount of framing ahead of it — measured directly
    /// against a real `ffmpeg -c:a alac` MP4 file's nested `alac` box, which
    /// carries `[size][fourcc="alac"][version+flags][24-byte config]` (36
    /// bytes total), 4 bytes more than the bare config and 8 more than the
    /// version-plus-config shape some other muxers write. Anchoring on the
    /// end rather than assuming a fixed prefix handles all three without a
    /// muxer-specific branch.
    ///
    /// No primary specification exists for this format — Apple never
    /// published one — so this is measured directly against real encoder
    /// output, per D6/D7, rather than cited to a document.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when `data` is shorter than [`LEN`], or the
    /// sample rate or channel count is zero.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let Some(start) = data.len().checked_sub(LEN) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(body) = data.get(start..) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(frame_length) = body.get(0..4).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(&compatible_version) = body.get(4) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(&bit_depth) = body.get(5) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(&pb) = body.get(6) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(&mb) = body.get(7) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(&kb) = body.get(8) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(&num_channels) = body.get(9) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(max_run) = body.get(10..12).and_then(|s| <[u8; 2]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(max_frame_bytes) = body.get(12..16).and_then(|s| <[u8; 4]>::try_from(s).ok())
        else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(avg_bit_rate) = body.get(16..20).and_then(|s| <[u8; 4]>::try_from(s).ok())
        else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let Some(sample_rate) = body.get(20..24).and_then(|s| <[u8; 4]>::try_from(s).ok())
        else {
            return Err(Error::InvalidData("truncated ALACSpecificConfig"));
        };
        let sample_rate = u32::from_be_bytes(sample_rate);
        if num_channels == 0 || sample_rate == 0 {
            return Err(Error::InvalidData(
                "ALACSpecificConfig states zero channels or sample rate",
            ));
        }
        Ok(Self {
            frame_length: u32::from_be_bytes(frame_length),
            compatible_version,
            bit_depth,
            pb,
            mb,
            kb,
            num_channels,
            max_run: u16::from_be_bytes(max_run),
            max_frame_bytes: u32::from_be_bytes(max_frame_bytes),
            avg_bit_rate: u32::from_be_bytes(avg_bit_rate),
            sample_rate,
        })
    }

    /// Fold the config into the parameters a container reports.
    ///
    /// `sample_fmt` is `s16p`, measured against a real `-c:a alac` decode
    /// with `ffprobe 8.1`.
    #[must_use]
    pub fn to_codec_parameters(&self) -> CodecParameters {
        let mut params = CodecParameters::audio().with_codec(CodecId::Alac);
        if self.avg_bit_rate > 0 {
            params.bit_rate = Some(u64::from(self.avg_bit_rate));
        }
        params.audio = Some(AudioParameters {
            sample_rate: self.sample_rate,
            format: Some(vaco_sampfmt::SampleFmt::S16P),
            layout: Some(
                ChannelLayout::default_for(u32::from(self.num_channels))
                    .unwrap_or_else(|| ChannelLayout::unspecified(u32::from(self.num_channels))),
            ),
            bits_per_coded_sample: None,
            // `bitDepth` is a raw, attacker-controllable byte (0..=255) in
            // an `alac` sample-entry's magic cookie, but ALAC only ever
            // codes 16, 20, 24 or 32-bit samples — measured against every
            // `-sample_fmt` `ffmpeg -c:a alac` accepts. Reporting anything
            // else verbatim would put a fabricated value straight into
            // probe output, the same class of bug
            // `fuzz/fuzz_targets/registry_discovery.rs` found in JPEG's
            // `precision` field (crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0).
            bits_per_raw_sample: matches!(self.bit_depth, 16 | 20 | 24 | 32)
                .then_some(self.bit_depth),
            initial_padding: 0,
        });
        params
    }
}

/// Validates already-framed ALAC frames and reports stream parameters.
///
/// Like Opus, ALAC has **no in-band configuration at all** — every decode
/// parameter lives in the magic cookie the container carries — so
/// [`Parser::set_extradata`] is the only path that describes the stream.
/// Each `parse` call's input must be exactly one already-framed packet.
#[derive(Debug)]
pub struct AlacParser {
    config: Option<AlacSpecificConfig>,
    params: Option<CodecParameters>,
    budget: Budget,
    packets: u64,
}

impl AlacParser {
    /// A parser with no magic cookie yet.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            config: None,
            params: None,
            budget: Budget::new(limits),
            packets: 0,
        }
    }

    /// The magic cookie, once one has been supplied.
    #[must_use]
    pub const fn config(&self) -> Option<&AlacSpecificConfig> {
        self.config.as_ref()
    }

    /// Packets validated so far.
    #[must_use]
    pub const fn packets(&self) -> u64 {
        self.packets
    }
}

impl Parser for AlacParser {
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

    /// Read an `ALACSpecificConfig`.
    ///
    /// # Errors
    ///
    /// Whatever [`AlacSpecificConfig::parse`] returns.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        let config = AlacSpecificConfig::parse(extradata)?;
        self.params = Some(config.to_codec_parameters());
        self.config = Some(config);
        Ok(())
    }

    /// `frame_length / sample_rate`, a stream constant off the magic cookie.
    ///
    /// ALAC's own frame syntax states each frame's actual sample count
    /// in-band (the last frame of a stream is typically shorter), but
    /// reading it means parsing the ALAC frame header's Rice-coding
    /// parameters — decode-adjacent work this parse-only crate does not do.
    /// This is therefore a stream constant, right for every frame except
    /// possibly the last — the same shape `vaco-parse-aac`'s configured path
    /// already accepts for AAC. Named cut, not a silent approximation.
    fn packet_duration(&self, _packet: &[u8]) -> Option<Rational> {
        let config = self.config.as_ref()?;
        let samples = i32::try_from(config.frame_length).ok()?;
        let rate = i32::try_from(config.sample_rate).ok()?;
        if samples <= 0 || rate <= 0 {
            return None;
        }
        Some(Rational::new(samples, rate))
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    /// Byte-for-byte the config measured from a real `ffmpeg -c:a alac`
    /// 44.1 kHz stereo MP4 file's `alac` box, wrapper included: `[size=0x24]
    /// [fourcc="alac"] [version+flags=0] [24-byte config]`.
    fn wrapped_fixture() -> Vec<u8> {
        let cfg: [u8; LEN] = [
            0x00, 0x00, 0x10, 0x00, // frameLength = 4096
            0x00, // compatibleVersion
            0x10, // bitDepth = 16
            0x28, // pb = 40
            0x0a, // mb = 10
            0x0e, // kb = 14
            0x02, // numChannels = 2
            0x00, 0x00, // maxRun
            0x00, 0x00, 0x40, 0x04, // maxFrameBytes = 16388
            0x00, 0x15, 0x88, 0x80, // avgBitRate = 1411200
            0x00, 0x00, 0xac, 0x44, // sampleRate = 44100
        ];
        let mut wrapped = vec![0x00, 0x00, 0x00, 0x24];
        wrapped.extend_from_slice(b"alac");
        wrapped.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        wrapped.extend_from_slice(&cfg);
        wrapped
    }

    #[test]
    fn parses_wrapped_and_bare_forms_identically() {
        let wrapped = wrapped_fixture();
        let bare = &wrapped[wrapped.len() - LEN..];
        let from_wrapped = AlacSpecificConfig::parse(&wrapped).expect("valid wrapped cookie");
        let from_bare = AlacSpecificConfig::parse(bare).expect("valid bare cookie");
        assert_eq!(from_wrapped, from_bare);
        assert_eq!(from_wrapped.frame_length, 4096);
        assert_eq!(from_wrapped.bit_depth, 16);
        assert_eq!(from_wrapped.num_channels, 2);
        assert_eq!(from_wrapped.sample_rate, 44_100);
        assert_eq!(from_wrapped.avg_bit_rate, 1_411_200);
    }

    #[test]
    fn a_boxed_parser_via_extradata_describes_the_stream_and_states_a_duration() {
        let mut parser = AlacParser::new(Limits::strict());
        parser.set_extradata(&wrapped_fixture()).expect("valid cookie");
        let params = parser.parameters().expect("described");
        assert_eq!(params.codec_id, Some(CodecId::Alac));
        let audio = params.audio.as_ref().expect("audio parameters");
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(audio.format, Some(vaco_sampfmt::SampleFmt::S16P));
        let duration = parser.packet_duration(&[]).expect("a stream constant");
        assert_eq!(duration, Rational::new(4096, 44_100));
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..48usize {
            let data = vec![0xffu8; len];
            let _ = AlacSpecificConfig::parse(&data);
        }
    }

    #[test]
    fn a_zero_channel_count_is_rejected_not_a_panic() {
        let mut data = wrapped_fixture();
        let idx = data.len() - LEN + 9; // numChannels byte
        if let Some(b) = data.get_mut(idx) {
            *b = 0;
        }
        assert!(AlacSpecificConfig::parse(&data).is_err());
    }

    /// `bitDepth` is a raw byte an attacker fully controls; ALAC only codes
    /// 16/20/24/32-bit samples, so any other value must not reach
    /// `bits_per_raw_sample` — the same class of bug
    /// `fuzz/fuzz_targets/registry_discovery.rs` found in JPEG's `precision`
    /// (crash-b105f0b6cfac5b713adef84be6cdd3c1d57599a0), audited here rather
    /// than found by the fuzzer directly.
    #[test]
    fn an_implausible_bit_depth_is_not_reported_but_still_parses() {
        let mut data = wrapped_fixture();
        let idx = data.len() - LEN + 5; // bitDepth byte
        let Some(b) = data.get_mut(idx) else {
            unreachable!("fixture is long enough");
        };
        *b = 164;
        let cfg = AlacSpecificConfig::parse(&data).expect("still a structurally valid cookie");
        assert_eq!(cfg.bit_depth, 164, "the raw field is preserved");
        let params = cfg.to_codec_parameters();
        let audio = params.audio.expect("audio parameters");
        assert_eq!(
            audio.bits_per_raw_sample, None,
            "an implausible bit depth must not reach reported metadata"
        );
    }

    #[test]
    fn every_real_alac_bit_depth_is_reported() {
        for depth in [16u8, 20, 24, 32] {
            let mut data = wrapped_fixture();
            let idx = data.len() - LEN + 5;
            if let Some(b) = data.get_mut(idx) {
                *b = depth;
            }
            let cfg = AlacSpecificConfig::parse(&data).expect("valid cookie");
            let audio = cfg.to_codec_parameters().audio.expect("audio parameters");
            assert_eq!(audio.bits_per_raw_sample, Some(depth));
        }
    }
}
