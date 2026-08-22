//! Unsynchronisation: removing the padding byte an `ID3v2`-aware encoder
//! inserts so an MPEG frame sync (`$FF` followed by a byte with its top
//! three bits set) can never occur inside tag data, which would otherwise
//! let a naive MPEG decoder scanning for the next frame sync jump into the
//! middle of a tag.
//!
//! `ID3v2.3.0` §5 / `ID3v2.4.0` §6.1. The encoding rule is asymmetric — insert a
//! `$00` after `$FF` when the next byte is `$00` *or* has its top three bits
//! set (`%111xxxxx`) — but decoding does not need to reconstruct that
//! distinction: "all the `$FF 00` combinations have to be replaced with the
//! `$FF 00 00` combination during the unsynchronisation", so decoding is
//! simply *"remove every `$00` that immediately follows an `$FF`"*, with no
//! lookahead past it required.
//!
//! Applies at two different scopes depending on version, both handled by the
//! one function here: `ID3v2.3.0`'s `Flags::UNSYNCHRONISATION` covers the
//! *whole tag* (call this once over all the frame data), while `ID3v2.4.0`
//! additionally allows a single frame to set its own unsynchronisation flag
//! independently (call this once over just that frame's content).

use vaco_limits::Budget;

/// Remove `ID3v2` unsynchronisation from `data`: every `$00` immediately
/// following an `$FF` is dropped.
///
/// The result can never be longer than `data` — this only removes bytes —
/// so the allocation is bounded by the input's own already-accounted-for
/// size, not by anything the tag declares. `budget` still charges for it,
/// consistent with "every buffer sized from input goes through `Budget`".
///
/// # Errors
///
/// [`vaco_core::Error::LimitExceeded`] if `budget` cannot cover a buffer the
/// size of `data`.
pub fn remove(data: &[u8], budget: &mut Budget) -> vaco_core::Result<Vec<u8>> {
    let mut out = budget.alloc::<u8>(data.len())?;
    let mut n = 0usize;
    let mut prev_was_ff = false;
    for &b in data {
        if prev_was_ff && b == 0x00 {
            prev_was_ff = false;
            continue;
        }
        if let Some(slot) = out.get_mut(n) {
            *slot = b;
        }
        n += 1;
        prev_was_ff = b == 0xFF;
    }
    out.truncate(n);
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn run(data: &[u8]) -> Vec<u8> {
        let mut budget = Budget::new(Limits::permissive());
        remove(data, &mut budget).unwrap()
    }

    #[test]
    fn strips_the_padding_byte_after_ff() {
        assert_eq!(run(&[0x12, 0xFF, 0x00, 0x34]), vec![0x12, 0xFF, 0x34]);
    }

    #[test]
    fn a_real_frame_sync_stays_escaped_but_still_strips_its_zero() {
        // The encoder would have written FF 00 E0 for a literal FF E0 byte
        // pair that looks like a frame sync; decoding removes the 00,
        // recovering the original FF E0.
        assert_eq!(run(&[0xFF, 0x00, 0xE0]), vec![0xFF, 0xE0]);
    }

    #[test]
    fn consecutive_ff_00_pairs_all_strip() {
        assert_eq!(
            run(&[0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00]),
            vec![0xFF, 0xFF, 0xFF]
        );
    }

    #[test]
    fn an_ff_not_followed_by_zero_is_untouched() {
        assert_eq!(run(&[0xFF, 0x01, 0x02]), vec![0xFF, 0x01, 0x02]);
    }

    #[test]
    fn no_ff_bytes_is_a_no_op() {
        assert_eq!(run(&[1, 2, 3, 4]), vec![1, 2, 3, 4]);
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(run(&[]), Vec::<u8>::new());
    }

    #[test]
    fn a_trailing_ff_with_no_following_byte_is_kept() {
        assert_eq!(run(&[1, 0xFF]), vec![1, 0xFF]);
    }
}
