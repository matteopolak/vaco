//! `leb128()` and `uvlc()`, AV1 spec §4.10.5 and §4.10.3.
//!
//! Neither is Exp-Golomb: `leb128()` is a byte-oriented little-endian varint
//! (the same shape WebAssembly and protobuf use) and `uvlc()` is a unary
//! prefix followed by a fixed-width suffix, closer to Elias gamma than to
//! `ue(v)`. `vaco-bitstream` supplies neither — its `golomb` module is
//! specifically H.26x's `ue(v)`/`se(v)` — so both live here, built on
//! [`BitReader`]'s primitives. If a second byte-oriented codec (VP9's frame
//! marker, say) ever wants `leb128()`, it is a two-line function worth lifting
//! into `vaco-bitstream` at that point; one caller does not justify moving it
//! yet.
//!
//! Both use the reader's sticky-overrun model: a malformed or truncated code
//! flags the reader via [`BitReader::flag_malformed`] and returns a value that
//! is safe to keep computing with (never used to size an allocation without a
//! budget check first).

use vaco_bitstream::BitReader;

/// `leb128()`, §4.10.5: up to eight base-128 groups, least significant first,
/// each gated by a continuation bit in position 7.
///
/// Returns `(value, bytes_consumed)`. The specification bounds `leb128_bytes`
/// at 8, which already keeps `value` within `u64`; a ninth continuation bit
/// (only reachable on non-conforming input) flags the reader rather than
/// reading further, so a hostile stream of `0xFF` bytes cannot turn this into
/// an unbounded loop.
pub fn leb128(r: &mut BitReader<'_>) -> (u64, u32) {
    let mut value: u64 = 0;
    let mut bytes = 0u32;
    for i in 0..8u32 {
        let byte = r.get(8);
        bytes += 1;
        value |= u64::from(byte & 0x7f) << (i * 7);
        if byte & 0x80 == 0 {
            return (value, bytes);
        }
    }
    // A ninth group: not representable in the spec's own encoding. Flag rather
    // than loop further — `get(8)` already advanced the reader, so leaving the
    // loop here is enough to stop it costing more than eight reads.
    r.flag_malformed();
    (value, bytes)
}

/// `uvlc()`, §4.10.3: a unary run of zero-bits terminated by a one (the prefix,
/// `leadingZeros` long), then that many bits of suffix, offset so the whole
/// thing is injective.
///
/// The specification caps `leadingZeros` at 32 and defines the value at that
/// boundary as `(1 << 32) - 1` without reading a suffix at all — reproduced
/// here exactly, including the cap, so a run of zero-bits longer than the
/// specification permits cannot become an unbounded read either.
pub fn uvlc(r: &mut BitReader<'_>) -> u64 {
    let mut leading_zeros: u32 = 0;
    loop {
        if r.get_bit() != 0 {
            break;
        }
        leading_zeros += 1;
        if leading_zeros >= 32 {
            return (1u64 << 32) - 1;
        }
    }
    let value = u64::from(r.get(leading_zeros));
    value + (1u64 << leading_zeros) - 1
}

/// `su(n)`, §4.10.6: an `n`-bit unsigned value read as sign-and-magnitude
/// through [`BitReader::get_signed`]'s two's-complement path would be wrong —
/// AV1's `su` is `value - (1 << (n-1))` when the top bit is set, which is
/// sign-magnitude-*offset*, not two's complement. `n` must be at least 1.
pub fn su(r: &mut BitReader<'_>, n: u32) -> i32 {
    let n = n.max(1);
    let value = i64::from(r.get(n));
    let sign_mask = 1i64 << (n - 1);
    let out = if value & sign_mask != 0 {
        value - 2 * sign_mask
    } else {
        value
    };
    // `n <= 32` (the reader's own `get` ceiling), so `out` fits in an `i32`
    // whichever branch above produced it.
    out as i32
}

/// `ns(n)`, §4.10.7: a non-symmetric code for a value in `0..n`. Named after
/// the specification's own pseudocode (`w`, `m`, `v`), which this mirrors
/// line for line — see the module-level citation.
#[allow(
    clippy::many_single_char_names,
    reason = "these are the specification's own variable names for ns(n); renaming them would make \
              the code harder, not easier, to check against §4.10.7"
)]
pub fn ns(r: &mut BitReader<'_>, n: u32) -> u32 {
    if n <= 1 {
        return 0;
    }
    // `FloorLog2(n) + 1`. `u32::ilog2` is exact for every `n >= 1`.
    let w = n.ilog2() + 1;
    let m = (1u32 << w).wrapping_sub(n);
    let v = r.get(w.saturating_sub(1));
    if v < m {
        return v;
    }
    let extra_bit = r.get_bit();
    (v << 1).wrapping_sub(m).wrapping_add(extra_bit)
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
    fn leb128_single_byte() {
        let mut r = BitReader::new(&[0x0b]);
        assert_eq!(leb128(&mut r), (11, 1));
    }

    #[test]
    fn leb128_multi_byte() {
        // 300 = 0b1_0010_1100 -> groups: 0101100 (with continuation), 0000010
        let mut r = BitReader::new(&[0xAC, 0x02]);
        assert_eq!(leb128(&mut r), (300, 2));
    }

    #[test]
    fn leb128_caps_at_eight_bytes() {
        let data = [0xFFu8; 9];
        let mut r = BitReader::new(&data);
        let (_, bytes) = leb128(&mut r);
        assert_eq!(bytes, 8);
        assert!(r.overrun(), "a ninth continuation bit is flagged");
    }

    #[test]
    fn uvlc_zero() {
        // done=1 immediately -> leadingZeros=0 -> value=0
        let mut r = BitReader::new(&[0b1000_0000]);
        assert_eq!(uvlc(&mut r), 0);
    }

    #[test]
    fn uvlc_small_values() {
        // 1 leading zero, then 1 suffix bit: 0 1 <bit>
        let mut r = BitReader::new(&[0b0111_0000]);
        // bits: 0(zero) 1(done) 1(suffix=1) -> value = 1 + (1<<1) - 1 = 2
        assert_eq!(uvlc(&mut r), 2);
    }

    #[test]
    fn uvlc_never_hangs_on_all_zero_input() {
        let data = [0u8; 64];
        let mut r = BitReader::new(&data);
        let v = uvlc(&mut r);
        assert_eq!(v, (1u64 << 32) - 1);
    }

    #[test]
    fn su_reads_offset_sign_magnitude() {
        // n=4: values 0..7 positive, 8..15 map to -(v - 8) i.e. v=8 -> 0? check spec:
        // value=f(n); signMask=1<<(n-1)=8; if value&8 !=0 -> value-2*8
        let mut r = BitReader::new(&[0b1001_0000]); // f(4) = 0b1001 = 9
        assert_eq!(su(&mut r, 4), 9 - 16);
    }

    #[test]
    fn ns_within_range_never_reads_the_extra_bit() {
        // n=3: w = floor(log2(3))+1 = 1+1 = 2; m = 4-3=1
        // v = f(1). If v(=0) < m(=1) return v=0, consuming exactly 1 bit.
        let mut r = BitReader::new(&[0b0111_1111]);
        assert_eq!(ns(&mut r, 3), 0);
        assert_eq!(r.bit_pos(), 1);
    }
}
