//! The Configuration Record's CRC (RFC 9043 §4.3.2/§4.9.3).
//!
//! `Vaco-Spec-Ref: rfc9043 RFC 9043 §4.9.3 ("the standard IEEE CRC polynomial
//! (0x104C11DB7)... initial value 0, without pre-inversion, and without
//! post-inversion")`.
//!
//! The RFC names the polynomial and the two things this variant omits
//! (compared to the everyday zlib/gzip CRC-32) but gives no pseudocode, and
//! "standard IEEE CRC polynomial" is ambiguous between the two common bit
//! orderings (reflected, as zlib/gzip/PNG use; and non-reflected, as
//! MPEG-2's CRC-32 uses) — both use the same generator polynomial. This was
//! settled by measurement, not by picking the more familiar one: a real
//! `ffmpeg 8.1` Matroska `CodecPrivate` blob's own trailing 4 bytes bring the
//! **non-reflected** (MSB-first per byte) table, run with `init = 0` and no
//! final complement, to exactly 0 — the reflected variant this module
//! originally implemented gave a nonzero, meaningless value on the same real
//! bytes. `Vaco-Spec-Ref: rfc9043 blackbox: a real ffmpeg-encoded FFV1
//! Configuration Record's own CRC, which must and does check out under the
//! non-reflected table` (see `provenance/vaco-codec-ffv1.toml`).

/// The non-reflected (MSB-first) CRC-32 table for the generator polynomial
/// `0x04C11DB7` (RFC 9043's `0x104C11DB7` with its degree-32 leading bit
/// dropped).
fn table() -> [u32; 256] {
    std::array::from_fn(|i| {
        let mut c = u32::try_from(i).unwrap_or(0) << 24;
        let mut j = 0;
        while j < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
            j += 1;
        }
        c
    })
}

/// Compute the CRC over `data`, continuing from `crc` (pass `0` for a fresh
/// computation — RFC 9043's "initial value 0").
#[must_use]
pub(crate) fn crc32_ffv1_continue(crc: u32, data: &[u8]) -> u32 {
    let t = table();
    let mut c = crc;
    for &b in data {
        let idx = (((c >> 24) ^ u32::from(b)) & 0xFF) as usize;
        c = t.get(idx).copied().unwrap_or(0) ^ (c << 8);
    }
    c
}

/// A fresh CRC over `data` (RFC 9043 §4.9.3: init 0, no post-inversion).
#[must_use]
pub(crate) fn crc32_ffv1(data: &[u8]) -> u32 {
    crc32_ffv1_continue(0, data)
}

/// The 4 parity bytes to append to `data` so that `crc32_ffv1(data ++
/// parity) == 0` — "equivalent to storing the CRC remainder in the 32-bit
/// parity" (RFC 9043 §4.3.2/§4.9.3).
///
/// Big-endian: what continues this non-reflected register to exactly 0,
/// verified directly by [`crate_tests::self_consistency`] rather than
/// assumed.
#[must_use]
pub(crate) fn crc32_ffv1_parity(data: &[u8]) -> [u8; 4] {
    crc32_ffv1(data).to_be_bytes()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code exercising the module, not the untrusted-input surface the lint protects"
)]
mod crate_tests {
    use super::*;

    #[test]
    fn self_consistency() {
        // The property a from-scratch CRC implementation can check against
        // itself without recalling a reference vector: appending the data's
        // own checksum (in the byte order crc32_ffv1_parity picks) must make
        // the whole buffer's checksum 0 again — an oracle that would be
        // exactly as wrong as the implementation if it just re-ran the same
        // computation, except this checks a *different* invariant (that the
        // parity bytes chosen actually zero the running register), not the
        // same arithmetic twice.
        for data in [
            &b""[..],
            &b"a"[..],
            &b"RFC 9043"[..],
            &[0u8; 64][..],
            &[0xFFu8; 37][..],
        ] {
            let parity = crc32_ffv1_parity(data);
            let mut whole = data.to_vec();
            whole.extend_from_slice(&parity);
            assert_eq!(crc32_ffv1(&whole), 0, "data={data:?}");
        }
    }

    #[test]
    fn nonzero_for_arbitrary_input() {
        // Not a strong correctness check (a wrong polynomial would also
        // produce nonzero output), just confirms this isn't a stub returning
        // a constant.
        assert_ne!(crc32_ffv1(b"hello"), crc32_ffv1(b"world"));
    }

    /// The measurement that settled reflected-vs-non-reflected: a real
    /// `ffmpeg`-produced Configuration Record's own trailing CRC must check
    /// out under this module's algorithm.
    #[test]
    fn real_ffmpeg_configuration_record_crc_checks_out() {
        let data = include_bytes!("../tests/fixtures/yuv420_extradata.bin");
        assert_eq!(crc32_ffv1(data), 0);
    }
}
