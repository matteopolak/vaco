//! The two tables every AAC header parser needs, plus the profile names.
//!
//! # Provenance (D7/D9)
//!
//! Both tables are dictated by the format: any conforming parser reads the same
//! index and gets the same answer, so they are merger / scenes a faire rather
//! than authorial choice.
//!
//! * [`SAMPLING_FREQUENCY`] is ISO/IEC 14496-3 subpart 1 §1.6.2.4 Table 1.16
//!   (`samplingFrequencyIndex`). The same table indexes the ADTS
//!   `sampling_frequency_index` field (subpart 4 §4.4.1.1).
//! * [`channels_for_config`] and [`layout_for_config`] are the
//!   `channelConfiguration` table, ISO/IEC 14496-3 subpart 1 §1.6.3.4 Table 1.19
//!   for values 1..=7, extended by the 2009 amendment for 11..=14. The channel
//!   *positions* come from the specification; the mask spelling is
//!   `vaco-chlayout`'s vocabulary.
//! * [`profile_name`] is a display table, recorded by probing `ffprobe 8.1` —
//!   see `docs/codec/vaco-parse-aac.md` for the transcript.

use vaco_chlayout::ChannelLayout;

/// `samplingFrequencyIndex` → Hz. Index 13 and 14 are reserved; index 15 is the
/// escape to an explicit 24-bit rate and has no table entry.
///
/// ISO/IEC 14496-3 subpart 1 §1.6.2.4 Table 1.16.
pub const SAMPLING_FREQUENCY: [u32; 16] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350, 0, 0,
    0,
];

/// The escape index that introduces an explicit 24-bit `samplingFrequency`.
pub const SAMPLING_FREQUENCY_INDEX_ESCAPE: u8 = 0xf;

/// The rate a `samplingFrequencyIndex` names, or `None` for a reserved index
/// (13, 14) or the escape (15).
#[must_use]
pub fn frequency_for_index(index: u8) -> Option<u32> {
    match SAMPLING_FREQUENCY.get(usize::from(index)) {
        Some(&0) | None => None,
        Some(&hz) => Some(hz),
    }
}

/// The nearest `samplingFrequencyIndex` for a rate, or the escape when the rate
/// is not in the table.
///
/// The inverse of [`frequency_for_index`] on the table's own values; used by the
/// round-trip property tests and by anything that has to re-emit a header.
#[must_use]
pub fn index_for_frequency(hz: u32) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the table has 13 usable entries, so the index fits a u8"
    )]
    match SAMPLING_FREQUENCY.iter().position(|&f| f == hz && f != 0) {
        Some(i) => i as u8,
        None => SAMPLING_FREQUENCY_INDEX_ESCAPE,
    }
}

/// `(channelConfiguration, channel count, layout mask)`.
///
/// Configuration 0 means "read the program config element from the payload",
/// which a header parser cannot do — it is absent from this table on purpose.
/// Configurations 8, 9, 10 and 15 are reserved and are likewise absent; the
/// reference rejects a stream that declares one (`invalid default channel
/// configuration`), and so do we.
const CHANNEL_CONFIGURATION: [(u8, u32, u64); 11] = [
    // 1: centre front
    (1, 1, 0x4),
    // 2: left, right front
    (2, 2, 0x3),
    // 3: centre, left, right front
    (3, 3, 0x7),
    // 4: centre, left, right front, rear surround
    (4, 4, 0x107),
    // 5: centre, left, right front, left, right surround
    (5, 5, 0x37),
    // 6: 5 plus LFE
    (6, 6, 0x3f),
    // 7: centre, left/right front, left/right outside, left/right back, LFE
    (7, 8, 0x63f),
    // 11: 6.1
    (11, 7, 0x13f),
    // 12: 7.1
    (12, 8, 0x63f),
    // 13: 22.2
    (13, 24, 0x1f8_0003_ffff),
    // 14: 7.1 top (5.1 plus a height pair)
    (14, 8, 0x503f),
];

/// How many channels a `channelConfiguration` implies.
///
/// `None` for configuration 0 (the count lives in a program config element
/// inside the payload) and for the reserved values.
#[must_use]
pub fn channels_for_config(config: u8) -> Option<u32> {
    CHANNEL_CONFIGURATION
        .iter()
        .find(|&&(c, _, _)| c == config)
        .map(|&(_, n, _)| n)
}

/// The layout a `channelConfiguration` implies, in `vaco-chlayout`'s vocabulary.
#[must_use]
pub fn layout_for_config(config: u8) -> Option<ChannelLayout> {
    CHANNEL_CONFIGURATION
        .iter()
        .find(|&&(c, _, _)| c == config)
        .and_then(|&(_, _, mask)| ChannelLayout::from_mask(mask))
}

/// Whether a `channelConfiguration` is one the reference accepts.
///
/// Configuration 0 is *syntactically* legal — it defers to a program config
/// element — so it is not rejected here; it simply yields no channel count.
#[must_use]
pub const fn is_reserved_config(config: u8) -> bool {
    matches!(config, 8..=10 | 15..)
}

/// The display name `ffprobe` prints for an AAC profile value.
///
/// The profile value is `audioObjectType - 1`; see
/// [`crate::AudioObjectType::profile`]. Values with no name print as the bare
/// integer, which is what `-show_streams` does for e.g. ER AAC LC (16).
#[must_use]
pub const fn profile_name(value: i32) -> Option<&'static str> {
    Some(match value {
        0 => "Main",
        1 => "LC",
        2 => "SSR",
        3 => "LTP",
        4 => "HE-AAC",
        22 => "LD",
        28 => "HE-AACv2",
        38 => "ELD",
        _ => return None,
    })
}
