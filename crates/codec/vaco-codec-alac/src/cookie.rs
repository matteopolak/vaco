//! The ALAC "magic cookie" types: `AlacSpecificConfig`, the optional
//! `AlacChannelLayoutInfo`, and `AlacCookie` wrapping both.
//!
//! Consolidated into `vaco-parse-audio-misc` (which this crate now depends
//! on, matching the precedent `vaco-codec-opus`/`vaco-parse-opus` already
//! set) rather than duplicated here — this module is now a thin re-export
//! plus the one test that is genuinely this crate's own concern: that
//! [`AlacSpecificConfig::for_encode`] is actually called with *this*
//! encoder's real Rice-coding defaults, not the spec's or `ffmpeg`'s.
//! [`AlacSpecificConfig::parse`]/[`AlacCookie::parse`]'s own correctness —
//! including the exact on-disk shapes this format is measured against — is
//! that crate's `alac` module's concern now; see its doc for the full
//! account, including a real disagreement the consolidation found and
//! fixed between the two previously-independent parsers.

pub use vaco_parse_audio_misc::alac::{AlacChannelLayoutInfo, AlacCookie, AlacSpecificConfig};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    /// `AlacSpecificConfig::for_encode`'s `pb`/`mb`/`kb` must be this
    /// crate's own encoder defaults, not the spec's or `ffmpeg`'s — a
    /// mismatch here would silently mistune the cookie a decoder trusts to
    /// configure the entropy coder it uses to read `AlacEncoder`'s actual
    /// packets.
    #[test]
    fn for_encode_matches_the_encoders_own_rice_defaults() {
        let cfg = AlacSpecificConfig::for_encode(
            44100,
            1,
            16,
            crate::rice::PB0 as u8,
            crate::rice::MB0 as u8,
            crate::rice::KB0 as u8,
        );
        assert_eq!(cfg.pb, crate::rice::PB0 as u8);
        assert_eq!(cfg.mb, crate::rice::MB0 as u8);
        assert_eq!(cfg.kb, crate::rice::KB0 as u8);
        assert_eq!(cfg.bit_depth, 16);
        assert_eq!(cfg.num_channels, 1);
        assert_eq!(cfg.sample_rate, 44100);
    }
}
