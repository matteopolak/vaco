//! The ALAC "magic cookie": `ALACSpecificConfig` plus the optional
//! `ALACChannelLayoutInfo`, exactly as `set_extradata` receives it from a
//! container.
//!
//! Field order, widths and the two on-disk shapes (bare 24 bytes, or wrapped
//! in an outer `alac`/`frma` atom) come from Apple's own
//! `ALACMagicCookieDescription.txt` (see `provenance/vaco-codec-alac.toml`,
//! id `alac-magic-cookie`) and were cross-checked against a real cookie
//! extracted from an `ffmpeg -c:a alac` output file's `stsd` box — see the
//! `real_ffmpeg_mono_cookie` test below, which pins the exact bytes measured.

use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};

/// The mandatory 24-byte `ALACSpecificConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlacSpecificConfig {
    pub frame_length: u32,
    pub compatible_version: u8,
    pub bit_depth: u8,
    /// Tuning parameter; carried through but not required by this crate's
    /// own entropy coder (see `rice.rs`'s doc comment for why).
    pub pb: u8,
    pub mb: u8,
    pub kb: u8,
    pub num_channels: u8,
    pub max_run: u16,
    pub max_frame_bytes: u32,
    pub avg_bit_rate: u32,
    pub sample_rate: u32,
}

const CONFIG_LEN: usize = 24;
const CHANNEL_LAYOUT_INFO_LEN: usize = 24;

impl AlacSpecificConfig {
    fn parse_bare(b: &[u8]) -> Result<Self> {
        if b.len() < CONFIG_LEN {
            return Err(Error::InvalidData(
                "alac: cookie shorter than ALACSpecificConfig",
            ));
        }
        Ok(Self {
            frame_length: be32(b, 0),
            compatible_version: byte(b, 4),
            bit_depth: byte(b, 5),
            pb: byte(b, 6),
            mb: byte(b, 7),
            kb: byte(b, 8),
            num_channels: byte(b, 9),
            max_run: be16(b, 10),
            max_frame_bytes: be32(b, 12),
            avg_bit_rate: be32(b, 16),
            sample_rate: be32(b, 20),
        })
    }

    /// A [`vaco_chlayout::ChannelLayout`] for this config alone, per
    /// `ALACMagicCookieDescription.txt`'s "if the channel layout is absent"
    /// rule: mono, stereo L/R, or unspecified beyond that.
    #[must_use]
    pub fn default_layout(&self) -> ChannelLayout {
        match self.num_channels {
            1 => ChannelLayout::MONO,
            2 => ChannelLayout::STEREO,
            n => ChannelLayout::unspecified(u32::from(n)),
        }
    }

    /// This crate's own encoder default: `pb`/`mb`/`kb` from
    /// [`crate::rice::PB0`]/[`MB0`](crate::rice::MB0)/[`KB0`](crate::rice::KB0)
    /// (`AlacEncoder::send_frame` never uses any other triple), `frame_length`
    /// the ALAC-conventional 4096 (informational only for this encoder: every
    /// packet's own element header states its real sample count explicitly
    /// via `partialFrame`/`numSamples` — see `frame_codec::encode` — so
    /// nothing here ever needs to fall back to it), and `max_frame_bytes`/
    /// `avg_bit_rate` left `0` ("unknown"/VBR), matching the real
    /// `ffmpeg -c:a alac` cookie [`AlacCookie`]'s own tests measured (that
    /// file's non-zero values are per-encode statistics gathered only once
    /// encoding finishes, which is not yet the case at `add_stream` time —
    /// `0` is what a real encoder emits before it knows them, not a
    /// placeholder unique to this crate).
    #[must_use]
    pub fn for_encode(sample_rate: u32, num_channels: u8, bit_depth: u8) -> Self {
        Self {
            frame_length: 4096,
            compatible_version: 0,
            bit_depth,
            pb: crate::rice::PB0 as u8,
            mb: crate::rice::MB0 as u8,
            kb: crate::rice::KB0 as u8,
            num_channels,
            max_run: 0,
            max_frame_bytes: 0,
            avg_bit_rate: 0,
            sample_rate,
        }
    }

    /// Serialize the bare 24-byte `ALACSpecificConfig` (no compatibility
    /// wrapper, no `ALACChannelLayoutInfo` — [`AlacSpecificConfig::default_layout`]
    /// already recovers mono/stereo from `num_channels` alone, so this
    /// crate's encoder never needs the optional channel-layout extension).
    #[must_use]
    pub fn write_bare(&self) -> [u8; CONFIG_LEN] {
        let mut out = [0u8; CONFIG_LEN];
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
}

/// The optional `ALACChannelLayoutInfo`, when a cookie carries one.
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
        Some(Self {
            channel_layout_tag: be32(b, 12),
        })
    }

    /// The documented tags this crate recognises, mapped to a concrete
    /// [`ChannelLayout`]. `None` for any tag not in that table (multichannel
    /// beyond what `ReadMe.txt` lists is not implemented — see the crate
    /// doc's "what did not land" section).
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

// `ALACChannelLayoutInfo`'s tags are `(family << 16) | channel_count`, per
// `ALACMagicCookieDescription.txt`'s enum. Only mono/stereo are given names
// here because `ChannelLayout` has no constructor this crate exercises for
// the others yet (3.0B/4.0B/5.0D/5.1D/6.1/7.1B) — the numeric tags below are
// still exactly what the spec states, so a future patch adding those layouts
// only needs new `ChannelLayout` values, not new tag numbers.
const K_MONO: u32 = (100 << 16) | 1;
const K_STEREO: u32 = (101 << 16) | 2;

fn byte(b: &[u8], off: usize) -> u8 {
    b.get(off).copied().unwrap_or(0)
}

fn be16(b: &[u8], off: usize) -> u16 {
    let hi = u16::from(byte(b, off));
    let lo = u16::from(byte(b, off + 1));
    (hi << 8) | lo
}

fn be32(b: &[u8], off: usize) -> u32 {
    let mut v = 0u32;
    for i in 0..4 {
        v = (v << 8) | u32::from(byte(b, off + i));
    }
    v
}

/// A parsed magic cookie: the mandatory config, plus a channel layout if the
/// cookie states one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlacCookie {
    pub config: AlacSpecificConfig,
    pub channel_layout: Option<AlacChannelLayoutInfo>,
}

impl AlacCookie {
    /// The layout to use: the explicit `ALACChannelLayoutInfo` tag if present
    /// and recognised, otherwise [`AlacSpecificConfig::default_layout`].
    #[must_use]
    pub fn layout(&self) -> ChannelLayout {
        self.channel_layout
            .and_then(AlacChannelLayoutInfo::layout)
            .unwrap_or_else(|| self.config.default_layout())
    }

    /// Parse a magic cookie as `set_extradata` receives it: either the bare
    /// 24 (or 24+24) byte form, or the "compatibility" form wrapping it in a
    /// `frma`/`alac`/terminator envelope (`ALACMagicCookieDescription.txt`'s
    /// "Compatibility" section). Both are handled because a container's own
    /// box-parsing layer forwards whatever it stored verbatim — the doc is
    /// explicit that the cookie "is treated as opaque" up to this point.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if `extradata` is too short to hold even the
    /// mandatory config.
    pub fn parse(extradata: &[u8]) -> Result<Self> {
        let body = strip_compat_wrapper(extradata);
        let config = AlacSpecificConfig::parse_bare(body)?;
        let channel_layout = body
            .get(CONFIG_LEN..)
            .and_then(AlacChannelLayoutInfo::parse);
        Ok(Self {
            config,
            channel_layout,
        })
    }
}

/// If `data` starts with the "Compatibility" wrapper (`Format Atom` = size 12,
/// id `frma`, type `alac`, followed by a 36-byte `ALAC Specific Info` header
/// whose last 24 or 48 bytes are the real cookie), strip it down to the bare
/// cookie. Otherwise returns `data` unchanged — including the "just the 24 or
/// 48-byte cookie, no wrapper" case a `Parser`/muxer commonly hands over
/// directly rather than the full ISO `AudioSampleEntry` this doc's MP4
/// section describes (this crate's own `set_extradata` contract is "the
/// cookie", not "the whole sample entry" — a demuxer's own box walk is
/// expected to have already stripped `AudioSampleEntry`/`SoundDescriptionBox`
/// framing before calling in, matching every other codec in this tree).
fn strip_compat_wrapper(data: &[u8]) -> &[u8] {
    let is_frma =
        data.get(4..8) == Some(b"frma".as_slice()) && data.get(8..12) == Some(b"alac".as_slice());
    if !is_frma {
        return data;
    }
    // Format Atom (12) + ALAC Specific Info header (12: size, 'alac', flags).
    data.get(24..).unwrap_or(&[])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// The exact 24-byte `ALACSpecificConfig` measured 2026-08-28 from a real
    /// `ffmpeg -c:a alac` mono output file's `stsd` box (see the crate's
    /// closing report for the extraction method: a small MP4 box walk, no
    /// container-parsing crate). Frozen here as a regression fixture.
    const REAL_MONO_COOKIE: [u8; 24] = [
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
        let cookie = AlacCookie::parse(&REAL_MONO_COOKIE).unwrap();
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
        if let Some(numch) = bytes.get_mut(9) {
            *numch = 2;
        }
        let cookie = AlacCookie::parse(&bytes).unwrap();
        assert_eq!(cookie.layout(), ChannelLayout::STEREO);
    }

    #[test]
    fn channel_layout_info_overrides_bare_channel_count() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&REAL_MONO_COOKIE);
        // ALACChannelLayoutInfo: size(4)=24, id(4)="chan", flags(4)=0,
        // tag(4)=kALACChannelLayoutTag_Stereo, reserved(4)=0, reserved(4)=0.
        bytes.extend_from_slice(&24u32.to_be_bytes());
        bytes.extend_from_slice(b"chan");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&(((101u32) << 16) | 2).to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let cookie = AlacCookie::parse(&bytes).unwrap();
        assert_eq!(cookie.layout(), ChannelLayout::STEREO);
    }

    #[test]
    fn compat_wrapper_is_stripped() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&12u32.to_be_bytes());
        bytes.extend_from_slice(b"frma");
        bytes.extend_from_slice(b"alac");
        bytes.extend_from_slice(&36u32.to_be_bytes());
        bytes.extend_from_slice(b"alac");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&REAL_MONO_COOKIE);
        let cookie = AlacCookie::parse(&bytes).unwrap();
        assert_eq!(cookie.config.sample_rate, 44100);
    }

    #[test]
    fn too_short_is_invalid_data() {
        assert!(AlacCookie::parse(&[0u8; 4]).is_err());
    }

    /// `AlacSpecificConfig::for_encode`'s `pb`/`mb`/`kb` must be this crate's
    /// own encoder defaults, not the spec's or `ffmpeg`'s — a mismatch here
    /// would silently mistune the cookie a decoder trusts to configure the
    /// entropy coder it uses to read `AlacEncoder`'s actual packets.
    #[test]
    fn for_encode_matches_the_encoders_own_rice_defaults() {
        let cfg = AlacSpecificConfig::for_encode(44100, 1, 16);
        assert_eq!(cfg.pb, crate::rice::PB0 as u8);
        assert_eq!(cfg.mb, crate::rice::MB0 as u8);
        assert_eq!(cfg.kb, crate::rice::KB0 as u8);
        assert_eq!(cfg.bit_depth, 16);
        assert_eq!(cfg.num_channels, 1);
        assert_eq!(cfg.sample_rate, 44100);
    }

    /// `write_bare` then `parse_bare` must recover the exact same config —
    /// the property the encoder side actually depends on: whatever this
    /// crate declares in `CodecPrivate` must be exactly what its own
    /// `set_extradata`/`AlacCookie::parse` would read back.
    #[test]
    fn for_encode_round_trips_through_write_and_parse() {
        let cfg = AlacSpecificConfig::for_encode(48000, 2, 32);
        let bytes = cfg.write_bare();
        let cookie = AlacCookie::parse(&bytes).unwrap();
        assert_eq!(cookie.config, cfg);
        assert_eq!(cookie.layout(), ChannelLayout::STEREO);
    }
}
