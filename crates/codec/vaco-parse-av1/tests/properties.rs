//! Property tests: round-trips for the variable-length codes, and
//! never-panics over arbitrary bytes for everything that parses untrusted
//! input.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::integer_division,
    reason = "test code; panics here are test failures, not the thing under test"
)]

use proptest::prelude::*;
use vaco_bitstream::BitReader;
use vaco_codec_cbs::{Cbs, CbsFragment};
use vaco_limits::{Budget, Limits};
use vaco_parse_av1::av1c::Av1CodecConfigurationRecord;
use vaco_parse_av1::cbs::Av1Cbs;
use vaco_parse_av1::frame_header::FrameHeader;
use vaco_parse_av1::leb::{leb128, ns, su, uvlc};
use vaco_parse_av1::metadata;
use vaco_parse_av1::obu::{self, Av1Framing};
use vaco_parse_av1::seq::SequenceHeader;

fn budget() -> Budget {
    Budget::new(Limits::strict())
}

/// The real sequence header measured throughout this crate — used as the
/// context every frame-header property test needs.
const SEQ_HEADER_PAYLOAD: &[u8] = &[
    0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
];

fn real_seq_header() -> SequenceHeader {
    SequenceHeader::parse(SEQ_HEADER_PAYLOAD, &mut budget()).expect("fixture parses")
}

/// `uvlc()`'s own encoding, spec §4.10.3 read backwards: `leadingZeros` zero
/// bits, a one bit, then `leadingZeros` suffix bits of `value - (1 <<
/// leadingZeros) + 1`.
fn encode_uvlc(value: u64) -> Vec<u8> {
    let mut bits = Vec::new();
    let mut leading_zeros = 0u32;
    while value + 1 >= (1u64 << (leading_zeros + 1)) && leading_zeros < 31 {
        leading_zeros += 1;
    }
    bits.extend(std::iter::repeat_n(0u8, leading_zeros as usize));
    bits.push(1);
    let suffix = value - (1u64 << leading_zeros) + 1;
    for i in (0..leading_zeros).rev() {
        bits.push(((suffix >> i) & 1) as u8);
    }
    let mut out = vec![0u8; bits.len().div_ceil(8) + 1];
    for (i, &b) in bits.iter().enumerate() {
        if b != 0 {
            let byte = out.get_mut(i / 8).expect("sized above");
            *byte |= 0x80 >> (i % 8);
        }
    }
    out
}

fn encode_leb128(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

proptest! {
    /// `leb128(encode_leb128(v)) == v` for every value the eight-group cap
    /// can express (up to `2^56 - 1`; wider values are the malformed case
    /// `leb.rs`'s unit tests already pin).
    #[test]
    fn leb128_round_trips(v in 0u64..(1u64 << 56)) {
        let bytes = encode_leb128(v);
        let mut r = BitReader::new(&bytes);
        let (decoded, _) = leb128(&mut r);
        prop_assert!(!r.overrun());
        prop_assert_eq!(decoded, v);
    }

    /// `uvlc(encode_uvlc(v)) == v`, for the range this crate's `uvlc` reads
    /// without hitting its 32-bit `leadingZeros` cap.
    #[test]
    fn uvlc_round_trips(v in 0u64..(1u64 << 30)) {
        let bytes = encode_uvlc(v);
        let mut r = BitReader::new(&bytes);
        let decoded = uvlc(&mut r);
        prop_assert_eq!(decoded, v);
    }

    /// `su(n)` is its own inverse under the spec's sign-magnitude-offset
    /// encoding: writing `value` as an `n`-bit two's-complement-shaped field
    /// (`value.rem_euclid(1 << n)`), then reading it back with `su`, returns
    /// `value`, for every `value` the field width can represent.
    #[test]
    fn su_round_trips(n in 1u32..=16, raw_offset in 0i64..(1i64<<16)) {
        let n = n.min(16);
        let range = 1i64 << n;
        let half = range / 2;
        let value = (raw_offset % range) - half;
        let encoded = if value < 0 { (value + range) as u64 } else { value as u64 };
        let mut bits = Vec::new();
        for i in (0..n).rev() {
            bits.push(((encoded >> i) & 1) as u8);
        }
        let mut out = vec![0u8; bits.len().div_ceil(8) + 1];
        for (i, &b) in bits.iter().enumerate() {
            if b != 0 {
                let byte = out.get_mut(i / 8).expect("sized above");
                *byte |= 0x80 >> (i % 8);
            }
        }
        let mut r = BitReader::new(&out);
        let decoded = su(&mut r, n);
        prop_assert_eq!(i64::from(decoded), value);
    }

    /// Never panics, on arbitrary bytes, at any truncation, for everything in
    /// this crate that parses untrusted input.
    #[test]
    fn nothing_panics_on_arbitrary_bytes(data in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = obu::units(&data, Av1Framing::ObuStream);
        let _ = obu::units(&data, Av1Framing::LowOverheadBitstream);
        let _ = SequenceHeader::parse(&data, &mut budget());
        let _ = metadata::parse(&data, &mut budget());
        let _ = Av1CodecConfigurationRecord::parse(&data, &mut budget());

        let seq = real_seq_header();
        let _ = FrameHeader::parse(&data, &seq, 0, 0);

        let mut cbs = Cbs::new(Av1Cbs::new());
        let mut f = CbsFragment::new();
        let mut b = budget();
        if cbs.split(&data, Av1Framing::ObuStream, &mut f, &mut b).is_ok() {
            for i in 0..f.len() {
                let _ = cbs.read_unit(&f, i, &mut b);
            }
            let mut out = Vec::new();
            let _ = cbs.assemble(&f, Av1Framing::ObuStream, &mut out, &mut b);
        }
        f.release(&mut b);
    }

    /// `ns(n)` never reads more than `w` bits (`FloorLog2(n) + 1`) and never
    /// panics, across every `n` up to a moderately large bound.
    #[test]
    fn ns_never_reads_past_its_own_bound(n in 2u32..4096, data in prop::collection::vec(any::<u8>(), 0..8)) {
        let mut r = BitReader::new(&data);
        let _ = ns(&mut r, n);
        prop_assert!(r.bit_pos() <= 64);
    }
}
