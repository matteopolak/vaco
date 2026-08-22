//! Supplemental enhancement information, ITU-T H.265 §7.3.5 and Annex D.
//!
//! # What is decoded and what is not
//!
//! An SEI NAL unit is a sequence of `(payloadType, payloadSize, payload)`
//! messages. The framing is always parsed; the *payloads* are decoded only for
//! the messages that carry information a stream description needs, and every
//! other payload is kept as bytes. That is not laziness — an unrecognised
//! payload that is stored verbatim can be re-emitted exactly, whereas one that
//! is half-understood cannot.
//!
//! # Why the SPS is an argument
//!
//! `pic_timing` (§D.2.3) is unparsable without it. Whether it begins with
//! `pic_struct` depends on `frame_field_info_present_flag`, and whether the
//! `au_cpb_removal_delay_minus1` that follows is present — and how many bits
//! wide it is — depends on the VUI's HRD. An SEI parsed against the wrong SPS
//! produces plausible nonsense.
//!
//! # HEVC's version differs from H.264's in two ways worth knowing
//!
//! * **`pic_struct` lives behind `frame_field_info_present_flag`**, not H.264's
//!   `pic_struct_present_flag`, and Table D.2 has *thirteen* values rather than
//!   nine — 9 through 12 describe the top/bottom field pairing of a
//!   field-coded picture.
//! * **Prefix and suffix SEI are different NAL unit types** (39 and 40), where
//!   H.264 has one. The payload syntax is the same; which types may appear in
//!   which is Table D.1's business, not a parser's.

use vaco_bitstream::{BitReader, ByteReader};
use vaco_codec_core::FieldOrder;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::nal::HevcNalHeader;
use crate::sps::Sps;
use crate::util::{MAX_SEI_FF_BYTES, more_rbsp_data};

/// Payload types this crate decodes, Annex D Table D.1.
pub mod payload_type {
    /// `buffering_period`, §D.2.2.
    pub const BUFFERING_PERIOD: u32 = 0;
    /// `pic_timing`, §D.2.3.
    pub const PIC_TIMING: u32 = 1;
    /// `filler_payload`, §D.2.5.
    pub const FILLER: u32 = 3;
    /// `user_data_registered_itu_t_t35`, §D.2.6.
    pub const USER_DATA_REGISTERED: u32 = 4;
    /// `user_data_unregistered`, §D.2.7.
    pub const USER_DATA_UNREGISTERED: u32 = 5;
    /// `recovery_point`, §D.2.8.
    pub const RECOVERY_POINT: u32 = 6;
    /// `active_parameter_sets`, §D.2.21.
    pub const ACTIVE_PARAMETER_SETS: u32 = 129;
    /// `decoded_picture_hash`, §D.2.19. Suffix SEI only.
    pub const DECODED_PICTURE_HASH: u32 = 132;
    /// `mastering_display_colour_volume`, §D.2.28.
    pub const MASTERING_DISPLAY_COLOUR_VOLUME: u32 = 137;
    /// `content_light_level_info`, §D.2.35.
    pub const CONTENT_LIGHT_LEVEL: u32 = 144;
    /// `alternative_transfer_characteristics`, §D.2.38.
    pub const ALTERNATIVE_TRANSFER_CHARACTERISTICS: u32 = 147;
    /// `alpha_channel_info`, §F.14.2.
    pub const ALPHA_CHANNEL_INFO: u32 = 165;
}

/// `pic_struct`, Table D.2: how the picture's fields are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PicStruct(pub u8);

impl PicStruct {
    /// `NumClockTS`, Table D.2 — how many `sub_layer_hrd` timestamps follow.
    ///
    /// Zero for a value the table does not define, which stops the loop rather
    /// than guessing.
    #[must_use]
    pub const fn num_clock_ts(self) -> u32 {
        match self.0 {
            0..=2 | 9..=12 => 1,
            3 | 4 | 7 => 2,
            5 | 6 | 8 => 3,
            _ => 0,
        }
    }

    /// The field order this `pic_struct` implies.
    ///
    /// Table D.2's thirteen values collapse onto three answers:
    ///
    /// | `pic_struct` | meaning | field order |
    /// |---|---|---|
    /// | 0, 7, 8 | frame, doubling, tripling | progressive |
    /// | 1, 9, 11 | top field (paired or not) | top first |
    /// | 2, 10, 12 | bottom field | bottom first |
    /// | 3, 5 | top then bottom | top first |
    /// | 4, 6 | bottom then top | bottom first |
    ///
    /// Values 9 through 12 are HEVC's addition: a single field that is *paired*
    /// with the previous or next one in output order.
    #[must_use]
    pub const fn field_order(self) -> FieldOrder {
        match self.0 {
            0 | 7 | 8 => FieldOrder::Progressive,
            1 | 3 | 5 | 9 | 11 => FieldOrder::TopFirst,
            2 | 4 | 6 | 10 | 12 => FieldOrder::BottomFirst,
            _ => FieldOrder::Unknown,
        }
    }
}

/// A decoded SEI payload.
///
/// `Other` is not a failure: an unrecognised payload is kept whole so it can be
/// re-emitted or inspected later.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SeiPayload<'a> {
    /// §D.2.3.
    PicTiming {
        /// `pic_struct`, present only with `frame_field_info_present_flag`.
        pic_struct: Option<PicStruct>,
        /// `source_scan_type`: 0 interlaced, 1 progressive, 2 unknown.
        source_scan_type: Option<u8>,
        /// `duplicate_flag`.
        duplicate: Option<bool>,
        /// `au_cpb_removal_delay_minus1`, present only with an HRD.
        au_cpb_removal_delay_minus1: Option<u32>,
        /// `pic_dpb_output_delay`.
        pic_dpb_output_delay: Option<u32>,
    },
    /// §D.2.8: where a decoder joining the stream can start.
    RecoveryPoint {
        /// `recovery_poc_cnt` — signed in HEVC, where H.264's is unsigned.
        recovery_poc_cnt: i32,
        /// `exact_match_flag`.
        exact_match: bool,
        /// `broken_link_flag`.
        broken_link: bool,
    },
    /// §D.2.7: a 128-bit UUID and whatever the producer put after it. This is
    /// where `x265` writes its version and settings string.
    UserDataUnregistered {
        /// `uuid_iso_iec_11578`.
        uuid: [u8; 16],
        /// The rest of the payload.
        data: &'a [u8],
    },
    /// §D.2.6: `itu_t_t35_country_code` and the rest, uninterpreted.
    UserDataRegistered {
        /// `itu_t_t35_country_code`, plus the extension byte when it is 0xFF.
        country_code: u16,
        /// The rest of the payload.
        data: &'a [u8],
    },
    /// §D.2.28: HDR mastering display primaries and luminance.
    MasteringDisplay {
        /// `display_primaries_x/y` for the three primaries, in units of
        /// 0.00002. **Green, blue, red** in that order — HEVC's ordering, which
        /// is not the order a `mastering-display` metadata string uses.
        primaries: [(u16, u16); 3],
        /// `white_point_x/y`, same units.
        white_point: (u16, u16),
        /// `max_display_mastering_luminance`, in units of 0.0001 cd/m².
        max_luminance: u32,
        /// `min_display_mastering_luminance`, same units.
        min_luminance: u32,
    },
    /// §D.2.35: HDR content light levels, in cd/m².
    ContentLightLevel {
        /// `max_content_light_level`.
        max_content_light_level: u16,
        /// `max_pic_average_light_level`.
        max_pic_average_light_level: u16,
    },
    /// §D.2.38: the transfer function to use instead of the VUI's — how HLG is
    /// carried in a stream whose VUI says BT.709.
    AlternativeTransferCharacteristics {
        /// `preferred_transfer_characteristics`, an H.273 code point.
        preferred_transfer_characteristics: u8,
    },
    /// §D.2.19: an MD5, CRC or checksum over each decoded plane.
    DecodedPictureHash {
        /// `hash_type`: 0 MD5, 1 CRC, 2 checksum.
        hash_type: u8,
        /// The raw hash bytes for every plane, concatenated.
        data: &'a [u8],
    },
    /// §D.2.5: padding, and nothing else.
    Filler {
        /// How many bytes.
        len: usize,
    },
    /// Anything else, kept whole.
    Other {
        /// `payloadType`.
        payload_type: u32,
        /// The payload bytes.
        data: &'a [u8],
    },
}

/// One message from an SEI NAL unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeiMessage<'a> {
    /// `payloadType`.
    pub payload_type: u32,
    /// `payloadSize`, as declared. May exceed the bytes actually present, in
    /// which case [`SeiMessage::truncated`] is set and `payload` holds what
    /// there was.
    pub payload_size: u32,
    /// Whether the declared size ran past the end of the NAL unit.
    pub truncated: bool,
    /// Whether the unit was a suffix SEI (type 40) rather than a prefix one.
    pub suffix: bool,
    /// The decoded payload.
    pub payload: SeiPayload<'a>,
}

/// Parse an SEI NAL unit into its messages.
///
/// `sps` is the active sequence parameter set. Without it, `pic_timing` cannot
/// be decoded and is returned as [`SeiPayload::Other`] rather than guessed at.
///
/// # Errors
///
/// [`Error::InvalidData`] if the unit is not an SEI unit or its message header
/// is structurally impossible, [`Error::UnexpectedEof`] on truncation before
/// the first message, [`Error::LimitExceeded`] on a budget cap.
pub fn parse<'a>(
    rbsp: &'a [u8],
    sps: Option<&Sps>,
    budget: &mut Budget,
) -> Result<Vec<SeiMessage<'a>>> {
    let header = HevcNalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
    if !header.nal_unit_type.is_sei() {
        return Err(Error::InvalidData("not an SEI NAL unit"));
    }
    let suffix = header.nal_unit_type == crate::nal::NalUnitType::SUFFIX_SEI_NUT;
    let mut out = Vec::new();
    let mut reader = ByteReader::new(rbsp);
    reader.skip(2); // the two NAL header bytes

    // The loop is bounded by the bytes available: every message consumes at
    // least two (a type byte and a size byte), so `len / 2` is a hard ceiling
    // no malformed input can exceed.
    let max_messages = rbsp.len().div_ceil(2);
    for _ in 0..max_messages {
        if !has_more_messages(&reader, rbsp) {
            break;
        }
        let payload_type = read_ff_coded(&mut reader)?;
        let payload_size = read_ff_coded(&mut reader)?;
        budget.consume_fuel(1)?;

        let start = reader.pos();
        let available = reader.remaining();
        let want = payload_size as usize;
        let truncated = want > available;
        let take = want.min(available);
        let data = rbsp.get(start..start + take).unwrap_or(&[]);
        reader.skip(take);

        let payload = decode_payload(payload_type, data, sps);
        out.push(SeiMessage {
            payload_type,
            payload_size,
            truncated,
            suffix,
            payload,
        });
        if truncated {
            break;
        }
    }
    Ok(out)
}

/// Whether another `sei_message()` follows, §7.3.5.
fn has_more_messages(reader: &ByteReader<'_>, rbsp: &[u8]) -> bool {
    if reader.overrun() || reader.remaining() == 0 {
        return false;
    }
    let mut bits = BitReader::new(rbsp);
    bits.skip_bytes(reader.pos());
    more_rbsp_data(&bits, rbsp)
}

/// The `ff_byte`-prefixed integer coding `payloadType` and `payloadSize` share,
/// §7.3.5.
///
/// A run of `0xFF` each adding 255, then a final byte. The run is unbounded in
/// the syntax, so a NAL unit of nothing but `FF` bytes is a valid-looking header
/// with an astronomical payload type. Bounded by [`MAX_SEI_FF_BYTES`], and
/// additionally by the bytes actually present.
fn read_ff_coded(reader: &mut ByteReader<'_>) -> Result<u32> {
    let mut value: u32 = 0;
    for _ in 0..MAX_SEI_FF_BYTES {
        if reader.remaining() == 0 {
            return Err(Error::UnexpectedEof);
        }
        let b = reader.u8();
        value = value
            .checked_add(u32::from(b))
            .ok_or(Error::InvalidData("SEI payload header overflowed"))?;
        if b != 0xFF {
            return Ok(value);
        }
    }
    Err(Error::InvalidData(
        "SEI payload header has too many ff_bytes",
    ))
}

/// `sei_payload()`, Annex D, for the types this crate understands.
///
/// A payload that does not decode falls back to [`SeiPayload::Other`] rather
/// than failing the whole unit: one malformed message must not lose the ones
/// after it, and every one of these is advisory.
fn decode_payload<'a>(payload_type: u32, data: &'a [u8], sps: Option<&Sps>) -> SeiPayload<'a> {
    use crate::sei::payload_type as pt;
    let fallback = SeiPayload::Other { payload_type, data };
    match payload_type {
        pt::PIC_TIMING => match sps {
            Some(sps) => decode_pic_timing(data, sps).unwrap_or(fallback),
            None => fallback,
        },
        pt::RECOVERY_POINT => {
            use vaco_codec_golomb::GolombDecode;
            let mut r = BitReader::new(data);
            // §D.3.8 bounds `recovery_poc_cnt` by MaxPicOrderCntLsb / 2, which
            // is at most 2^15; the type's range is the loosest safe bound.
            let Ok(recovery_poc_cnt) = r.se_v_range(i32::MIN + 1, i32::MAX) else {
                return fallback;
            };
            let p = SeiPayload::RecoveryPoint {
                recovery_poc_cnt,
                exact_match: r.get_bit() != 0,
                broken_link: r.get_bit() != 0,
            };
            if r.overrun() { fallback } else { p }
        }
        pt::USER_DATA_UNREGISTERED => {
            let Some(head) = data.get(..16) else {
                return fallback;
            };
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(head);
            SeiPayload::UserDataUnregistered {
                uuid,
                data: data.get(16..).unwrap_or(&[]),
            }
        }
        pt::USER_DATA_REGISTERED => match data.first() {
            Some(&0xFF) => match data.get(1) {
                Some(&ext) => SeiPayload::UserDataRegistered {
                    country_code: 0xFF00 | u16::from(ext),
                    data: data.get(2..).unwrap_or(&[]),
                },
                None => fallback,
            },
            Some(&c) => SeiPayload::UserDataRegistered {
                country_code: u16::from(c),
                data: data.get(1..).unwrap_or(&[]),
            },
            None => fallback,
        },
        pt::MASTERING_DISPLAY_COLOUR_VOLUME => {
            if data.len() < 24 {
                return fallback;
            }
            let mut r = ByteReader::new(data);
            let primaries = [
                (r.be16(), r.be16()),
                (r.be16(), r.be16()),
                (r.be16(), r.be16()),
            ];
            SeiPayload::MasteringDisplay {
                primaries,
                white_point: (r.be16(), r.be16()),
                max_luminance: r.be32(),
                min_luminance: r.be32(),
            }
        }
        pt::CONTENT_LIGHT_LEVEL => {
            if data.len() < 4 {
                return fallback;
            }
            let mut r = ByteReader::new(data);
            SeiPayload::ContentLightLevel {
                max_content_light_level: r.be16(),
                max_pic_average_light_level: r.be16(),
            }
        }
        pt::ALTERNATIVE_TRANSFER_CHARACTERISTICS => match data.first() {
            Some(&t) => SeiPayload::AlternativeTransferCharacteristics {
                preferred_transfer_characteristics: t,
            },
            None => fallback,
        },
        pt::DECODED_PICTURE_HASH => match data.first() {
            Some(&hash_type) => SeiPayload::DecodedPictureHash {
                hash_type,
                data: data.get(1..).unwrap_or(&[]),
            },
            None => fallback,
        },
        pt::FILLER => SeiPayload::Filler { len: data.len() },
        _ => fallback,
    }
}

/// `pic_timing( payloadSize )`, §D.2.3.
///
/// Returns `None` when the payload runs out, which the caller turns into
/// [`SeiPayload::Other`].
fn decode_pic_timing<'a>(data: &[u8], sps: &Sps) -> Option<SeiPayload<'a>> {
    let vui = sps.vui.as_ref()?;
    let mut r = BitReader::new(data);
    let mut pic_struct = None;
    let mut source_scan_type = None;
    let mut duplicate = None;
    if vui.frame_field_info_present {
        pic_struct = Some(PicStruct(r.get(4) as u8));
        source_scan_type = Some(r.get(2) as u8);
        duplicate = Some(r.get_bit() != 0);
    }
    let mut au_cpb_removal_delay_minus1 = None;
    let mut pic_dpb_output_delay = None;
    if let Some(hrd) = vui.hrd.as_ref()
        && (hrd.nal_hrd_present || hrd.vcl_hrd_present)
    {
        let a = u32::from(hrd.au_cpb_removal_delay_length_minus1) + 1;
        let d = u32::from(hrd.dpb_output_delay_length_minus1) + 1;
        au_cpb_removal_delay_minus1 = Some(r.get(a.min(32)));
        pic_dpb_output_delay = Some(r.get(d.min(32)));
    }
    if r.overrun() {
        return None;
    }
    Some(SeiPayload::PicTiming {
        pic_struct,
        source_scan_type,
        duplicate,
        au_cpb_removal_delay_minus1,
        pic_dpb_output_delay,
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// The prefix SEI `x265` writes at the front of every stream: payload type
    /// 5 (`user_data_unregistered`), a UUID, then its version string. Taken
    /// from `sd.265`, truncated after the first few characters.
    const X265_SEI: &[u8] = &[
        0x4e, 0x01, // NAL header: PREFIX_SEI_NUT
        0x05, 0x18, // payloadType 5, payloadSize 24
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x06, 0x2c, 0xa2, 0xde, 0x09, 0xb5,
        0x17, // 16 bytes of UUID
        b'x', b'2', b'6', b'5', b' ', b'(', b'b', b'u', // 8 bytes of payload
        0x80, // rbsp_trailing_bits
    ];

    #[test]
    fn a_real_x265_sei() {
        let msgs = parse(X265_SEI, None, &mut budget()).expect("parses");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload_type, 5);
        assert_eq!(msgs[0].payload_size, 24);
        assert!(!msgs[0].truncated);
        assert!(!msgs[0].suffix);
        match msgs[0].payload {
            SeiPayload::UserDataUnregistered { uuid, data } => {
                assert_eq!(uuid[0], 0xff);
                assert_eq!(data, b"x265 (bu");
            }
            ref other => panic!("wrong payload: {other:?}"),
        }
    }

    #[test]
    fn content_light_level_and_mastering_display_decode() {
        // payloadType 144, size 4.
        let cll = [0x4e, 0x01, 0x90, 0x04, 0x03, 0xe8, 0x01, 0xf4, 0x80];
        let msgs = parse(&cll, None, &mut budget()).expect("parses");
        assert_eq!(
            msgs[0].payload,
            SeiPayload::ContentLightLevel {
                max_content_light_level: 1000,
                max_pic_average_light_level: 500,
            }
        );

        // payloadType 137, size 24. Green, blue, red — HEVC's order.
        let mut mdcv = vec![0x4e, 0x01, 0x89, 0x18];
        for v in [13250u16, 34500, 7500, 3000, 34000, 16000, 15635, 16450] {
            mdcv.extend_from_slice(&v.to_be_bytes());
        }
        mdcv.extend_from_slice(&10_000_000u32.to_be_bytes());
        mdcv.extend_from_slice(&50u32.to_be_bytes());
        mdcv.push(0x80);
        let msgs = parse(&mdcv, None, &mut budget()).expect("parses");
        match msgs[0].payload {
            SeiPayload::MasteringDisplay {
                primaries,
                white_point,
                max_luminance,
                min_luminance,
            } => {
                assert_eq!(primaries[0], (13250, 34500), "green first");
                assert_eq!(primaries[2], (34000, 16000), "red last");
                assert_eq!(white_point, (15635, 16450));
                assert_eq!(max_luminance, 10_000_000);
                assert_eq!(min_luminance, 50);
            }
            ref other => panic!("wrong payload: {other:?}"),
        }
    }

    #[test]
    fn the_alternative_transfer_characteristics_message_is_one_byte() {
        // payloadType 147, size 1, value 18 (HLG).
        let data = [0x4e, 0x01, 0x93, 0x01, 18, 0x80];
        let msgs = parse(&data, None, &mut budget()).expect("parses");
        assert_eq!(
            msgs[0].payload,
            SeiPayload::AlternativeTransferCharacteristics {
                preferred_transfer_characteristics: 18,
            }
        );
    }

    #[test]
    fn a_suffix_sei_is_marked_as_one() {
        // 0x50 0x01 -> nal_unit_type 40, SUFFIX_SEI_NUT.
        let data = [0x50, 0x01, 0x84, 0x11, 0x00, 0x80];
        let msgs = parse(&data, None, &mut budget()).expect("parses");
        assert!(msgs[0].suffix);
        assert_eq!(msgs[0].payload_type, 132);
    }

    #[test]
    fn a_run_of_ff_bytes_is_refused_rather_than_counted_to_four_billion() {
        let mut data = vec![0x4e, 0x01];
        data.extend(std::iter::repeat_n(0xFFu8, 4096));
        data.push(0x80);
        assert!(parse(&data, None, &mut budget()).is_err());
    }

    #[test]
    fn a_declared_size_past_the_end_is_reported_not_read() {
        // payloadType 5, payloadSize 200, but only two bytes follow.
        let data = [0x4e, 0x01, 0x05, 0xC8, 0x01, 0x80];
        let msgs = parse(&data, None, &mut budget()).expect("parses");
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].truncated);
        assert_eq!(msgs[0].payload_size, 200);
    }

    #[test]
    fn a_unit_of_the_wrong_type_is_refused() {
        let data = [0x40, 0x01, 0x05, 0x00, 0x80];
        assert!(matches!(
            parse(&data, None, &mut budget()),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn every_truncation_of_a_real_sei_is_handled() {
        for n in 0..X265_SEI.len() {
            let _ = parse(&X265_SEI[..n], None, &mut budget());
        }
    }

    #[test]
    fn table_d2_field_orders() {
        use vaco_codec_core::FieldOrder as F;
        for (v, expect) in [
            (0u8, F::Progressive),
            (1, F::TopFirst),
            (2, F::BottomFirst),
            (3, F::TopFirst),
            (4, F::BottomFirst),
            (5, F::TopFirst),
            (6, F::BottomFirst),
            (7, F::Progressive),
            (8, F::Progressive),
            (9, F::TopFirst),
            (10, F::BottomFirst),
            (11, F::TopFirst),
            (12, F::BottomFirst),
            (13, F::Unknown),
            (15, F::Unknown),
        ] {
            assert_eq!(PicStruct(v).field_order(), expect, "pic_struct {v}");
        }
        assert_eq!(PicStruct(0).num_clock_ts(), 1);
        assert_eq!(PicStruct(5).num_clock_ts(), 3);
        assert_eq!(PicStruct(13).num_clock_ts(), 0);
    }
}
