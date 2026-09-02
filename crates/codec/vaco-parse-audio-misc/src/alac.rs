//! `ALACSpecificConfig`: the 24-byte "magic cookie" that carries an ALAC
//! stream's decode parameters, plus the optional `ALACChannelLayoutInfo`
//! extension.
//!
//! # One parser, not two
//!
//! This used to be duplicated: this crate had its own `AlacSpecificConfig`
//! (`CodecParameters` reporting only, no channel-layout-info support), and
//! `vaco-codec-alac` had a second, independent copy (`AlacCookie`, with
//! channel-layout-info support and this crate's own encoder-only methods).
//! `vaco-codec-alac` now depends on this crate and reuses these types
//! directly (`crate::cookie` re-exports them), matching the precedent
//! `vaco-codec-opus` already set for `vaco-parse-opus`.
//!
//! Consolidating exposed a real disagreement between the two: this crate's
//! own `parse` took the *last* [`LEN`] bytes of whatever it was given, which
//! is correct for a bare config or one with only a fixed-size prefix ahead
//! of it, but silently reads the wrong 24 bytes for `vaco-codec-alac`'s own
//! `frma`-wrapped "Compatibility" shape *when that shape is followed by an
//! optional trailing [`AlacChannelLayoutInfo`]* — `vaco-codec-alac`'s own
//! parser, in turn, always read from the *front*, which is right for that
//! wrapper but wrong for this crate's own `[size]["alac"][version+flags]
//! [24-byte config]` shape (see `strip_full_box_wrapper`'s doc). Neither bug
//! was reachable through a real pipeline today (see the next section), but
//! both were latent. [`AlacCookie::parse`] below resolves this by explicit
//! magic-byte detection rather than a length guess: a recognised wrapper
//! (`frma`/`alac` Compatibility, or a raw `[size]["alac"][flags]` box) is
//! stripped by its own known, fixed size, front-anchoring what remains;
//! anything else is treated as front-anchored already (bare config,
//! optionally followed by an `ALACChannelLayoutInfo`). This is safe for
//! every shape measured below, and unlike "last N bytes" is not fooled by a
//! trailing extension block on any of them.
//!
//! # What a real demuxer actually hands over today
//!
//! Measured directly rather than assumed: `vaco-demux-mp4`'s own
//! `track.rs` already strips its `alac` box down to the bare 24-byte config
//! before ever calling [`Parser::set_extradata`] — `CodecConfig::data` for a
//! full box carries 4 bytes of version+flags ahead of the record, and an
//! earlier, real bug (see that file's own comment) came from handing those
//! 28 bytes over unstripped. `vaco-format-audio-simple`'s CAF demuxer skips
//! the `kuki` chunk outright and never calls this parser at all. So today,
//! in this tree, [`AlacCookie::parse`] only ever receives a bare 24-byte
//! config in practice — the wrapper-detection below is real, tested,
//! spec-following defensive code for whichever caller eventually hands over
//! something else, not a currently-exercised path.
//!
//! Checked directly against the reference (`ffmpeg 9.0.1 -c:a alac`, mono,
//! stereo and 5.1 fixtures, `-f caf`): the `kuki` chunk is the `frma`
//! Compatibility wrapper every time, 48 bytes exactly (`[Format Atom: size
//! =12,"frma","alac"][ALAC Specific Info header: size=36,"alac",flags=0]
//! [24-byte config]`) — the reference's own encoder **never** emits a
//! trailing `ALACChannelLayoutInfo`, at any channel count tried, so this
//! project has no real-encoder fixture of that specific shape. It is still
//! handled (Apple's own `ALACMagicCookieDescription.txt` states it as
//! legal, and other encoders — `afconvert`, iTunes — do write one for
//! layouts a bare channel count cannot name), just not exercised by the one
//! oracle D6 permits reading.
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
use vaco_core::{Error, Rational, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// Bytes in the config proper, once any wrapper is stripped.
pub const LEN: usize = 24;

/// Bytes in an `ALACChannelLayoutInfo` block.
const CHANNEL_LAYOUT_INFO_LEN: usize = 24;

/// `ALACChannelLayoutInfo`'s tags are `(family << 16) | channel_count`, per
/// `ALACMagicCookieDescription.txt`'s enum. Only mono/stereo are given names
/// here because [`ChannelLayout`] has no constructor this crate exercises
/// for the others yet (3.0B/4.0B/5.0D/5.1D/6.1/7.1B) — the numeric tags
/// below are still exactly what the spec states, so a future patch adding
/// those layouts only needs new `ChannelLayout` values, not new tag numbers.
const K_MONO: u32 = (100 << 16) | 1;
const K_STEREO: u32 = (101 << 16) | 2;

/// A parsed `ALACSpecificConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlacSpecificConfig {
    pub frame_length: u32,
    pub compatible_version: u8,
    pub bit_depth: u8,
    /// Tuning parameter; carried through but not required by
    /// `vaco-codec-alac`'s own entropy coder (see that crate's `rice.rs` doc
    /// comment for why).
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
    /// Parse a bare `ALACSpecificConfig` starting at `data[0]` — no wrapper
    /// detection, exactly [`LEN`] bytes read from the front. The public
    /// entry point for a whole cookie (wrapper included) is
    /// [`AlacCookie::parse`]; this is `pub(crate)` because every real caller
    /// wants at least the channel-layout handling that function adds.
    fn parse_bare(b: &[u8]) -> Result<Self> {
        let Some(body) = b.get(..LEN) else {
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
        let Some(sample_rate) = body.get(20..24).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
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

    /// Parse an `ALACSpecificConfig` from a container's magic-cookie bytes,
    /// discarding any `ALACChannelLayoutInfo` extension. Most callers want
    /// [`AlacCookie::parse`] instead, which keeps it.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when `data` is shorter than a recognised
    /// wrapper needs, or the sample rate or channel count is zero.
    pub fn parse(data: &[u8]) -> Result<Self> {
        AlacCookie::parse(data).map(|cookie| cookie.config)
    }

    /// A [`ChannelLayout`] for this config alone, per
    /// `ALACMagicCookieDescription.txt`'s "if the channel layout is absent"
    /// rule: mono, stereo L/R, or unspecified beyond that. Prefer
    /// [`AlacCookie::layout`] when an explicit `ALACChannelLayoutInfo` might
    /// be present — it overrides this when it names a layout this crate
    /// recognises.
    #[must_use]
    pub fn default_layout(&self) -> ChannelLayout {
        match self.num_channels {
            1 => ChannelLayout::MONO,
            2 => ChannelLayout::STEREO,
            n => ChannelLayout::unspecified(u32::from(n)),
        }
    }

    /// This crate's own encoder default: `pb`/`mb`/`kb` from
    /// `vaco_codec_alac::rice::{PB0,MB0,KB0}` (`AlacEncoder::send_frame`
    /// never uses any other triple), `frame_length` the ALAC-conventional
    /// 4096 (informational only for that encoder: every packet's own
    /// element header states its real sample count explicitly via
    /// `partialFrame`/`numSamples`, so nothing there ever needs to fall back
    /// to it), and `max_frame_bytes`/`avg_bit_rate` left `0`
    /// ("unknown"/VBR), matching a real `ffmpeg -c:a alac` cookie's own
    /// pre-encode-statistics state.
    ///
    /// Takes `pb`/`mb`/`kb` as parameters rather than reaching into
    /// `vaco-codec-alac`'s own constants directly, since this crate is the
    /// lower layer and does not depend on the encoder that uses this.
    #[must_use]
    pub const fn for_encode(sample_rate: u32, num_channels: u8, bit_depth: u8, pb: u8, mb: u8, kb: u8) -> Self {
        Self {
            frame_length: 4096,
            compatible_version: 0,
            bit_depth,
            pb,
            mb,
            kb,
            num_channels,
            max_run: 0,
            max_frame_bytes: 0,
            avg_bit_rate: 0,
            sample_rate,
        }
    }

    /// Serialize the bare 24-byte `ALACSpecificConfig` (no compatibility
    /// wrapper, no `ALACChannelLayoutInfo` — [`AlacSpecificConfig::default_layout`]
    /// already recovers mono/stereo from `num_channels` alone, so
    /// `vaco-codec-alac`'s encoder never needs the optional channel-layout
    /// extension).
    #[must_use]
    pub fn write_bare(&self) -> [u8; LEN] {
        let mut out = [0u8; LEN];
        out[0..4].copy_from_slice(&self.frame_length.to_be_bytes());
        out[4] = self.compatible_version;
        out[5] = self.bit_depth;
        out[6] = self.pb;
        out[7] = self.mb;
        out[8] = self.kb;
        out[9] = self.num_channels;
        out[10..12].copy_from_slice(&self.max_run.to_be_bytes());
        out[12..16].copy_from_slice(&self.max_frame_bytes.to_be_bytes());
        out[16..20].copy_from_slice(&self.avg_bit_rate.to_be_bytes());
        out[20..24].copy_from_slice(&self.sample_rate.to_be_bytes());
        out
    }

    /// Fold the config alone into the parameters a container reports —
    /// [`AlacCookie::to_codec_parameters`] additionally honours an explicit
    /// `ALACChannelLayoutInfo` when present.
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
            layout: Some(self.default_layout()),
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

/// The optional `ALACChannelLayoutInfo` extension, when a cookie carries one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlacChannelLayoutInfo {
    pub channel_layout_tag: u32,
}

impl AlacChannelLayoutInfo {
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < CHANNEL_LAYOUT_INFO_LEN {
            return None;
        }
        if b.get(4..8) != Some(b"chan".as_slice()) {
            return None;
        }
        let tag = b.get(12..16).and_then(|s| <[u8; 4]>::try_from(s).ok())?;
        Some(Self {
            channel_layout_tag: u32::from_be_bytes(tag),
        })
    }

    /// The documented tags this crate recognises, mapped to a concrete
    /// [`ChannelLayout`]. `None` for any tag not in that table (multichannel
    /// beyond what `ReadMe.txt` lists is not implemented).
    #[must_use]
    pub fn layout(self) -> Option<ChannelLayout> {
        if self.channel_layout_tag == K_MONO {
            Some(ChannelLayout::MONO)
        } else if self.channel_layout_tag == K_STEREO {
            Some(ChannelLayout::STEREO)
        } else {
            None
        }
    }
}

/// A parsed magic cookie: the mandatory config, plus a channel layout if the
/// cookie states one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlacCookie {
    pub config: AlacSpecificConfig,
    pub channel_layout: Option<AlacChannelLayoutInfo>,
}

impl AlacCookie {
    /// The layout to use: the explicit `ALACChannelLayoutInfo` tag if
    /// present and recognised, otherwise [`AlacSpecificConfig::default_layout`].
    #[must_use]
    pub fn layout(&self) -> ChannelLayout {
        self.channel_layout
            .and_then(AlacChannelLayoutInfo::layout)
            .unwrap_or_else(|| self.config.default_layout())
    }

    /// Fold the whole cookie into the parameters a container reports —
    /// like [`AlacSpecificConfig::to_codec_parameters`], but honouring an
    /// explicit `ALACChannelLayoutInfo` when this cookie carries one.
    #[must_use]
    pub fn to_codec_parameters(&self) -> CodecParameters {
        let mut params = self.config.to_codec_parameters();
        if let Some(audio) = params.audio.as_mut() {
            audio.layout = Some(self.layout());
        }
        params
    }

    /// Parse a magic cookie as a container hands it over: a bare 24 (or
    /// 24+24) byte config, the MP4 `alac` box's own inner shape (with its
    /// box header still attached — see [`strip_full_box_wrapper`]), or the
    /// `frma`/`alac` "Compatibility" wrapper (see [`strip_frma_wrapper`]).
    /// See this module's own doc for why detection is by explicit magic
    /// bytes rather than a length heuristic, and for what a real demuxer in
    /// this tree actually hands over today (a bare config, always).
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `data` is too short for the shape it
    /// matches (or, absent a recognised wrapper, too short to hold even the
    /// bare config), or states zero channels or sample rate.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if let Some(body) = strip_frma_wrapper(data) {
            let config = AlacSpecificConfig::parse_bare(body)?;
            let channel_layout = body.get(LEN..).and_then(AlacChannelLayoutInfo::parse);
            return Ok(Self {
                config,
                channel_layout,
            });
        }
        if let Some(body) = strip_full_box_wrapper(data) {
            let config = AlacSpecificConfig::parse_bare(body)?;
            return Ok(Self {
                config,
                channel_layout: None,
            });
        }
        let config = AlacSpecificConfig::parse_bare(data)?;
        let channel_layout = data.get(LEN..).and_then(AlacChannelLayoutInfo::parse);
        Ok(Self {
            config,
            channel_layout,
        })
    }
}

/// The `frma`/`alac` "Compatibility" wrapper
/// (`ALACMagicCookieDescription.txt`'s second on-disk shape): `[Format
/// Atom: size=12,"frma","alac"][ALAC Specific Info header: size=36,"alac",
/// flags=0][24-byte config][optional 24-byte ALACChannelLayoutInfo]`.
/// Returns the bytes from the config onward (so a caller can still look for
/// a trailing channel-layout block), or `None` if `data` does not start
/// with this wrapper's magic.
fn strip_frma_wrapper(data: &[u8]) -> Option<&[u8]> {
    let is_frma =
        data.get(4..8) == Some(b"frma".as_slice()) && data.get(8..12) == Some(b"alac".as_slice());
    if !is_frma {
        return None;
    }
    // Format Atom (12) + ALAC Specific Info header (12: size, "alac", flags).
    data.get(24..)
}

/// The MP4 `alac` box's own inner shape, still carrying its own box header:
/// `[size=36]["alac"][version+flags=0][24-byte config]`. Distinct from
/// `vaco-format-isom::stsd::CodecConfig::data`, which already has the outer
/// 8 bytes stripped by the generic box walk — a caller with *that* slice
/// has a 4-byte version+flags directly followed by the 24-byte config, no
/// magic of its own to detect, and is expected to strip those 4 bytes
/// itself before calling in (exactly what `vaco-demux-mp4::track.rs` does,
/// and why: a length-based guess at this shape is not safe to make inside
/// an ALAC-specific parser once a trailing `ALACChannelLayoutInfo` is
/// possible on the *other* shapes this function does not match).
fn strip_full_box_wrapper(data: &[u8]) -> Option<&[u8]> {
    let is_full_box =
        data.get(0..4) == Some(&(u32::try_from(LEN + 12).unwrap_or(0)).to_be_bytes())
            && data.get(4..8) == Some(b"alac".as_slice());
    if !is_full_box {
        return None;
    }
    data.get(12..12 + LEN)
}

/// Validates already-framed ALAC frames and reports stream parameters.
///
/// Like Opus, ALAC has **no in-band configuration at all** — every decode
/// parameter lives in the magic cookie the container carries — so
/// [`Parser::set_extradata`] is the only path that describes the stream.
/// Each `parse` call's input must be exactly one already-framed packet.
#[derive(Debug)]
pub struct AlacParser {
    cookie: Option<AlacCookie>,
    params: Option<CodecParameters>,
    budget: Budget,
    packets: u64,
}

impl AlacParser {
    /// A parser with no magic cookie yet.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            cookie: None,
            params: None,
            budget: Budget::new(limits),
            packets: 0,
        }
    }

    /// The config half of the magic cookie, once one has been supplied.
    #[must_use]
    pub fn config(&self) -> Option<&AlacSpecificConfig> {
        self.cookie.as_ref().map(|c| &c.config)
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

    /// Read a magic cookie, honouring an explicit `ALACChannelLayoutInfo`
    /// when the container's cookie carries one.
    ///
    /// # Errors
    ///
    /// Whatever [`AlacCookie::parse`] returns.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        let cookie = AlacCookie::parse(extradata)?;
        self.params = Some(cookie.to_codec_parameters());
        self.cookie = Some(cookie);
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
        let config = self.config()?;
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
        let mut wrapped = vec![0x00, 0x00, 0x00, 0x24];
        wrapped.extend_from_slice(b"alac");
        wrapped.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        wrapped.extend_from_slice(&bare_fixture());
        wrapped
    }

    const fn bare_fixture() -> [u8; LEN] {
        [
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
        ]
    }

    /// Byte-for-byte the `kuki` chunk measured from a real `ffmpeg 9.0.1
    /// -c:a alac -f caf` mono output file, at every channel count tried
    /// (mono/stereo/5.1): the `frma`/`alac` "Compatibility" wrapper, 48
    /// bytes, no trailing `ALACChannelLayoutInfo` — see this module's own
    /// doc for why that specific tail is untested against the reference.
    fn frma_fixture(config: [u8; LEN]) -> Vec<u8> {
        let mut wrapped = vec![0x00, 0x00, 0x00, 0x0c];
        wrapped.extend_from_slice(b"frma");
        wrapped.extend_from_slice(b"alac");
        wrapped.extend_from_slice(&[0x00, 0x00, 0x00, 0x24]);
        wrapped.extend_from_slice(b"alac");
        wrapped.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        wrapped.extend_from_slice(&config);
        wrapped
    }

    fn channel_layout_info(tag: u32) -> Vec<u8> {
        let mut info = Vec::new();
        info.extend_from_slice(&24u32.to_be_bytes());
        info.extend_from_slice(b"chan");
        info.extend_from_slice(&0u32.to_be_bytes());
        info.extend_from_slice(&tag.to_be_bytes());
        info.extend_from_slice(&0u32.to_be_bytes());
        info.extend_from_slice(&0u32.to_be_bytes());
        info
    }

    #[test]
    fn parses_wrapped_and_bare_forms_identically() {
        let wrapped = wrapped_fixture();
        let bare = bare_fixture();
        let from_wrapped = AlacSpecificConfig::parse(&wrapped).expect("valid wrapped cookie");
        let from_bare = AlacSpecificConfig::parse(&bare).expect("valid bare cookie");
        assert_eq!(from_wrapped, from_bare);
        assert_eq!(from_wrapped.frame_length, 4096);
        assert_eq!(from_wrapped.bit_depth, 16);
        assert_eq!(from_wrapped.num_channels, 2);
        assert_eq!(from_wrapped.sample_rate, 44_100);
        assert_eq!(from_wrapped.avg_bit_rate, 1_411_200);
    }

    /// The measured-real `frma`-wrapped shape (see `frma_fixture`'s own
    /// doc) parses to the same config as the bare and MP4-box forms.
    #[test]
    fn a_frma_wrapped_cookie_matches_the_bare_config() {
        let cookie = AlacCookie::parse(&frma_fixture(bare_fixture())).expect("valid frma cookie");
        assert_eq!(cookie.config, AlacSpecificConfig::parse(&bare_fixture()).expect("valid bare config"));
        assert!(cookie.channel_layout.is_none());
    }

    /// The regression this consolidation exists for: a trailing
    /// `ALACChannelLayoutInfo` after a `frma`-wrapped config must not be
    /// misread as (part of) the config -- the defect a "last `LEN` bytes"
    /// heuristic has on this exact shape.
    #[test]
    fn a_frma_wrapped_cookie_with_trailing_channel_layout_info_is_not_corrupted() {
        let mut data = frma_fixture(bare_fixture());
        data.extend_from_slice(&channel_layout_info((101 << 16) | 2)); // stereo
        let cookie = AlacCookie::parse(&data).expect("valid cookie");
        assert_eq!(cookie.config, AlacSpecificConfig::parse(&bare_fixture()).expect("valid bare config"));
        assert_eq!(cookie.layout(), ChannelLayout::STEREO);
    }

    /// The same regression, without a `frma` wrapper at all -- a bare
    /// config immediately followed by channel-layout-info must also parse
    /// correctly, since nothing requires the wrapper to be present for the
    /// extension to be.
    #[test]
    fn a_bare_cookie_with_trailing_channel_layout_info_is_not_corrupted() {
        let mut data = bare_fixture().to_vec();
        data.extend_from_slice(&channel_layout_info((100 << 16) | 1)); // mono
        let cookie = AlacCookie::parse(&data).expect("valid cookie");
        assert_eq!(cookie.config.num_channels, 2, "bare_fixture states stereo");
        assert_eq!(
            cookie.layout(),
            ChannelLayout::MONO,
            "the explicit tag overrides the bare channel count"
        );
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
            let _ = AlacCookie::parse(&data);
        }
    }

    #[test]
    fn a_zero_channel_count_is_rejected_not_a_panic() {
        let mut data = bare_fixture().to_vec();
        data[9] = 0; // numChannels byte
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
        let mut data = bare_fixture();
        data[5] = 164; // bitDepth byte
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
            let mut data = bare_fixture();
            data[5] = depth;
            let cfg = AlacSpecificConfig::parse(&data).expect("valid cookie");
            let audio = cfg.to_codec_parameters().audio.expect("audio parameters");
            assert_eq!(audio.bits_per_raw_sample, Some(depth));
        }
    }

    #[test]
    fn for_encode_round_trips_through_write_and_parse() {
        let cfg = AlacSpecificConfig::for_encode(48000, 2, 32, 40, 10, 14);
        let bytes = cfg.write_bare();
        let cookie = AlacCookie::parse(&bytes).expect("valid cookie");
        assert_eq!(cookie.config, cfg);
        assert_eq!(cookie.layout(), ChannelLayout::STEREO);
    }

    #[test]
    fn too_short_is_invalid_data() {
        assert!(AlacCookie::parse(&[0u8; 4]).is_err());
    }

    /// The exact 24-byte `ALACSpecificConfig` measured 2026-08-28 from a
    /// real `ffmpeg -c:a alac` mono output file's `stsd` box (a small MP4
    /// box walk, no container-parsing crate). Frozen here as a regression
    /// fixture -- ported from `vaco-codec-alac::cookie`'s own tests when
    /// this module absorbed that crate's parsing logic.
    const REAL_MONO_COOKIE: [u8; LEN] = [
        0x00, 0x00, 0x10, 0x00, // frameLength = 4096
        0x00, // compatibleVersion = 0
        0x10, // bitDepth = 16
        0x28, // pb = 40
        0x0a, // mb = 10
        0x0e, // kb = 14
        0x01, // numChannels = 1
        0x00, 0x00, // maxRun = 0 (spec text says "should be 255"; ffmpeg emits 0)
        0x00, 0x00, 0x20, 0x04, // maxFrameBytes = 8196
        0x00, 0x0a, 0xc4, 0x40, // avgBitRate = 705600
        0x00, 0x00, 0xac, 0x44, // sampleRate = 44100
    ];

    #[test]
    fn real_ffmpeg_mono_cookie() {
        let cookie = AlacCookie::parse(&REAL_MONO_COOKIE).expect("valid cookie");
        assert_eq!(cookie.config.frame_length, 4096);
        assert_eq!(cookie.config.compatible_version, 0);
        assert_eq!(cookie.config.bit_depth, 16);
        assert_eq!(cookie.config.pb, 40);
        assert_eq!(cookie.config.mb, 10);
        assert_eq!(cookie.config.kb, 14);
        assert_eq!(cookie.config.num_channels, 1);
        assert_eq!(cookie.config.max_frame_bytes, 8196);
        assert_eq!(cookie.config.avg_bit_rate, 705_600);
        assert_eq!(cookie.config.sample_rate, 44100);
        assert_eq!(cookie.layout(), ChannelLayout::MONO);
    }

    #[test]
    fn stereo_channel_count_yields_stereo_layout() {
        let mut bytes = REAL_MONO_COOKIE;
        bytes[9] = 2;
        let cookie = AlacCookie::parse(&bytes).expect("valid cookie");
        assert_eq!(cookie.layout(), ChannelLayout::STEREO);
    }

    #[test]
    fn channel_layout_info_overrides_bare_channel_count() {
        let mut bytes = REAL_MONO_COOKIE.to_vec();
        bytes.extend_from_slice(&channel_layout_info((101u32 << 16) | 2));
        let cookie = AlacCookie::parse(&bytes).expect("valid cookie");
        assert_eq!(cookie.layout(), ChannelLayout::STEREO);
    }

    #[test]
    fn compat_wrapper_is_stripped() {
        let cookie = AlacCookie::parse(&frma_fixture(REAL_MONO_COOKIE)).expect("valid cookie");
        assert_eq!(cookie.config.sample_rate, 44100);
    }
}
