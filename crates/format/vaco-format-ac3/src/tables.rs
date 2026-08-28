//! Small, spec-stated tables shared by [`crate::syncinfo`] and [`crate::bsi`].
//!
//! None of these reach the 32-element `provenance-check` threshold; ATSC
//! A/52:2018 is still the source (`Vaco-Spec-Ref: atsc-a52-2018`), just not
//! one that needs a `[[table]]` row.

/// `Table 5.18`'s bit-rate column, indexed by `frmsizecod >> 1`. §4.4.1.3.
pub const BITRATES_KBPS: [u16; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

/// `fscod` -> sample rate in Hz, both formats. §4.4.1.3 / §E.1.3.1.6.
pub const SAMPLE_RATES: [u32; 3] = [48000, 44100, 32000];

/// `numblkscod` -> blocks per E-AC-3 syncframe. §E.1.3.1.9.
pub const NUMBLKS: [u32; 4] = [1, 2, 3, 6];

pub const SAMPLES_PER_BLOCK: u32 = 256;

/// `acmod` -> full-bandwidth channel count, excluding LFE. §5.3.2.4 Table 5.8.
pub const ACMOD_CHANNELS: [u8; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

/// Whether `acmod` codes a centre channel (other than mono-only `acmod==1`,
/// which never carries `cmixlev`). §5.4.2.4.
#[must_use]
pub const fn has_center(acmod: u8) -> bool {
    acmod & 0x1 != 0 && acmod != 0x1
}

/// Whether `acmod` codes surround channels. §5.4.2.5.
#[must_use]
pub const fn has_surround(acmod: u8) -> bool {
    acmod & 0x4 != 0
}

/// `acmod`/`lfeon` -> speaker-position layout. §5.3.2.4 Table 5.8. `acmod`
/// 0 is dual mono (two independent programme channels, not a stereo pair),
/// approximated as [`vaco_chlayout::ChannelLayout::STEREO`] since the
/// reference reports it as `stereo` too (measured, see
/// `vaco-demux-raw::ac3`'s module docs); every other entry is a positional
/// layout measured against seven real `ffmpeg -c:a ac3` encodes (mono
/// through 5.1).
#[must_use]
pub fn acmod_layout(acmod: u8, lfeon: bool) -> vaco_chlayout::ChannelLayout {
    use vaco_chlayout::{Channel, ChannelLayout};
    let base: &[Channel] = match acmod {
        0 | 2 => &[Channel::FrontLeft, Channel::FrontRight],
        1 => &[Channel::FrontCenter],
        3 => &[
            Channel::FrontLeft,
            Channel::FrontCenter,
            Channel::FrontRight,
        ],
        4 => &[Channel::FrontLeft, Channel::FrontRight, Channel::BackCenter],
        5 => &[
            Channel::FrontLeft,
            Channel::FrontCenter,
            Channel::FrontRight,
            Channel::BackCenter,
        ],
        6 => &[
            Channel::FrontLeft,
            Channel::FrontRight,
            Channel::SideLeft,
            Channel::SideRight,
        ],
        _ => &[
            Channel::FrontLeft,
            Channel::FrontCenter,
            Channel::FrontRight,
            Channel::SideLeft,
            Channel::SideRight,
        ],
    };
    let mut mask = 0u64;
    for ch in base {
        if let Some(bit) = ch.bit() {
            mask |= 1u64 << bit;
        }
    }
    if lfeon && let Some(bit) = Channel::LowFrequency.bit() {
        mask |= 1u64 << bit;
    }
    ChannelLayout::from_mask(mask).unwrap_or_else(|| {
        let channels = base.len() as u32 + u32::from(lfeon);
        ChannelLayout::unspecified(channels)
    })
}
