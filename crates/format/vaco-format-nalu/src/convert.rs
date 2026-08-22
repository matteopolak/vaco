//! Converting between the two framings.
//!
//! Written from ITU-T H.264 Annex B and ISO/IEC 14496-15 §5.3.3.
//!
//! # Why this is not a bitstream filter
//!
//! `h264_mp4toannexb` is a bitstream filter, and one will eventually live in
//! `vaco-bsf-h2645` (plan 15 §6.1). It will be a thin thing, because everything
//! it does that is *framing* is here and everything else it does — deciding
//! which parameter sets to splice in front of which access unit, and rewriting
//! extradata — is codec knowledge that belongs with the codec. Keeping the
//! framing conversion at this layer means the demuxer that has to do the same
//! job inline, without instantiating a filter chain, does not need one.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::framing::{Framing, LengthSize, units};

/// Rewrite a length-prefixed sample as an Annex B byte stream, appending to
/// `out`.
///
/// Every unit is emitted with a four-byte start code. Three would be legal and
/// one byte shorter, but four is what every producer writes and what MPEG-TS
/// packetisers expect to find, and the difference is not worth a knob.
///
/// Stops at the first malformed length prefix, exactly as [`units`] does,
/// returning how many *units* were emitted rather than an error: a truncated
/// final unit in an otherwise good sample is a damaged file, not an unusable
/// one, and the caller decides whether to care.
///
/// # Errors
///
/// [`Error::LimitExceeded`] if the output would exceed the budget.
pub fn length_prefixed_to_annexb(
    sample: &[u8],
    length_size: LengthSize,
    out: &mut Vec<u8>,
    budget: &mut Budget,
) -> Result<usize> {
    // The output is never longer than the input plus three bytes per unit, and
    // a unit costs at least `length_size` bytes of input, so this bounds it.
    let worst = (sample.len() as u64).saturating_add(
        (sample.len() as u64)
            .div_ceil(length_size.len() as u64)
            .saturating_mul(4),
    );
    budget.charge(worst)?;

    let mut count = 0usize;
    for nal in units(sample, Framing::LengthPrefixed(length_size)) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal.data);
        count += 1;
    }
    Ok(count)
}

/// Rewrite an Annex B byte stream as a length-prefixed sample, appending to
/// `out`.
///
/// # The failure this can actually hit
///
/// A NAL unit longer than `length_size` can express is not representable, and
/// silently truncating the length is how a file gets written that no decoder can
/// read. With a one- or two-byte length that is a realistic input, not a
/// pathological one, so it is [`Error::InvalidData`] rather than a debug
/// assertion.
///
/// # Errors
///
/// [`Error::InvalidData`] if a unit is too long for the prefix width;
/// [`Error::LimitExceeded`] if the output would exceed the budget.
pub fn annexb_to_length_prefixed(
    stream: &[u8],
    length_size: LengthSize,
    out: &mut Vec<u8>,
    budget: &mut Budget,
) -> Result<usize> {
    // Each unit costs at least three bytes of input (its start code) and gains
    // at most `length_size` bytes of prefix in exchange.
    let worst = (stream.len() as u64).saturating_add(
        (stream.len() as u64)
            .div_ceil(3)
            .saturating_mul(length_size.len() as u64),
    );
    budget.charge(worst)?;

    let max = length_size.max_unit_len();
    let mut count = 0usize;
    for nal in units(stream, Framing::AnnexB) {
        if nal.data.len() as u64 > max {
            return Err(Error::InvalidData(
                "NAL unit is longer than the length prefix can express",
            ));
        }
        let len = nal.data.len() as u32;
        match length_size.get() {
            1 => out.push(len as u8),
            2 => out.extend_from_slice(&(len as u16).to_be_bytes()),
            _ => out.extend_from_slice(&len.to_be_bytes()),
        }
        out.extend_from_slice(nal.data);
        count += 1;
    }
    Ok(count)
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
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    #[test]
    fn round_trips_through_both_directions() {
        let annexb = [
            0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1E, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80, 0, 0, 0, 1, 0x65,
            0x88,
        ];
        let mut b = budget();
        let mut avcc = Vec::new();
        let n = annexb_to_length_prefixed(&annexb, LengthSize::FOUR, &mut avcc, &mut b).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&avcc[..4], &[0, 0, 0, 4]);

        let mut back = Vec::new();
        let m = length_prefixed_to_annexb(&avcc, LengthSize::FOUR, &mut back, &mut b).unwrap();
        assert_eq!(m, 3);
        // Not byte-identical to the input: the three-byte start code became
        // four. The *units* are what must survive.
        let orig: Vec<&[u8]> = units(&annexb, Framing::AnnexB).map(|n| n.data).collect();
        let round: Vec<&[u8]> = units(&back, Framing::AnnexB).map(|n| n.data).collect();
        assert_eq!(orig, round);
    }

    #[test]
    fn a_unit_too_long_for_the_prefix_is_rejected() {
        let mut stream = vec![0, 0, 0, 1];
        stream.extend(std::iter::repeat_n(0xAAu8, 300));
        let mut b = budget();
        let mut out = Vec::new();
        let err =
            annexb_to_length_prefixed(&stream, LengthSize::ONE, &mut out, &mut b).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn a_truncated_prefix_ends_the_conversion_without_erroring() {
        // Declares five bytes, supplies two.
        let sample = [0, 0, 0, 2, 0x67, 0xAA, 0, 0, 0, 5, 0x68];
        let mut b = budget();
        let mut out = Vec::new();
        let n = length_prefixed_to_annexb(&sample, LengthSize::FOUR, &mut out, &mut b).unwrap();
        assert_eq!(n, 1);
        assert_eq!(out, vec![0, 0, 0, 1, 0x67, 0xAA]);
    }

    #[test]
    fn an_empty_input_produces_nothing() {
        let mut b = budget();
        let mut out = Vec::new();
        assert_eq!(
            length_prefixed_to_annexb(&[], LengthSize::FOUR, &mut out, &mut b).unwrap(),
            0
        );
        assert_eq!(
            annexb_to_length_prefixed(&[], LengthSize::FOUR, &mut out, &mut b).unwrap(),
            0
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_tiny_budget_refuses_before_allocating() {
        let mut b = Budget::new(Limits::tiny());
        let mut out = Vec::new();
        let big = vec![0u8; 1 << 20];
        assert!(matches!(
            length_prefixed_to_annexb(&big, LengthSize::ONE, &mut out, &mut b),
            Err(Error::LimitExceeded { .. })
        ));
        assert!(out.is_empty());
    }
}
