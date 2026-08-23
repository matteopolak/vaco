//! RFC 8794 variable-length integers: element IDs, data sizes, and the signed
//! flavour Matroska lacing borrows for its size deltas.
//!
//! Every function here is total over its declared input space and allocates
//! nothing on the read side — the read half operates on borrowed slices so it
//! can run directly over attacker-controlled bytes without a copy.

use vaco_core::{Error, Result};

/// Longest element ID this crate will read, in octets.
///
/// RFC 8794 section 11.2.4 gives `EBMLMaxIDLength` a default and a minimum of
/// 4. A header declaring more is rejected rather than clamped: the value is
/// attacker-controlled and widening it buys nothing a caller cannot opt into
/// by reading the raw VINT functions directly.
pub const MAX_ID_LEN: u8 = 4;

/// Longest element data size this crate will read, in octets.
///
/// RFC 8794 section 6.3: eight octets expresses up to `2^56 - 2`, already 72
/// PB, and it is also the ceiling `EBMLMaxSizeLength` may declare.
pub const MAX_SIZE_LEN: u8 = 8;

/// Octet length of a VINT from its leading octet, or `None` when the octet is
/// zero and the length would exceed eight.
#[must_use]
pub const fn vint_len(first: u8) -> Option<u8> {
    if first == 0 {
        None
    } else {
        Some(first.leading_zeros() as u8 + 1)
    }
}

/// An element's data size, which RFC 8794 section 6.2 allows to be unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Known(u64),
    Unknown,
}

impl Size {
    /// The size in octets, or `None` when unknown.
    #[must_use]
    pub const fn known(self) -> Option<u64> {
        match self {
            Self::Known(n) => Some(n),
            Self::Unknown => None,
        }
    }
}

/// The all-ones bit pattern that marks "unknown" at a VINT width of `len`
/// octets (RFC 8794 section 6.2).
#[must_use]
pub const fn all_ones(len: u8) -> u64 {
    let data_bits = 7u32 * len as u32;
    if data_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << data_bits) - 1
    }
}

/// Decode an element ID from `buf`, returning it and the octets consumed.
///
/// The ID keeps its length marker — RFC 8794 section 5: "the `VINT_MARKER` and
/// `VINT_DATA` of the Element ID are used together" — so `0x1A45DFA3` is the
/// stored value, not a stripped one.
///
/// # Errors
///
/// [`Error::InvalidData`] for a zero leading octet or a length above
/// `max_id_len`, and [`Error::UnexpectedEof`] when `buf` is too short.
pub fn read_id(buf: &[u8], max_id_len: u8) -> Result<(u32, usize)> {
    let first = *buf.first().ok_or(Error::UnexpectedEof)?;
    let len = vint_len(first).ok_or(Error::InvalidData("element id longer than 8 octets"))?;
    if len > max_id_len {
        return Err(Error::InvalidData("element id longer than EBMLMaxIDLength"));
    }
    let bytes = buf
        .get(..len as usize)
        .ok_or(Error::UnexpectedEof)?
        .iter()
        .fold(0u32, |acc, &b| (acc << 8) | u32::from(b));
    Ok((bytes, len as usize))
}

/// Decode an element data size from `buf`, returning it and the octets
/// consumed.
///
/// # Errors
///
/// As [`read_id`], against `max_size_len`.
pub fn read_size(buf: &[u8], max_size_len: u8) -> Result<(Size, usize)> {
    let first = *buf.first().ok_or(Error::UnexpectedEof)?;
    let len = vint_len(first).ok_or(Error::InvalidData("element size longer than 8 octets"))?;
    if len > max_size_len {
        return Err(Error::InvalidData(
            "element size longer than EBMLMaxSizeLength",
        ));
    }
    let slice = buf.get(..len as usize).ok_or(Error::UnexpectedEof)?;
    let mut value = u64::from(first & !(0x80u8 >> (len - 1)));
    for &b in slice.iter().skip(1) {
        value = (value << 8) | u64::from(b);
    }
    let size = if value == all_ones(len) {
        Size::Unknown
    } else {
        Size::Known(value)
    };
    Ok((size, len as usize))
}

/// Decode the signed VINT that EBML lacing uses for its size deltas.
///
/// RFC 9559 section 10.3.3: the unsigned value is read as a normal VINT and
/// then `2^((7n)-1) - 1` is subtracted, where `n` is the octet length. The
/// encoding is a Matroska-specific *use* of the generic VINT this crate reads,
/// not a new grammar, which is why it lives beside [`read_size`].
///
/// # Errors
///
/// As [`read_size`]; an unknown-size marker here is [`Error::InvalidData`].
pub fn read_signed_vint(buf: &[u8]) -> Result<(i64, usize)> {
    let (size, used) = read_size(buf, MAX_SIZE_LEN)?;
    let raw = match size {
        Size::Known(v) => v,
        Size::Unknown => return Err(Error::InvalidData("lace size delta is the unknown marker")),
    };
    // 7*n - 1 <= 55 for n <= 8, so the shift and the cast are both in range.
    let bias = (1i64 << (7 * used as u32 - 1)) - 1;
    Ok((raw.cast_signed().wrapping_sub(bias), used))
}

// ------------------------------------------------------------------ writing

/// Encode `value` as an EBML VINT in exactly `len` octets (RFC 8794 section
/// 5). `len` is clamped to `1..=8`; a `value` too wide for `len` is truncated
/// by the shift, so callers that care about width should route through
/// [`vint_min`] instead.
#[must_use]
pub fn vint(value: u64, len: u8) -> Vec<u8> {
    let len = len.clamp(1, 8);
    let mut out = Vec::new();
    for i in (0..len).rev() {
        out.push(((value >> (8 * u32::from(i))) & 0xFF) as u8);
    }
    if let Some(first) = out.first_mut() {
        *first |= 0x80 >> (len - 1);
    }
    out
}

/// The shortest VINT encoding of `value` that does not collide with the
/// unknown-size marker at that width.
#[must_use]
pub fn vint_min(value: u64) -> Vec<u8> {
    for len in 1..=8u8 {
        if value < all_ones(len) {
            return vint(value, len);
        }
    }
    vint(value, 8)
}

/// The all-ones VINT that marks an element as unknown-size, in `len` octets.
#[must_use]
pub fn vint_unknown(len: u8) -> Vec<u8> {
    let len = len.clamp(1, 8);
    vint(all_ones(len), len)
}

/// An element ID, big-endian with its length marker already part of the
/// value — the encoding [`read_id`] expects back.
#[must_use]
pub fn id_bytes(id: u32) -> Vec<u8> {
    let len = match id {
        0x80..=0xFF => 1,
        0x4000..=0xFFFF => 2,
        0x20_0000..=0xFF_FFFF => 3,
        _ => 4,
    };
    id.to_be_bytes()
        .get(4 - len..)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// The signed VINT of RFC 9559 section 10.3.3: bias by `2^(7n-1) - 1`.
#[must_use]
pub fn signed_vint(value: i64) -> Vec<u8> {
    for len in 1..=8u8 {
        let bias = (1i64 << (7 * u32::from(len) - 1)) - 1;
        let Some(biased) = value.checked_add(bias) else {
            continue;
        };
        let bits = 7 * u32::from(len);
        let width_ok = if bits >= 64 {
            true
        } else {
            biased >= 0 && (biased as u64) < (1u64 << bits) - 1
        };
        if biased >= 0 && width_ok {
            return vint(biased as u64, len);
        }
    }
    vint(0, 8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn element_ids_keep_their_marker() {
        assert_eq!(
            read_id(&[0x1A, 0x45, 0xDF, 0xA3], 4).unwrap(),
            (0x1A45_DFA3, 4)
        );
        assert_eq!(read_id(&[0xA3], 4).unwrap(), (0xA3, 1));
    }

    #[test]
    fn a_zero_leading_octet_is_rejected() {
        assert!(read_id(&[0x00, 0, 0, 0, 0], 4).is_err());
        assert!(read_size(&[0x00, 0, 0, 0, 0, 0, 0, 0, 0], 8).is_err());
    }

    #[test]
    fn sizes_strip_their_marker() {
        assert_eq!(read_size(&[0x81], 8).unwrap(), (Size::Known(1), 1));
        assert_eq!(read_size(&[0x40, 0x7F], 8).unwrap(), (Size::Known(127), 2));
    }

    #[test]
    fn all_ones_is_the_unknown_marker_at_every_length() {
        for len in 1..=8u8 {
            let bytes = vint_unknown(len);
            assert_eq!(
                read_size(&bytes, 8).unwrap(),
                (Size::Unknown, usize::from(len)),
                "length {len}"
            );
        }
    }

    #[test]
    fn vint_min_round_trips() {
        for v in [0u64, 1, 126, 127, 128, 16383, 16384, u64::from(u32::MAX)] {
            let bytes = vint_min(v);
            assert_eq!(read_size(&bytes, 8).unwrap(), (Size::Known(v), bytes.len()));
        }
    }

    #[test]
    fn signed_lace_vints_round_trip() {
        for v in [
            0i64, 1, -1, 63, -63, 64, -64, 8191, -8191, 8192, -8192, 1_000_000, -1_000_000,
        ] {
            let bytes = signed_vint(v);
            let (got, used) = read_signed_vint(&bytes).unwrap();
            assert_eq!(got, v, "encoding of {v} was {bytes:02X?}");
            assert_eq!(used, bytes.len());
        }
    }

    /// RFC 9559 table 38 gives the octets for a delta of -300.
    #[test]
    fn the_rfc_lace_delta_example_decodes() {
        assert_eq!(read_signed_vint(&[0x5E, 0xD3]).unwrap(), (-300, 2));
        assert_eq!(read_size(&[0x43, 0x20], 8).unwrap(), (Size::Known(800), 2));
    }

    #[test]
    fn id_bytes_round_trips_through_read_id() {
        for id in [0x80u32, 0xA3, 0x4286, 0x1A45_DFA3, 0x1853_8067] {
            let bytes = id_bytes(id);
            assert_eq!(read_id(&bytes, 4).unwrap(), (id, bytes.len()));
        }
    }
}
