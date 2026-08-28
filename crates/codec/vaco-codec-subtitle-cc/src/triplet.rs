//! `cc_data` triplet framing.
//!
//! A `cc_data` byte stream (ANSI/CTA-708 Table 2, reproduced as
//! `MPEG_cc_data()` in ATSC A/53 Part 4 §6.2.3.1) is a sequence of 3-byte
//! triplets: a marker/valid/type byte, then two data bytes. The top 5 bits of
//! the first byte are marker bits (conventionally all set); this crate
//! ignores them rather than rejecting a triplet whose encoder set them
//! differently, matching this project's "detection is strict, demuxing is
//! lenient" rule — a decoder's job is to recover what it can, not to police
//! an upstream encoder's marker bits.

/// The four kinds a `cc_data` triplet's `cc_type` field distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcType {
    /// Line-21 field 1 byte pair (CC1/CC2, `cc_type` = `0b00`).
    Ntsc608Field1,
    /// Line-21 field 2 byte pair (CC3/CC4, `cc_type` = `0b01`).
    Ntsc608Field2,
    /// DTVCC packet data continuation (`cc_type` = `0b10`).
    Dtvcc708PacketData,
    /// DTVCC packet start: `data[0]` is the packet header byte
    /// (sequence number and size), `data[1]` is the first payload byte
    /// (`cc_type` = `0b11`).
    Dtvcc708PacketStart,
}

impl CcType {
    const fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0b00 => Self::Ntsc608Field1,
            0b01 => Self::Ntsc608Field2,
            0b10 => Self::Dtvcc708PacketData,
            _ => Self::Dtvcc708PacketStart,
        }
    }
}

/// One parsed `cc_data` triplet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Triplet {
    /// This triplet's kind.
    pub cc_type: CcType,
    /// The two data bytes (`cc_data_1`, `cc_data_2`).
    pub data: [u8; 2],
}

/// Iterate the triplets in a `cc_data` byte slice.
///
/// `cc_valid == 0` triplets are padding (used to hold the fixed 9600 bit/s
/// DTVCC bandwidth allocation open when there is nothing to send) and are
/// skipped; a trailing 1- or 2-byte remainder that cannot form a full triplet
/// is skipped too. Both increment `skipped` so the drop is countable.
pub fn iter_triplets<'a>(
    cc_data: &'a [u8],
    skipped: &'a mut u64,
) -> impl Iterator<Item = Triplet> + 'a {
    cc_data
        .chunks(3)
        .filter_map(move |chunk| {
            let [marker_and_type, d1, d2] = chunk else {
                *skipped += 1;
                return None;
            };
            let cc_valid = (marker_and_type & 0x04) != 0;
            if !cc_valid {
                *skipped += 1;
                return None;
            }
            Some(Triplet {
                cc_type: CcType::from_bits(*marker_and_type),
                data: [*d1, *d2],
            })
        })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn skips_invalid_and_padding() {
        let mut skipped = 0;
        let data = [
            0xFC, 0x80, 0x80, // marker=11111, valid=1, type=00 (field 1)
            0xF8, 0x00, 0x00, // cc_valid unset -> skipped
            0x00, // trailing partial triplet -> skipped
        ];
        let triplets: Vec<_> = iter_triplets(&data, &mut skipped).collect();
        assert_eq!(triplets.len(), 1);
        assert_eq!(triplets[0].cc_type, CcType::Ntsc608Field1);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn cc_type_bits() {
        assert_eq!(CcType::from_bits(0b000), CcType::Ntsc608Field1);
        assert_eq!(CcType::from_bits(0b001), CcType::Ntsc608Field2);
        assert_eq!(CcType::from_bits(0b010), CcType::Dtvcc708PacketData);
        assert_eq!(CcType::from_bits(0b011), CcType::Dtvcc708PacketStart);
    }
}
