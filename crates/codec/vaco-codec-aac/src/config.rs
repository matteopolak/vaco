//! The decoder's configuration layer unifies whatever
//! `vaco-parse-aac` handed over — an `AdtsHeader` (raw ADTS, no out-of-band
//! config at all) or an `AudioSpecificConfig` (MP4 `esds`, or LATM/LOAS) —
//! into one [`DecoderConfig`], resolving the actual channel layout (a table
//! lookup, or a [`crate::pce::ProgramConfigElement`] when
//! `channelConfiguration == 0`), and gating object types this crate cannot
//! yet decode.
//!
//! # Object-type gating
//!
//! This crate implements AAC-LC only. Every other object type —
//! Main/SSR/LTP, the ER family, and HE-AAC/PS's SBR wrapper
//! — is rejected here, at configuration time, with a specific
//! [`Error::Unsupported`] rather than silently decoded as if it were LC. That
//! is the same "gate rather than guess" call this workspace made for
//! MPEG-2.5 Layer III (`vaco-codec-mpegaudio`) and for the same reason: a
//! decoder that emits plausible-looking wrong samples is worse than one that
//! says it cannot.
//!
//! # Channel-configuration coverage
//!
//! `channelConfiguration` 1 (mono), 2 (stereo), 5 (5.0) and 6 (5.1) are resolved
//! directly — by far the overwhelming majority of real AAC-LC content, and
//! the four configurations this workspace could confidently state the exact
//! `SCE`/`CPE`/`LFE` element ordering for without needing the ISO/IEC
//! 14496-3 Table 42 text on hand for the rarer ones (3, 4, 7, 11, 12, 14)
//! to check rather than recall. Those rarer configurations are rejected with
//! [`Error::Unsupported`] rather than guessed at — a wrong element-count
//! assumption there would desync every channel element's decode after the
//! first, the same class of bug this workspace has now found and fixed
//! twice in other codecs by *not* trusting an unchecked recollection.
//! `channelConfiguration == 0` is resolved exactly, from a
//! [`crate::pce::ProgramConfigElement`], because that path never has to
//! guess at all.

use vaco_bitstream::BitReader;
use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, Result};
use vaco_parse_aac::{AdtsHeader, AudioObjectType, AudioSpecificConfig, tables};

use crate::pce::{ProgramConfigElement, find_leading_program_config_element};

/// The channel layout a [`DecoderConfig`] has resolved, or is still waiting
/// on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelResolution {
    /// Resolved: `channelConfiguration` named a known layout directly.
    Known {
        /// Total channel count.
        count: u32,
    },
    /// Resolved from a [`ProgramConfigElement`] read out of the bitstream.
    FromPce {
        /// Total channel count, from [`ProgramConfigElement::channel_count`].
        count: u32,
        /// The known native output layout, when this PCE's element ordering
        /// does not require the decoder to permute planes first.
        layout: Option<ChannelLayout>,
    },
    /// `channelConfiguration == 0` and no program config element has been
    /// found yet. [`DecoderConfig::try_resolve_pending`] attempts to clear
    /// this from the payload the decoder is about to read.
    Pending,
}

impl ChannelResolution {
    /// The channel count, if resolved.
    #[must_use]
    pub const fn count(&self) -> Option<u32> {
        match self {
            Self::Known { count } | Self::FromPce { count, .. } => Some(*count),
            Self::Pending => None,
        }
    }
}

/// The subset of `channelConfiguration` values this crate resolves without a
/// program config element. See the module doc for why 3/4/7/11/12/14 are
/// deliberately absent rather than guessed.
fn known_channel_count(channel_configuration: u8) -> Option<u32> {
    match channel_configuration {
        1 => Some(1),
        2 => Some(2),
        5 => Some(5),
        6 => Some(6),
        _ => None,
    }
}

/// A decoder's resolved configuration: the object type (gated to AAC-LC),
/// core sample rate, frame length, and channel resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderConfig {
    /// The core object type. Always [`AudioObjectType::AAC_LC`] — see the
    /// module doc's "object-type gating" section — kept as a field rather
    /// than assumed so callers that print diagnostics have it to hand.
    pub object_type: AudioObjectType,
    /// The core sampling frequency, in Hz.
    pub sample_rate: u32,
    /// Samples per frame at `sample_rate`: 1024 normally, 960 when
    /// `frameLengthFlag` is set (only meaningful for an `AudioSpecificConfig`
    /// source; raw ADTS has no such flag and is always 1024).
    pub frame_length: u32,
    /// The raw `channelConfiguration` value this configuration came from.
    pub channel_configuration: u8,
    /// The resolved (or still-pending) channel layout.
    pub channels: ChannelResolution,
}

impl DecoderConfig {
    /// Gate an object type to what this crate can decode.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for anything other than AAC-LC.
    fn gate_object_type(object_type: AudioObjectType) -> Result<()> {
        if object_type == AudioObjectType::AAC_LC {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "vaco-codec-aac: only AAC-LC (audioObjectType 2) is implemented; \
                 Main/SSR/LTP, the ER family, and HE-AAC/PS's SBR wrapper are not \
                 (see docs/codec/vaco-codec-aac.md)",
            ))
        }
    }

    /// Build a configuration from a raw ADTS header. ADTS carries no
    /// `frameLengthFlag`, so `frame_length` is always 1024.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] if the header's object type is not AAC-LC, or
    /// if `channelConfiguration` is a reserved value ADTS's own 3-bit field
    /// can still encode (8..=10 or 15, per [`tables::is_reserved_config`]).
    pub fn from_adts_header(header: &AdtsHeader) -> Result<Self> {
        Self::gate_object_type(header.object_type)?;
        if tables::is_reserved_config(header.channel_configuration) {
            return Err(Error::Unsupported(
                "vaco-codec-aac: reserved ADTS channel_configuration",
            ));
        }
        let channels = match known_channel_count(header.channel_configuration) {
            Some(count) => ChannelResolution::Known { count },
            None if header.channel_configuration == 0 => ChannelResolution::Pending,
            None => {
                return Err(Error::Unsupported(
                    "vaco-codec-aac: channel_configuration 3/4/7/11/12/14 are not \
                     resolved without ISO/IEC 14496-3 Table 42's exact element \
                     ordering on hand to verify against — gated rather than guessed \
                     (see docs/codec/vaco-codec-aac.md)",
                ));
            }
        };
        Ok(Self {
            object_type: header.object_type,
            sample_rate: header.sampling_frequency,
            frame_length: 1024,
            channel_configuration: header.channel_configuration,
            channels,
        })
    }

    /// Build a configuration from a parsed `AudioSpecificConfig` (an MP4
    /// `esds`'s `DecoderSpecificInfo`, or a LATM `StreamMuxConfig`'s inline
    /// copy).
    ///
    /// # Errors
    ///
    /// As [`DecoderConfig::from_adts_header`]. Also rejects a configuration
    /// that signals SBR or Parametric Stereo (`cfg.has_sbr()`, or `cfg.ps`
    /// anything but absent/unknown) with [`Error::Unsupported`] because
    /// HE-AAC/HE-AACv2 is outside this decoder's scope.
    pub fn from_audio_specific_config(cfg: &AudioSpecificConfig) -> Result<Self> {
        Self::gate_object_type(cfg.object_type)?;
        if cfg.has_sbr() {
            return Err(Error::Unsupported(
                "vaco-codec-aac: SBR (HE-AAC) is not implemented — #446, not this crate",
            ));
        }
        if tables::is_reserved_config(cfg.channel_configuration) {
            return Err(Error::Unsupported(
                "vaco-codec-aac: reserved channelConfiguration",
            ));
        }
        let channels = match known_channel_count(cfg.channel_configuration) {
            Some(count) => ChannelResolution::Known { count },
            None if cfg.channel_configuration == 0 => ChannelResolution::Pending,
            None => {
                return Err(Error::Unsupported(
                    "vaco-codec-aac: channel_configuration 3/4/7/11/12/14 are not \
                     resolved without ISO/IEC 14496-3 Table 42's exact element \
                     ordering on hand to verify against — gated rather than guessed \
                     (see docs/codec/vaco-codec-aac.md)",
                ));
            }
        };
        Ok(Self {
            object_type: cfg.object_type,
            sample_rate: cfg.sampling_frequency,
            frame_length: cfg.frame_length(),
            channel_configuration: cfg.channel_configuration,
            channels,
        })
    }

    /// Whether this configuration still needs a program config element
    /// before decode can proceed.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self.channels, ChannelResolution::Pending)
    }

    /// Resolve a still-[`ChannelResolution::Pending`] configuration from an
    /// explicitly-supplied [`ProgramConfigElement`] — the caller having
    /// already found one some other way (an MP4 sample description's own
    /// PCE box, for instance). A no-op, returning `Ok(())`, if this
    /// configuration is already resolved.
    ///
    /// # Errors
    ///
    /// Never on its own; kept fallible for symmetry with
    /// [`DecoderConfig::try_resolve_pending`] and because a future check
    /// (e.g. cross-validating the PCE's own `sampling_frequency_index`
    /// against this configuration's) belongs here rather than at every call
    /// site.
    pub fn resolve_with_pce(&mut self, pce: &ProgramConfigElement) -> Result<()> {
        if self.is_pending() {
            self.channels = ChannelResolution::FromPce {
                count: pce.channel_count(),
                layout: pce.known_output_layout(),
            };
        }
        Ok(())
    }

    /// The native output layout this configuration establishes, when known.
    ///
    /// Direct channel configurations use AAC's fixed mapping. A PCE carries
    /// its own element ordering, so it can name a native layout only when the
    /// decoder has established that its emitted plane order already matches.
    #[must_use]
    pub fn output_layout(&self) -> Option<ChannelLayout> {
        if self.channel_configuration != 0 {
            return tables::layout_for_config(self.channel_configuration);
        }
        match &self.channels {
            ChannelResolution::FromPce { layout, .. } => layout.clone(),
            ChannelResolution::Known { .. } | ChannelResolution::Pending => None,
        }
    }

    /// If this configuration is [`ChannelResolution::Pending`], try to
    /// resolve it by reading a leading program config element off `r` — see
    /// [`find_leading_program_config_element`]'s own doc for exactly which
    /// streams this can and cannot find one in. Positioned identically to
    /// that function: on a hit, `r` ends up just past the PCE; on a miss
    /// (`Ok(false)`), `r` is left untouched.
    ///
    /// # Errors
    ///
    /// Whatever [`find_leading_program_config_element`] returns for a
    /// truncated PCE.
    pub fn try_resolve_pending(&mut self, r: &mut BitReader<'_>) -> Result<bool> {
        if !self.is_pending() {
            return Ok(true);
        }
        match find_leading_program_config_element(r)? {
            Some(pce) => {
                self.resolve_with_pce(&pce)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::{ChannelResolution, DecoderConfig};
    use crate::pce::{ChannelElementRef, ProgramConfigElement};
    use vaco_bitstream::BitWriter;
    use vaco_parse_aac::{AdtsHeader, AudioObjectType};

    /// Encode a minimal, valid ADTS header (`protection_absent = 1`, so no
    /// CRC), matching `AdtsHeader::parse`'s own bit layout exactly.
    /// `AdtsHeader` is `#[non_exhaustive]`, so a struct literal is not an
    /// option outside `vaco-parse-aac` — going through a real encode and
    /// `AdtsHeader::parse` is not a workaround, it is the only way in, and it
    /// doubles as a check that this crate's understanding of the header
    /// layout matches the parser's.
    fn adts_header_bytes(object_type: u8, channel_configuration: u8) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put(12, 0xfff); // syncword
        w.put(1, 0); // ID: MPEG-4
        w.put(2, 0); // layer
        w.put(1, 1); // protection_absent
        w.put(2, u32::from(object_type) - 1); // profile
        w.put(4, 3); // sampling_frequency_index (48000)
        w.put(1, 0); // private_bit
        w.put(3, u32::from(channel_configuration));
        w.put(1, 0); // original_copy
        w.put(1, 0); // home
        w.put(1, 0); // copyright_id_bit
        w.put(1, 0); // copyright_id_start
        w.put(13, 7); // aac_frame_length: header only, no payload
        w.put(11, 0x7ff); // buffer_fullness: VBR
        w.put(2, 0); // raw_data_blocks - 1
        w.finish()
    }

    fn parse(object_type: u8, channel_configuration: u8) -> vaco_core::Result<DecoderConfig> {
        let bytes = adts_header_bytes(object_type, channel_configuration);
        let header = AdtsHeader::parse(&bytes).unwrap();
        DecoderConfig::from_adts_header(&header)
    }

    #[test]
    fn aac_lc_stereo_resolves_directly() {
        let cfg = parse(2, 2).unwrap();
        assert_eq!(cfg.channels, ChannelResolution::Known { count: 2 });
        assert_eq!(cfg.frame_length, 1024);
        assert!(!cfg.is_pending());
    }

    #[test]
    fn aac_lc_51_resolves_directly() {
        let cfg = parse(2, 6).unwrap();
        assert_eq!(cfg.channels, ChannelResolution::Known { count: 6 });
    }

    #[test]
    fn aac_lc_50_resolves_directly() {
        let cfg = parse(2, 5).unwrap();
        assert_eq!(cfg.channels, ChannelResolution::Known { count: 5 });
        assert_eq!(
            cfg.output_layout().and_then(|layout| layout.name()),
            Some("5.0")
        );
    }

    #[test]
    fn a_non_lc_object_type_is_rejected() {
        // object_type 1 == AAC Main, not LC.
        assert!(parse(1, 2).is_err());
    }

    #[test]
    fn channel_configuration_zero_is_pending() {
        let cfg = parse(2, 0).unwrap();
        assert!(cfg.is_pending());
    }

    #[test]
    fn pce_21_layout_is_retained_after_resolution() {
        let mut cfg = parse(2, 0).unwrap();
        let pce = ProgramConfigElement {
            element_instance_tag: 0,
            object_type: AudioObjectType::AAC_LC,
            sampling_frequency_index: 3,
            front: vec![ChannelElementRef {
                is_cpe: true,
                tag: 0,
            }],
            side: Vec::new(),
            back: Vec::new(),
            lfe: vec![1],
            mono_mixdown_element_number: None,
            stereo_mixdown_element_number: None,
            matrix_mixdown: None,
        };
        cfg.resolve_with_pce(&pce).unwrap();
        assert_eq!(cfg.output_layout().map(|layout| layout.mask()), Some(0xb));
    }

    #[test]
    fn an_unresolvable_channel_configuration_is_rejected_not_guessed() {
        assert!(parse(2, 7).is_err());
    }

    #[test]
    fn a_reserved_channel_configuration_is_rejected() {
        // ADTS's own `channel_configuration` field is only 3 bits (0..=7), so
        // a reserved value (8..=10, 15) can only arise from an
        // `AudioSpecificConfig`, whose field is 4 bits. Built directly
        // rather than through `vaco-parse-aac`'s own encoder (it has none;
        // parsing is this workspace's boundary for AAC in the default
        // build) — 16 bits total, so the trailing sync-extension check
        // (`bits_left() >= 16`) sees zero bits left and does not fire.
        let mut w = BitWriter::new();
        w.put(5, 2); // audioObjectType = AAC_LC
        w.put(4, 3); // samplingFrequencyIndex (48000)
        w.put(4, 9); // channelConfiguration: reserved
        w.put(1, 0); // frameLengthFlag
        w.put(1, 0); // dependsOnCoreCoder
        w.put(1, 0); // extensionFlag
        let bytes = w.finish();
        let asc = vaco_parse_aac::AudioSpecificConfig::parse(&bytes).unwrap();
        assert!(DecoderConfig::from_audio_specific_config(&asc).is_err());
    }
}
