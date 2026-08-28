//! ATSC A/53 closed-caption extraction from a `user_data_registered_itu_t_t35`
//! SEI message.
//!
//! # What this is for
//!
//! CEA-608/708 captions reach an H.264 stream inside an SEI message of type 4
//! (§D.1.6), wrapped in the ATSC A/53 identification prefix. This module
//! recognises that prefix and hands back the raw `cc_data` triplet bytes,
//! which is exactly the shape `vaco-codec-subtitle-cc` decodes and the shape
//! `FrameSideData::ClosedCaptions` carries.
//!
//! # The wrapper, measured
//!
//! The prefix is four checks deep, and every constant below was verified
//! against a real broadcast capture rather than recalled — see the crate's
//! test module for the fixture:
//!
//! | Field | Value | Width |
//! |---|---|---|
//! | `itu_t_t35_country_code` | `0xB5` (United States) | 8 |
//! | `itu_t_t35_provider_code` | `0x0031` (ATSC) | 16 |
//! | `user_identifier` | `0x47413934`, ASCII `GA94` | 32 |
//! | `user_data_type_code` | `0x03`, i.e. `MPEG_cc_data()` | 8 |
//!
//! `cc_data()` itself (ANSI/CTA-708 Table 2, reproduced in ATSC A/53 Part 4
//! §6.2.3.1 Table 6.10) then follows:
//!
//! ```text
//! reserved(1) process_cc_data_flag(1) additional_data_flag(1) cc_count(5)
//! em_data(8)
//! cc_count x { marker_bits(5) cc_valid(1) cc_type(2) cc_data_1(8) cc_data_2(8) }
//! marker_bits(8)
//! ```
//!
//! [`cc_data_triplets`] returns the `cc_count * 3` payload bytes only — not
//! the two-byte header and not the trailing marker — because that is what a
//! caption decoder consumes and what the reference's own `A53_CC` side data
//! contains. Verified byte-for-byte against 361 frames of a real capture.
//!
//! # Consume captions in presentation order — getting this wrong fails silently
//!
//! This is the one thing a caller must get right. CEA-608 is a *sequential*
//! byte stream carrying a stateful command language: a control code in one
//! picture sets the mode that the character pairs in later pictures land in.
//! Pictures reach a parser in **decode** order, which with B-frames is not
//! presentation order — so concatenating the payloads in the order they are
//! parsed interleaves the caption stream and destroys it.
//!
//! Measured, on a real broadcast capture: the same 361 payloads decoded in
//! decode order produce `"    s  itesciti. now"`, and in presentation order
//! produce `" its cities now."`. **Both decode with zero parity errors** —
//! nothing in the caption layer signals the mistake, because every byte pair
//! is individually valid and only their sequence is wrong.
//!
//! So attach what this module returns to *its own picture* and let the
//! reordering that already happens between decode and output carry it, which
//! is what `FrameSideData::ClosedCaptions` on a `Frame` does by construction.
//! Do not accumulate payloads into a buffer as they are parsed.
//!
//! # Allocation
//!
//! None. Every function here returns a borrowed subslice of the caller's SEI
//! payload; the `cc_count` field can only ever select at most `31 * 3` bytes,
//! and a slice that does not fit is rejected rather than clamped.

use crate::sei::SeiPayload;

/// `itu_t_t35_country_code` for the United States, which is what ATSC A/53
/// captions are registered under.
pub const T35_COUNTRY_CODE_USA: u16 = 0xB5;

/// `itu_t_t35_provider_code` assigned to ATSC.
pub const T35_PROVIDER_CODE_ATSC: u16 = 0x0031;

/// `user_identifier` `GA94`, the A/53 Table 6.7 assignment that says an
/// `ATSC_user_data()` structure follows.
pub const USER_IDENTIFIER_GA94: u32 = 0x4741_3934;

/// `user_data_type_code` for `MPEG_cc_data()`, A/53 Table 6.9.
pub const USER_DATA_TYPE_CC: u8 = 0x03;

/// `cc_count` is a 5-bit field, so a single `cc_data()` carries at most 31
/// triplets of three bytes each. Nothing derived from it can exceed this, and
/// that is what makes the whole module allocation-free.
pub const MAX_CC_DATA_BYTES: usize = 31 * 3;

/// Extract the `cc_data` triplet bytes from one already-parsed SEI payload,
/// or `None` if this is not an A/53 caption message.
///
/// Returns `None` — never an error — for every message that simply is not
/// this one: a different payload type, a different country or provider, a
/// `DTG1` (active-format-description) rather than `GA94` identifier, or an
/// `ATSC_user_data()` carrying bar data instead of captions. A stream is
/// expected to be full of those, so they are not failures.
#[must_use]
pub fn cc_data_from_sei<'a>(payload: &SeiPayload<'a>) -> Option<&'a [u8]> {
    let SeiPayload::UserDataRegistered { country_code, data } = payload else {
        return None;
    };
    if *country_code != T35_COUNTRY_CODE_USA {
        return None;
    }
    atsc_user_data(data)
}

/// Parse the bytes that follow `itu_t_t35_country_code` — the provider code,
/// `user_identifier`, `user_data_type_code` and `cc_data()`.
///
/// Split out from [`cc_data_from_sei`] because the MPEG-2 `user_data()` path
/// reaches the same structure by a different route (a start code rather than
/// an SEI), so this is the part the two genuinely share.
#[must_use]
fn atsc_user_data(after_country: &[u8]) -> Option<&[u8]> {
    let provider = u16::from_be_bytes(*after_country.first_chunk::<2>()?);
    if provider != T35_PROVIDER_CODE_ATSC {
        return None;
    }
    let rest = after_country.get(2..)?;
    cc_data_after_identifier(rest)
}

/// Parse `user_identifier`, `user_data_type_code` and the `cc_data()` that
/// follows them.
///
/// Public because the MPEG-2 route arrives here directly: A/53's `user_data()`
/// start-code carriage has no T.35 country or provider code at all, it begins
/// at `user_data_identifier`.
#[must_use]
pub fn cc_data_after_identifier(data: &[u8]) -> Option<&[u8]> {
    let identifier = u32::from_be_bytes(*data.first_chunk::<4>()?);
    if identifier != USER_IDENTIFIER_GA94 {
        return None;
    }
    let type_code = *data.get(4)?;
    if type_code != USER_DATA_TYPE_CC {
        return None;
    }
    cc_data_triplets(data.get(5..)?)
}

/// Take the `cc_count * 3` triplet bytes out of a `cc_data()` structure.
///
/// `None` when `process_cc_data_flag` is clear — CEA-708 defines that flag as
/// "the `cc_data` that follows is present but is not to be processed", so
/// honouring it is the difference between reproducing the transmitted caption
/// and inventing one the broadcaster explicitly suppressed — or when the
/// declared `cc_count` runs past the bytes actually present, which is a
/// truncated message rather than a shorter one.
#[must_use]
pub fn cc_data_triplets(cc_data: &[u8]) -> Option<&[u8]> {
    let header = *cc_data.first()?;
    let process_cc_data = header & 0x40 != 0;
    if !process_cc_data {
        return None;
    }
    let cc_count = usize::from(header & 0x1F);
    // Byte 1 is `em_data`, which carries no caption content.
    let triplets = cc_data.get(2..2 + cc_count * 3)?;
    Some(triplets)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    /// The first caption SEI payload of `transformers_EIA608_H264.ts`, from
    /// `itu_t_t35_country_code` onward, captured from the real file.
    const REAL_PAYLOAD: &[u8] = &[
        0xB5, 0x00, 0x31, 0x47, 0x41, 0x39, 0x34, 0x03, 0xD4, 0xFF, 0xFC, 0x80, 0x80, 0xFD, 0x80,
        0x80, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00,
        0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00,
        0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00,
        0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFA, 0x00, 0x00, 0xFF,
    ];

    fn real_sei() -> SeiPayload<'static> {
        SeiPayload::UserDataRegistered {
            country_code: u16::from(REAL_PAYLOAD[0]),
            data: &REAL_PAYLOAD[1..],
        }
    }

    #[test]
    fn extracts_real_capture_triplets() {
        let cc = cc_data_from_sei(&real_sei()).expect("real A/53 caption SEI");
        // cc_count is 20 in this capture (header byte 0xD4).
        assert_eq!(cc.len(), 20 * 3);
        assert_eq!(&cc[..6], &[0xFC, 0x80, 0x80, 0xFD, 0x80, 0x80]);
        // The trailing marker byte must not be included.
        assert_ne!(cc.last(), Some(&0xFF));
    }

    #[test]
    fn rejects_wrong_country_provider_identifier_and_type() {
        for (index, wrong) in [(0usize, 0xA5u8), (2, 0x32), (3, 0x48), (7, 0x06)] {
            let mut bytes = REAL_PAYLOAD.to_vec();
            bytes[index] = wrong;
            let payload = SeiPayload::UserDataRegistered {
                country_code: u16::from(bytes[0]),
                data: &bytes[1..],
            };
            assert_eq!(
                cc_data_from_sei(&payload),
                None,
                "byte {index} set to {wrong:#x} must not be accepted"
            );
        }
    }

    #[test]
    fn honours_process_cc_data_flag() {
        let mut bytes = REAL_PAYLOAD.to_vec();
        bytes[8] &= !0x40; // clear process_cc_data_flag
        let payload = SeiPayload::UserDataRegistered {
            country_code: u16::from(bytes[0]),
            data: &bytes[1..],
        };
        assert_eq!(cc_data_from_sei(&payload), None);
    }

    #[test]
    fn truncated_cc_count_is_rejected_not_clamped() {
        // Declare 31 triplets but supply the real payload's 20.
        let mut bytes = REAL_PAYLOAD.to_vec();
        bytes[8] = 0xC0 | 31;
        let payload = SeiPayload::UserDataRegistered {
            country_code: u16::from(bytes[0]),
            data: &bytes[1..],
        };
        assert_eq!(cc_data_from_sei(&payload), None);
    }

    #[test]
    fn never_exceeds_the_five_bit_count_bound() {
        // Every reachable cc_count, fully supplied, stays within the bound.
        for count in 0..=31usize {
            let mut cc = vec![0xC0 | u8::try_from(count).expect("0..=31"), 0xFF];
            cc.resize(2 + count * 3, 0x00);
            let got = cc_data_triplets(&cc).expect("well-formed cc_data");
            assert_eq!(got.len(), count * 3);
            assert!(got.len() <= MAX_CC_DATA_BYTES);
        }
    }

    #[test]
    fn empty_and_short_inputs_do_not_panic() {
        assert_eq!(cc_data_triplets(&[]), None);
        assert_eq!(cc_data_triplets(&[0xC0]), None);
        assert_eq!(cc_data_after_identifier(&[]), None);
        assert_eq!(cc_data_after_identifier(&[0x47, 0x41]), None);
    }
}
