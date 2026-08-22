//! Supplemental enhancement information, ITU-T H.264 §7.3.2.3 and Annex D.
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
//! `pic_timing` (§D.1.3) is unparsable without it. Whether it begins with
//! `cpb_removal_delay` depends on whether the active SPS declared an HRD, and
//! the *width in bits* of that field is `cpb_removal_delay_length_minus1 + 1`,
//! which is an HRD field. Whether `pic_struct` is present depends on
//! `pic_struct_present_flag`. An SEI parsed against the wrong SPS produces
//! plausible nonsense, which is why [`parse`] takes the active one and reports
//! the payload as raw bytes when it has none.

use vaco_bitstream::{BitReader, ByteReader};
use vaco_codec_core::FieldOrder;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::nal::{H264NalHeader, NalUnitType};
use crate::sps::Sps;
use crate::util::{MAX_SEI_FF_BYTES, more_rbsp_data};

/// Payload types this crate decodes, Annex D Table D-1.
pub mod payload_type {
    /// `buffering_period`, §D.1.2.
    pub const BUFFERING_PERIOD: u32 = 0;
    /// `pic_timing`, §D.1.3.
    pub const PIC_TIMING: u32 = 1;
    /// `filler_payload`, §D.1.5.
    pub const FILLER: u32 = 3;
    /// `user_data_registered_itu_t_t35`, §D.1.6.
    pub const USER_DATA_REGISTERED: u32 = 4;
    /// `user_data_unregistered`, §D.1.7.
    pub const USER_DATA_UNREGISTERED: u32 = 5;
    /// `recovery_point`, §D.1.8.
    pub const RECOVERY_POINT: u32 = 6;
    /// `frame_packing_arrangement`, §D.1.27.
    pub const FRAME_PACKING_ARRANGEMENT: u32 = 45;
    /// `display_orientation`, §D.1.28.
    pub const DISPLAY_ORIENTATION: u32 = 47;
    /// `mastering_display_colour_volume`, §D.1.29.
    pub const MASTERING_DISPLAY_COLOUR_VOLUME: u32 = 137;
    /// `content_light_level_info`, §D.1.31.
    pub const CONTENT_LIGHT_LEVEL: u32 = 144;
    /// `alternative_transfer_characteristics`, §D.1.32.
    pub const ALTERNATIVE_TRANSFER_CHARACTERISTICS: u32 = 147;
}

/// `pic_struct`, Table D-1: how the picture's fields are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PicStruct(pub u8);

impl PicStruct {
    /// `NumClockTS`, Table D-1: how many `clock_timestamp` structures follow.
    ///
    /// Zero for a value the table does not define, which stops the loop rather
    /// than guessing.
    #[must_use]
    pub const fn num_clock_ts(self) -> u32 {
        match self.0 {
            0..=2 => 1,
            3 | 4 | 7 => 2,
            5 | 6 | 8 => 3,
            _ => 0,
        }
    }

    /// The field order this `pic_struct` implies.
    ///
    /// Table D-1's nine values collapse onto five field orders:
    ///
    /// | `pic_struct` | meaning | field order |
    /// |---|---|---|
    /// | 0, 7, 8 | frame, frame doubling, frame tripling | progressive |
    /// | 1 | top field | top first |
    /// | 2 | bottom field | bottom first |
    /// | 3, 5 | top then bottom (with repeat) | top first |
    /// | 4, 6 | bottom then top (with repeat) | bottom first |
    ///
    /// Confirmed against `ffmpeg 8.1` for the case that matters: an
    /// MBAFF stream from `libx264` carries `pic_struct = 3` in every access
    /// unit, and `ffprobe` reports `field_order=tt`. Probed with
    /// `-bsf:v trace_headers` to read the value and `-show_streams` to read the
    /// conclusion, so the mapping is measured rather than assumed.
    #[must_use]
    pub const fn field_order(self) -> FieldOrder {
        match self.0 {
            1 | 3 | 5 => FieldOrder::TopFirst,
            2 | 4 | 6 => FieldOrder::BottomFirst,
            0 | 7 | 8 => FieldOrder::Progressive,
            _ => FieldOrder::Unknown,
        }
    }
}

/// One `clock_timestamp()`, §D.1.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockTimestamp {
    /// `ct_type`.
    pub ct_type: u8,
    /// `nuit_field_based_flag`.
    pub nuit_field_based: bool,
    /// `counting_type`.
    pub counting_type: u8,
    /// `discontinuity_flag`.
    pub discontinuity: bool,
    /// `cnt_dropped_flag`.
    pub cnt_dropped: bool,
    /// `n_frames`.
    pub n_frames: u8,
    /// `hours_value`, `minutes_value`, `seconds_value`, each present only if
    /// its flag was set.
    pub hours: Option<u8>,
    /// See `hours`.
    pub minutes: Option<u8>,
    /// See `hours`.
    pub seconds: Option<u8>,
    /// `time_offset`, present only when `time_offset_length > 0`.
    pub time_offset: Option<i32>,
}

/// A decoded SEI message.
///
/// `Other` is not a failure: an unrecognised payload is kept whole so it can be
/// re-emitted or inspected later.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SeiPayload<'a> {
    /// §D.1.3. `cpb_removal_delay` / `dpb_output_delay` are present only when
    /// the SPS declared an HRD.
    PicTiming {
        /// `cpb_removal_delay`.
        cpb_removal_delay: Option<u32>,
        /// `dpb_output_delay`.
        dpb_output_delay: Option<u32>,
        /// `pic_struct`, present only when `pic_struct_present_flag`.
        pic_struct: Option<PicStruct>,
        /// The `clock_timestamp()` structures that were present.
        timestamps: Vec<ClockTimestamp>,
    },
    /// §D.1.8: where a decoder joining the stream can start.
    RecoveryPoint {
        /// `recovery_frame_cnt`.
        recovery_frame_cnt: u32,
        /// `exact_match_flag`.
        exact_match: bool,
        /// `broken_link_flag`.
        broken_link: bool,
        /// `changing_slice_group_idc`.
        changing_slice_group_idc: u8,
    },
    /// §D.1.7: a 128-bit UUID and whatever the producer put after it. This is
    /// where `libx264` writes its version and settings string.
    UserDataUnregistered {
        /// `uuid_iso_iec_11578`.
        uuid: [u8; 16],
        /// The rest of the payload.
        data: &'a [u8],
    },
    /// §D.1.6: `itu_t_t35_country_code` and the rest, uninterpreted.
    UserDataRegistered {
        /// `itu_t_t35_country_code`, plus the extension byte when it is 0xFF.
        country_code: u16,
        /// The rest of the payload.
        data: &'a [u8],
    },
    /// §D.1.27: how two views share one frame.
    FramePacking {
        /// `frame_packing_arrangement_id`.
        id: u32,
        /// `frame_packing_arrangement_cancel_flag`.
        cancel: bool,
        /// `frame_packing_arrangement_type`.
        arrangement_type: u8,
        /// `content_interpretation_type`.
        content_interpretation_type: u8,
        /// `quincunx_sampling_flag`.
        quincunx_sampling: bool,
    },
    /// §D.1.28: the rotation and flip a display should apply.
    DisplayOrientation {
        /// `display_orientation_cancel_flag`.
        cancel: bool,
        /// `hor_flip`.
        hor_flip: bool,
        /// `ver_flip`.
        ver_flip: bool,
        /// `anticlockwise_rotation`, in units of 1/65536 of a full turn.
        anticlockwise_rotation: u16,
    },
    /// §D.1.29: HDR mastering display primaries and luminance.
    MasteringDisplay {
        /// `display_primaries_x/y` for the three primaries, in units of
        /// 0.00002.
        primaries: [(u16, u16); 3],
        /// `white_point_x/y`, same units.
        white_point: (u16, u16),
        /// `max_display_mastering_luminance`, in units of 0.0001 cd/m².
        max_luminance: u32,
        /// `min_display_mastering_luminance`, same units.
        min_luminance: u32,
    },
    /// §D.1.31: HDR content light levels, in cd/m².
    ContentLightLevel {
        /// `max_content_light_level`.
        max_content_light_level: u16,
        /// `max_pic_average_light_level`.
        max_pic_average_light_level: u16,
    },
    /// §D.1.32: the transfer function to use instead of the VUI's.
    AlternativeTransferCharacteristics {
        /// `preferred_transfer_characteristics`, an H.273 code point.
        preferred_transfer_characteristics: u8,
    },
    /// §D.1.5: padding, and nothing else.
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
    let header = H264NalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
    if header.nal_unit_type != NalUnitType::Sei {
        return Err(Error::InvalidData("not an SEI NAL unit"));
    }
    let mut out = Vec::new();
    let mut reader = ByteReader::new(rbsp);
    reader.skip(1); // the NAL header byte

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

        let payload = decode_payload(payload_type, data, sps, budget)?;
        out.push(SeiMessage {
            payload_type,
            payload_size,
            truncated,
            payload,
        });
        if truncated {
            break;
        }
    }
    Ok(out)
}

/// Whether another `sei_message()` follows, §7.3.2.3.
fn has_more_messages(reader: &ByteReader<'_>, rbsp: &[u8]) -> bool {
    if reader.overrun() || reader.remaining() == 0 {
        return false;
    }
    let mut bits = BitReader::new(rbsp);
    bits.skip_bytes(reader.pos());
    more_rbsp_data(&bits, rbsp)
}

/// The `ff_byte`-prefixed integer coding `payloadType` and `payloadSize` share,
/// §7.3.2.3.1.
///
/// A run of `0xFF` each adding 255, then a final byte. The run is bounded by
/// [`MAX_SEI_FF_BYTES`], so a NAL unit consisting only of `FF` bytes is
/// refused instead of counting to four billion.
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
fn decode_payload<'a>(
    payload_type: u32,
    data: &'a [u8],
    sps: Option<&Sps>,
    budget: &mut Budget,
) -> Result<SeiPayload<'a>> {
    use self::payload_type as pt;
    Ok(match payload_type {
        pt::PIC_TIMING => match sps {
            Some(sps) => decode_pic_timing(data, sps, budget)?,
            None => SeiPayload::Other { payload_type, data },
        },
        pt::RECOVERY_POINT => {
            let mut r = BitReader::new(data);
            // §D.2.8 bounds `recovery_frame_cnt` by MaxFrameNum - 1.
            let recovery_frame_cnt = vaco_codec_golomb::GolombDecode::ue_v_max(&mut r, 65_535)?;
            let p = SeiPayload::RecoveryPoint {
                recovery_frame_cnt,
                exact_match: r.get_bit() != 0,
                broken_link: r.get_bit() != 0,
                changing_slice_group_idc: r.get(2) as u8,
            };
            r.check()?;
            p
        }
        pt::USER_DATA_UNREGISTERED => {
            let Some(head) = data.first_chunk::<16>() else {
                return Ok(SeiPayload::Other { payload_type, data });
            };
            SeiPayload::UserDataUnregistered {
                uuid: *head,
                data: data.get(16..).unwrap_or(&[]),
            }
        }
        pt::USER_DATA_REGISTERED => {
            let Some(&first) = data.first() else {
                return Ok(SeiPayload::Other { payload_type, data });
            };
            // §D.1.6: a country code of 0xFF is followed by an extension byte.
            if first == 0xFF {
                let second = data.get(1).copied().unwrap_or(0);
                SeiPayload::UserDataRegistered {
                    country_code: (u16::from(first) << 8) | u16::from(second),
                    data: data.get(2..).unwrap_or(&[]),
                }
            } else {
                SeiPayload::UserDataRegistered {
                    country_code: u16::from(first),
                    data: data.get(1..).unwrap_or(&[]),
                }
            }
        }
        pt::FRAME_PACKING_ARRANGEMENT => {
            let mut r = BitReader::new(data);
            let id = vaco_codec_golomb::GolombDecode::ue_v_max(&mut r, u32::MAX - 1)?;
            let cancel = r.get_bit() != 0;
            let p = if cancel {
                SeiPayload::FramePacking {
                    id,
                    cancel,
                    arrangement_type: 0,
                    content_interpretation_type: 0,
                    quincunx_sampling: false,
                }
            } else {
                let arrangement_type = r.get(7) as u8;
                let quincunx_sampling = r.get_bit() != 0;
                let content_interpretation_type = r.get(6) as u8;
                SeiPayload::FramePacking {
                    id,
                    cancel,
                    arrangement_type,
                    content_interpretation_type,
                    quincunx_sampling,
                }
            };
            r.check()?;
            p
        }
        pt::DISPLAY_ORIENTATION => {
            let mut r = BitReader::new(data);
            let cancel = r.get_bit() != 0;
            let p = if cancel {
                SeiPayload::DisplayOrientation {
                    cancel,
                    hor_flip: false,
                    ver_flip: false,
                    anticlockwise_rotation: 0,
                }
            } else {
                SeiPayload::DisplayOrientation {
                    cancel,
                    hor_flip: r.get_bit() != 0,
                    ver_flip: r.get_bit() != 0,
                    anticlockwise_rotation: r.get(16) as u16,
                }
            };
            r.check()?;
            p
        }
        pt::MASTERING_DISPLAY_COLOUR_VOLUME => {
            let mut r = BitReader::new(data);
            let mut primaries = [(0u16, 0u16); 3];
            for slot in &mut primaries {
                *slot = (r.get(16) as u16, r.get(16) as u16);
            }
            let p = SeiPayload::MasteringDisplay {
                primaries,
                white_point: (r.get(16) as u16, r.get(16) as u16),
                max_luminance: r.get(32),
                min_luminance: r.get(32),
            };
            r.check()?;
            p
        }
        pt::CONTENT_LIGHT_LEVEL => {
            let mut r = BitReader::new(data);
            let p = SeiPayload::ContentLightLevel {
                max_content_light_level: r.get(16) as u16,
                max_pic_average_light_level: r.get(16) as u16,
            };
            r.check()?;
            p
        }
        pt::ALTERNATIVE_TRANSFER_CHARACTERISTICS => {
            let mut r = BitReader::new(data);
            let p = SeiPayload::AlternativeTransferCharacteristics {
                preferred_transfer_characteristics: r.get(8) as u8,
            };
            r.check()?;
            p
        }
        pt::FILLER => SeiPayload::Filler { len: data.len() },
        _ => SeiPayload::Other { payload_type, data },
    })
}

/// `pic_timing()`, §D.1.3.
fn decode_pic_timing<'a>(data: &[u8], sps: &Sps, budget: &mut Budget) -> Result<SeiPayload<'a>> {
    let vui = sps.vui.as_ref();
    let hrd = vui.and_then(|v| v.nal_hrd.as_ref().or(v.vcl_hrd.as_ref()));
    let pic_struct_present = vui.is_some_and(|v| v.pic_struct_present);
    let time_offset_length = hrd.map_or(0, |h| u32::from(h.time_offset_length));

    let mut r = BitReader::new(data);
    let (cpb_removal_delay, dpb_output_delay) = match hrd {
        Some(h) => (
            Some(r.get(u32::from(h.cpb_removal_delay_length_minus1) + 1)),
            Some(r.get(u32::from(h.dpb_output_delay_length_minus1) + 1)),
        ),
        None => (None, None),
    };

    let mut pic_struct = None;
    let mut timestamps = Vec::new();
    if pic_struct_present {
        let ps = PicStruct(r.get(4) as u8);
        pic_struct = Some(ps);
        // At most three, from Table D-1: not input-driven in any meaningful
        // sense, but charged anyway so a stream of SEI units cannot be free.
        let n = ps.num_clock_ts();
        budget.consume_fuel(u64::from(n))?;
        for _ in 0..n {
            if r.get_bit() == 0 {
                continue;
            }
            let mut ts = ClockTimestamp {
                ct_type: r.get(2) as u8,
                nuit_field_based: r.get_bit() != 0,
                counting_type: r.get(5) as u8,
                ..ClockTimestamp::default()
            };
            let full_timestamp = r.get_bit() != 0;
            ts.discontinuity = r.get_bit() != 0;
            ts.cnt_dropped = r.get_bit() != 0;
            ts.n_frames = r.get(8) as u8;
            if full_timestamp {
                ts.seconds = Some(r.get(6) as u8);
                ts.minutes = Some(r.get(6) as u8);
                ts.hours = Some(r.get(5) as u8);
            } else if r.get_bit() != 0 {
                ts.seconds = Some(r.get(6) as u8);
                if r.get_bit() != 0 {
                    ts.minutes = Some(r.get(6) as u8);
                    if r.get_bit() != 0 {
                        ts.hours = Some(r.get(5) as u8);
                    }
                }
            }
            if time_offset_length > 0 {
                ts.time_offset = Some(r.get_signed(time_offset_length.min(32)));
            }
            timestamps.push(ts);
        }
    }
    r.check()?;
    Ok(SeiPayload::PicTiming {
        cpb_removal_delay,
        dpb_output_delay,
        pic_struct,
        timestamps,
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

    #[test]
    fn num_clock_ts_matches_table_d1() {
        let expected = [1u32, 1, 1, 2, 2, 3, 3, 2, 3];
        for (v, &n) in expected.iter().enumerate() {
            assert_eq!(PicStruct(v as u8).num_clock_ts(), n, "pic_struct {v}");
        }
        for v in 9..16u8 {
            assert_eq!(PicStruct(v).num_clock_ts(), 0, "pic_struct {v}");
        }
    }

    #[test]
    fn pic_struct_three_is_top_field_first() {
        // The value `libx264` writes for an interlaced stream; `ffprobe 8.1`
        // reports `field_order=tt` for it.
        assert_eq!(PicStruct(3).field_order(), FieldOrder::TopFirst);
        assert_eq!(PicStruct(4).field_order(), FieldOrder::BottomFirst);
        assert_eq!(PicStruct(0).field_order(), FieldOrder::Progressive);
        assert_eq!(PicStruct(9).field_order(), FieldOrder::Unknown);
    }

    /// The `user_data_unregistered` message `libx264` writes, header and UUID
    /// taken byte-for-byte from a real stream. Payload type 5, size coded as
    /// `FF FF AD` = 255 + 255 + 173 = 683.
    #[test]
    fn the_x264_user_data_message() {
        let mut nal = vec![0x06u8, 0x05, 0xFF, 0xFF, 0xAD];
        nal.extend_from_slice(&[
            0xDC, 0x45, 0xE9, 0xBD, 0xE6, 0xD9, 0x48, 0xB7, 0x96, 0x2C, 0xD8, 0x20, 0xD9, 0x23,
            0xEE, 0xEF,
        ]);
        nal.extend(std::iter::repeat_n(b'x', 683 - 16));
        nal.push(0x80); // rbsp_trailing_bits
        let msgs = parse(&nal, None, &mut budget()).expect("parses");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload_type, 5);
        assert_eq!(msgs[0].payload_size, 683);
        assert!(!msgs[0].truncated);
        match &msgs[0].payload {
            SeiPayload::UserDataUnregistered { uuid, data } => {
                assert_eq!(uuid[0], 0xDC);
                assert_eq!(data.len(), 683 - 16);
            }
            other => panic!("wrong payload: {other:?}"),
        }
    }

    #[test]
    fn a_declared_size_larger_than_the_unit_is_reported_not_trusted() {
        // Type 5, size 200, but only three bytes follow.
        let nal = [0x06u8, 0x05, 200, 1, 2, 3];
        let msgs = parse(&nal, None, &mut budget()).expect("parses");
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].truncated);
        assert_eq!(msgs[0].payload_size, 200);
    }

    #[test]
    fn an_all_ff_unit_is_refused_rather_than_counted() {
        let mut nal = vec![0x06u8];
        nal.extend(std::iter::repeat_n(0xFFu8, 4096));
        let err = parse(&nal, None, &mut budget()).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn recovery_point_decodes() {
        // ue(0) = '1', exact_match = 1, broken_link = 0, idc = 00, then
        // trailing bits.
        let nal = [0x06u8, 0x06, 0x01, 0b1100_0000, 0x80];
        let msgs = parse(&nal, None, &mut budget()).expect("parses");
        match msgs.first().map(|m| &m.payload) {
            Some(SeiPayload::RecoveryPoint {
                recovery_frame_cnt,
                exact_match,
                broken_link,
                ..
            }) => {
                assert_eq!(*recovery_frame_cnt, 0);
                assert!(*exact_match);
                assert!(!*broken_link);
            }
            other => panic!("wrong payload: {other:?}"),
        }
    }

    #[test]
    fn content_light_level_decodes() {
        let nal = [0x06u8, 144, 4, 0x03, 0xE8, 0x01, 0x2C, 0x80];
        let msgs = parse(&nal, None, &mut budget()).expect("parses");
        assert_eq!(
            msgs.first().map(|m| m.payload.clone()),
            Some(SeiPayload::ContentLightLevel {
                max_content_light_level: 1000,
                max_pic_average_light_level: 300,
            })
        );
    }

    #[test]
    fn every_truncation_of_a_real_unit_is_handled() {
        let nal = [
            0x06u8,
            0x01,
            0x01,
            0b0011_0010,
            0x80,
            0x06,
            144,
            4,
            0x03,
            0xE8,
            0x01,
            0x2C,
            0x80,
        ];
        for n in 0..nal.len() {
            let _ = parse(&nal[..n], None, &mut budget());
        }
    }

    #[test]
    fn a_non_sei_unit_is_rejected() {
        assert!(matches!(
            parse(&[0x67, 0x00], None, &mut budget()),
            Err(Error::InvalidData(_))
        ));
    }
}
