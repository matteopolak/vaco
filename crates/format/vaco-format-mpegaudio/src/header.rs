//! The 4-byte frame header shared by every MPEG-1/2/2.5 Layer I/II/III frame.

/// `1111 1111 111` — the 11-bit frame sync, MSB-aligned in the first 21 bits
/// of the header word.
const SYNC_MASK: u32 = 0x7FF << 21;
const SYNC_VALUE: u32 = 0x7FF << 21;

/// `MPEG Audio version ID` (header bits 20-19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Mpeg25,
    Mpeg2,
    Mpeg1,
}

impl Version {
    const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            0b00 => Some(Self::Mpeg25),
            0b10 => Some(Self::Mpeg2),
            0b11 => Some(Self::Mpeg1),
            _ => None,
        }
    }

    /// Whether the low-sample-rate extension's bit-allocation and
    /// scalefactor-band tables apply, as opposed to the MPEG-1 ones.
    #[must_use]
    pub const fn is_low_sample_rate(self) -> bool {
        matches!(self, Self::Mpeg2 | Self::Mpeg25)
    }

    const fn to_bits(self) -> u32 {
        match self {
            Self::Mpeg25 => 0b00,
            Self::Mpeg2 => 0b10,
            Self::Mpeg1 => 0b11,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    I,
    II,
    III,
}

impl Layer {
    const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            0b11 => Some(Self::I),
            0b10 => Some(Self::II),
            0b01 => Some(Self::III),
            _ => None,
        }
    }

    const fn to_bits(self) -> u32 {
        match self {
            Self::I => 0b11,
            Self::II => 0b10,
            Self::III => 0b01,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    Stereo,
    JointStereo,
    DualChannel,
    Mono,
}

impl ChannelMode {
    const fn from_bits(bits: u32) -> Self {
        match bits {
            0b00 => Self::Stereo,
            0b01 => Self::JointStereo,
            0b10 => Self::DualChannel,
            _ => Self::Mono,
        }
    }

    #[must_use]
    pub const fn channels(self) -> u8 {
        if matches!(self, Self::Mono) { 1 } else { 2 }
    }

    const fn to_bits(self) -> u32 {
        match self {
            Self::Stereo => 0b00,
            Self::JointStereo => 0b01,
            Self::DualChannel => 0b10,
            Self::Mono => 0b11,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    None,
    Ms5015,
    Reserved,
    CcittJ17,
}

impl Emphasis {
    const fn from_bits(bits: u32) -> Self {
        match bits {
            0b00 => Self::None,
            0b01 => Self::Ms5015,
            0b10 => Self::Reserved,
            _ => Self::CcittJ17,
        }
    }

    const fn to_bits(self) -> u32 {
        match self {
            Self::None => 0b00,
            Self::Ms5015 => 0b01,
            Self::Reserved => 0b10,
            Self::CcittJ17 => 0b11,
        }
    }
}

/// Bit rate tables in kbps, one row per (version family, layer). Index 0 is
/// free-format and 15 is the forbidden value; both are rejected by
/// [`MpegAudioHeader::parse`] before a row is ever selected.
///
/// `Vaco-Spec-Ref: iso-11172-3` Table B.1 (MPEG-1) and
/// `Vaco-Spec-Ref: iso-13818-3` Table B.1 (the low-sample-rate extension,
/// which is one table shared by Layer II and Layer III).
const BITRATE_MPEG1_I: [u16; 16] = [
    0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0,
];
const BITRATE_MPEG1_II: [u16; 16] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
];
const BITRATE_MPEG1_III: [u16; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const BITRATE_LSF_I: [u16; 16] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0,
];
const BITRATE_LSF_II_III: [u16; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];

/// One frame's fixed-size header, plus the fields it decodes to (bit rate,
/// sample rate, frame length, side-info length) via the tables above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent single-bit field the spec defines separately"
)]
pub struct MpegAudioHeader {
    pub version: Version,
    pub layer: Layer,
    pub has_crc: bool,
    pub bitrate_index: u8,
    pub sample_rate_index: u8,
    pub padding: bool,
    pub private_bit: bool,
    pub channel_mode: ChannelMode,
    pub mode_extension: u8,
    pub copyright: bool,
    pub original: bool,
    pub emphasis: Emphasis,
}

impl MpegAudioHeader {
    pub const LEN: usize = 4;

    /// Parse the 4-byte header at the front of `word` (its top 32 bits).
    ///
    /// Rejects a reserved version, reserved layer, forbidden (`1111`) bit
    /// rate, or reserved sample rate — a syntactically valid header per
    /// `Vaco-Spec-Ref: iso-11172-3` §2.4.1.3, not merely a matched sync
    /// pattern. Free-format (bit rate index 0) is accepted: its length has to
    /// be measured against the next sync, which this type does not do.
    #[must_use]
    pub fn parse(word: u32) -> Option<Self> {
        if word & SYNC_MASK != SYNC_VALUE {
            return None;
        }
        let version = Version::from_bits((word >> 19) & 0b11)?;
        let layer = Layer::from_bits((word >> 17) & 0b11)?;
        let has_crc = (word >> 16) & 1 == 0;
        let bitrate_index = ((word >> 12) & 0b1111) as u8;
        let sample_rate_index = ((word >> 10) & 0b11) as u8;
        if bitrate_index == 0b1111 || sample_rate_index == 0b11 {
            return None;
        }
        let padding = (word >> 9) & 1 == 1;
        let private_bit = (word >> 8) & 1 == 1;
        let channel_mode = ChannelMode::from_bits((word >> 6) & 0b11);
        let mode_extension = ((word >> 4) & 0b11) as u8;
        let copyright = (word >> 3) & 1 == 1;
        let original = (word >> 2) & 1 == 1;
        let emphasis = Emphasis::from_bits(word & 0b11);
        Some(Self {
            version,
            layer,
            has_crc,
            bitrate_index,
            sample_rate_index,
            padding,
            private_bit,
            channel_mode,
            mode_extension,
            copyright,
            original,
            emphasis,
        })
    }

    /// Parse the header at `data[0..4]`. `None` if `data` is shorter than
    /// [`Self::LEN`] or the bytes fail [`Self::parse`].
    #[must_use]
    pub fn parse_bytes(data: &[u8]) -> Option<Self> {
        let [a, b, c, d] = *data.first_chunk::<4>()?;
        Self::parse(u32::from_be_bytes([a, b, c, d]))
    }

    /// Encode back to the 4-byte word [`Self::parse`] reads. The inverse of
    /// `parse`: `parse(h.to_word())` round-trips for every `h` `parse`
    /// itself could have produced.
    #[must_use]
    pub const fn to_word(self) -> u32 {
        SYNC_VALUE
            | (self.version.to_bits() << 19)
            | (self.layer.to_bits() << 17)
            | ((!self.has_crc as u32) << 16)
            | ((self.bitrate_index as u32) << 12)
            | ((self.sample_rate_index as u32) << 10)
            | ((self.padding as u32) << 9)
            | ((self.private_bit as u32) << 8)
            | (self.channel_mode.to_bits() << 6)
            | ((self.mode_extension as u32) << 4)
            | ((self.copyright as u32) << 3)
            | ((self.original as u32) << 2)
            | self.emphasis.to_bits()
    }

    #[must_use]
    pub const fn to_bytes(self) -> [u8; 4] {
        self.to_word().to_be_bytes()
    }

    const fn bitrate_row(self) -> &'static [u16; 16] {
        match (matches!(self.version, Version::Mpeg1), self.layer) {
            (true, Layer::I) => &BITRATE_MPEG1_I,
            (true, Layer::II) => &BITRATE_MPEG1_II,
            (true, Layer::III) => &BITRATE_MPEG1_III,
            (false, Layer::I) => &BITRATE_LSF_I,
            (false, Layer::II | Layer::III) => &BITRATE_LSF_II_III,
        }
    }

    /// `None` for free-format (index 0); the forbidden index 15 cannot occur
    /// on a value [`Self::parse`] produced.
    #[must_use]
    pub fn bitrate_kbps(self) -> Option<u16> {
        if self.bitrate_index == 0 {
            return None;
        }
        self.bitrate_row()
            .get(usize::from(self.bitrate_index))
            .copied()
    }

    /// `Vaco-Spec-Ref: iso-11172-3` Table 2.4.2.5's three rates per version.
    #[must_use]
    pub const fn sample_rate_hz(self) -> u32 {
        match (self.version, self.sample_rate_index) {
            (Version::Mpeg1, 0) => 44100,
            (Version::Mpeg1, 1) => 48000,
            (Version::Mpeg1, _) => 32000,
            (Version::Mpeg2, 0) => 22050,
            (Version::Mpeg2, 1) => 24000,
            (Version::Mpeg2, _) => 16000,
            (Version::Mpeg25, 0) => 11025,
            (Version::Mpeg25, 1) => 12000,
            (Version::Mpeg25, _) => 8000,
        }
    }

    #[must_use]
    pub const fn channels(self) -> u8 {
        self.channel_mode.channels()
    }

    /// `384` for Layer I; `1152` for Layer II; `1152` for Layer III at
    /// MPEG-1, halved to `576` by the low-sample-rate extension.
    #[must_use]
    pub const fn samples_per_frame(self) -> u32 {
        match self.layer {
            Layer::I => 384,
            Layer::II => 1152,
            Layer::III => {
                if matches!(self.version, Version::Mpeg1) {
                    1152
                } else {
                    576
                }
            }
        }
    }

    /// The frame's total length in bytes, header included. `None` for
    /// free-format, whose length the caller must measure against the next
    /// sync (`Vaco-Spec-Ref: iso-11172-3` §2.4.3.1, "free format").
    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "the frame-length formula is defined by the spec as a floor division; the padding term does not change that"
    )]
    pub fn frame_len(self) -> Option<u32> {
        let kbps = u32::from(self.bitrate_kbps()?);
        let rate = self.sample_rate_hz();
        if rate == 0 {
            return None;
        }
        let bits_per_sec = kbps * 1000;
        let padding = u32::from(self.padding);
        Some(match self.layer {
            Layer::I => (12 * bits_per_sec / rate + padding) * 4,
            Layer::II => 144 * bits_per_sec / rate + padding,
            Layer::III => {
                let coeff = if matches!(self.version, Version::Mpeg1) {
                    144
                } else {
                    72
                };
                coeff * bits_per_sec / rate + padding
            }
        })
    }

    /// Layer III side-information length in bytes, right after the header (and
    /// the optional CRC): 32/17 at MPEG-1 stereo/mono, 17/9 under the
    /// low-sample-rate extension. `None` outside Layer III.
    ///
    /// `Vaco-Spec-Ref: iso-11172-3` §2.4.1.7 (Table 2.4.1.1's side info sizes)
    /// and `Vaco-Spec-Ref: iso-13818-3` Annex A for the halved MPEG-2 sizes.
    #[must_use]
    pub const fn side_info_len(self) -> Option<usize> {
        if !matches!(self.layer, Layer::III) {
            return None;
        }
        let stereo = self.channels() == 2;
        Some(match (matches!(self.version, Version::Mpeg1), stereo) {
            (true, true) => 32,
            (true, false) | (false, true) => 17,
            (false, false) => 9,
        })
    }

    #[must_use]
    pub const fn crc_len(self) -> usize {
        if self.has_crc { 2 } else { 0 }
    }
}

/// The `(version, sample_rate_index)` pair whose [`MpegAudioHeader::sample_rate_hz`]
/// is `hz`, for a caller building a header from a sample rate rather than
/// parsing one. `None` for anything not among the nine valid rates.
#[must_use]
pub const fn version_for_sample_rate(hz: u32) -> Option<(Version, u8)> {
    match hz {
        44100 => Some((Version::Mpeg1, 0)),
        48000 => Some((Version::Mpeg1, 1)),
        32000 => Some((Version::Mpeg1, 2)),
        22050 => Some((Version::Mpeg2, 0)),
        24000 => Some((Version::Mpeg2, 1)),
        16000 => Some((Version::Mpeg2, 2)),
        11025 => Some((Version::Mpeg25, 0)),
        12000 => Some((Version::Mpeg25, 1)),
        8000 => Some((Version::Mpeg25, 2)),
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn to_word_round_trips_through_parse() {
        let h = MpegAudioHeader::parse(header_word(0b11, 0b01, 9, 0, 0b00)).expect("valid header");
        assert_eq!(MpegAudioHeader::parse(h.to_word()), Some(h));
    }

    #[test]
    fn version_for_sample_rate_covers_every_valid_rate() {
        for hz in [
            44100u32, 48000, 32000, 22050, 24000, 16000, 11025, 12000, 8000,
        ] {
            let (version, idx) = version_for_sample_rate(hz).expect("valid rate");
            let h = MpegAudioHeader {
                version,
                layer: Layer::III,
                has_crc: false,
                bitrate_index: 4,
                sample_rate_index: idx,
                padding: false,
                private_bit: false,
                channel_mode: ChannelMode::Mono,
                mode_extension: 0,
                copyright: false,
                original: false,
                emphasis: Emphasis::None,
            };
            assert_eq!(h.sample_rate_hz(), hz);
        }
        assert!(version_for_sample_rate(44099).is_none());
    }

    fn header_word(version: u32, layer: u32, bitrate: u32, rate: u32, mode: u32) -> u32 {
        (0x7FFu32 << 21)
            | (version << 19)
            | (layer << 17)
            | (1 << 16)
            | (bitrate << 12)
            | (rate << 10)
            | (mode << 6)
    }

    #[test]
    fn mpeg1_layer3_128kbps_44100_stereo_frame_len_is_417() {
        let h = MpegAudioHeader::parse(header_word(0b11, 0b01, 9, 0, 0b00)).expect("valid header");
        assert_eq!(h.bitrate_kbps(), Some(128));
        assert_eq!(h.sample_rate_hz(), 44100);
        assert_eq!(h.frame_len(), Some(417));
        assert_eq!(h.side_info_len(), Some(32));
    }

    #[test]
    fn reserved_fields_are_rejected() {
        assert!(MpegAudioHeader::parse(header_word(0b01, 0b01, 8, 0, 0b00)).is_none());
        assert!(MpegAudioHeader::parse(header_word(0b11, 0b00, 8, 0, 0b00)).is_none());
        assert!(MpegAudioHeader::parse(header_word(0b11, 0b01, 0b1111, 0, 0b00)).is_none());
        assert!(MpegAudioHeader::parse(header_word(0b11, 0b01, 8, 0b11, 0b00)).is_none());
    }

    #[test]
    fn non_sync_is_rejected() {
        assert!(MpegAudioHeader::parse(0).is_none());
    }

    #[test]
    fn mpeg2_layer3_low_sample_rate_side_info_is_smaller() {
        let h = MpegAudioHeader::parse(header_word(0b10, 0b01, 8, 0, 0b00)).expect("valid header");
        assert_eq!(h.samples_per_frame(), 576);
        assert_eq!(h.side_info_len(), Some(17));
        let mono =
            MpegAudioHeader::parse(header_word(0b10, 0b01, 8, 0, 0b11)).expect("valid header");
        assert_eq!(mono.side_info_len(), Some(9));
    }
}
