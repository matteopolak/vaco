//! Shared reading helpers: `more_rbsp_data()` and the loop bounds that keep
//! input-driven syntax loops finite.

use vaco_bitstream::BitReader;

/// Bit position of the `rbsp_stop_one_bit`, ITU-T H.264 §7.3.2.11.
///
/// The RBSP ends with a single `1` bit followed by zero-valued alignment bits,
/// so the stop bit is the *lowest set bit of the last non-zero byte*. An RBSP
/// with no non-zero byte has no trailing bits at all and is malformed;
/// `None` says so.
#[must_use]
pub(crate) fn rbsp_stop_bit_pos(rbsp: &[u8]) -> Option<u64> {
    let last = rbsp.iter().rposition(|&b| b != 0)?;
    let byte = *rbsp.get(last)?;
    Some((last as u64) * 8 + u64::from(7 - byte.trailing_zeros()))
}

/// `more_rbsp_data()`, ITU-T H.264 §7.2.
///
/// True when the reader has not yet reached the `rbsp_stop_one_bit`. Three
/// syntax structures depend on it — the PPS tail, the SEI message loop, and
/// `slice_data` — and getting it wrong shows up as a PPS whose
/// `transform_8x8_mode_flag` is read out of the trailing bits.
///
/// Returns false rather than true whenever it cannot tell (an empty or
/// all-zero RBSP, or a reader already past the end), because "there is more
/// data" is the answer that keeps a loop going.
#[must_use]
pub(crate) fn more_rbsp_data(reader: &BitReader<'_>, rbsp: &[u8]) -> bool {
    if reader.overrun() {
        return false;
    }
    rbsp_stop_bit_pos(rbsp).is_some_and(|stop| reader.bit_pos() < stop)
}

/// The largest number of iterations any `do … while` in H.264 slice syntax is
/// allowed to run here.
///
/// The specification bounds `ref_pic_list_modification` and
/// `dec_ref_pic_marking` indirectly — through `num_ref_idx_active` (at most 32)
/// and the DPB size (at most 16 frames, 32 fields) — but it states no direct
/// limit on the number of commands, and a malformed stream can encode a
/// terminating value that never arrives. This is the direct limit, generous
/// enough that no conforming stream reaches it and small enough that a hostile
/// one is refused immediately.
///
/// Fuel is charged per command as well (via
/// [`BoundedGolomb`](vaco_codec_golomb::BoundedGolomb)), so this is the second
/// of two independent bounds, not the only one.
pub(crate) const MAX_SYNTAX_COMMANDS: u32 = 256;

/// The largest `ff_byte` run accepted in an SEI message header.
///
/// §7.3.2.3.1 codes `payloadType` and `payloadSize` as a run of `0xFF` bytes
/// followed by a final byte. The run is unbounded in the syntax, so a NAL unit
/// of nothing but `FF` bytes is a valid-looking header with an astronomical
/// payload type. Bounded here, and additionally by the bytes actually present.
pub(crate) const MAX_SEI_FF_BYTES: u32 = 64;

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
        // 0x80 = 1000_0000: stop bit at bit 0 of byte 1.
        assert_eq!(rbsp_stop_bit_pos(&[0xFF, 0x80]), Some(8));
        // 0x81 = 1000_0001: stop bit at bit 7.
        assert_eq!(rbsp_stop_bit_pos(&[0xFF, 0x81]), Some(15));
        // Trailing zero bytes are alignment and do not move it.
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
        // Stop bit is at index 5 (the lowest set bit).
        let mut r = BitReader::new(&rbsp);
        for expected_pos in 0..5 {
            assert!(
                more_rbsp_data(&r, &rbsp),
                "should still have data at {expected_pos}"
            );
            let _ = r.get_bit();
        }
        assert!(!more_rbsp_data(&r, &rbsp));
    }

    #[test]
    fn an_empty_rbsp_never_has_more_data() {
        let r = BitReader::new(&[]);
        assert!(!more_rbsp_data(&r, &[]));
    }
}
