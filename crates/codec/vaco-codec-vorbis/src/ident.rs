//! Identification header parsing (spec section 4.2.2) and Vorbis's own
//! per-channel-count output ordering (spec section 4.3.9), which disagrees
//! with this project's SMPTE-derived default channel order from six
//! channels on: Vorbis puts the LFE last and keeps front-left/right split by
//! a center channel, where a `Native` 5.1 mask would not.
//!
//! `Vaco-Spec-Ref: vorbis-i sections 4.2.2 and 4.3.9`

use vaco_chlayout::{Channel, ChannelLayout};
use vaco_core::{Error, Result};

use crate::bitreader::BitReaderLsb;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Ident {
    pub(crate) channels: u8,
    pub(crate) sample_rate: u32,
    pub(crate) blocksize_0: u32,
    pub(crate) blocksize_1: u32,
}

impl Ident {
    /// Parse the identification header packet (without its leading
    /// `[packet_type][vorbis]` common header — the caller strips that).
    pub(crate) fn parse(body: &[u8]) -> Result<Self> {
        let mut r = BitReaderLsb::new(body);
        let vorbis_version = r.get(32);
        if vorbis_version != 0 {
            return Err(Error::InvalidData(
                "vorbis: identification header version is not 0",
            ));
        }
        let channels = r.get(8);
        let sample_rate = r.get(32);
        let _bitrate_maximum = r.get_signed(32);
        let _bitrate_nominal = r.get_signed(32);
        let _bitrate_minimum = r.get_signed(32);
        let blocksize_0_exp = r.get(4);
        let blocksize_1_exp = r.get(4);
        let framing_flag = r.get_bool();
        if r.overran() {
            return Err(Error::InvalidData(
                "vorbis: eop decoding identification header",
            ));
        }
        if channels == 0 || sample_rate == 0 {
            return Err(Error::InvalidData(
                "vorbis: identification header has zero channels or sample rate",
            ));
        }
        if !framing_flag {
            return Err(Error::InvalidData(
                "vorbis: identification header framing bit is unset",
            ));
        }
        let blocksize_0 = 1u32 << blocksize_0_exp.min(31);
        let blocksize_1 = 1u32 << blocksize_1_exp.min(31);
        if !is_valid_blocksize(blocksize_0) || !is_valid_blocksize(blocksize_1) {
            return Err(Error::InvalidData("vorbis: blocksize outside {64..=8192}"));
        }
        if blocksize_0 > blocksize_1 {
            return Err(Error::InvalidData(
                "vorbis: blocksize_0 greater than blocksize_1",
            ));
        }
        let channels = u8::try_from(channels)
            .map_err(|_| Error::InvalidData("vorbis: channel count too large"))?;
        Ok(Self {
            channels,
            sample_rate,
            blocksize_0,
            blocksize_1,
        })
    }
}

const fn is_valid_blocksize(n: u32) -> bool {
    matches!(n, 64 | 128 | 256 | 512 | 1024 | 2048 | 4096 | 8192)
}

/// Vorbis I's own output channel order (spec section 4.3.9), for the counts
/// it defines explicitly. Beyond eight channels the spec leaves ordering to
/// the application, so this returns an unspecified layout of that count
/// rather than guessing a mapping no encoder is bound to.
#[must_use]
pub(crate) fn output_channel_layout(channels: u8) -> ChannelLayout {
    use Channel::{
        BackCenter, BackLeft, BackRight, FrontCenter, FrontLeft, FrontRight, LowFrequency,
        SideLeft, SideRight,
    };
    let list: Vec<Channel> = match channels {
        1 => vec![FrontCenter],
        2 => vec![FrontLeft, FrontRight],
        3 => vec![FrontLeft, FrontCenter, FrontRight],
        4 => vec![FrontLeft, FrontRight, BackLeft, BackRight],
        5 => vec![FrontLeft, FrontCenter, FrontRight, BackLeft, BackRight],
        6 => vec![
            FrontLeft,
            FrontCenter,
            FrontRight,
            BackLeft,
            BackRight,
            LowFrequency,
        ],
        7 => vec![
            FrontLeft,
            FrontCenter,
            FrontRight,
            SideLeft,
            SideRight,
            BackCenter,
            LowFrequency,
        ],
        8 => vec![
            FrontLeft,
            FrontCenter,
            FrontRight,
            SideLeft,
            SideRight,
            BackLeft,
            BackRight,
            LowFrequency,
        ],
        n => return ChannelLayout::unspecified(u32::from(n)),
    };
    ChannelLayout::custom(list).unwrap_or_else(|| ChannelLayout::unspecified(u32::from(channels)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_and_stereo_use_the_shared_constants_shape() {
        assert_eq!(output_channel_layout(1).channels, 1);
        assert_eq!(output_channel_layout(2).channels, 2);
    }

    #[test]
    fn six_channel_order_puts_lfe_last_unlike_native_5_1() {
        let layout = output_channel_layout(6);
        assert_eq!(layout.channels, 6);
        // A `Native` 5.1 mask orders L R C LFE BL BR; Vorbis orders
        // L C R BL BR LFE, so this must not collapse to `Native`.
        assert!(!matches!(layout.order, vaco_chlayout::ChannelOrder::Native));
    }

    #[test]
    fn nine_channels_is_left_to_the_application() {
        let layout = output_channel_layout(9);
        assert_eq!(layout.channels, 9);
        assert!(matches!(
            layout.order,
            vaco_chlayout::ChannelOrder::Unspecified
        ));
    }
}
