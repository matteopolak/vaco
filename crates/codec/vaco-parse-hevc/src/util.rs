//! Shared reading helpers: `more_rbsp_data()` and the ceilings that keep
//! input-driven syntax loops finite.

use vaco_bitstream::BitReader;

/// Bit position of the `rbsp_stop_one_bit`, ITU-T H.265 §7.3.2.11.
///
/// The RBSP ends with a single `1` bit followed by zero-valued alignment bits,
/// so the stop bit is the *lowest set bit of the last non-zero byte*. An RBSP
/// with no non-zero byte has no trailing bits at all and is malformed; `None`
/// says so.
#[must_use]
pub(crate) fn rbsp_stop_bit_pos(rbsp: &[u8]) -> Option<u64> {
    let last = rbsp.iter().rposition(|&b| b != 0)?;
    let byte = *rbsp.get(last)?;
    Some((last as u64) * 8 + u64::from(7 - byte.trailing_zeros()))
}

/// `more_rbsp_data()`, ITU-T H.265 §7.4.3.11.
///
/// True when the reader has not yet reached the `rbsp_stop_one_bit`. Three
/// structures here depend on it — the VPS, SPS and PPS extension tails and the
/// SEI message loop — and getting it wrong shows up as a parameter set whose
/// extension flags are read out of the trailing bits.
///
/// Returns false rather than true whenever it cannot tell (an empty or all-zero
/// RBSP, or a reader already past the end), because "there is more data" is the
/// answer that keeps a loop going.
#[must_use]
pub(crate) fn more_rbsp_data(reader: &BitReader<'_>, rbsp: &[u8]) -> bool {
    if reader.overrun() {
        return false;
    }
    rbsp_stop_bit_pos(rbsp).is_some_and(|stop| reader.bit_pos() < stop)
}

/// `Ceil( Log2( n ) )`, which HEVC uses to size four `u(v)` fields:
/// `slice_segment_address`, `short_term_ref_pic_set_idx`, `lt_idx_sps` and
/// `list_entry_l0`/`l1`.
///
/// Zero for `n <= 1`, which is what every one of those call sites wants: a
/// field with one possible value occupies no bits at all.
#[must_use]
pub(crate) const fn ceil_log2(n: u64) -> u32 {
    if n <= 1 {
        return 0;
    }
    u64::BITS - (n - 1).leading_zeros()
}

/// The largest `sps_max_sub_layers_minus1` the syntax can express, §7.4.3.2.
///
/// `sps_max_sub_layers_minus1` is `u(3)`, so this is structural rather than a
/// policy — but it is stated here because three separate loops are sized by it
/// and a reader should be able to see the bound without deriving it.
pub(crate) const MAX_SUB_LAYERS: u32 = 8;

/// The largest `num_short_term_ref_pic_sets` accepted, §7.4.3.2.
///
/// The specification caps it at 64 outright.
pub(crate) const MAX_SHORT_TERM_RPS: u32 = 64;

/// The largest `num_long_term_ref_pics_sps` accepted, §7.4.3.2, which the
/// specification caps at 32.
pub(crate) const MAX_LONG_TERM_RPS: u32 = 32;

/// The largest number of pictures one short-term reference picture set may
/// name, §7.4.8: `num_negative_pics` and `num_positive_pics` are each bounded
/// by `sps_max_dec_pic_buffering_minus1`, which Annex A caps at 16.
pub(crate) const MAX_DELTA_POCS: u32 = 16;

/// The largest `ff_byte` run accepted in an SEI message header.
///
/// §7.3.5 codes `payloadType` and `payloadSize` as a run of `0xFF` bytes
/// followed by a final byte. The run is unbounded in the syntax, so a NAL unit
/// of nothing but `FF` bytes is a valid-looking header with an astronomical
/// payload type. Bounded here, and additionally by the bytes actually present.
pub(crate) const MAX_SEI_FF_BYTES: u32 = 64;

/// The largest `num_entry_point_offsets` accepted in a slice segment header.
///
/// §7.4.7.1 bounds it by the number of CTB rows or tiles in the picture, which
/// a parser could compute — but the computation needs the whole SPS geometry
/// and the bound below is reached by nothing real: 8192 entry points is a
/// picture with 8192 tiles, four times Annex A's largest tile count at any
/// level. Fuel is charged per offset as well, so this is the second of two
/// independent bounds.
pub(crate) const MAX_ENTRY_POINTS: u32 = 8192;

/// The largest `slice_segment_header_extension_length` accepted, §7.4.7.1,
/// which the specification caps at 256.
pub(crate) const MAX_SLICE_HEADER_EXTENSION: u32 = 256;

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
    fn the_stop_bit_is_the_lowest_set_bit_of_the_last_nonzero_byte() {
        assert_eq!(rbsp_stop_bit_pos(&[0xFF, 0x80]), Some(8));
        assert_eq!(rbsp_stop_bit_pos(&[0xFF, 0x81]), Some(15));
        assert_eq!(rbsp_stop_bit_pos(&[0xFF, 0x80, 0x00, 0x00]), Some(8));
    }

    #[test]
    fn an_all_zero_rbsp_has_no_stop_bit() {
        assert_eq!(rbsp_stop_bit_pos(&[0, 0, 0]), None);
        assert_eq!(rbsp_stop_bit_pos(&[]), None);
    }

    #[test]
    fn more_rbsp_data_stops_exactly_at_the_stop_bit() {
        let rbsp = [0b1010_1100u8];
        let mut r = BitReader::new(&rbsp);
        for _ in 0..5 {
            assert!(more_rbsp_data(&r, &rbsp));
            let _ = r.get_bit();
        }
        assert!(!more_rbsp_data(&r, &rbsp));
    }

    #[test]
    fn ceil_log2_matches_the_definition() {
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(255), 8);
        assert_eq!(ceil_log2(256), 8);
        assert_eq!(ceil_log2(257), 9);
        assert_eq!(ceil_log2(u64::from(u32::MAX)), 32);
    }
}
