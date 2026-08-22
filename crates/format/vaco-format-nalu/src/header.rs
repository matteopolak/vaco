//! The NAL unit header, in the three layouts the H.26x family uses.
//!
//! Written from ITU-T H.264 §7.3.1, ITU-T H.265 §7.3.1.2 and ITU-T H.266
//! §7.3.1.2.
//!
//! Three codecs, three bit layouts, one struct. The alternative — a header type
//! per codec — makes every framing utility generic over a trait to read one or
//! two bytes, which is more machinery than the problem deserves. The *meaning*
//! of `nal_type` is codec-specific and stays in the codec crate; only the
//! layout lives here.

/// Which codec's header layout to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKind {
    /// H.264: one byte — `forbidden_zero_bit(1) nal_ref_idc(2) nal_unit_type(5)`.
    H264,
    /// HEVC: two bytes — `forbidden_zero_bit(1) nal_unit_type(6)
    /// nuh_layer_id(6) nuh_temporal_id_plus1(3)`.
    H265,
    /// VVC: two bytes — `forbidden_zero_bit(1) nuh_reserved_zero_bit(1)
    /// nuh_layer_id(6) nal_unit_type(5) nuh_temporal_id_plus1(3)`.
    ///
    /// Note the order differs from HEVC: the layer id comes *first*.
    H266,
}

impl HeaderKind {
    /// Bytes the header occupies: one for H.264, two for HEVC and VVC.
    #[must_use]
    #[allow(clippy::len_without_is_empty, reason = "a NAL header is never empty")]
    pub const fn len(self) -> usize {
        match self {
            Self::H264 => 1,
            Self::H265 | Self::H266 => 2,
        }
    }
}

/// A decoded NAL unit header.
///
/// Fields absent from a given codec's header take their neutral value:
/// `nal_ref_idc` is 0 outside H.264, and `nuh_layer_id`/`temporal_id` are 0 in
/// H.264 (which has a single layer and no temporal id in the header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NalHeader {
    /// Which layout this was read with.
    pub kind: HeaderKind,
    /// `forbidden_zero_bit`. **Must** be 0 in a conforming stream; a 1 here is
    /// the cheapest available "these bytes are not a NAL unit" signal, and
    /// [`NalHeader::is_conforming`] is what a resynchronising parser checks.
    pub forbidden_zero_bit: bool,
    /// `nal_unit_type`, in the codec's own numbering. 5 bits in H.264 and VVC,
    /// 6 in HEVC.
    pub nal_unit_type: u8,
    /// `nal_ref_idc`: H.264 only, 0 means the unit is not used for reference.
    pub nal_ref_idc: u8,
    /// `nuh_layer_id`: HEVC and VVC. 0 is the base layer.
    pub nuh_layer_id: u8,
    /// `TemporalId` — that is, `nuh_temporal_id_plus1 - 1`. HEVC and VVC.
    ///
    /// `nuh_temporal_id_plus1` is required to be non-zero, so a zero encoding
    /// is malformed; it is reported here as `temporal_id_plus1 == 0` rather
    /// than by wrapping.
    pub temporal_id: u8,
    /// The raw `nuh_temporal_id_plus1`, so a caller can detect the forbidden 0.
    pub temporal_id_plus1: u8,
}

impl NalHeader {
    /// Read a header from the front of a NAL unit's bytes.
    ///
    /// Returns `None` if the unit is shorter than the header.
    #[must_use]
    pub fn parse(kind: HeaderKind, nal: &[u8]) -> Option<Self> {
        match kind {
            HeaderKind::H264 => {
                let b = *nal.first()?;
                Some(Self {
                    kind,
                    forbidden_zero_bit: b & 0x80 != 0,
                    nal_unit_type: b & 0x1F,
                    nal_ref_idc: (b >> 5) & 0x03,
                    nuh_layer_id: 0,
                    temporal_id: 0,
                    temporal_id_plus1: 1,
                })
            }
            HeaderKind::H265 => {
                let (&a, &b) = (nal.first()?, nal.get(1)?);
                let tid_plus1 = b & 0x07;
                Some(Self {
                    kind,
                    forbidden_zero_bit: a & 0x80 != 0,
                    nal_unit_type: (a >> 1) & 0x3F,
                    nal_ref_idc: 0,
                    nuh_layer_id: ((a & 0x01) << 5) | (b >> 3),
                    temporal_id: tid_plus1.saturating_sub(1),
                    temporal_id_plus1: tid_plus1,
                })
            }
            HeaderKind::H266 => {
                let (&a, &b) = (nal.first()?, nal.get(1)?);
                let tid_plus1 = b & 0x07;
                Some(Self {
                    kind,
                    forbidden_zero_bit: a & 0x80 != 0,
                    nal_unit_type: (b >> 3) & 0x1F,
                    nal_ref_idc: 0,
                    nuh_layer_id: a & 0x3F,
                    temporal_id: tid_plus1.saturating_sub(1),
                    temporal_id_plus1: tid_plus1,
                })
            }
        }
    }

    /// Whether the header satisfies the constraints every conforming stream
    /// obeys: `forbidden_zero_bit == 0`, and a non-zero
    /// `nuh_temporal_id_plus1` where the codec has one.
    #[must_use]
    pub const fn is_conforming(self) -> bool {
        !self.forbidden_zero_bit && self.temporal_id_plus1 != 0
    }

    /// The payload after the header, given the whole unit.
    #[must_use]
    pub fn payload<'a>(&self, nal: &'a [u8]) -> &'a [u8] {
        nal.get(self.kind.len()..).unwrap_or(&[])
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn h264_sps_header() {
        // 0x67 = 0 11 00111: nal_ref_idc 3, type 7 (SPS).
        let h = NalHeader::parse(HeaderKind::H264, &[0x67]).expect("one byte is enough");
        assert!(!h.forbidden_zero_bit);
        assert_eq!(h.nal_ref_idc, 3);
        assert_eq!(h.nal_unit_type, 7);
        assert!(h.is_conforming());
    }

    #[test]
    fn h264_non_reference_slice() {
        // 0x01 = 0 00 00001: nal_ref_idc 0, type 1.
        let h = NalHeader::parse(HeaderKind::H264, &[0x01]).expect("one byte is enough");
        assert_eq!((h.nal_ref_idc, h.nal_unit_type), (0, 1));
    }

    #[test]
    fn h264_forbidden_bit_is_reported_not_masked() {
        let h = NalHeader::parse(HeaderKind::H264, &[0xE7]).expect("one byte is enough");
        assert!(h.forbidden_zero_bit);
        assert!(!h.is_conforming());
        // The rest is still decoded, so a resynchroniser can log what it saw.
        assert_eq!(h.nal_unit_type, 7);
    }

    #[test]
    fn hevc_vps_header() {
        // 0x40 0x01 = forbidden 0, type 32 (VPS), layer 0, tid_plus1 1.
        let h = NalHeader::parse(HeaderKind::H265, &[0x40, 0x01]).expect("two bytes");
        assert_eq!(h.nal_unit_type, 32);
        assert_eq!(h.nuh_layer_id, 0);
        assert_eq!(h.temporal_id, 0);
    }

    #[test]
    fn hevc_layer_id_spans_the_byte_boundary() {
        // layer id 33 = 0b100001 -> high bit into byte 0, low five into byte 1.
        let h = NalHeader::parse(HeaderKind::H265, &[0x41, 0b0000_1001]).expect("two bytes");
        assert_eq!(h.nuh_layer_id, 33);
        assert_eq!(h.temporal_id_plus1, 1);
    }

    #[test]
    fn vvc_puts_the_layer_id_first() {
        // byte0 = 0 0 000000 -> layer 0; byte1 = 01100 001 -> type 12, tid 0.
        let h = NalHeader::parse(HeaderKind::H266, &[0x00, 0b0110_0001]).expect("two bytes");
        assert_eq!(h.nuh_layer_id, 0);
        assert_eq!(h.nal_unit_type, 12);
        assert_eq!(h.temporal_id, 0);
    }

    #[test]
    fn zero_temporal_id_plus1_is_not_conforming() {
        let h = NalHeader::parse(HeaderKind::H265, &[0x40, 0x00]).expect("two bytes");
        assert_eq!(h.temporal_id_plus1, 0);
        assert_eq!(h.temporal_id, 0);
        assert!(!h.is_conforming());
    }

    #[test]
    fn short_units_yield_none_rather_than_panicking() {
        assert!(NalHeader::parse(HeaderKind::H264, &[]).is_none());
        assert!(NalHeader::parse(HeaderKind::H265, &[0x40]).is_none());
        assert!(NalHeader::parse(HeaderKind::H266, &[]).is_none());
    }
}
