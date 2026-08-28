//! BER length encoding (SMPTE ST 336 §7), the write-side counterpart of
//! `vaco-demux-mxf::ber`'s decoder. Definite forms only, same as that crate
//! reads.
//!
//! # Two conventions, not one — measured across two real fixtures
//!
//! A real `ffmpeg -f mxf` file does **not** use one BER-length convention
//! throughout. Walking every KLV in `bitexact1.mxf` (single-track) and a
//! freshly generated two-track fixture (`twotrack_bitexact.mxf`) by
//! decoding each length prefix directly found a consistent split:
//!
//! - **[`encode`] (this crate's fixed-width form)** is what a real file
//!   actually uses for: the Partition Pack family (header/body/footer,
//!   confirmed `cmp`-identical including this field), the Fill Item, the
//!   Generic Container System Item, essence elements, the Index Table
//!   Segment, and — measured, not assumed — every essence *descriptor*
//!   class (`MPEGVideoDescriptor` 0x51, `AES3PCMDescriptor` 0x47; D-10's
//!   `CDCIEssenceDescriptor` 0x28 not independently re-checked but grouped
//!   with the others on the strength of the pattern). All of these showed
//!   a `0x83 + 3 bytes` prefix even where the value would fit in fewer
//!   bytes (e.g. `MPEGVideoDescriptor`, value `291`, real bytes
//!   `83 00 01 23` — minimal would be `82 01 23`).
//! - **[`encode_minimal`] (short form when possible, else the smallest
//!   long form)** is what a real file uses for everything else in the
//!   structural-metadata graph: the Primer Pack (measured: `82 07 10` for
//!   value `1808`), `Preface`/`Identification`/`ContentStorage`/
//!   `MaterialPackage`/`SourcePackage`/`Track`/`Sequence`/`SourceClip`/
//!   `TimecodeComponent`/`MultipleDescriptor` (every one of these, across
//!   both fixtures, used a single short-form byte when the value was under
//!   128, or the minimal long form otherwise — e.g. `MultipleDescriptor`,
//!   value `96`, real byte `60`, not `83 00 00 60`), and the Random Index
//!   Pack (measured: one byte, value `40`).
//!
//! So the earlier framing of this ("the Primer Pack diverges, not yet
//! individually re-verified whether the rest does too") undersold it in
//! one direction and oversold it in another: it is not Primer-Pack-specific
//! (most of the structural graph shares its convention), but it is also
//! not universal (the descriptor classes and everything essence/partition/
//! index-shaped keeps the fixed-width form this crate already had right).
//! `klv::write_structural_set` is the write-side switch between the two,
//! keyed on the same class byte `ul::structural_set_key` already encodes.

/// Longest a BER length prefix this crate writes can be: one marker byte
/// plus eight value bytes (enough for any `u64`, matching the read side's
/// own `MAX_ENCODED_LEN`).
const MAX_ENCODED_LEN: usize = 9;

/// A definite-form BER length, encoded into a fixed, non-allocating buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EncodedLen {
    buf: [u8; MAX_ENCODED_LEN],
    len: u8,
}

impl EncodedLen {
    #[must_use]
    pub(crate) fn as_slice(&self) -> &[u8] {
        self.buf.get(..usize::from(self.len)).unwrap_or(&[])
    }
}

/// Encode `value` as a fixed-width, 4-value-byte long form (`0x83` + 3
/// bytes) when it fits (up to 16 MiB minus one), and an 8-value-byte long
/// form (`0x88` + 8 bytes) otherwise.
///
/// This is the convention a real file uses for the Partition Pack family,
/// the Fill Item, the System Item, essence elements, the Index Table
/// Segment, and every essence descriptor class — see the module docs for
/// the measurement. Essence elements can legitimately exceed 16 MiB (a
/// large uncompressed frame), so those widen to the 8-byte form rather than
/// silently truncating — [`vaco-demux-mxf::ber::decode`] accepts either
/// width. (A real `-f mxf_opatom` file was observed always using the
/// 8-byte form for its one clip-wrapped essence element regardless of
/// size, not only past 16 MiB — this crate does not reproduce that
/// unconditional widening, a smaller, separate divergence not chased this
/// session.)
#[must_use]
pub(crate) fn encode(value: u64) -> EncodedLen {
    if value < 0x0100_0000 {
        let be = (value as u32).to_be_bytes();
        let mut buf = [0u8; MAX_ENCODED_LEN];
        buf[0] = 0x83;
        buf[1] = be[1];
        buf[2] = be[2];
        buf[3] = be[3];
        return EncodedLen { buf, len: 4 };
    }
    let be = value.to_be_bytes();
    let mut buf = [0u8; MAX_ENCODED_LEN];
    buf[0] = 0x88; // 8 more bytes follow.
    buf[1..9].copy_from_slice(&be);
    EncodedLen { buf, len: 9 }
}

/// Encode `value` in the minimal number of bytes: BER short form (a single
/// byte, `0x00..=0x7F`) below 128, otherwise the long form with the fewest
/// value bytes that can hold it (never more than 8). This is the
/// convention a real file uses for the Primer Pack, most of the
/// structural-metadata graph, and the Random Index Pack — see the module
/// docs for the measurement.
#[must_use]
pub(crate) fn encode_minimal(value: u64) -> EncodedLen {
    if value < 0x80 {
        let mut buf = [0u8; MAX_ENCODED_LEN];
        buf[0] = value as u8;
        return EncodedLen { buf, len: 1 };
    }
    let be = value.to_be_bytes();
    // How many of the 8 big-endian bytes are actually needed: the first
    // nonzero one and everything after it. `value >= 0x80` here, so at
    // least one byte is always needed and this loop always finds one.
    let first_nonzero = be.iter().position(|&b| b != 0).unwrap_or(7);
    let n = 8 - first_nonzero;
    let mut buf = [0u8; MAX_ENCODED_LEN];
    buf[0] = 0x80 | (n as u8);
    if let (Some(dst), Some(src)) = (buf.get_mut(1..=n), be.get(first_nonzero..)) {
        dst.copy_from_slice(src);
    }
    EncodedLen {
        buf,
        len: (1 + n) as u8,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_small_value_in_the_fixed_four_byte_long_form() {
        assert_eq!(encode(104).as_slice(), &[0x83, 0x00, 0x00, 0x68]);
    }

    #[test]
    fn widens_to_eight_bytes_past_sixteen_mebibytes() {
        let enc = encode(0x0100_0000);
        assert_eq!(enc.as_slice()[0], 0x88);
        assert_eq!(enc.as_slice().len(), 9);
    }

    #[test]
    fn round_trips_through_a_reimplementation_of_the_decode_side_rule() {
        for v in [0u64, 1, 127, 128, 255, 65535, 1 << 20, 1 << 25] {
            let enc = encode(v);
            assert_eq!(decode_shim(enc.as_slice()), v);
        }
    }

    #[test]
    fn minimal_form_matches_the_measured_shapes() {
        // Primer Pack, `bitexact1.mxf`: value 1808 -> `82 07 10`.
        assert_eq!(encode_minimal(1808).as_slice(), &[0x82, 0x07, 0x10]);
        // MultipleDescriptor, `twotrack_bitexact.mxf`: value 96 -> `60`.
        assert_eq!(encode_minimal(96).as_slice(), &[0x60]);
        // Random Index Pack, `bitexact1.mxf`: value 40 -> `28`.
        assert_eq!(encode_minimal(40).as_slice(), &[0x28]);
        // The short/long-form boundary itself.
        assert_eq!(encode_minimal(127).as_slice(), &[0x7f]);
        assert_eq!(encode_minimal(128).as_slice(), &[0x81, 0x80]);
    }

    #[test]
    fn minimal_form_round_trips_through_the_decode_side_rule() {
        for v in [0u64, 1, 79, 127, 128, 255, 256, 65535, 1 << 20, 1 << 25, u64::MAX] {
            let enc = encode_minimal(v);
            assert_eq!(decode_shim_either_form(enc.as_slice()), v);
        }
    }

    proptest::proptest! {
        /// The property the coordinating dispatch specifically asked for
        /// while this file was open again: every `u64` this encoder can be
        /// given round-trips through a from-scratch reimplementation of
        /// BER's own decode rule, for *both* encodings. This crate's own
        /// BER encoder has already had one real bug this package (an
        /// 8-byte value written under a marker declaring only 7 more
        /// bytes), caught by the narrower unit test above rather than by a
        /// property test — this widens the same kind of check to every
        /// value, not just the fixed short list already covered.
        #[test]
        fn every_value_round_trips_through_both_encodings(v: u64) {
            let fixed = encode(v);
            proptest::prop_assert_eq!(decode_shim_either_form(fixed.as_slice()), v);
            let minimal = encode_minimal(v);
            proptest::prop_assert_eq!(decode_shim_either_form(minimal.as_slice()), v);
            // `encode_minimal` is never wider than `encode` for the same
            // value -- that is the entire point of the function existing.
            proptest::prop_assert!(minimal.as_slice().len() <= fixed.as_slice().len());
        }
    }

    // A from-scratch reimplementation of BER's own decode rule (not a
    // dependency on the sibling crate's private `ber` module from a unit
    // test), just enough to prove the encoder's bytes decode back to the
    // same value.
    fn decode_shim(bytes: &[u8]) -> u64 {
        let b0 = bytes[0];
        assert!(b0 & 0x80 != 0, "this encoder always uses the long form");
        let n = usize::from(b0 & 0x7f);
        let mut buf = [0u8; 8];
        buf[8 - n..].copy_from_slice(&bytes[1..=n]);
        u64::from_be_bytes(buf)
    }

    // As `decode_shim`, but also accepts the short form (`encode_minimal`
    // can produce either).
    fn decode_shim_either_form(bytes: &[u8]) -> u64 {
        let b0 = bytes[0];
        if b0 & 0x80 == 0 {
            return u64::from(b0);
        }
        decode_shim(bytes)
    }
}
