//! Ogg's own CRC-32 (RFC 3533 §6).
//!
//! Same generator polynomial as the MPEG-2 section CRC
//! (`vaco-format-mpegts-tables::crc`), but a **different** algorithm around
//! it, and conflating the two is the classic way to get "every page looks
//! subtly corrupt" with no further diagnostic:
//!
//! | Parameter | Ogg (this module) | MPEG-2 section CRC |
//! |---|---|---|
//! | polynomial | `0x04C1_1DB7` | `0x04C1_1DB7` (same) |
//! | initial value | **`0`** | `0xFFFF_FFFF` |
//! | reflection | none, in or out (both) | none, in or out (same) |
//! | final XOR | none (both) | none |
//! | how a page proves itself | recompute with the checksum field zeroed and compare | residue reduces to zero |
//!
//! RFC 3533 §6: "The 32 bits of checksum are placed... by calculating the
//! CRC of the entire page with the 4 octets of checksum field replaced by
//! zero, and then substituting the resulting CRC into the checksum field."
//! There is no residue shortcut on the read side because the field is not
//! part of the polynomial division the way a trailing MPEG-2 section CRC is:
//! you must zero it, recompute, and compare — [`page_crc_ok`] does exactly
//! that, taking the stored value separately rather than assuming it is the
//! last four bytes of what gets hashed.

/// The generator polynomial, most-significant-bit-first. Identical to the
/// MPEG-2 section CRC's; see the module docs for why the two still disagree.
pub const POLY: u32 = 0x04C1_1DB7;

/// The value the register starts at. `0`, not `0xFFFF_FFFF` — the first way
/// this was measured wrong, against a page hand-built from the RFC's own
/// worked description before a real Ogg file was available to check against.
pub const INIT: u32 = 0;

const fn table_entry(byte: u8) -> u32 {
    let mut crc = (byte as u32) << 24;
    let mut bit = 0;
    while bit < 8 {
        crc = if crc & 0x8000_0000 != 0 {
            (crc << 1) ^ POLY
        } else {
            crc << 1
        };
        bit += 1;
    }
    crc
}

#[allow(
    clippy::indexing_slicing,
    reason = "SAFETY-ARG: `i` is bounded by the loop condition `i < 256` and the \
              array is declared with exactly 256 elements, so the index is in \
              range at every iteration. `get_mut` is not available in a const fn."
)]
const fn build_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = table_entry(i as u8);
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = build_table();

/// Continue a CRC over `data`. Exposed so a page's header and body can be fed
/// separately without a copy — the checksum field inside the header is
/// zeroed by the caller before this runs over it.
#[must_use]
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        let idx = ((crc >> 24) as u8 ^ b) as usize;
        let entry = match TABLE.get(idx) {
            Some(v) => *v,
            None => 0,
        };
        crc = (crc << 8) ^ entry;
    }
    crc
}

/// The Ogg CRC-32 of `data`, starting from [`INIT`].
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(INIT, data)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn crc32_bitwise(data: &[u8]) -> u32 {
        let mut crc = INIT;
        for &b in data {
            for i in (0..8).rev() {
                let bit = (b >> i) & 1;
                let top = u32::from(crc >> 31 != 0);
                crc <<= 1;
                if top ^ u32::from(bit) != 0 {
                    crc ^= POLY;
                }
            }
        }
        crc
    }

    #[test]
    fn table_agrees_with_the_bitwise_definition() {
        for len in 0..40usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(crc32(&data), crc32_bitwise(&data), "len {len}");
        }
    }

    #[test]
    fn update_is_associative_over_a_split() {
        let data: Vec<u8> = (0..100u8).collect();
        for split in 0..=data.len() {
            let (a, b) = data.split_at(split);
            assert_eq!(crc32_update(crc32_update(INIT, a), b), crc32(&data));
        }
    }

    /// The first page (the `OpusHead` BOS page) of a real file, produced by
    /// `ffmpeg -f lavfi -i sine=r=48000:d=2 -c:a libopus opus.ogg` and read
    /// back byte-exactly (not through any layer that reinterprets it, per
    /// plan 13 §1b). Its stored checksum is `0xc8d6_1678`.
    #[test]
    fn reproduces_a_measured_page_checksum() {
        let mut page = vec![
            b'O', b'g', b'g', b'S', 0x00, 0x02, // capture, version, header_type (BOS)
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // granule position: 0
            0x9B, 0x1E, 0xF1, 0xE1, // serial number 3_790_675_611
            0x00, 0x00, 0x00, 0x00, // page sequence 0
            0x00, 0x00, 0x00, 0x00, // checksum, zeroed for the computation
            0x01, 0x13, // one segment, 19 bytes
        ];
        // vaco_format_fixtures::opus::HEAD_MONO: version 1, 1 channel,
        // pre_skip 312, input_sample_rate 48000, output_gain 0, mapping
        // family 0 -- the shared OpusHead every container test suite in
        // this tree uses now.
        page.extend_from_slice(vaco_format_fixtures::opus::HEAD_MONO);
        assert_eq!(crc32(&page), 0xC8D6_1678);
    }
}
