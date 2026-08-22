//! The two vocabularies: individual channels, and the standard layouts.
//!
//! # Provenance
//!
//! Both tables are **interface facts**, not authorial choice (D7/D9): the
//! spellings are what the reference tool emits and accepts, and a command line
//! or an `ffprobe` field that does not match them byte for byte is a conformance
//! failure. They were recorded by probing the shipped binary — never by reading
//! its source — as follows, against `FFmpeg` 8.1:
//!
//! * `ffmpeg -hide_banner -layouts` prints the `NAME`/`DESCRIPTION` pairs of
//!   [`CHANNELS`] and the `NAME`/`DECOMPOSITION` pairs of [`LAYOUTS`], in the
//!   order both are listed here. That order is itself observable, so it is
//!   preserved rather than sorted.
//! * The **bit index** of each channel is not printed anywhere. It was recovered
//!   from the `USR<n>` parse form, which names a channel by its numeric id:
//!   feeding `-ch_layout USR<n>` for `n` in `0..=70` and reading back the name
//!   the tool prints yields the whole assignment, holes included. See
//!   `docs/model/vaco-chlayout.md` for the transcript.
//! * The masks in [`LAYOUTS`] were then computed from the decompositions and
//!   verified in both directions — `-ch_layout 0x1f80003ffff` must print `22.2`,
//!   and a `5.1` WAV must carry `dwChannelMask = 0x3f` in its
//!   `WAVE_FORMAT_EXTENSIBLE` header.
//!
//! # Why bits 0..=17 are not ours to choose
//!
//! The first eighteen positions are Microsoft's published `dwChannelMask` bit
//! assignment for `WAVE_FORMAT_EXTENSIBLE`, in that order. Every container that
//! carries a channel mask carries *that* mask, so the numbering is dictated by
//! the format rather than selected — merger, in D9's terms. Everything from bit
//! 29 up is the reference's own extension of the same idea; the gaps at 18..=28,
//! 45..=60 and 63 are unassigned and print as `USR<n>`.
//!
//! # 22.2
//!
//! The 24-channel `22.2` layout is SMPTE ST 2036-2's arrangement (also
//! catalogued by ITU-R BS.2051 as System H). The *positions* come from those
//! specifications; the short names and the layout's spelling come from the
//! reference.

use crate::Channel;

/// `(channel, bit index, short name, description)` for every channel that has a
/// name, in bit order — which is also the order `-layouts` prints them in.
///
/// The description column is the human-readable text `-layouts` prints beside
/// the name. It is display text, so it is reproduced exactly.
#[rustfmt::skip]
pub(crate) const CHANNELS: [(Channel, u8, &str, &str); 36] = [
    (Channel::FrontLeft,           0,  "FL",   "front left"),
    (Channel::FrontRight,          1,  "FR",   "front right"),
    (Channel::FrontCenter,         2,  "FC",   "front center"),
    (Channel::LowFrequency,        3,  "LFE",  "low frequency"),
    (Channel::BackLeft,            4,  "BL",   "back left"),
    (Channel::BackRight,           5,  "BR",   "back right"),
    (Channel::FrontLeftOfCenter,   6,  "FLC",  "front left-of-center"),
    (Channel::FrontRightOfCenter,  7,  "FRC",  "front right-of-center"),
    (Channel::BackCenter,          8,  "BC",   "back center"),
    (Channel::SideLeft,            9,  "SL",   "side left"),
    (Channel::SideRight,           10, "SR",   "side right"),
    (Channel::TopCenter,           11, "TC",   "top center"),
    (Channel::TopFrontLeft,        12, "TFL",  "top front left"),
    (Channel::TopFrontCenter,      13, "TFC",  "top front center"),
    (Channel::TopFrontRight,       14, "TFR",  "top front right"),
    (Channel::TopBackLeft,         15, "TBL",  "top back left"),
    (Channel::TopBackCenter,       16, "TBC",  "top back center"),
    (Channel::TopBackRight,        17, "TBR",  "top back right"),
    // Bits 18..=28 are unassigned and print as `USR18`..`USR28`.
    (Channel::DownmixLeft,         29, "DL",   "downmix left"),
    (Channel::DownmixRight,        30, "DR",   "downmix right"),
    (Channel::WideLeft,            31, "WL",   "wide left"),
    (Channel::WideRight,           32, "WR",   "wide right"),
    (Channel::SurroundDirectLeft,  33, "SDL",  "surround direct left"),
    (Channel::SurroundDirectRight, 34, "SDR",  "surround direct right"),
    (Channel::LowFrequency2,       35, "LFE2", "low frequency 2"),
    (Channel::TopSideLeft,         36, "TSL",  "top side left"),
    (Channel::TopSideRight,        37, "TSR",  "top side right"),
    (Channel::BottomFrontCenter,   38, "BFC",  "bottom front center"),
    (Channel::BottomFrontLeft,     39, "BFL",  "bottom front left"),
    (Channel::BottomFrontRight,    40, "BFR",  "bottom front right"),
    (Channel::SideSurroundLeft,    41, "SSL",  "side surround left"),
    (Channel::SideSurroundRight,   42, "SSR",  "side surround right"),
    (Channel::TopSurroundLeft,     43, "TTL",  "top surround left"),
    (Channel::TopSurroundRight,    44, "TTR",  "top surround right"),
    // Bits 45..=60 are unassigned.
    (Channel::BinauralLeft,        61, "BIL",  "binaural left"),
    (Channel::BinauralRight,       62, "BIR",  "binaural right"),
    // Bit 63 is unassigned; a mask of 0x8000_0000_0000_0000 is `USR63`.
];

/// `(name, mask)` for every standard layout, in `-layouts` listing order.
///
/// **The order is load-bearing twice over.** It is the order `-layouts` prints,
/// and it is what [`crate::ChannelLayout::default_for`] resolves against: the
/// `<n>c` parse form means "the first entry with `n` channels", so moving
/// `5.1` after `6.0` would silently change what `-ch_layout 6c` produces.
///
/// No two entries share a mask, so the reverse lookup in
/// [`crate::ChannelLayout::name`] is unambiguous; `named_masks_are_unique`
/// asserts it.
#[rustfmt::skip]
pub(crate) const LAYOUTS: [(&str, u64); 40] = [
    ("mono",           0x4),
    ("stereo",         0x3),
    ("2.1",            0xb),
    ("3.0",            0x7),
    ("3.0(back)",      0x103),
    ("4.0",            0x107),
    ("quad",           0x33),
    ("quad(side)",     0x603),
    ("3.1",            0xf),
    ("5.0",            0x37),
    ("5.0(side)",      0x607),
    ("4.1",            0x10f),
    ("5.1",            0x3f),
    ("5.1(side)",      0x60f),
    ("6.0",            0x707),
    ("6.0(front)",     0x6c3),
    ("3.1.2",          0x500f),
    ("hexagonal",      0x137),
    ("6.1",            0x70f),
    ("6.1(back)",      0x13f),
    ("6.1(front)",     0x6cb),
    ("7.0",            0x637),
    ("7.0(front)",     0x6c7),
    ("7.1",            0x63f),
    ("7.1(wide)",      0xff),
    ("7.1(wide-side)", 0x6cf),
    ("5.1.2",          0x560f),
    ("5.1.2(back)",    0x503f),
    ("octagonal",      0x737),
    ("cube",           0x2d033),
    ("5.1.4",          0x2d60f),
    ("7.1.2",          0x563f),
    ("7.1.4",          0x2d63f),
    ("7.2.3",          0x8_0001_563f),
    ("9.1.4",          0x2d6ff),
    ("9.1.6",          0x30_0002_d6ff),
    ("hexadecagonal",  0x1_8003_f737),
    ("binaural",       0x6000_0000_0000_0000),
    ("downmix",        0x6000_0000),
    ("22.2",           0x1f8_0003_ffff),
];
